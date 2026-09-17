#include "application.h"

#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "boards/zectrix-s3-epaper-4.2/config.h"
#include "board.h"

#include "display.h"
#include "settings.h"
#include "ui/rawdraw_ui_manager.h"
#include "ui/renderers/rawdraw/wifi_renderer.h"
#include "wifi_manager.h"

#include <esp_mac.h>
#include <esp_log.h>
#include <esp_sleep.h>
#include <esp_sntp.h>
#include <esp_system.h>
#include <esp_timer.h>
#include <esp_wifi.h>
#include <atomic>
#include <cstdint>
#include <cstring>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include "common/server_pairing.h"
#include "common/sleep_manager.h"
#include "ssid_manager.h"
#include "page_sync.h"
#include "notify.h"
#include "input.h"
#include "lifecycle.h"
#include "power.h"
#include "shim_power.h"

#include <ctime>

namespace {
constexpr char kTag[] = "Application";
constexpr char kPowerNamespace[] = "power";

// The settings menu's rows are addressed by id, and the labels/order live in
// rust/src/settings.rs (`rf_settings_item_t`). Only the device-supplied parts
// are here: what a row does, and the values it reads.



// The last Wi-Fi disconnect reason, for the 网络 page's 失败原因 row. Cleared as
// soon as any other state arrives, so a stale reason never reads as current.
static int g_last_wifi_error = 0;

// What the user asked the Wi-Fi switch to be — or UNKNOWN before they have said
// anything, and after the device moves the radio itself (the config AP). The
// switch draws this rather than the connection, so a press is answered by the
// row's own state and never by a radio that is not there yet. The rule lives in
// settings.rs (`wifi_switch_shown`); this is only where the C side keeps it.
static uint8_t g_wifi_switch_intent = RF_SETTINGS_WIFI_SWITCH_UNKNOWN;

// Which destructive settings row is armed and since when, for the two-press
// confirm. The window rule itself is settings.rs::confirm_step.
static uint8_t g_reset_armed_id = RF_SETTINGS_CONFIRM_NONE;
static uint64_t g_reset_armed_at_ms = 0;

/// The password the device holds for `ssid`, or empty when it has none (an open
/// network, or credentials that were cleared). The settings page keeps it behind
/// dots until the user asks for it: SettingsRenderer::TogglePasswordReveal.
static std::string WifiPasswordFor(const std::string& ssid) {
    if (ssid.empty()) return "";
    for (const auto& item : SsidManager::GetInstance().GetSsidList()) {
        if (item.ssid == ssid) return item.password;
    }
    return "";
}

// The 网络 section is a read-out; these are the values only the device knows.
void RefreshNetworkStatusItems(rawdraw::SettingsRenderer* renderer, bool connected) {
    if (!renderer) return;
    auto& wifi = WifiManager::GetInstance();
    const bool up = connected || wifi.IsConnected();
    renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_IP, up ? wifi.GetIpAddress() : "--");
    if (up) {
        char buf[24];
        snprintf(buf, sizeof(buf), "%d dBm", wifi.GetRssi());
        renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_SIGNAL, buf);
    } else {
        renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_SIGNAL, "--");
    }
    renderer->SetItemValue(RF_SETTINGS_ITEM_SERVER,
                           page_sync_server_reachable() ? "可达" : "不可达");

    // The network the device is set up for (the one it is on, else the saved
    // one), the key it holds for it, and why the last attempt failed if it did.
    // The saved list is what the device would connect to next, so the page can
    // still answer "which network is this set up for?" with the radio down —
    // the rows used to read "--" and empty, which is exactly what a user
    // checks before taking the device somewhere else. The password row follows
    // this name, so the saved key stays visible too (still dotted).
    const std::string live = wifi.GetSsid();
    const auto& saved_list = SsidManager::GetInstance().GetSsidList();
    const char* saved = saved_list.empty() ? "" : saved_list.front().ssid.c_str();
    const char* chosen = rf_settings_shown_ssid(live.c_str(), saved);
    const std::string shown = chosen ? chosen : "";
    renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_SSID, shown.empty() ? "--" : shown);
    renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_PASSWORD, WifiPasswordFor(shown));
    renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_ERROR,
                           g_last_wifi_error ? rawdraw::WifiRenderer::ReasonToMessage(g_last_wifi_error)
                                             : "—");
}

void UpdateWifiSettingsItem(rawdraw::SettingsRenderer* renderer, bool connected,
                            const char* value = nullptr) {
    if (!renderer) return;
    // The switch draws what the user asked for, not what the radio is doing —
    // the "连接状态" row below it is the connection's own report. Deriving the
    // switch from `connected` meant a press with the radio down redrew the
    // state it already had, so the row looked dead in both directions.
    renderer->SetItemChecked(
        RF_SETTINGS_ITEM_WIFI_TOGGLE,
        rf_settings_wifi_switch_shown(g_wifi_switch_intent, connected ? 1 : 0) != 0);
    renderer->SetItemValue(RF_SETTINGS_ITEM_WIFI_STATE,
                           value ? value : (connected ? "已连接" : "未连接"));
    RefreshNetworkStatusItems(renderer, connected);
}

void StartSntpClockSyncOnce() {
    static bool s_started = false;
    if (s_started) return;

    setenv("TZ", "CST-8", 1);
    tzset();
    esp_sntp_setoperatingmode(SNTP_OPMODE_POLL);
    esp_sntp_setservername(0, "ntp.aliyun.com");
    esp_sntp_setservername(1, "cn.pool.ntp.org");
    esp_sntp_setservername(2, "pool.ntp.org");
    esp_sntp_set_time_sync_notification_cb([](struct timeval*) {
        time_t now = 0;
        time(&now);
        struct tm local_tm = {};
        localtime_r(&now, &local_tm);
        char time_buf[32] = {};
        strftime(time_buf, sizeof(time_buf), "%Y-%m-%d %H:%M:%S", &local_tm);
        ESP_LOGI(kTag, "SNTP time synchronized: %s", time_buf);
        Application::GetInstance().UpdateStatusBarForUi();
    });
    esp_sntp_init();
    s_started = true;
    ESP_LOGI(kTag, "SNTP started: tz=Asia/Shanghai servers=ntp.aliyun.com,cn.pool.ntp.org,pool.ntp.org");
}

std::atomic<bool> s_pairing_started{false};
std::atomic<bool> s_pairing_done{false};

/** 注册配网状态回调（配网页/配网态变化 → 屏与状态机）。
 *  EnterWifiConfigMode 与开机/无 base_url 两条起 AP 路径共用，
 *  否则开机路径起 AP 时回调为空，5 个 provisioning 事件全被丢弃。 */
void RegisterProvisioningStateCallback() {
    WifiManager::GetInstance().SetProvisioningStateCallback(
        [](const std::string& state, int reason) {
            auto& app = Application::GetInstance();
            app.UpdateWifiStatusForProvisioning(state, reason);
            if (state == "provisioned") {
                app.TransitionLifecycle(kLifecycleWifiConnecting, "provisioned");
            } else if (state == "error") {
                app.TransitionLifecycle(
                    kLifecycleApProvision,
                    ("provision failed reason=" + std::to_string(reason)).c_str());
            } else {
                app.TransitionLifecycle(kLifecycleApProvision, state.c_str());
            }
        });
}

