/**
 * @file shim.cpp
 * @brief The only ESP-IDF surface the Rust modules (`rust/src/`) call into.
 *
 * Rust owns the logic (schedule parsing, page table, notification state machine,
 * signature); this file owns the hardware and RTOS: the panel, PSRAM, HTTP,
 * FreeRTOS tasks/timers and the pairing helpers. Keeping the boundary here means
 * the Rust side never depends on IDF typedefs, and it also fixes the link order:
 * these objects live in the component's archive and the Rust staticlib is
 * attached right after it, so the shim is pulled in first and its `rf_*` calls
 * resolve against the archive that follows. (Static archives are not rescanned,
 * so the reverse direction would need a link group.)
 *
 * It also defines the public `device_sign_pair_start`, `page_sync_*` and
 * `notify_*` symbols the rest of the firmware already calls, so no C++ caller
 * changed when the implementations moved to Rust.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include <esp_heap_caps.h>
#include <esp_log.h>
#include <esp_mac.h>
#include <esp_random.h>
#include <esp_timer.h>
#include <freertos/FreeRTOS.h>
#include <freertos/semphr.h>
#include <freertos/task.h>
#include <freertos/timers.h>

#include "board.h"
#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "device_signature.h"
#include "http_client_wrapper.h"
#include "notify.h"
#include "page_sync.h"
#include "server_pairing.h"

// ─────────────────────────────── runtime ───────────────────────────────

namespace {

const char *kShimTag = "RustFw";

CustomLcdDisplay *lcd() {
    static CustomLcdDisplay *cached = nullptr;
    if (cached != nullptr) {
        return cached;
    }
    cached = static_cast<CustomLcdDisplay *>(Board::GetInstance().GetDisplay());
    return cached;
}

// One mutex guarding all Rust-side module state. Priority-inheriting, unlike a
// spinlock. Lock order is `state -> display`; the display mutex is never held
// while taking this one (page_sync/notify read their flags atomically).
SemaphoreHandle_t state_mutex() {
    static SemaphoreHandle_t m = nullptr;
    if (m == nullptr) {
        m = xSemaphoreCreateMutex();
    }
    return m;
}

TimerHandle_t g_timer = nullptr;
void (*g_timer_cb)(void) = nullptr;

void timer_trampoline(TimerHandle_t) {
    if (g_timer_cb != nullptr) {
        g_timer_cb();
    }
}

// Whether `rf_fb_begin` currently holds the display mutex.
bool g_fb_taken = false;

} // namespace

extern "C" void rf_abort(void) {
    abort();
}

extern "C" void rf_log(int level, const char *tag, const char *msg) {
    switch (level) {
        case 1: ESP_LOGE(tag, "%s", msg); break;
        case 2: ESP_LOGW(tag, "%s", msg); break;
        case 4: ESP_LOGD(tag, "%s", msg); break;
        default: ESP_LOGI(tag, "%s", msg); break;
    }
}

extern "C" uint8_t *rf_alloc(size_t bytes) {
    return static_cast<uint8_t *>(heap_caps_malloc(bytes, MALLOC_CAP_SPIRAM));
}

extern "C" void rf_free(uint8_t *p) {
    if (p != nullptr) {
        heap_caps_free(p);
    }
}

extern "C" void rf_state_lock(void) {
    SemaphoreHandle_t m = state_mutex();
    if (m != nullptr) {
        xSemaphoreTake(m, portMAX_DELAY);
    }
}

extern "C" void rf_state_unlock(void) {
    SemaphoreHandle_t m = state_mutex();
    if (m != nullptr) {
        xSemaphoreGive(m);
    }
}

extern "C" int rf_task_create(void (*entry)(void *), const char *name,
                              uint32_t stack_bytes, uint8_t priority, void *arg) {
    return xTaskCreate(entry, name, stack_bytes, arg, priority, nullptr) == pdPASS ? 0 : -1;
}

extern "C" void rf_task_exit(void) {
    vTaskDelete(nullptr);
}

extern "C" void rf_delay_ms(uint32_t ms) {
    vTaskDelay(pdMS_TO_TICKS(ms));
}

extern "C" uint64_t rf_now_us(void) {
    return (uint64_t)esp_timer_get_time();
}

extern "C" int rf_timer_create_once(const char *name, uint32_t period_ms, void (*cb)(void)) {
    if (g_timer != nullptr) {
        return 0;
    }
    g_timer_cb = cb;
    g_timer = xTimerCreate(name, pdMS_TO_TICKS(period_ms), pdFALSE, nullptr, timer_trampoline);
    return g_timer != nullptr ? 0 : -1;
}

extern "C" void rf_timer_start(void) {
    if (g_timer != nullptr) {
        xTimerStart(g_timer, 0);
    }
}

extern "C" void rf_timer_stop(void) {
    if (g_timer != nullptr) {
        xTimerStop(g_timer, 0);
    }
}

extern "C" void rf_timer_delete(void) {
    if (g_timer != nullptr) {
        xTimerStop(g_timer, 0);
        xTimerDelete(g_timer, 0);
        g_timer = nullptr;
    }
    g_timer_cb = nullptr;
}

// ─────────────────────────────── http ───────────────────────────────

extern "C" int rf_http_get(const char *url, const char *token, char *buf, int *len,
                           int timeout_ms) {
    return http_wrapper_get(url, token, buf, len, timeout_ms);
}

extern "C" int rf_http_post_json(const char *url, const char *token, const char *body,
                                 char *buf, int *len, int timeout_ms) {
    return http_wrapper_post_json(url, token, body, buf, len, timeout_ms);
}

// ─────────────────────────── pairing helpers ───────────────────────────

extern "C" int rf_build_endpoint(const char *path, char *out, int out_len) {
    return server_pairing_build_endpoint(path, out, out_len) ? 1 : 0;
}

extern "C" int rf_get_token(char *out, int out_len) {
    return server_pairing_get_token(out, out_len) ? 1 : 0;
}

extern "C" int rf_get_device_id(char *out, int out_len) {
    return server_pairing_get_device_id(out, out_len) ? 1 : 0;
}

// ─────────────────────────────── display ───────────────────────────────

extern "C" void rf_set_display(void *display) {
    // The display is resolved through Board on demand; this exists so the
    // firmware's existing `page_sync_set_display(...)` injection point stays.
    (void)display;
}

extern "C" int rf_fb_len(void) {
    CustomLcdDisplay *d = lcd();
    if (d == nullptr) {
        return 0;
    }
    return d->GetFBWidth() * d->GetFBHeight() * 2 / 8;
}

extern "C" uint8_t *rf_fb_begin(void) {
    CustomLcdDisplay *d = lcd();
    if (d == nullptr) {
        return nullptr;
    }
    uint8_t *fb = d->GetFramebuffer();
    if (fb == nullptr) {
        return nullptr;
    }
    xSemaphoreTake(d->GetMutex(), portMAX_DELAY);
    g_fb_taken = true;
    return fb;
}

extern "C" void rf_fb_end(void) {
    CustomLcdDisplay *d = lcd();
    if (d != nullptr && g_fb_taken) {
        g_fb_taken = false;
        xSemaphoreGive(d->GetMutex());
    }
}

extern "C" void rf_request_full_refresh(void) {
    CustomLcdDisplay *d = lcd();
    if (d != nullptr) {
        d->RequestUrgentFullRefresh();
    }
}

extern "C" void rf_draw_empty_hint(void) {
    CustomLcdDisplay *d = lcd();
    if (d == nullptr) {
        return;
    }
    // The hint is drawn with the display's own text API (and its mutex, taken
    // inside DrawTexts) — building std::vector<TextItem> is a display concern.
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
    d->DrawTexts(texts, true);
}

// ───────────────────── device signature (public ABI) ─────────────────────

/* Implemented in Rust. Writes mac_hex / timestamp / nonce_b64 / sig_b64, each
 * NUL-terminated; a buffer too small for its field is left as an empty string.
 * Must stay inside `extern "C"`: this is a C++ translation unit, and the Rust
 * symbol is unmangled. */
