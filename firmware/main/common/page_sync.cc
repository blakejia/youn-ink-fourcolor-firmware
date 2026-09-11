/**
 * @file page_sync.cc
 * @brief Canvas Loop 页组同步（最小 RAM 测试版）
 */
#include "page_sync.h"

#include <esp_log.h>
#include <esp_heap_caps.h>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include <string.h>
#include <stdlib.h>
#include <cJSON.h>

#include "server_pairing.h"
#include "http_client_wrapper.h"
#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "display.h"
#include "wifi_manager.h"

static const char* kTag = "PageSync";

static page_sync_entry_t s_pages[PAGE_SYNC_MAX_PAGES];
static int s_page_count = 0;
static char s_schedule_md5[33] = {0};
static int s_current_index = 0;
static uint64_t s_current_page_started_us = 0;
static bool s_manual_hold = false;   // 手动翻页后暂停自动轮换 30s
static uint64_t s_manual_hold_until_us = 0;
static bool s_displaying = false;    // 画板当前全屏显示中
static bool s_suspended = false;     // 屏幕被 UI/通知占用，画板不得绘制
static bool s_running = false;

extern Display* create_board_display_placeholder();  // not used; resolved via board

// 获取显示实例：通过 Board 单例。为避免循环依赖，用弱引用声明。
// 实际接线在 application.cc 中调用 page_sync_set_display()。
static Display* s_display = nullptr;

void page_sync_set_display(void* display) {
    s_display = static_cast<Display*>(display);
}

static uint64_t now_us(void) {
    return (uint64_t)esp_timer_get_time();
}

// ── schedule 解析 ─────────────────────────────────────────────
static bool fetch_schedule(char* out_buf, int* out_len) {
    char url[320];
    if (!server_pairing_build_endpoint("/api/pages/schedule", url, sizeof(url))) {
        return false;
    }
    char token[65];
    server_pairing_get_token(token, sizeof(token));
    int status = http_wrapper_get(url, token, out_buf, out_len, 10000);
    return status == 200;
}

static void free_pages(void) {
    for (int i = 0; i < s_page_count; i++) {
        if (s_pages[i].bitmap) {
            heap_caps_free(s_pages[i].bitmap);
            s_pages[i].bitmap = nullptr;
        }
    }
    s_page_count = 0;
}

static bool download_bitmap(const char* md5, uint8_t* out) {
    char path[96];
    snprintf(path, sizeof(path), "/api/pages/bitmap/%.32s.bin", md5);
    char url[320];
    if (!server_pairing_build_endpoint(path, url, sizeof(url))) {
        return false;
    }
    static char tmp[PAGE_BITMAP_SIZE + 64];  // static to avoid big stack; task stack is fine (single task)
    int tmp_len = sizeof(tmp);
    char token[65];
    server_pairing_get_token(token, sizeof(token));
    int status = http_wrapper_get(url, token, tmp, &tmp_len, 15000);
    bool ok = (status == 200 && tmp_len == PAGE_BITMAP_SIZE);
    if (ok) {
        memcpy(out, tmp, PAGE_BITMAP_SIZE);
    } else {
        ESP_LOGW(kTag, "bitmap %s: status=%d len=%d", md5, status, tmp_len);
    }
    return ok;
}