void ServerPairingTaskTrampoline(void*) {
    ServerPairStatus st = server_pairing_init();
    if (st == SERVER_PAIR_NEEDS_PROVISION) {
        // 有 WiFi 凭据但缺 base_url：必须真起配网 AP。此前只置 ApProvision
        // 而不起 AP，等于假状态：没有 AP 可连、屏上无提示，且 DOWN 长按
        // 被「配网中忽略」永久吞掉（Settings 是唯一重置网络入口）。
        ESP_LOGW(kTag, "ServerPairing: no base_url — 启动配网 AP");
        auto& wifi = WifiManager::GetInstance();
        RegisterProvisioningStateCallback();
        wifi.StartConfigAp();  // 触发 ConfigModeEnter → ApProvision + 配网页
        if (wifi.IsConfigMode()) {
            if (auto* ui = Application::GetInstance().GetRawDrawUiManager()) {
                ui->ShowWifiConfigPage(wifi.GetApSsid(), wifi.GetApPassword(),
                                       wifi.GetApWebUrl());
            }
        } else {
            ESP_LOGE(kTag, "ServerPairing: 配网 AP 启动失败");
            Application::GetInstance().TransitionLifecycle(
                kLifecycleError, "config AP start failed");
        }
        vTaskDelete(nullptr);
        return;
    }
    if (st == SERVER_PAIR_NEEDS_PAIRING) {
        char dev_id[32] = {0};
        server_pairing_get_device_id(dev_id, sizeof(dev_id));
        Application::GetInstance().TransitionLifecycle(
            kLifecyclePairStart, "needs pairing");
        server_pairing_set_display_cb([](const char* code, int expires_in) {
            if (code == nullptr) {
                if (expires_in < 0) {
                    // 配对失败：上屏 WiFi 状态页显示错误（此前既不报错也无提示）
                    ESP_LOGE(kTag, "ServerPairing: 配对失败，显示错误页");
                    auto& app = Application::GetInstance();
                    app.UpdateWifiStatusForProvisioning("error", expires_in);
                    if (auto* ui = app.GetRawDrawUiManager()) {
                        ui->ShowWifiConfigPage("", "", "");
                    }
                }
                return;  // expires_in >= 0：配对成功清除显示，SyncIdle 跃迁随后即到
            }
            ESP_LOGI(kTag, "ServerPairing: 配对码 %s (有效 %d 秒)", code, expires_in);
            Application::GetInstance().TransitionLifecycle(
                kLifecyclePairWaitCode, code);
            auto* ui = Application::GetInstance().GetRawDrawUiManager();
            if (ui != nullptr) {
                ui->ShowPairingCodePage(code, expires_in);
            }
        });
        ESP_LOGI(kTag, "ServerPairing: device=%s 开始配对流程", dev_id);
        // 有界重试：run() 单次失败不立刻放弃，但也不无限重试。
        constexpr int kMaxPairAttempts = 3;
        bool paired = false;
        for (int attempt = 1; attempt <= kMaxPairAttempts; ++attempt) {
            if (server_pairing_run()) {
                paired = true;
                break;
            }
            ESP_LOGE(kTag, "ServerPairing: 第 %d/%d 次配对失败", attempt, kMaxPairAttempts);
            if (attempt < kMaxPairAttempts) {
                vTaskDelay(pdMS_TO_TICKS(60000));
            }
        }
        if (!paired) {
            ESP_LOGE(kTag, "ServerPairing: 配对失败（超时/网络不可达）");
            Application::GetInstance().TransitionLifecycle(
                kLifecycleError, "pairing failed");
            // 复位去重门，让后续 WiFi 重连事件还能再试一次
            s_pairing_started.store(false);
            vTaskDelete(nullptr);
            return;
        }
        ESP_LOGI(kTag, "ServerPairing: 配对成功，token 已写入 NVS");
    }
    char tok[65] = {0};
    if (server_pairing_get_token(tok, sizeof(tok))) {
        ESP_LOGI(kTag, "ServerPairing: token ready (%d chars)", (int)strlen(tok));
    }
    // Canvas Loop: page_sync 需要显示实例（spec 2026-09-01 §3）
    auto& board = Board::GetInstance();
    page_sync_set_display(board.GetDisplay());
    page_sync_start();
    Application::GetInstance().TransitionLifecycle(
        kLifecycleSyncIdle, "paired, sync running");
    s_pairing_done.store(true);
    ESP_LOGI(kTag, "PageSync: started");
    notify_init();
    vTaskDelete(nullptr);
}

// Charger-insert deep-sleep wake (ext1). The level follows charge_status.cc,
// which is the authority (attach = LOW, unplugged = HIGH); the sleep log
// prints the raw pin so the matrix confirms it. If the hardware disagrees,
// flip CHARGE_DETECT_PLUG_PULLS_LOW in config.h and this follows.
void EnableChargerInsertWakeup() {
    // v6.0 deprecates esp_sleep_enable_ext1_wakeup in favour of the _io pair.
#if CHARGE_DETECT_PLUG_PULLS_LOW
    esp_sleep_enable_ext1_wakeup_io(1ULL << CHARGE_DETECT_GPIO, ESP_EXT1_WAKEUP_ANY_LOW);
#else
    esp_sleep_enable_ext1_wakeup_io(1ULL << CHARGE_DETECT_GPIO, ESP_EXT1_WAKEUP_ANY_HIGH);
#endif
}
}  // namespace

Application::Application() = default;

Application::~Application() {
    if (sleep_timer_ != nullptr) {
        esp_timer_stop(sleep_timer_);
        esp_timer_delete(sleep_timer_);
        sleep_timer_ = nullptr;
    }
}

