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

#include <esp_attr.h>
#include <esp_heap_caps.h>
#include <esp_log.h>
 #include <esp_mac.h>
 #include <esp_random.h>
#include <esp_sleep.h>
#include <esp_system.h>
 #include <esp_timer.h>
 #include <freertos/FreeRTOS.h>
 #include <freertos/semphr.h>
 #include <freertos/task.h>
 #include <freertos/timers.h>

#include "shim_power.h"

#include "board.h"
#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "device_signature.h"
#include "http_client_wrapper.h"
#include "notify.h"
#include "page_sync.h"
#include "server_pairing.h"
#include "rust/include/time_gate_policy.h"

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
// Panel content survives deep sleep in RTC memory, so a wake that changes
// nothing can skip a >= 15 s full refresh. RTC_DATA_ATTR is retained across
// deep sleep and soft resets, lost on power-on (magic fails -> repaint).
RTC_DATA_ATTR static rf_panel_record_t g_panel_rec;
// md5 of the frame we asked for; committed only when the refresh goes idle, so
// an interrupted refresh cannot be recorded as done.
static char g_pending_md5[33];
static int g_pending_index = -1;
static bool g_pending_valid = false;
// Guards g_panel_rec against g_pending_* on both writer sides (page-sync task
// marks pending, the display refresh task commits) and the record reader. A
// spinlock, not a mutex: the sections are a few byte copies, must not block,
// and taking no other lock inside keeps it a leaf that cannot deadlock
// against the display mutex.
static portMUX_TYPE g_panel_mux = portMUX_INITIALIZER_UNLOCKED;

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

// Task 1 duration ledger: wakes, awake ms, radio-on upper bound, schedule
// GETs, refresh submits. RTC_DATA_ATTR like g_fail_streak below — every
// duty-cycle sleep is a reboot that clears RAM, so DRAM counters would report
// zeros on battery. Each wake's schedule GET therefore carries the cumulative
// total through the previous completed cycle (this cycle books after its
// fetch). Zeroed on cold boot / power loss: RTC memory does not survive that,
// which is the correct fresh start.
RTC_DATA_ATTR static uint32_t g_wakes, g_awake_ms, g_radio_ms, g_http_gets, g_refresh_submit_ms;
// Real panel activity (relative estimate only): cumulative refresh
// transactions and measured busy milliseconds. RTC_DATA_ATTR because every
// duty-cycle sleep is a reboot. No current sensor exists, so these are never
// converted to mAh or a battery percentage.
RTC_DATA_ATTR static uint32_t g_panel_refreshes, g_panel_busy_ms;

// ─────────────────────────────── http ───────────────────────────────

extern "C" int rf_http_get(const char *url, const char *token, char *buf, int *len,
                           int timeout_ms) {
    // Task 1 duration ledger: the single HTTP GET exit — count every schedule
    // poll, bitmap fetch and notify poll the same way (attempts, not successes).
    g_http_gets++;
    return http_wrapper_get(url, token, buf, len, timeout_ms);
}

