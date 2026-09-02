/**
 * @file notify.cc
 * @brief 待确认通知模块实现
 *
 * BOOT 短按：page_sync 画板显示中 → 后台任务异步 GET /api/notifications/next，
 * 有通知则把 bitmap_base64 解码进 framebuffer 并请求全刷，进入 NOTIFYING；
 * 上/下键 ack(agree|reject)；BOOT 再按或 5min 超时 dismiss，恢复画板当前页。
 *
 * 位图经 framebuffer 安全路径显示（与 page_sync::show_page 相同）：不直接调
 * DisplayRaw4ColorImage，避免与后台 refresh_task 并发抢 EPD 导致 busy 卡死。
 *
 * 并发模型：notify_init / notify_request_next / notify_is_active /
 * notify_post_ack / notify_dismiss 只从主按钮回调上下文调用（与 page_sync 一致），
 * 只有后台任务与 FreeRTOS timer 回调跨上下文，且后台任务仅在 FETCHING 下运行。
 */
#include "notify.h"

#include <esp_heap_caps.h>
#include <esp_log.h>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include <freertos/timers.h>
#include <mbedtls/base64.h>
#include <stdio.h>
#include <string.h>
#include <cJSON.h>
#include "board.h"
#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "http_client_wrapper.h"
#include "page_sync.h"
#include "server_pairing.h"