void Application::Initialize(bool quiet) {
    // Quiet is honoured only on a provisioned AND paired device: the
    // provisioning and pairing pages are rendered by the UI manager, so a
    // quiet boot there would strand the user in a flow with no screen.
    bool effective_quiet = quiet;
    if (effective_quiet) {
        const bool provisioned = !SsidManager::GetInstance().GetSsidList().empty();
        char tok[65] = {0};
        const bool paired = server_pairing_get_token(tok, sizeof(tok));
        if (!provisioned || !paired) {
            ESP_LOGI(kTag, "Quiet boot needs provisioning+pairing (provisioned=%d paired=%d), staying interactive",
                     (int)provisioned, (int)paired);
            effective_quiet = false;
        }
    }
    // Store before Board::GetInstance(): the board ctor reads IsQuietBoot()
    // to decide the panel bring-up.
    quiet_boot_.store(effective_quiet, std::memory_order_release);
    if (effective_quiet) {
        // Timer-wake duty cycle, not user activity: backdate the idle clock
        // past the grace window so the policy computes the duty-cycle wake
        // instead of holding the device inside the interactive grace window.
        NoteQuietWake();
    }

    auto& board = Board::GetInstance();
    SetDeviceState(kDeviceStateStarting);
    TransitionLifecycle(kLifecycleBoot, "init");

    AudioCodec* codec = board.GetAudioCodec();
    if (codec == nullptr) {
        ESP_LOGE(kTag, "Audio codec is null");
        SetDeviceState(kDeviceStateFatalError);
        return;
    }

    // A timer wake never plays UI sounds; the codec stays powered down.
    if (!quiet_boot_.load(std::memory_order_acquire)) {
        audio_service_.Initialize(codec);
        audio_service_.Start();
    }

    Display* display = board.GetDisplay();
    if (display == nullptr) {
        ESP_LOGW(kTag, "No display available, skipping init");
        SetDeviceState(kDeviceStateFatalError);
        return;
    }


    // The UI manager owns the provisioning/pairing pages and the settings
    // tree; a duty-cycle wake builds none of it (and the board ctor already
    // skipped the panel bring-up behind IsQuietBoot()).
    if (quiet_boot_.load(std::memory_order_acquire)) {
        ESP_LOGI(kTag, "Quiet boot: UI manager skipped, panel bring-up skipped");
    } else {
        // A paired device has the canvas painting this panel on the cycle that
        // begins seconds from now, and that frame covers all of it. Hold back
        // the UI's shell paint rather than spend a second full refresh on a
        // frame the canvas is about to supersede (see the member). Unpaired
        // keeps painting: provisioning and the pairing code are the UI's.
        ui_boot_paint_deferred_.store(server_pairing_init() == SERVER_PAIR_OK,
                                      std::memory_order_release);
        BuildRawDrawUi(static_cast<CustomLcdDisplay*>(display));
    }

    // Set up WiFi status callback to update StatusBar
    board.SetNetworkEventCallback([this](NetworkEvent event, const std::string& data) {
        switch (event) {
            case NetworkEvent::Connected:
                ESP_LOGI(kTag, "WiFi connected: %s", data.c_str());
                wifi_connected_.store(true, std::memory_order_release);
                StartSntpClockSyncOnce();
                StartServerPairingOnce();
                // 已配对且配对任务不再运行（如重新配网后）时补一次 SyncIdle：
                // Connected 此前没有任何跃迁，而 provisioned/ConfigModeExit 会把
                // lifecycle 设成 WifiConnecting，且配对任务被 s_pairing_started
                // 永久去重 → 永卡 WifiConnecting，自动休眠永久失效。
                if (s_pairing_done.load()) {
                    char tok[8] = {0};
                    if (server_pairing_get_token(tok, sizeof(tok))) {
                        Application::GetInstance().TransitionLifecycle(
                            kLifecycleSyncIdle, "wifi reconnected, paired");
                    }
                }
                UpdateStatusBarForUi();
                // First cycle promptly after WiFi comes up — but as a timer,
                // never inline: the cycle holds an HTTP sync plus a 15-25 s
                // paint and would stall the WiFi event task, so it runs in
                // the esp_timer task via the dispatcher below. Skip the arm
                // while a cycle is already running: the cycle's entry-stop
                // cannot cover an arm from this task mid-cycle, and the
                // running cycle's terminal policy call re-arms anyway.
                if (!cycle_in_progress_.load(std::memory_order_acquire)) {
                    RearmPowerTimer(3000);
                }
                break;
            case NetworkEvent::Disconnected:
                ESP_LOGI(kTag, "WiFi disconnected");
                wifi_connected_.store(false, std::memory_order_release);
                UpdateStatusBarForUi();
                break;
            case NetworkEvent::Connecting:
            case NetworkEvent::Scanning:
                wifi_connected_.store(false, std::memory_order_release);
                Application::GetInstance().TransitionLifecycle(
                    kLifecycleWifiConnecting, "STA connecting");
                UpdateStatusBarForUi();
                break;
            case NetworkEvent::WifiConfigModeEnter:
                ESP_LOGI(kTag, "WiFi config mode entered: %s", data.c_str());
                wifi_connected_.store(false, std::memory_order_release);
                // The device is driving the radio now, so the switch goes back
                // to reporting it: an intent from before the config AP would
                // otherwise read as ON while the station is down.
                g_wifi_switch_intent = RF_SETTINGS_WIFI_SWITCH_UNKNOWN;
                Application::GetInstance().TransitionLifecycle(
                    kLifecycleApProvision, "config AP entered");
                // 开机/无 base_url 路径也要注册回调，否则 ap_client_connected/
                // provisioning/provisioned/error 全部无人接收（屏与状态机不动）。
                RegisterProvisioningStateCallback();
                if (rawdraw_ui_manager_) {
                    auto& wifi = WifiManager::GetInstance();
                    rawdraw_ui_manager_->ShowWifiConfigPage(wifi.GetApSsid(),
                                                            wifi.GetApPassword(),
                                                            wifi.GetApWebUrl());
                }
                UpdateStatusBarForUi();
                break;
            case NetworkEvent::WifiConfigModeExit:
                wifi_connected_.store(WifiManager::GetInstance().IsConnected(),
                                      std::memory_order_release);
                Application::GetInstance().TransitionLifecycle(
                    kLifecycleWifiConnecting, "config AP exited");
                UpdateStatusBarForUi();
                break;
            case NetworkEvent::ModemDetecting:
            case NetworkEvent::ModemErrorNoSim:
            case NetworkEvent::ModemErrorRegDenied:
            case NetworkEvent::ModemErrorInitFailed:
            case NetworkEvent::ModemErrorTimeout:
                wifi_connected_.store(false, std::memory_order_release);
                UpdateStatusBarForUi();
                break;
        }
    });

    // Start network (non-blocking, WiFi connects asynchronously)
    board.RequestNetwork();

    // Quiet backstop (F22): a quiet boot is provisioned+paired by
    // construction, so nothing else is in flight — if WiFi never connects,
    // no timer would ever arm and the device would stay awake on battery
    // forever. This guarantees one cycle (failed sync → backoff ladder →
    // sleep); the connected path's 3 s arm replaces it when WiFi comes up,
    // and the dispatcher's never-ran check prevents a second cycle.
    if (quiet_boot_.load(std::memory_order_acquire)) {
        RearmPowerTimer(30000);
    }

    SetDeviceState(kDeviceStateIdle);
}