static bool sync_once(void) {
    static char json_buf[8192];
    int json_len = sizeof(json_buf);
    if (!fetch_schedule(json_buf, &json_len)) {
        ESP_LOGW(kTag, "schedule fetch failed");
        return false;
    }

    cJSON* root = cJSON_Parse(json_buf);
    if (!root) {
        ESP_LOGW(kTag, "schedule json parse failed");
        return false;
    }
    cJSON* md5_item = cJSON_GetObjectItem(root, "schedule_md5");
    if (!cJSON_IsString(md5_item)) {
        cJSON_Delete(root);
        return false;
    }
    char new_schedule_md5[33];
    strncpy(new_schedule_md5, md5_item->valuestring, 32);
    new_schedule_md5[32] = '\0';
    if (strcmp(new_schedule_md5, s_schedule_md5) == 0) {
        cJSON_Delete(root);
        return true;  // unchanged — the 99% path
    }
    // Schedule changed: parse pages
    cJSON* pages = cJSON_GetObjectItem(root, "pages");
    if (!cJSON_IsArray(pages)) {
        cJSON_Delete(root);
        return false;
    }
    int count = cJSON_GetArraySize(pages);
    if (count > PAGE_SYNC_MAX_PAGES) count = PAGE_SYNC_MAX_PAGES;

    page_sync_entry_t new_pages[PAGE_SYNC_MAX_PAGES] = {};
    int new_count = 0;
    cJSON* p;
    cJSON_ArrayForEach(p, pages) {
        if (new_count >= count) break;
        cJSON* md5 = cJSON_GetObjectItem(p, "md5");
        cJSON* dur = cJSON_GetObjectItem(p, "duration_minutes");
        cJSON* order = cJSON_GetObjectItem(p, "order");
        if (!cJSON_IsString(md5) || !cJSON_IsNumber(dur) || !cJSON_IsNumber(order)) continue;
        strncpy(new_pages[new_count].md5, md5->valuestring, 32);
        new_pages[new_count].duration_seconds = (uint32_t)dur->valueint * 60;
        new_pages[new_count].order = order->valueint;
        new_count++;
    }
    cJSON_Delete(root);

    // Download missing bitmaps
    int downloaded = 0;
    for (int i = 0; i < new_count; i++) {
        // reuse existing RAM copy if same md5
        bool have = false;
        for (int j = 0; j < s_page_count; j++) {
            if (s_pages[j].on_ram && strcmp(s_pages[j].md5, new_pages[i].md5) == 0) {
                new_pages[i].bitmap = s_pages[j].bitmap;
                new_pages[i].on_ram = true;
                s_pages[j].bitmap = nullptr;  // transfer ownership
                have = true;
                break;
            }
        }
        if (!have) {
            new_pages[i].bitmap = (uint8_t*)heap_caps_malloc(PAGE_BITMAP_SIZE, MALLOC_CAP_SPIRAM);
            if (new_pages[i].bitmap && download_bitmap(new_pages[i].md5, new_pages[i].bitmap)) {
                new_pages[i].on_ram = true;
                downloaded++;
            } else {
                ESP_LOGW(kTag, "bitmap download failed: %s", new_pages[i].md5);
                if (new_pages[i].bitmap) {
                    heap_caps_free(new_pages[i].bitmap);
                    new_pages[i].bitmap = nullptr;
                }
            }
        }
    }

    bool all_ready = true;
    for (int i = 0; i < new_count; i++) {
        if (!new_pages[i].on_ram) {
            all_ready = false;
            break;
        }
    }

    free_pages();
    memcpy(s_pages, new_pages, sizeof(new_pages));
    s_page_count = new_count;
    if (all_ready) {
        // 只有全部位图就绪才提交 schedule_md5。旧行为无条件提交，
        // 一旦某页下载失败就会在下一轮命中 md5 早退而永不重试（该页永久空白）。
        strncpy(s_schedule_md5, new_schedule_md5, sizeof(s_schedule_md5) - 1);
        s_current_index = 0;
        s_current_page_started_us = now_us();
    }
    ESP_LOGI(kTag, "schedule updated: %d pages, %d downloaded%s", s_page_count, downloaded,
             all_ready ? "" : " (retrying missing bitmaps)");
    return true;
}

static void show_page(int index) {
    if (index < 0 || index >= s_page_count) return;
    if (!s_pages[index].on_ram || !s_pages[index].bitmap) return;
    if (s_suspended) return;  // 屏幕被 UI/通知占用
    if (!s_display) {
        ESP_LOGW(kTag, "no display wired");
        return;
    }
    auto* lcd = static_cast<CustomLcdDisplay*>(s_display);
    // 安全路径：不直接调 DisplayRaw4ColorImage（它阻塞写屏会与后台
    // refresh_task 并发抢 EPD，导致 busy 卡死——见 kEnableDirectRawPhotoRefresh 注释）。
    // 改为把 2bpp 位图 blit 进共享 framebuffer，再走 EpdRefreshScheduler 统一刷新。
    uint8_t* fb = lcd->GetFramebuffer();
    int fb_len = lcd->GetFBWidth() * lcd->GetFBHeight() * 2 / 8;
    if (!fb || fb_len != PAGE_BITMAP_SIZE) {
        ESP_LOGE(kTag, "framebuffer size mismatch: fb_len=%d want=%d", fb_len, PAGE_BITMAP_SIZE);
        return;
    }
    // 2bpp 像素序一致（rawdraw set_pixel 与 server pack_2bpp 均 MSB-first），直接拷贝。
    xSemaphoreTake(lcd->GetMutex(), portMAX_DELAY);
    memcpy(fb, s_pages[index].bitmap, PAGE_BITMAP_SIZE);
    xSemaphoreGive(lcd->GetMutex());
    lcd->RequestUrgentFullRefresh();
    s_displaying = true;
    ESP_LOGI(kTag, "show page %d/%d md5=%.8s via framebuffer", index + 1, s_page_count,
             s_pages[index].md5);
}
// ── 空页提示 ────────────────────────────────────────────────
static bool s_empty_hint_shown = false;