extern "C" {
void devsig_sign(const char *device_id,
                 const uint8_t *mac_raw,
                 int64_t timestamp,
                 const uint8_t *nonce_raw,
                 char *mac_hex_out, size_t mac_hex_len,
                 char *ts_out, size_t ts_len,
                 char *nonce_out, size_t nonce_len,
                 char *sig_out, size_t sig_len);
}

#define DEVSIG_MAC_LEN 6
#define DEVSIG_NONCE_LEN 16

extern "C" void device_sign_pair_start(const char *device_id,
                                       char *mac_out, size_t mac_len,
                                       char *ts_out, size_t ts_len,
                                       char *nonce_out, size_t nonce_len,
                                       char *sig_out, size_t sig_len) {
    if (device_id == nullptr) {
        return;
    }

    uint8_t mac[DEVSIG_MAC_LEN];
    if (esp_read_mac(mac, ESP_MAC_WIFI_STA) != ESP_OK) {
        return; /* leave every field untouched, as before */
    }

    uint8_t nonce[DEVSIG_NONCE_LEN];
    esp_fill_random(nonce, sizeof(nonce));

    /* Unix seconds from the SNTP-synced wall clock (not boot uptime). */
    devsig_sign(device_id, mac, (int64_t)time(NULL), nonce,
                mac_out, mac_len, ts_out, ts_len,
                nonce_out, nonce_len, sig_out, sig_len);
}