void Application::BuildRawDrawUi(CustomLcdDisplay* lcd) {
    rawdraw_ui_manager_ = std::make_unique<ui::RawDrawUiManager>();
    rawdraw_ui_manager_->Init(lcd, [this, lcd](const rawdraw::Rect&, bool urgent) {
        // Every UI-side repaint (page switch, dirty rect, clock) funnels through
        // here; the canvas asks the display directly, so the label is enough to
        // tell the two refresh sources apart in the log.
        if (ui_boot_paint_deferred_.load(std::memory_order_acquire)) {
            // Cold boot with the canvas about to paint the whole panel: see the
            // member. The render has already landed in the framebuffer, so only
            // the panel refresh is skipped.
            return;
        }
        if (urgent) {
            lcd->RequestUrgentFullRefresh("ui");
        } else {
            lcd->RequestUrgentRefresh("ui");
        }
    });
    if (auto* sr = rawdraw_ui_manager_->GetSettingsRenderer()) {
        // What each row does. The menu's shape and its navigation are Rust;
        // this is the only place that knows how to restart, clear credentials,
        // sleep or toggle the radio.
        sr->SetItemHandler([this, sr](uint8_t id, bool target) {
            // Any press retires a pending "press again" prompt; a destructive
            // press below puts it back if it is still only armed.
            sr->SetItemValue(RF_SETTINGS_ITEM_RESET_NETWORK, "");
            sr->SetItemValue(RF_SETTINGS_ITEM_RESET_DEVICE, "");

            // The two resets wipe something, so they ask twice: the first press
            // arms (settings.rs::confirm_step owns the window), the second runs.
            if (rf_settings_is_destructive(id)) {
                rf_settings_confirm_t c = {};
                const uint64_t now_ms = static_cast<uint64_t>(esp_timer_get_time() / 1000);
                rf_settings_confirm(g_reset_armed_id, g_reset_armed_at_ms, id, now_ms, &c);
                g_reset_armed_id = c.armed_id;
                g_reset_armed_at_ms = c.armed_at_ms;
                if (!c.act) {
                    // The prompt names the window it obeys, and takes the
                    // number from the rule itself (settings.rs) so the two
                    // cannot drift apart.
                    const unsigned window_s = (unsigned)(rf_settings_confirm_window_ms() / 1000);
                    char prompt[32];
                    snprintf(prompt, sizeof(prompt), "%u 秒内再按一次", window_s);
                    ESP_LOGW(kTag, "Settings: %u armed; press again within %u s to confirm",
                             (unsigned)id, window_s);
                    sr->SetItemValue(id, prompt);
                    return;
                }
                ESP_LOGW(kTag, "Settings: %u confirmed", (unsigned)id);
            }

            switch (id) {
                case RF_SETTINGS_ITEM_RESTART:
                    ESP_LOGW(kTag, "Settings: restart requested");
                    esp_restart();
                    break;
                case RF_SETTINGS_ITEM_RESET_NETWORK:
                    // Wi-Fi only: the pairing (base_url + token, the `server`
                    // namespace) survives, so re-provisioning reconnects without
                    // asking for a code again.
                    ESP_LOGW(kTag, "Reset network: clearing WiFi credentials, keeping pairing");
                    SsidManager::GetInstance().Clear();
                    esp_restart();
                    break;
                case RF_SETTINGS_ITEM_RESET_DEVICE:
                    // Everything the user set up: Wi-Fi and the pairing. The
                    // device comes back needing the provisioning page and a code.
                    ESP_LOGW(kTag, "Reset device: clearing WiFi credentials and pairing token");
                    SsidManager::GetInstance().Clear();
                    server_pairing_clear();
                    esp_restart();
                    break;
                case RF_SETTINGS_ITEM_SLEEP:
                    // No row emits this while 系统 hides 省电模式 (the automatic
                    // sleep policy still puts the device down). Kept, with the
                    // entry point, so bringing the row back is one line.
                    ESP_LOGI(kTag, "Manual sleep requested from settings");
                    EnterManualSleep();
                    break;
                case RF_SETTINGS_ITEM_WIFI_TOGGLE: {
                    // The renderer asked the model what this press means and
                    // handed us the answer, so the direction is never inferred
                    // from the connection. Inferring it is what made the row
                    // look dead: with the radio down the switch read OFF, the
                    // press asked for ON, and the redraw read the connection
                    // again and drew OFF.
                    auto& wifi = WifiManager::GetInstance();
                    g_wifi_switch_intent = target ? RF_SETTINGS_WIFI_SWITCH_ON
                                                  : RF_SETTINGS_WIFI_SWITCH_OFF;
                    if (target) {
                        ESP_LOGI(kTag, "Wi-Fi setting turned ON");
                        if (!wifi.IsConnected()) {
                            // Restart a stalled attempt: StartStation() returns
                            // early while the station is already active, so a
                            // second press would otherwise do nothing at all.
                            wifi.StopStation();
                            wifi.StartStation();
                        }
                    } else {
                        ESP_LOGI(kTag, "Wi-Fi setting turned OFF");
                        wifi.StopStation();
                        wifi_connected_.store(false, std::memory_order_release);
                    }
                    UpdateWifiSettingsItem(sr, wifi_connected_.load(std::memory_order_acquire));
                    UpdateStatusBarForUi();
                    break;
                }
                case RF_SETTINGS_ITEM_WIFI_PASSWORD:
                    // 网络's second actionable row. The dots come off for as long
                    // as the user is on the row; the renderer puts them back.
                    sr->TogglePasswordReveal();
                    break;
                default:
                    break;
            }
        });
        sr->SetItemValue(RF_SETTINGS_ITEM_RESET_NETWORK, "清凭据重启");
        sr->SetItemValue(RF_SETTINGS_ITEM_SLEEP, "手动进入");
        UpdateWifiSettingsItem(sr, wifi_connected_.load(std::memory_order_acquire));
        sr->SetFirmwareVersion("v" PROJECT_VER);

        uint8_t mac_bytes[6] = {};
        esp_read_mac(mac_bytes, ESP_MAC_WIFI_STA);
        char mac_str[18];
        snprintf(mac_str, sizeof(mac_str), "%02X:%02X:%02X:%02X:%02X:%02X",
                 mac_bytes[0], mac_bytes[1], mac_bytes[2],
                 mac_bytes[3], mac_bytes[4], mac_bytes[5]);
        sr->SetDeviceInfo(mac_str, "ESP32-S3");
    }

    ESP_LOGI(kTag, "Rawdraw gallery UI initialized");
    if (esp_reset_reason() == ESP_RST_DEEPSLEEP) {
        ESP_LOGI(kTag, "Wake from deep sleep: flash activity LED and refresh UI");
        Board::GetInstance().FlashActivityLed();
        if (rawdraw_ui_manager_) {
            rawdraw_ui_manager_->RequestActivePageRefresh();
        }
    }
}

void Application::RequestPromotion() {
    // Interactive boots already have a UI; promotion is quiet-only and
    // one-shot per boot. Safe from any task — the main loop consumes it.
    if (!quiet_boot_.load(std::memory_order_acquire) ||
        promoted_.load(std::memory_order_acquire)) {
        return;
    }
    promote_requested_.store(true, std::memory_order_release);
}

void Application::ServicePromotion() {
    if (!promote_requested_.load(std::memory_order_acquire)) {
        return;
    }
    // Whoever owns UI construction is whoever runs this: the main loop
    // (app_main task, via Run()). Building the manager from the WiFi or
    // esp_timer task would race UpdateStatusBarForUi on the same unique_ptr.
    if (!quiet_boot_.load(std::memory_order_acquire) ||
        promoted_.load(std::memory_order_acquire) ||
        rawdraw_ui_manager_ != nullptr) {
        promote_requested_.store(false, std::memory_order_release);
        return;
    }
    auto* lcd = static_cast<CustomLcdDisplay*>(Board::GetInstance().GetDisplay());
    if (lcd == nullptr) {
        return;  // retry on the next main-loop tick
    }
    // F24: hand the panel to the UI before building it. Both steps are
    // needed, for different readers of the same ownership fact:
    // - page_sync_stop_display() clears DISPLAYING so RawDrawUiManager::Init's
    //   RenderAll is no longer suppressed (Init clears the framebuffer, then
    //   RenderAll early-returns while the canvas owns the panel, and the
    //   unguarded TriggerRefresh would flush the cleared white framebuffer).
    // - rf_panel_record_invalidate() drops the RTC claim that the canvas page
    //   is on the glass, since the glass is about to show a UI page instead
    //   (the §4.3.5 divergence the sleep path guards with invalidate_panel:
    //   without it the next wake's md5 compare matches and skips, leaving
    //   the blank frame up). The canvas returns via page_sync_allow_display()
    //   when the user leaves the UI.
    // Mirror cold-boot order otherwise: panel up first, then the UI, then
    // the status bar. BringUpPanel is idempotent; promoted_ keeps the build
    // one-shot.
    page_sync_stop_display();
    rf_panel_record_invalidate();
    lcd->BringUpPanel();
    BuildRawDrawUi(lcd);
    // F25: Init owns the SetOnRefreshIdle slot and replaces the shim's
    // commit-on-idle chain registered from rf_set_display, so re-chain it
    // now that the UI exists (one-shot promotion: the previous chain was
    // wiped, so this leaves exactly one).
    rf_panel_commit_hook_register();
    UpdateStatusBarForUi();
    promoted_.store(true, std::memory_order_release);
    promote_requested_.store(false, std::memory_order_release);
    ESP_LOGI(kTag, "Boot path: promoted to interactive");
    // A promotion via the config path (F21) arrives with the AP already up
    // but nothing on screen: re-render the provisioning page now that the
    // UI exists.
    if (WifiManager::GetInstance().IsConfigMode()) {
        auto& wifi = WifiManager::GetInstance();
        RegisterProvisioningStateCallback();
        rawdraw_ui_manager_->ShowWifiConfigPage(wifi.GetApSsid(),
                                                wifi.GetApPassword(),
                                                wifi.GetApWebUrl());
    }
}

// ── input routing ────────────────────────────────────────────────────────
// Who owns a gesture — the popup, the canvas or the UI — is decided in
// input.rs, including the guards that used to be re-derived at each call site
// (a popup answers before the canvas, Settings only answers UP-long if there is
// somewhere to go back to, provisioning ignores DOWN-long). Each handler below
// now only names the gesture.
void Application::OnUpClick() {
    ESP_LOGI(kTag, "UP click");
    RouteInput(RF_INPUT_UP, RF_INPUT_CLICK);
}