extern "C" int rf_http_post_json(const char *url, const char *token, const char *body,
                                 char *buf, int *len, int timeout_ms) {
    return http_wrapper_post_json(url, token, body, nullptr, 0, buf, len, timeout_ms);
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

extern "C" void rf_panel_commit_hook_register(void) {
    // Re-chain the commit-on-idle hook after RawDrawUiManager::Init replaced
    // the slot (promotion path). Exactly one chain per call: the caller must
    // call this once per wipe (promotion is one-shot, and Init wiped the
    // previous chain, so one re-chain leaves exactly one trampoline).
    CustomLcdDisplay *d = lcd();
    if (d != nullptr) {
        d->AddOnRefreshIdle([]() {
            portENTER_CRITICAL(&g_panel_mux);
            if (g_pending_valid) {
                g_panel_rec.magic = RF_PANEL_MAGIC;
                g_panel_rec.valid = 1;
                memcpy(g_panel_rec.displayed_md5, g_pending_md5,
                       sizeof(g_panel_rec.displayed_md5));
                g_panel_rec.displayed_index = g_pending_index;
                g_pending_valid = false;
            }
            portEXIT_CRITICAL(&g_panel_mux);
        });
    }
}

extern "C" void rf_set_display(void *display) {
    // The display is resolved through Board on demand; this exists so the
    // firmware's existing `page_sync_set_display(...)` injection point stays.
    (void)display;
    // Commit the pending md5 once the panel reports idle: this is what makes
    // "skip the repaint next wake" safe. Registered here because this is the
    // only Rust-side hook that runs after pairing (application.cc:
    // page_sync_set_display -> page_sync_set_display -> set_display -> here).
    // AddOnRefreshIdle chains instead of replacing: RawDrawUiManager::Init
    // already owns the Set slot with its input-unlock callback.
    static bool s_refresh_watch_registered = false;
    if (!s_refresh_watch_registered) {
        CustomLcdDisplay *d = lcd();
        if (d != nullptr) {
            rf_panel_commit_hook_register();
            s_refresh_watch_registered = true;
        }
    }
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
    // Task 1 duration ledger, single exit: canvas paints, the empty hint and
    // notification popups all funnel through here — one place, no per-caller
    // bookkeeping to forget. Counts SUBMITS only: the call returns once the
    // request is queued; the panel's multi-second waveform runs asynchronously
    // and is booked separately by rf_power_add_panel_refresh() in
    // EPD_TurnOnDisplay(). Submit time is deliberately NOT included there.
    const uint64_t t0 = (uint64_t)(esp_timer_get_time() / 1000);
    CustomLcdDisplay *d = lcd();
    if (d != nullptr) {
        d->RequestUrgentFullRefresh("canvas");
    }
    g_refresh_submit_ms += (uint32_t)((uint64_t)(esp_timer_get_time() / 1000) - t0);
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

// ─────────────────── panel record / wakeup / rails ───────────────────

extern "C" void rf_panel_record_get(rf_panel_record_t *out) {
    if (out == nullptr) return;
    portENTER_CRITICAL(&g_panel_mux);
    *out = g_panel_rec;
    portEXIT_CRITICAL(&g_panel_mux);
}

extern "C" void rf_panel_mark_pending(const char *md5, int index) {
    if (md5 == nullptr) return;
    // Format outside the critical section; only the publish is locked.
    char staged[33];
    snprintf(staged, sizeof(staged), "%s", md5);
    portENTER_CRITICAL(&g_panel_mux);
    memcpy(g_pending_md5, staged, sizeof(g_pending_md5));
    g_pending_index = index;
    g_pending_valid = true;
    portEXIT_CRITICAL(&g_panel_mux);
}

extern "C" void rf_panel_record_invalidate(void) {
    portENTER_CRITICAL(&g_panel_mux);
    g_panel_rec.valid = 0;
    g_panel_rec.magic = 0;
    g_pending_valid = false;
    portEXIT_CRITICAL(&g_panel_mux);
}

// Consecutive schedule-sync failures feeding the power-policy backoff. RTC
// memory: every duty-cycle sleep is a reboot, so a RAM counter would reset on
// each wake and the backoff ladder (60, 120, 240, …) could never climb —
// the device would boot ~720 times a day against a dead server instead of
// ~24. Lost on power-on (zeroed), which is the correct fresh start.
RTC_DATA_ATTR static uint32_t g_fail_streak;

extern "C" uint32_t rf_fail_streak_get(void) {
    return g_fail_streak;
}

extern "C" void rf_fail_streak_set(uint32_t streak) {
    g_fail_streak = streak;
}

extern "C" int rf_wakeup_cause(void) {
    // esp_sleep_get_wakeup_cause() is deprecated in v6.0; the replacement
    // returns a bitmap whose bit index is the esp_sleep_wakeup_cause_t value.
    const uint32_t causes = esp_sleep_get_wakeup_causes();
    if (causes & (1U << ESP_SLEEP_WAKEUP_TIMER)) return 1;
    if (causes & (1U << ESP_SLEEP_WAKEUP_EXT0))  return 2;
    if (causes & (1U << ESP_SLEEP_WAKEUP_EXT1))  return 3;
    return 0;
}

extern "C" void rf_rails_audio(int on) {
    Board::GetInstance().SetAudioRail(on != 0);
}

// Power-accounting reader/writers (counters defined above, next to rf_http_get
// which counts at the single GET exit). radio_ms stays an UPPER BOUND —
// see the definition-site comment.
extern "C" void rf_power_counters(uint32_t* w, uint32_t* a, uint32_t* r, uint32_t* g, uint32_t* f) {
    if (w) *w = g_wakes;
    if (a) *a = g_awake_ms;
    if (r) *r = g_radio_ms;
    if (g) *g = g_http_gets;
    if (f) *f = g_refresh_submit_ms;
}
extern "C" void rf_power_count_wake(void)   { g_wakes++; }
extern "C" void rf_power_add_awake_ms(uint32_t ms) { g_awake_ms += ms; }
extern "C" void rf_power_add_radio_ms(uint32_t ms) { g_radio_ms += ms; }
extern "C" void rf_power_add_panel_refresh(uint32_t busy_ms) {
    g_panel_refreshes++;
    g_panel_busy_ms += busy_ms;
}
extern "C" void rf_panel_activity_counters(uint32_t* er, uint32_t* eb) {
    if (er) *er = g_panel_refreshes;
    if (eb) *eb = g_panel_busy_ms;
}

// Last reset reason, sampled once at first call (boot path) and cached in RTC
// RAM: the query string must carry WHY the device rebooted (brownout vs watchdog
// vs software) across deep sleeps, where `esp_reset_reason()` itself resets to
// ESP_RST_UNKNOWN after the next wake. Reported as `?rr=<n>` by fetch_schedule;
// values are the esp_reset_reason_t enum (e.g. 0xf=BROWNOUT, 0x8=RTCWDT on S3).
RTC_DATA_ATTR static uint32_t g_last_reset_reason;
extern "C" uint32_t rf_last_reset_reason(void) {
    if (g_last_reset_reason == 0) {
        g_last_reset_reason = (uint32_t)esp_reset_reason();
    }
    return g_last_reset_reason;
}
// Forward declaration: ZectrixReadBatterySample is defined in the board .cc
// and compiled into the firmware, but no header exports it to this TU.
extern "C" bool ZectrixReadBatterySample(uint16_t* mv, uint8_t* pct, uint8_t* charge);

// Battery telemetry: page_sync rides ?v=&p=&c= on the schedule GET. Values
// come from the board's ADC + charge snapshot; 0 / false return = no valid
// reading (mains / no battery / ADC failure), which Rust maps to "omit params".
extern "C" int rf_battery_sample(uint16_t* mv, uint8_t* pct, uint8_t* charge) {
    return ZectrixReadBatterySample(mv, pct, charge) ? 1 : 0;
}

// One-hour sliding battery-report gate (spec 2026-09-23): deep sleep clears
// RAM, so the due-at stamp lives in RTC slow memory — it survives every
// duty-cycle wake and is zeroed on cold boot (flash => report on first wake).
// Clock resolution (A): prefer SNTP time(); fall back to the PCF8563, which
// SNTP writes on every sync (so it holds time across power loss too). An
// implausible clock (< 2020) reports but never stamps — a 1970 boot cannot
// poison the window, and the first post-sync report arms it. Peek and arm are
// separate: page_sync arms only after a real sample reached the URL, so an
// ADC hiccup never buys an hour of silence.
//
// The decision now lives in `time_gate_policy.rs`. This shim only owns the
// RTC slow-memory stamp (`g_battery_due_at`) and reads the wall clock +
// PCF8563 fallback. The Rust policy returns `battery_sample_ok` plus the
// next arm stamp, and `time()==-1` is propagated as a negative `now_s`
// before any u32 narrowing (see RfReadNowSigned() in application.cc — we
// keep that helper mirrored here to avoid a header dependency on the
// application-side observer).
RTC_DATA_ATTR static uint32_t g_battery_due_at;
extern "C" int ZectrixRtcNowEpoch(uint32_t* epoch);  // board .cc (mechanism)

static int64_t ShimReadNowSigned(void) {
    return (int64_t)time(nullptr);
}
static uint32_t ShimReadRtcEpoch(void) {
    uint32_t epoch = 0;
    if (ZectrixRtcNowEpoch(&epoch) && epoch >= RF_TIME_GATE_NEVER) {
        return epoch;
    }
    return 0;
}
static int ShimDecideBattery(uint32_t last_arm, uint32_t* next_arm_out) {
    const int64_t now = ShimReadNowSigned();
    const uint32_t effective = ShimReadRtcEpoch();
    const bool now_valid = (now >= (int64_t)RF_TIME_GATE_NEVER);
    const bool rtc_valid = (effective >= RF_TIME_GATE_NEVER);
    rf_time_gate_policy_inputs_t in{};
    in.now_s = now;
    in.effective_clock_s = effective;
    in.last_sntp_sync_s = RF_TIME_GATE_NEVER;
    in.last_battery_arm_s = last_arm;
    in.sntp_min_period_s = 24u * 60u * 60u;
    in.battery_min_period_s = 60u * 60u;
    in.clock_valid_mask =
        (now_valid ? RF_TIME_GATE_CLOCK_NOW_VALID : 0u) |
        (rtc_valid ? RF_TIME_GATE_CLOCK_RTC_VALID : 0u) |
        (now_valid ? RF_TIME_GATE_CLOCK_EVER_SYNCED : 0u);
    rf_time_gate_policy_output_t out{};
    rf_time_gate_policy_decide(&in, &out);
    if (next_arm_out != nullptr) {
        *next_arm_out = out.next_last_battery_arm_s;
    }
    return (int)out.battery_sample_ok;
}

extern "C" int rf_battery_due(void) {
    return ShimDecideBattery(g_battery_due_at, nullptr);
}
extern "C" void rf_battery_arm(void) {
    uint32_t next_arm = RF_TIME_GATE_NEVER;
    if (ShimDecideBattery(g_battery_due_at, &next_arm)) {
        // Rust tells us to arm only when the wall clock is plausible.
        // On fallback / 1970 paths `next_arm` stays at NEVER (sentinel),
        // so we never poison g_battery_due_at.
        if (next_arm != RF_TIME_GATE_NEVER) {
            g_battery_due_at = next_arm;
        }
    }
}

extern "C" int rf_time_gate_wifi_cache_ok(void) {
    // The Wi-Fi cache is a stale-on-write record; it must only be
    // persisted when the wall clock is plausible. The decision lives
    // in `time_gate_policy.rs`; this shim is a one-call observer so
    // external C++ sites (wifi_station.cc) can consult it without
    // building the full input struct themselves.
    const int64_t now = ShimReadNowSigned();
    const uint32_t effective = ShimReadRtcEpoch();
    const bool now_valid = (now >= (int64_t)RF_TIME_GATE_NEVER);
    const bool rtc_valid = (effective >= RF_TIME_GATE_NEVER);
    rf_time_gate_policy_inputs_t in{};
    in.now_s = now;
    in.effective_clock_s = effective;
    in.last_sntp_sync_s = RF_TIME_GATE_NEVER;
    in.last_battery_arm_s = RF_TIME_GATE_NEVER;
    in.sntp_min_period_s = 24u * 60u * 60u;
    in.battery_min_period_s = 60u * 60u;
    in.clock_valid_mask =
        (now_valid ? RF_TIME_GATE_CLOCK_NOW_VALID : 0u) |
        (rtc_valid ? RF_TIME_GATE_CLOCK_RTC_VALID : 0u) |
        (now_valid ? RF_TIME_GATE_CLOCK_EVER_SYNCED : 0u);
    rf_time_gate_policy_output_t out{};
    rf_time_gate_policy_decide(&in, &out);
    return (int)out.wifi_cache_ok;
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