static void show_empty_hint(void) {
    if (!s_display) {
        ESP_LOGW(kTag, "no display wired for empty hint");
        return;
    }
    if (s_suspended) return;  // 屏幕被 UI/通知占用
    auto* lcd = static_cast<CustomLcdDisplay*>(s_display);
    // DrawTexts 内部自行获取 dirty_mutex（非递归），此处不得再持有它——
    // 否则同任务二次加锁会永久自锁并一直占着 dirty_mutex，冻死整机刷新。
    std::vector<Display::TextItem> texts;
    Display::TextItem title;
    title.content = "未配置画板页";
    title.x = 20;
    title.y = 100;
    title.size = 24;
    Display::TextItem sub1;
    sub1.content = "请在服务端添加页面";
    sub1.x = 20;
    sub1.y = 140;
    sub1.size = 16;
    Display::TextItem sub2;
    sub2.content = "http://10.0.0.90:9002";
    sub2.x = 20;
    sub2.y = 170;
    sub2.size = 16;
    texts.push_back(title);
    texts.push_back(sub1);
    texts.push_back(sub2);
    lcd->DrawTexts(texts, true);  // clear=true 内部完成清屏 + 加锁
    lcd->RequestUrgentFullRefresh();
    s_displaying = true;
    ESP_LOGI(kTag, "show empty page hint (no pages configured)");
}

void page_sync_next(void) {
    if (s_page_count <= 0) return;
    s_current_index = (s_current_index + 1) % s_page_count;
    s_current_page_started_us = now_us();
    s_manual_hold = true;
    s_manual_hold_until_us = now_us() + 30ULL * 1000000ULL;
    show_page(s_current_index);
}

void page_sync_prev(void) {
    if (s_page_count <= 0) return;
    s_current_index = (s_current_index - 1 + s_page_count) % s_page_count;
    s_current_page_started_us = now_us();
    s_manual_hold = true;
    s_manual_hold_until_us = now_us() + 30ULL * 1000000ULL;
    show_page(s_current_index);
}

bool page_sync_is_displaying(void) { return s_displaying; }

void page_sync_stop_display(void) {
    // UI/通知接管屏幕：挂起画板绘制，否则下一轮轮换会把 UI 页面盖回画布。
    s_suspended = true;
    s_displaying = false;
}

void page_sync_resume_display(void) {
    s_suspended = false;
    s_displaying = true;
}

void page_sync_redraw_current(void) {
    if (s_page_count > 0) {
        show_page(s_current_index);
    } else {
        show_empty_hint();
        s_empty_hint_shown = true;
    }
}

void page_sync_allow_display(void) {
    if (!s_suspended) return;
    s_suspended = false;
    page_sync_redraw_current();
}
static void page_sync_task(void* arg) {
    ESP_LOGI(kTag, "page_sync task started");
    int tick = 0;
    while (s_running) {
        sync_once();
        // 屏幕被 UI/通知占用：只保持数据同步，不绘制（否则会盖掉 UI 页面/通知）
        if (s_suspended) {
            tick++;
            vTaskDelay(pdMS_TO_TICKS(10000));
            continue;
        }
        // 无页时显示空页提示（不空白）
        if (s_page_count == 0) {
            if (!s_empty_hint_shown) {
                show_empty_hint();
                s_empty_hint_shown = true;
            }
        } else {
            s_empty_hint_shown = false;
            // Page rotation check: run every loop (10s)
            uint64_t elapsed = now_us() - s_current_page_started_us;
            uint32_t dur = s_pages[s_current_index].duration_seconds;
            if (dur > 0 && elapsed >= (uint64_t)dur * 1000000ULL) {
                s_current_index = (s_current_index + 1) % s_page_count;
                s_current_page_started_us = now_us();
                show_page(s_current_index);
            } else if (elapsed == 0 && s_pages[s_current_index].on_ram) {
                // first display
                show_page(s_current_index);
            }
            // Show current page immediately after first sync
            if (tick == 0 && s_page_count > 0) {
                show_page(s_current_index);
            }
        }
        tick++;
        vTaskDelay(pdMS_TO_TICKS(10000));  // 10s loop
    }
    vTaskDelete(nullptr);
}


void page_sync_start(void) {
    if (s_running) return;
    // 启动画板 = 画板接管屏幕（覆盖此前任何 UI 挂起）
    s_suspended = false;
    s_running = true;
    if (xTaskCreatePinnedToCore(page_sync_task, "page_sync", 8192, nullptr, 3, nullptr, 1) != pdPASS) {
        ESP_LOGE(kTag, "failed to create page_sync task");
        s_running = false;
    }
}

const uint8_t* page_sync_current_bitmap(void) {
    if (s_current_index >= 0 && s_current_index < s_page_count && s_pages[s_current_index].on_ram) {
        return s_pages[s_current_index].bitmap;
    }
    return nullptr;
}