void Application::OnUpDoubleClick() {
    ESP_LOGI(kTag, "UP double click");
    RouteInput(RF_INPUT_UP, RF_INPUT_DOUBLE_CLICK);
}

void Application::OnDownClick() {
    ESP_LOGI(kTag, "DOWN click");
    RouteInput(RF_INPUT_DOWN, RF_INPUT_CLICK);
}

void Application::OnUpLongPress() {
    ESP_LOGI(kTag, "UP long press");
    RouteInput(RF_INPUT_UP, RF_INPUT_LONG_PRESS);
}

void Application::OnDownLongPress() {
    ESP_LOGI(kTag, "DOWN long press");
    RouteInput(RF_INPUT_DOWN, RF_INPUT_LONG_PRESS);
}

void Application::OnWifiConfigComboLongPress() {
    ESP_LOGI(kTag, "UP+DOWN long press");
    RouteInput(RF_INPUT_UP, RF_INPUT_COMBO_LONG_PRESS);
}

void Application::OnBootClick() {
    ESP_LOGI(kTag, "BOOT click");
    RouteInput(RF_INPUT_BOOT, RF_INPUT_CLICK);
}

void Application::OnBootLongPress() {
    ESP_LOGI(kTag, "BOOT long press");
    RouteInput(RF_INPUT_BOOT, RF_INPUT_LONG_PRESS);
}

void Application::RouteInput(uint8_t button, uint8_t gesture) {
    rf_input_inputs_t in = {};
    in.button = button;
    in.gesture = gesture;
    in.notify_active = notify_is_active() ? 1 : 0;
    in.canvas_displaying = page_sync_is_displaying() ? 1 : 0;
    in.on_settings = (rawdraw_ui_manager_ &&
                      rawdraw_ui_manager_->GetCurrentPage() == ui::RawDrawPageId::Settings)
                         ? 1
                         : 0;
    in.previous_is_settings =
        (rawdraw_ui_manager_ &&
         rawdraw_ui_manager_->GetPreviousPage() == ui::RawDrawPageId::Settings)
            ? 1
            : 0;
    in.config_mode = WifiManager::GetInstance().IsConfigMode() ? 1 : 0;
    in.provisioning = (GetLifecycleState() == kLifecycleApProvision) ? 1 : 0;

    rf_input_decision_t d = {};
    rf_input_decide(&in, &d);

    // Side effects that belong to "the user pressed something" rather than to
    // whoever owns the screen. A long press counts as activity (it repaints and
    // restarts the idle clock); a click only flashes the LED — and the old
    // handlers skipped that flash when UP/DOWN was answering a popup, while
    // BOOT flashed either way.
    if (gesture == RF_INPUT_LONG_PRESS || gesture == RF_INPUT_COMBO_LONG_PRESS) {
        NoteButtonActivity();
    } else if (button == RF_INPUT_BOOT || d.action != RF_INPUT_ACTION_NOTIFY_ACK) {
        Board::GetInstance().FlashActivityLed();
    }

    switch (d.action) {
        case RF_INPUT_ACTION_IGNORE:
            return;

        case RF_INPUT_ACTION_UI_INPUT: {
            if (rawdraw_ui_manager_ == nullptr) {
                return;
            }
            rawdraw::ButtonEvent::Type type = rawdraw::ButtonEvent::kUpClick;
            switch (button) {
                case RF_INPUT_UP:
                    type = (gesture == RF_INPUT_DOUBLE_CLICK)
                               ? rawdraw::ButtonEvent::kUpDoubleClick
                               : rawdraw::ButtonEvent::kUpClick;
                    break;
                case RF_INPUT_DOWN:
                    type = rawdraw::ButtonEvent::kDownClick;
                    break;
                default:
                    type = (gesture == RF_INPUT_LONG_PRESS)
                               ? rawdraw::ButtonEvent::kBootLongPress
                               : rawdraw::ButtonEvent::kBootClick;
                    break;
            }
            rawdraw_ui_manager_->HandleInput(rawdraw::ButtonEvent{type});
            return;
        }

        case RF_INPUT_ACTION_NOTIFY_ACK:
            notify_post_ack(d.agree ? "agree" : "reject");
            return;

        case RF_INPUT_ACTION_NOTIFY_DISMISS:
            notify_dismiss();
            return;

        case RF_INPUT_ACTION_NOTIFY_FETCH_NEXT:
            notify_request_next();
            return;

        case RF_INPUT_ACTION_CANVAS_PREV:
            page_sync_prev();
            return;

        case RF_INPUT_ACTION_CANVAS_NEXT:
            page_sync_next();
            return;

        case RF_INPUT_ACTION_ENTER_SETTINGS:
            EnterSettingsFromInput(d.enter_settings != 0, d.drop_orphan_notification != 0);
            return;

        case RF_INPUT_ACTION_LEAVE_SETTINGS:
            if (d.switch_to_previous != 0 && rawdraw_ui_manager_ != nullptr) {
                ESP_LOGI(kTag, "input: leaving settings");
                rawdraw_ui_manager_->SwitchPage(rawdraw_ui_manager_->GetPreviousPage());
            }
            // Leaving Settings hands the panel back to the canvas, which was
            // suspended when Settings was entered.
            page_sync_allow_display();
            return;

        case RF_INPUT_ACTION_ENTER_WIFI_CONFIG:
            EnterWifiConfigMode();
            return;

        case RF_INPUT_ACTION_EXIT_WIFI_CONFIG:
            WifiManager::GetInstance().StartStation();
            EnterSettingsFromInput(d.enter_settings != 0, d.drop_orphan_notification != 0);
            return;

        case RF_INPUT_ACTION_STOP_CANVAS:
            page_sync_stop_display();
            EnterSettingsFromInput(d.enter_settings != 0, d.drop_orphan_notification != 0);
            return;

        default:
            ESP_LOGW(kTag, "input: unknown action %u", (unsigned)d.action);
            return;
    }
}

void Application::EnterSettingsFromInput(bool enter, bool drop_orphan) {
    if (!enter) {
        // Provisioning: the panel was handed back above, and the provisioning
        // page needs the user where they are.
        ESP_LOGI(kTag, "input: settings entry skipped (provisioning)");
        return;
    }
    if (drop_orphan) {
        // Quiet, not notify_dismiss(): the loud one paints the canvas back onto
        // the panel and then fights the SwitchPage below for it.
        notify_dismiss_quiet();
    }
    if (rawdraw_ui_manager_ != nullptr) {
        ESP_LOGI(kTag, "input: entering settings");
        rawdraw_ui_manager_->SwitchPage(ui::RawDrawPageId::Settings);
    }
}

void Application::NoteButtonActivity() {
    Board::GetInstance().FlashActivityLed();
    // Someone is in front of the device: whatever the boot held back, they are
    // waiting on a screen now and the canvas's frame is no longer the answer.
    ui_boot_paint_deferred_.store(false, std::memory_order_release);
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->RequestActivePageRefresh();
    } else {
        // Someone is in front of the device: a quiet boot must gain its UI
        // and panel (F20). Request only — the main loop owns construction.
        RequestPromotion();
    }
    // The policy's idle clock starts here: until now no interaction ever
    // reset the sleep timer, so a device mid-use could doze off.
    last_activity_ms_ = esp_timer_get_time() / 1000;
}