namespace {

constexpr char kTag[] = "Notify";
constexpr int kHttpTimeoutMs = 10000;
// GET 响应体 ≈ base64(30000B 位图) + JSON 元数据 ≈ 40000 + 余量
constexpr int kResponseBufSize = 45056;
// ttl 默认 300s，本地 5min 超时对齐，到点自动 dismiss
constexpr uint32_t kNotifyTimeoutMs = 5 * 60 * 1000;
constexpr size_t kFetchTaskStack = 8192;

enum class NotifyState { IDLE, FETCHING, NOTIFYING };

NotifyState s_state = NotifyState::IDLE;
TimerHandle_t s_timeout_timer = nullptr;
CustomLcdDisplay* s_lcd = nullptr;          // 懒绑定（notify 无 set_display 注入点）
char s_notification_id[40] = {0};           // uuid hex(32) + '\0'

void show_bitmap(const uint8_t* bitmap) {
    if (!s_lcd) {
        ESP_LOGW(kTag, "no display wired");
        return;
    }
    uint8_t* fb = s_lcd->GetFramebuffer();
    int fb_len = s_lcd->GetFBWidth() * s_lcd->GetFBHeight() * 2 / 8;
    if (!fb || fb_len != PAGE_BITMAP_SIZE) {
        ESP_LOGE(kTag, "framebuffer size mismatch: fb_len=%d want=%d", fb_len,
                 PAGE_BITMAP_SIZE);
        return;
    }
    // 2bpp 像素序一致（MSB-first），直接拷贝，走 EpdRefreshScheduler 统一刷新
    xSemaphoreTake(s_lcd->GetMutex(), portMAX_DELAY);
    memcpy(fb, bitmap, PAGE_BITMAP_SIZE);
    xSemaphoreGive(s_lcd->GetMutex());
    s_lcd->RequestUrgentFullRefresh();
    page_sync_stop_display();  // 通知全屏期间标记画板不再主导屏幕
}

void dismiss_locked_state() {
    s_state = NotifyState::IDLE;
    s_notification_id[0] = '\0';
    if (s_timeout_timer) {
        xTimerStop(s_timeout_timer, 0);
    }
    // 恢复画板当前页（自动轮换重置到当前页，替代原始内容快照）
    page_sync_resume_display();
    if (page_sync_is_displaying()) {
        page_sync_prev();
        page_sync_next();
    }
}

// 5min 超时：FreeRTOS timer 回调上下文（timer service task）
void timeout_timer_cb(TimerHandle_t /*timer*/) {
    ESP_LOGI(kTag, "notify timeout, auto dismiss");
    if (s_state == NotifyState::NOTIFYING) {
        dismiss_locked_state();
    }
}

// 解析 GET 响应：{"bitmap_base64": ..., "notification": {"id": ...}}
// out_bitmap 必须 ≥ PAGE_BITMAP_SIZE。成功返回 true 并填 out_id。
bool parse_next_response(const char* json, uint8_t* out_bitmap, char* out_id,
                         size_t out_id_len) {
    cJSON* root = cJSON_Parse(json);
    if (!root) {
        ESP_LOGW(kTag, "next json parse failed");
        return false;
    }
    cJSON* b64 = cJSON_GetObjectItem(root, "bitmap_base64");
    cJSON* notification = cJSON_GetObjectItem(root, "notification");
    cJSON* id = notification ? cJSON_GetObjectItem(notification, "id") : nullptr;
    if (!cJSON_IsString(b64) || !cJSON_IsString(id)) {
        ESP_LOGW(kTag, "next json missing bitmap_base64/notification.id");
        cJSON_Delete(root);
        return false;
    }
    size_t olen = 0;
    int ret = mbedtls_base64_decode(out_bitmap, PAGE_BITMAP_SIZE, &olen,
                                    reinterpret_cast<const unsigned char*>(b64->valuestring),
                                    strlen(b64->valuestring));
    if (ret != 0 || olen != PAGE_BITMAP_SIZE) {
        ESP_LOGW(kTag, "bitmap base64 decode failed: ret=%d len=%u want=%d", ret,
                 (unsigned)olen, PAGE_BITMAP_SIZE);
        cJSON_Delete(root);
        return false;
    }
    snprintf(out_id, out_id_len, "%s", id->valuestring);
    cJSON_Delete(root);
    return true;
}

// 后台任务：同步 GET /api/notifications/next，成功后切 NOTIFYING
void fetch_task(void* /*arg*/) {
    ESP_LOGI(kTag, "fetch task started");
    char device_id[32] = {0};
    if (!server_pairing_get_device_id(device_id, sizeof(device_id))) {
        ESP_LOGW(kTag, "no device_id, abort fetch");
        s_state = NotifyState::IDLE;
        vTaskDelete(nullptr);
        return;
    }
    char path[96];
    snprintf(path, sizeof(path), "/api/notifications/next?device_id=%s", device_id);
    char url[320];
    if (!server_pairing_build_endpoint(path, url, sizeof(url))) {
        ESP_LOGW(kTag, "cannot build endpoint");
        s_state = NotifyState::IDLE;
        vTaskDelete(nullptr);
        return;
    }
    char token[65] = {0};
    server_pairing_get_token(token, sizeof(token));

    // PSRAM 缓冲，避免占用任务栈
    char* buf = static_cast<char*>(heap_caps_malloc(kResponseBufSize, MALLOC_CAP_SPIRAM));
    uint8_t* bitmap = static_cast<uint8_t*>(heap_caps_malloc(PAGE_BITMAP_SIZE, MALLOC_CAP_SPIRAM));
    if (!buf || !bitmap) {
        ESP_LOGE(kTag, "alloc failed (buf=%p bitmap=%p)", buf, bitmap);
        if (buf) heap_caps_free(buf);
        if (bitmap) heap_caps_free(bitmap);
        s_state = NotifyState::IDLE;
        vTaskDelete(nullptr);
        return;
    }
    int buf_len = kResponseBufSize;
    int status = http_wrapper_get(url, token, buf, &buf_len, kHttpTimeoutMs);
    if (status == 204) {
        ESP_LOGI(kTag, "no pending notification (204)");
    } else if (status == 200) {
        buf[buf_len < kResponseBufSize ? buf_len : kResponseBufSize - 1] = '\0';
        char id[sizeof(s_notification_id)] = {0};
        if (parse_next_response(buf, bitmap, id, sizeof(id))) {
            snprintf(s_notification_id, sizeof(s_notification_id), "%s", id);
            show_bitmap(bitmap);
            s_state = NotifyState::NOTIFYING;
            xTimerStart(s_timeout_timer, 0);
            ESP_LOGI(kTag, "notify %s displaying", s_notification_id);
        } else {
            s_state = NotifyState::IDLE;
        }
    } else {
        ESP_LOGW(kTag, "next fetch failed (status=%d)", status);
        s_state = NotifyState::IDLE;
    }
    heap_caps_free(buf);
    heap_caps_free(bitmap);
    vTaskDelete(nullptr);
}

// 后台任务：POST /api/notifications/{id}/ack（fire-and-forget）
void ack_task(void* arg) {
    char* body = static_cast<char*>(arg);  // "{\"decision\":\"...\"}"
    char id[sizeof(s_notification_id)] = {0};
    snprintf(id, sizeof(id), "%s", s_notification_id);

    char path[96];
    snprintf(path, sizeof(path), "/api/notifications/%s/ack", id);
    char url[320];
    if (!server_pairing_build_endpoint(path, url, sizeof(url))) {
        ESP_LOGW(kTag, "cannot build ack endpoint");
    } else {
        char token[65] = {0};
        server_pairing_get_token(token, sizeof(token));
        char resp[256];
        int resp_len = sizeof(resp);
        int status = http_wrapper_post_json(url, token, body, resp, &resp_len,
                                            kHttpTimeoutMs);
        if (status == 200) {
            ESP_LOGI(kTag, "ack ok: %s -> %s", id, body);
        } else {
            ESP_LOGW(kTag, "ack failed: %s status=%d（通知保持 shown，5min 后服务端过期）",
                     id, status);
        }
    }
    heap_caps_free(body);
    vTaskDelete(nullptr);
}

}  // namespace