void Application::EnterWifiConfigMode() {
    wifi_connected_.store(false, std::memory_order_release);
    ESP_LOGI(kTag, "Entering WiFi config mode by long press");
    // The config page is rendered by the UI manager and AP mode blocks
    // sleeping: on a quiet boot ask the main loop to promote first, or the
    // AP comes up with a blank panel that drains until dead (F21). The
    // promotion re-renders the page once the UI exists.
    RequestPromotion();
    WifiManager::GetInstance().StartConfigAp();
    if (rawdraw_ui_manager_ && WifiManager::GetInstance().IsConfigMode()) {
        auto& wifi = WifiManager::GetInstance();
        // Register provisioning state callback for screen updates
        RegisterProvisioningStateCallback();
        rawdraw_ui_manager_->ShowWifiConfigPage(wifi.GetApSsid(),
                                                wifi.GetApPassword(),
                                                wifi.GetApWebUrl());
    }
    UpdateStatusBarForUi();
}

void Application::RearmPowerTimer(uint32_t delay_ms) {
    if (sleep_timer_ == nullptr) {
        esp_timer_create_args_t args = {};
        args.callback = [](void* arg) {
            static_cast<Application*>(arg)->OnPowerTimer();
        };
        args.arg = this;
        args.dispatch_method = ESP_TIMER_TASK;
        args.name = "app_sync_sleep";
        ESP_ERROR_CHECK(esp_timer_create(&args, &sleep_timer_));
    }
    esp_timer_stop(sleep_timer_);
    if (delay_ms == 0) {
        return;
    }
    ESP_LOGI(kTag, "Power policy re-armed in %u ms", delay_ms);
    ESP_ERROR_CHECK(esp_timer_start_once(sleep_timer_, (int64_t)delay_ms * 1000));
}

void Application::NoteQuietWake() {
    // Negative backdates the idle clock past any grace window; see
    // ServicePowerPolicy's idle_ms computation.
    last_activity_ms_ = -1;
}

void Application::OnPowerTimer() {
    // The timer fires for two reasons: (a) the first evaluation after WiFi
    // comes up, which must run the one-shot sync/paint cycle — until this
    // boot's cycle has run there is nothing to evaluate; (b) stay-awake
    // re-arms (mains/grace/busy), which only need a policy re-check —
    // re-running the HTTP sync plus a 15-25 s paint every 15/30/60 s would
    // restore the chatter this redesign removes. So: cycle when one is due
    // — never yet run this boot, or the server's poll interval has elapsed
    // (a device left awake on mains must still rotate its canvas) — and a
    // bare policy check otherwise.
    if (cycle_in_progress_.load(std::memory_order_acquire)) {
        // A WiFi reconnect armed this tick mid-cycle despite the guard in
        // the connected path; the running cycle's terminal policy call
        // re-arms, so just defer this tick past it.
        RearmPowerTimer(3000);
        return;
    }
    const int64_t now_ms = esp_timer_get_time() / 1000;
    const uint32_t poll_s = page_sync_poll_s();
    const bool never_ran = (last_cycle_ms_ < 0);
    const bool interval_elapsed =
        !never_ran && poll_s > 0 &&
        (uint64_t)(now_ms - last_cycle_ms_) >= (uint64_t)poll_s * 1000u;
    if (never_ran || interval_elapsed) {
        RunPowerCycle();
    } else {
        ServicePowerPolicy();
    }
}

void Application::StopRadioForPaint() {
    // Paired stop/start through WifiManager (wifi_manager.h:71-72,
    // wifi_manager.cc:114-177): StopStation() clears station_active_ AND tears
    // down the driver (WifiStation::Stop: unregister handlers, stop timers,
    // esp_wifi_scan_stop/disconnect/stop, destroy netif — wifi_station.cc:647-693).
    // A bare esp_wifi_stop() here would leave station_active_ true, so a later
    // StartStation() would early-return "already active" (wifi_manager.cc:121-124)
    // and never restart the driver. Both deep-sleep callers also route through
    // here now (they never return, so station_active_ is never read again —
    // no behavior change there, just one shared teardown).
    WifiManager::GetInstance().StopStation();
    wifi_connected_.store(false, std::memory_order_release);
    rf_rails_audio(0);          // amp off before audio power off (silent)
}

void Application::RunPowerCycle() {
    // The one-shot duty-cycle step both boot paths share (Task 6 calls this
    // once per quiet wake). Each stage takes and releases the module locks
    // in turn — state -> display order holds, never nested — so there is no
    // lock-order risk across the sequence.
    // The cycle must not be interruptible by its own re-arm timer: a mid-cycle
    // evaluation could see stale sync state with an idle panel/audio and
    // decide to sleep — cutting WiFi and the audio rail before anything is
    // painted, forever repeating on every wake. Stop the timer here; the
    // cycle's own final ServicePowerPolicy() call re-arms it, so there is no
    // gap. This also makes the attempt-flag check-then-clear below
    // single-threaded. (The dispatcher and the WiFi connected path carry a
    // cycle_in_progress_ guard on top, for arms that land mid-cycle anyway.)
    if (sleep_timer_ != nullptr) {
        esp_timer_stop(sleep_timer_);
    }
    // Visible to the WiFi connected path (different task): while this is set
    // it must not arm the timer, and the dispatcher above defers instead of
    // evaluating. Cleared after the terminal policy call returns (the sleep
    // path never returns, but RAM dies with it, so no stale set survives).
    cycle_in_progress_.store(true, std::memory_order_release);
    last_cycle_ms_ = esp_timer_get_time() / 1000;
    // Task 1 duration ledger: one count per wake here. The wake booking is
    // intentionally the FIRST counter write of the cycle, before this cycle's
    // own schedule GET: each wake's GET therefore carries w through the
    // current wake but a/r/g/f only through the previous completed cycle
    // (this cycle's spans book at the sleep teardown below). Per-wake
    // differencing stays exact; absolute snapshots mix the two instants.
    rf_power_count_wake();
    cycle_awake_base_ms_ = esp_timer_get_time() / 1000;
    // The backoff streak lives in RTC memory (rf_fail_streak_*) because RAM
    // is cleared on every wake. Mark the attempt and snapshot its outcome
    // here so the policy consumes this cycle's result — never a re-read that
    // could observe a half-finished cycle.
    sync_attempted_ = true;
    sync_result_ok_ = page_sync_sync_once();
    notify_request_next();
    // Unconditional, even on an empty schedule: that call draws and records
    // the empty hint, which is what makes the canvas's display-ownership
    // claim honest.
    // Fetch the page BEFORE the radio comes down: `paint_if_changed` downloads
    // the target page on demand (see page_sync.rs), so tearing the radio down
    // first would strand the update. Only cut the radio once it is resident —
    // on a failed fetch we keep it, so the existing retry semantics are intact.
    // Guard is triple (Ruling 2b): quiet boot (duty-cycle path) AND not yet
    // promoted AND the canvas still owns the glass. IsQuietBoot alone survives
    // promotion (a quiet boot turned interactive would cut the radio with no
    // timely restart); !promoted_ + is_displaying alone would also fire on an
    // interactive-cold-boot cycle (start() sets DISPLAYING on any paired boot,
    // :252-253 — the round-1 guard hit it). All three must hold.
    const bool paint_ready = page_sync_prepare_paint();
    const bool canvas_owns_quiet_cycle =
        IsQuietBoot() && !promoted_ && page_sync_is_displaying();
    if (paint_ready && canvas_owns_quiet_cycle) {
        StopRadioForPaint();
        radio_cut_for_paint_ = true;
    }
    page_sync_paint_if_changed();
    // The cold boot may have held the UI's first paint back for exactly this
    // frame; from here the UI paints normally (button activity would have
    // cleared the flag before this point).
    ui_boot_paint_deferred_.store(false, std::memory_order_release);
    ServicePowerPolicy();
    cycle_in_progress_.store(false, std::memory_order_release);
}

void Application::ServicePowerPolicy() {
    const int64_t now_ms = esp_timer_get_time() / 1000;
    Settings nvs(kPowerNamespace, true);
    rf_power_inputs_t in = {};
    in.mains = Board::GetInstance().IsPowerPresent() ? 1 : 0;
    in.notify_active = notify_is_active() ? 1 : 0;
    // CanSleepNow folds in the lifecycle gate (SyncIdle only) plus the panel/
    // audio busy bookkeeping, holds and deadlines — the three checks the old
    // EnterScheduledSleep spelled out by hand.
    in.busy = SleepManager::GetInstance().CanSleepNow() ? 0 : 1;
    in.sync_ok = page_sync_sync_ok() ? 1 : 0;
    in.screen_active = page_sync_screen_active() ? 1 : 0;
    in.on_canvas = page_sync_is_displaying() ? 1 : 0;
    // Negative last_activity_ms_ = quiet wake: backdate past any grace window
    // so the duty-cycle wake is computed straight away.
    in.idle_ms = (last_activity_ms_ < 0)
        ? UINT64_MAX
        : (uint64_t)(now_ms - last_activity_ms_);
    in.grace_ms = (uint32_t)nvs.GetInt("idle_grace_min", 3) * 60000u;
    in.max_sleep_s = (uint32_t)nvs.GetInt("max_sleep_min", 60) * 60u;
    in.poll_s = page_sync_poll_s();
    in.sleep_poll_s = page_sync_sleep_poll_s();
    // RTC-persisted across deep sleep (RAM is cleared on every wake, so a
    // member counter could never climb the ladder). Read BEFORE the decision
    // so the first failure sleeps 60 s (streak 0), then 120, 240, …; the
    // store below advances it only after this read.
    const uint32_t streak = rf_fail_streak_get();
    in.fail_streak = streak;
    // -1 means unknown/empty schedule; decide() maps negatives to None and
    // sleeps for the cap. Never pre-clamp to 0 here: 0 reads as "a page
    // changes right now" and floors at 60 s.
    in.seconds_until_next_page = page_sync_next_wake_s();
    rf_power_decision_t d = {};
    rf_power_decide(&in, &d);
    // Persist the ladder step for the NEXT wake now that this decision has
    // consumed the read above — but only if a sync was actually attempted
    // since the last evaluation (RunPowerCycle sets the flag; timer re-arms
    // on mains/grace/busy run no sync and must not ratchet the counter, or
    // the first battery sleep would jump straight to the cap). The outcome
    // comes from this cycle's snapshot, never a re-read: a timer-side
    // evaluation mid-cycle could otherwise observe stale sync state.
    // Success resets to 0, failure climbs (capped; decide() itself caps the
    // shift at 5, this just keeps the word sane).
    if (sync_attempted_) {
        sync_attempted_ = false;
        if (sync_result_ok_) {
            rf_fail_streak_set(0);
        } else {
            rf_fail_streak_set(streak + 1 > 8 ? 8 : streak + 1);
        }
    }

    if (!d.sleep) {
        // Paired with StopRadioForPaint above: StopStation() cleared
        // station_active_, so this StartStation() really restarts the driver
        // (wifi_manager.cc:121-124 early-return no longer fires). Without it
        // the session would live on with no Wi-Fi — and a paint in this cycle
        // synchronously marks Display busy, so decide() returns StayAwake{busy}
        // and this branch is exactly where the device lands. Deep-sleep
        // reboots clear the flag, so it is only ever consumed here.
        if (radio_cut_for_paint_) {
            radio_cut_for_paint_ = false;
            WifiManager::GetInstance().StartStation();
        }
        // Held awake on mains: this boot will never sleep again, so a quiet
        // boot must gain its UI and panel now — otherwise the device sits
        // awake with no screen and no way back except a power cycle (F20).
        if (in.mains) {
            RequestPromotion();
        }
        ESP_LOGI(kTag, "Stay awake (%u ms)", d.stay_awake_ms);
        RearmPowerTimer(d.stay_awake_ms);
        return;
    }
    if (d.invalidate_panel) {
        rf_panel_record_invalidate();
    }
    // pin2 is the raw CHARGE_DETECT_GPIO level: expect 1 while unplugged
    // (attach drives LOW per charge_status.cc), and mains must read 0 on
    // battery. If the matrix disagrees, the fix is CHARGE_DETECT_PLUG_PULLS_LOW
    // plus, if needed, charge_status's inversion — no pull is added here
    // deliberately (the level belongs to the charger IC's own network).
    ESP_LOGI(kTag, "Deep sleep %u s (mains=%d sync_ok=%d pin2=%d)", d.wake_s,
             (int)in.mains, (int)in.sync_ok, gpio_get_level(CHARGE_DETECT_GPIO));
    TransitionLifecycle(kLifecycleSleep, "power policy");
    wifi_connected_.store(false, std::memory_order_release);
    // F5: both duration spans close HERE, at one shared end instant just
    // before the radio goes off (booking must precede esp_deep_sleep_start,
    // which never returns). awake = aw0 -> radio off, radio(upper bound) =
    // cycle start -> radio off: same end, awake >= radio by construction,
    // same esp_timer ms clock. awake is NOT closed in RunPowerCycle above —
    // closing it before the policy ran left part of the cycle uncounted and
    // could read below radio for the same cycle.
    const int64_t teardown_ms = esp_timer_get_time() / 1000;
    rf_power_add_awake_ms((uint32_t)(teardown_ms - cycle_awake_base_ms_));
    // UPPER BOUND, not exact radio-on time (see header comment at aw0):
    // includes WiFi connect + all requests; never quote as "radio was on X".
    rf_power_add_radio_ms((uint32_t)(teardown_ms - last_cycle_ms_));
    // Shared with the pre-paint cutoff above. StopStation() also clears the
    // app's wifi flag and the audio rail; the sleep entry already cleared the
    // flag, so the repeat store inside is idempotent. station_active_ is never
    // read again on this path (no return from deep sleep).
    StopRadioForPaint();
    esp_sleep_enable_timer_wakeup((uint64_t)d.wake_s * 1000000ULL);
    esp_sleep_enable_ext0_wakeup((gpio_num_t)BOOT_BUTTON_GPIO, 0);
    // Charger-insert wake, but only while actually unplugged. decide() holds the
    // device awake on mains, so this branch is the only one reachable today and
    // the guard changes nothing — it just stops that from being load-bearing:
    // armed with the charger level while sitting on mains, the device would wake
    // on the level it is already at and loop. See config.h for the polarity.
    if (!Board::GetInstance().IsPowerPresent()) {
        EnableChargerInsertWakeup();
    }
    esp_deep_sleep_start();
}

void Application::StartServerPairingOnce() {
    if (s_pairing_started.exchange(true)) {
        return;
    }
    if (xTaskCreatePinnedToCore(
            &Application::ServerPairingTaskEntry, "srv_pair", 8192,
            this, 3, nullptr, 1) != pdPASS) {
        ESP_LOGE(kTag, "ServerPairing: failed to create task");
        s_pairing_started.store(false);
        return;
    }
    ESP_LOGI(kTag, "ServerPairing: task started");
}

void Application::ServerPairingTaskEntry(void* arg) {
    ServerPairingTaskTrampoline(arg);
}

bool Application::CanEnterSleepMode() const {
    // Only a paired, idle device may sleep; on top of this SleepManager checks
    // the busy sources, holds and the activity deadline.
    // A paired device that cannot reach the network must still be allowed to
    // sleep: a cycle that has been attempted and failed is the duty cycle's own
    // signal to retry on the next timer, whereas spinning awake at the busy
    // cadence flattens the battery. Provisioning and pairing stay excluded —
    // those flows need the user and the screen. Two consequences, both
    // intended: mains is unaffected (decide() short-circuits on mains before
    // it ever reads busy), and an interactive boot that cannot reach WiFi
    // now also sleeps once the grace expires — which is right for a battery
    // device, and any button activity re-stamps the grace so using it keeps
    // it awake.
    if (GetLifecycleState() == kLifecycleSyncIdle) return true;
    // last_cycle_ms_ < 0 below means no cycle ran yet this boot (the -1
    // sentinel); a real stamp is esp_timer ms and always >= 0 here.
    return GetLifecycleState() == kLifecycleWifiConnecting &&
           last_cycle_ms_ >= 0 && !sync_result_ok_;
}