extern "C" void notify_init(void) {
    if (!s_timeout_timer) {
        s_timeout_timer = xTimerCreate("notify_to", pdMS_TO_TICKS(kNotifyTimeoutMs),
                                       pdFALSE, nullptr, timeout_timer_cb);
        if (!s_timeout_timer) {
            ESP_LOGE(kTag, "timeout timer create failed");
        }
    }
    // 与 page_sync_set_display 同一显示实例：经 Board 单例取
    if (!s_lcd) {
        s_lcd = static_cast<CustomLcdDisplay*>(Board::GetInstance().GetDisplay());
    }
    s_state = NotifyState::IDLE;
    ESP_LOGI(kTag, "notify module initialized");
}

extern "C" void notify_deinit(void) {
    if (s_timeout_timer) {
        xTimerStop(s_timeout_timer, 0);
        xTimerDelete(s_timeout_timer, 0);
        s_timeout_timer = nullptr;
    }
    s_state = NotifyState::IDLE;
    s_notification_id[0] = '\0';
}

extern "C" void notify_request_next(void) {
    if (s_state != NotifyState::IDLE) return;
    s_state = NotifyState::FETCHING;
    // 非阻塞：HTTP GET 放后台任务，按钮回调立即返回
    if (xTaskCreate(fetch_task, "notify_fetch", kFetchTaskStack, nullptr, 3,
                    nullptr) != pdPASS) {
        ESP_LOGE(kTag, "fetch task create failed");
        s_state = NotifyState::IDLE;
    }
}

extern "C" bool notify_is_active(void) { return s_state == NotifyState::NOTIFYING; }

extern "C" void notify_post_ack(const char* decision) {
    if (s_state != NotifyState::NOTIFYING) return;
    // 复制 body 交后台任务持有（decision 来自调用方栈）
    char* body = static_cast<char*>(heap_caps_malloc(64, MALLOC_CAP_SPIRAM));
    if (!body) {
        ESP_LOGE(kTag, "ack body alloc failed, dismiss without ack");
        notify_dismiss();
        return;
    }
    snprintf(body, 64, "{\"decision\":\"%s\"}", decision);
    if (xTaskCreate(ack_task, "notify_ack", kFetchTaskStack, body, 3, nullptr) != pdPASS) {
        ESP_LOGE(kTag, "ack task create failed");
        heap_caps_free(body);
    }
    // ack 无论成败都关闭展示（失败时通知保持 shown，服务端 5min ttl 过期兜底）
    notify_dismiss();
}

extern "C" void notify_dismiss(void) {
    if (s_state != NotifyState::NOTIFYING) return;
    dismiss_locked_state();
    ESP_LOGI(kTag, "notify dismissed");
}