void Application::EnterManualSleep() {
    ESP_LOGI(kTag, "Entering manual deep sleep; stopping local services and WiFi");
    if (sleep_timer_ != nullptr) {
        esp_timer_stop(sleep_timer_);
    }
    // Settings owns the screen here, so an RTC panel record claiming the
    // canvas is up would be a lie on the next wake: drop it, exactly as the
    // policy does when sleeping off-canvas.
    rf_panel_record_invalidate();
    // Same teardown order as the policy path: amp off before audio power off
    // (silent shutdown, no pop), then the radio.
    StopRadioForPaint();
    UpdateStatusBarForUi();
    vTaskDelay(pdMS_TO_TICKS(300));
    esp_sleep_enable_ext0_wakeup(static_cast<gpio_num_t>(BOOT_BUTTON_GPIO), 0);
    // Charger-insert wake like the policy path — but only while actually
    // unplugged: asleep-on-mains with the charger level armed would wake
    // instantly.
    if (!Board::GetInstance().IsPowerPresent()) {
        EnableChargerInsertWakeup();
    }
    esp_deep_sleep_start();
}

void Application::Run() {
    // Reached on both paths (main.cc calls this unconditionally after
    // Initialize), in the app_main task — which is why ServicePromotion can
    // own UI construction here: no other task touches the manager pointer.
    while (true) {
        ServicePromotion();
        if (rawdraw_ui_manager_) {
            rawdraw_ui_manager_->PumpClockRefresh();
        }
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

bool Application::SetDeviceState(DeviceState state) {
    const DeviceState old_state = state_.exchange(state, std::memory_order_acq_rel);
    ESP_LOGI(kTag, "State %d -> %d", old_state, state);
    return true;
}

namespace {

const char* LifecycleName(LifecycleState s) {
    switch (s) {
        case kLifecycleUnknown: return "Unknown";
        case kLifecycleBoot: return "Boot";
        case kLifecycleWifiConnecting: return "WifiConnecting";
        case kLifecycleApProvision: return "ApProvision";
        case kLifecyclePairStart: return "PairStart";
        case kLifecyclePairWaitCode: return "PairWaitCode";
        case kLifecycleSyncIdle: return "SyncIdle";
        case kLifecycleSleep: return "Sleep";
        case kLifecycleError: return "Error";
        default: return "?";
    }
}

}  // namespace
void Application::TransitionLifecycle(LifecycleState next, const char* reason) {
    const LifecycleState old = lifecycle_.exchange(next, std::memory_order_acq_rel);
    if (old == next) {
        return;  // 只记录真正的变更，重复上报由各模块自带 LOG 覆盖
    }
    // 迁移合法性由 lifecycle.rs 判定（表驱动、主机可测）。可疑不等于致命：
    // 一次状态广播不值得让设备停下来，但要当场可见——历史上"永卡
    // WifiConnecting 导致自动休眠永久失效"那种事，就是没人看得见才活了很久。
    rf_lifecycle_verdict_t v = {};
    rf_lifecycle_verdict(static_cast<uint8_t>(old), static_cast<uint8_t>(next), &v);
    if (v.kind == RF_LIFECYCLE_SUSPICIOUS && v.message != nullptr) {
        ESP_LOGW(kTag, "Lifecycle: %s -> %s (%s) 可疑：%s", LifecycleName(old),
                 LifecycleName(next), reason ? reason : "", v.message);
    }
    ESP_LOGI(kTag, "Lifecycle: %s -> %s (%s)",
             LifecycleName(old), LifecycleName(next), reason ? reason : "");
}

void Application::Schedule(std::function<void()>&& callback) {
    if (callback) {
        callback();
    }
}

void Application::PlaySound(const std::string_view& sound) {
    audio_service_.PlaySound(sound);
}

void Application::PlaySound(const std::string_view& sound, int duration_ms) {
    audio_service_.PlaySound(sound, duration_ms);
}

void Application::MuteSound() {
    audio_service_.MuteOutput();
}

void Application::StopSound() {
    audio_service_.ResetDecoder();
}

void Application::UpdateStatusBarForUi() {
    auto& board = Board::GetInstance();
    int battery_level = -1;
    bool charging = false;
    bool discharging = false;
    board.GetBatteryLevel(battery_level, charging, discharging);

    if (rawdraw_ui_manager_) {
        const bool wifi_connected = wifi_connected_.load(std::memory_order_acquire);
        // Was IsHttpServerRunning(), which is hardcoded false: the dot never lit
        // even while the device was paired and polling.
        const bool server_reachable = page_sync_server_reachable();
        ui::RawDrawStatusBarData data = rawdraw_ui_manager_->GetStatusBarData();
        data.page_title = ui::RawDrawUiManager::GetPageTitle(rawdraw_ui_manager_->GetCurrentPage());
        data.wifi_connected = wifi_connected;
        data.server_connected = server_reachable;
        data.battery_level = battery_level;
        data.battery_charging = charging;
        rawdraw_ui_manager_->UpdateStatusBar(data);
        UpdateWifiSettingsItem(rawdraw_ui_manager_->GetSettingsRenderer(), wifi_connected);
        rawdraw_ui_manager_->RequestActivePageRefresh();
    }
    return;
}

void Application::UpdateWifiStatusForProvisioning(const std::string& state, int reason) {
    if (!rawdraw_ui_manager_) return;
    auto* renderer = rawdraw_ui_manager_->GetWifiRenderer();
    if (!renderer) return;
    rawdraw::WifiStatus status = renderer->GetStatus();
    auto& wifi = WifiManager::GetInstance();

    if (state == "ap_started") {
        status.state = rawdraw::WifiState::ApStarted;
        status.ap_ssid = wifi.GetApSsid();
        status.ap_password = wifi.GetApPassword();
        status.ap_url = wifi.GetApWebUrl();
    } else if (state == "ap_client_connected") {
        status.state = rawdraw::WifiState::ApClientConnected;
    } else if (state == "provisioning") {
        status.state = rawdraw::WifiState::Provisioning;
        status.ssid = wifi.GetSsid();
        status.provisioning_step = 10;  // starting
    } else if (state == "provisioned") {
        status.state = rawdraw::WifiState::Connected;
        status.ssid = wifi.GetSsid();
        // 显示配网时填写的服务端地址。此前误用设备自身 IP，
        // 屏上会显示成 http://<本机IP>:9002（与真实服务端无关）。
        char base_url[128] = {0};
        status.server_uri = server_pairing_get_base_url(base_url, sizeof(base_url))
            ? std::string(base_url)
            : std::string();
    } else if (state == "error") {
        status.state = rawdraw::WifiState::Error;
        status.error_code = reason;
        // error_msg will be filled by ReasonToMessage at render time
    }

    // The 网络 page shows the same reason; it goes away as soon as the device
    // is doing anything else.
    g_last_wifi_error = (state == "error") ? reason : 0;
    if (rawdraw_ui_manager_) {
        UpdateWifiSettingsItem(rawdraw_ui_manager_->GetSettingsRenderer(),
                               wifi_connected_.load(std::memory_order_acquire));
    }
    renderer->Update(status);
    rawdraw_ui_manager_->RequestActivePageRefresh();
}
