#include "application.h"

#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"
#include "boards/zectrix-s3-epaper-4.2/config.h"
#include "board.h"

#include "display.h"
#include "settings.h"
#include "ui/rawdraw_ui_manager.h"
#include "wifi_manager.h"

#include <esp_mac.h>
#include <esp_log.h>
#include <esp_sleep.h>
#include <esp_sntp.h>
#include <esp_system.h>
#include <esp_wifi.h>
#include <atomic>
#include <cstring>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include "common/server_pairing.h"
#include "common/sleep_manager.h"
#include "ssid_manager.h"
#include "page_sync.h"
#include "notify.h"

#include <ctime>

namespace {

constexpr char kTag[] = "Application";
constexpr char kSyncNamespace[] = "sync";
constexpr char kSyncIntervalKey[] = "sync_interval";


// Index space is the pushed item list, Section rows included:
// 0 系统 / 1 重启 / 2 重置网络 / 3 网络 / 4 Wi-Fi / 5 省电模式 / 6 关于 / 7 固件
constexpr int kSettingsWifiIndex = 4;



void UpdateWifiSettingsItem(rawdraw::SettingsRenderer* renderer, bool connected,
                            const char* value = nullptr) {
    if (!renderer) return;
    renderer->UpdateChecked(kSettingsWifiIndex, connected);
    renderer->UpdateItem(kSettingsWifiIndex, value ? value : (connected ? "已连接" : "未连接"));
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
}  // namespace

Application::Application() = default;

Application::~Application() {
    if (sleep_timer_ != nullptr) {
        esp_timer_stop(sleep_timer_);
        esp_timer_delete(sleep_timer_);
        sleep_timer_ = nullptr;
    }
}

void Application::Initialize() {
    auto& board = Board::GetInstance();
    SetDeviceState(kDeviceStateStarting);
    TransitionLifecycle(kLifecycleBoot, "init");

    AudioCodec* codec = board.GetAudioCodec();
    if (codec == nullptr) {
        ESP_LOGE(kTag, "Audio codec is null");
        SetDeviceState(kDeviceStateFatalError);
        return;
    }

    audio_service_.Initialize(codec);
    audio_service_.Start();

    Display* display = board.GetDisplay();
    if (display == nullptr) {
        ESP_LOGW(kTag, "No display available, skipping init");
        SetDeviceState(kDeviceStateFatalError);
        return;
    }


    auto* lcd = static_cast<CustomLcdDisplay*>(display);
    rawdraw_ui_manager_ = std::make_unique<ui::RawDrawUiManager>();
    rawdraw_ui_manager_->Init(lcd, [lcd](const rawdraw::Rect&, bool urgent) {
        // Every UI-side repaint (page switch, dirty rect, clock) funnels through
        // here; the canvas asks the display directly, so the label is enough to
        // tell the two refresh sources apart in the log.
        if (urgent) {
            lcd->RequestUrgentFullRefresh("ui");
        } else {
            lcd->RequestUrgentRefresh("ui");
        }
    });
    if (auto* sr = rawdraw_ui_manager_->GetSettingsRenderer()) {
        std::vector<rawdraw::SettingsItemDef> items;
        items.push_back({"系统", "", nullptr, rawdraw::SettingsItemType::Section, false});
        items.push_back({"重启", "执行", nullptr, rawdraw::SettingsItemType::Action, false,
                         []() { esp_restart(); }});
        items.push_back({"重置网络", "清凭据重启", nullptr, rawdraw::SettingsItemType::Action, false,
                         []() {
                             ESP_LOGW(kTag, "Reset network: clearing WiFi credentials and pairing token");
                             SsidManager::GetInstance().Clear();
                             server_pairing_clear();
                             esp_restart();
                         }});
        items.push_back({"网络", "", nullptr, rawdraw::SettingsItemType::Section, false});
        items.push_back({"Wi-Fi", "未连接", nullptr, rawdraw::SettingsItemType::Checkbox, false,
                         [this, sr]() {
                             auto& wifi = WifiManager::GetInstance();
                             if (wifi_connected_.load(std::memory_order_acquire) || wifi.IsConnected()) {
                                 ESP_LOGI(kTag, "Wi-Fi setting toggled OFF");
                                 wifi.StopStation();
                                 wifi_connected_.store(false, std::memory_order_release);
                                 UpdateWifiSettingsItem(sr, false);
                             } else {
                                 ESP_LOGI(kTag, "Wi-Fi setting toggled ON");
                                 UpdateWifiSettingsItem(sr, false, "连接中");
                                 wifi.StartStation();
                             }
                             UpdateStatusBarForUi();
                         }});
        items.push_back({"省电模式", "手动进入", nullptr,
                         rawdraw::SettingsItemType::Action, false,
                         [this]() {
                             ESP_LOGI(kTag, "Manual sleep requested from settings");
                             EnterManualSleep();
                         }});
        items.push_back({"关于", "", nullptr, rawdraw::SettingsItemType::Section, false});
        items.push_back({"固件", PROJECT_VER, nullptr, rawdraw::SettingsItemType::Normal, false});
        sr->SetItems(items);
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
        board.FlashActivityLed();
        if (rawdraw_ui_manager_) {
            rawdraw_ui_manager_->RequestActivePageRefresh();
        }
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
                ArmSyncSleepTimer();
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

    SetDeviceState(kDeviceStateIdle);
}

void Application::OnUpClick() {
    ESP_LOGI(kTag, "UP click");
    // 通知展示时上键确认（agree）
    if (notify_is_active()) {
        notify_post_ack("agree");
        return;
    }
    Board::GetInstance().FlashActivityLed();
    // 画板显示时上键翻页
    if (page_sync_is_displaying()) {
        page_sync_prev();
        return;
    }
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->HandleInput(rawdraw::ButtonEvent{rawdraw::ButtonEvent::kUpClick});
    }
}

void Application::OnUpDoubleClick() {
    ESP_LOGI(kTag, "UP double click");
    Board::GetInstance().FlashActivityLed();
    // The overlay is a UI affordance; while a notification or the canvas owns
    // the panel it would be drawn over and lose its backing snapshot.
    if (notify_is_active() || page_sync_is_displaying()) {
        return;
    }
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->HandleInput(
            rawdraw::ButtonEvent{rawdraw::ButtonEvent::kUpDoubleClick});
    }
}

void Application::OnDownClick() {
    ESP_LOGI(kTag, "DOWN click");
    // 通知展示时下键拒绝（reject）
    if (notify_is_active()) {
        notify_post_ack("reject");
        return;
    }
    Board::GetInstance().FlashActivityLed();
    // 画板显示时下键翻页
    if (page_sync_is_displaying()) {
        page_sync_next();
        return;
    }
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->HandleInput(rawdraw::ButtonEvent{rawdraw::ButtonEvent::kDownClick});
    }
}
void Application::OnUpLongPress() {
    ESP_LOGI(kTag, "UP long press");
    NoteButtonActivity();
    // 只有 Settings 页响应：返回上一个页面（之前是原地切 Settings 不动）
    if (rawdraw_ui_manager_ &&
        rawdraw_ui_manager_->GetCurrentPage() == ui::RawDrawPageId::Settings) {
        const auto prev = rawdraw_ui_manager_->GetPreviousPage();
        if (prev != ui::RawDrawPageId::Settings) {
            ESP_LOGI(kTag, "UP long press - leaving settings");
            rawdraw_ui_manager_->SwitchPage(prev);
        }
        // 离开 Settings：把屏幕还给画板（此前进 Settings 时挂起过）
        page_sync_allow_display();
    }
}

void Application::OnDownLongPress() {
    ESP_LOGI(kTag, "DOWN long press");
    NoteButtonActivity();
    // 统一收口：配网中忽略；离开当前屏前作废孤儿弹窗（否则切屏后
    // 通知还在后台收 UP/DOWN 当 agree/reject）。
    // 用 quiet 版：notify_dismiss 会把画板画回屏幕，与随后的 SwitchPage 抢屏。
    if (GetLifecycleState() == kLifecycleApProvision) {
        ESP_LOGI(kTag, "DOWN long press ignored during provisioning");
        return;
    }
    if (notify_is_active()) {
        notify_dismiss_quiet();
    }
    if (rawdraw_ui_manager_) {
        ESP_LOGI(kTag, "DOWN long press - entering settings");
        rawdraw_ui_manager_->SwitchPage(ui::RawDrawPageId::Settings);
    }
}

void Application::OnWifiConfigComboLongPress() {
    ESP_LOGI(kTag, "UP+DOWN long press");
    NoteButtonActivity();
    EnterWifiConfigMode();
}

void Application::OnBootClick() {
    ESP_LOGI(kTag, "BOOT click");
    Board::GetInstance().FlashActivityLed();
    // 通知展示时 BOOT 短按直接关闭（不发 ack）
    if (notify_is_active()) {
        notify_dismiss();
        return;
    }
    // 画板显示时 BOOT 短按拉取下一条待确认通知（异步，不阻塞回调）
    if (page_sync_is_displaying()) {
        notify_request_next();
        return;
    }
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->HandleInput(rawdraw::ButtonEvent{rawdraw::ButtonEvent::kBootClick});
    }
}

void Application::OnBootLongPress() {
    ESP_LOGI(kTag, "BOOT long press");
    NoteButtonActivity();
    if (WifiManager::GetInstance().IsConfigMode()) {
        ESP_LOGI(kTag, "BOOT long press - exiting WiFi config AP");
        WifiManager::GetInstance().StartStation();
        OnDownLongPress();
        return;
    }
    // 画板显示时 BOOT 长按退出画板：复用 OnDownLongPress（含弹窗作废）
    if (page_sync_is_displaying()) {
        page_sync_stop_display();
        OnDownLongPress();
        return;
    }
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->HandleInput(rawdraw::ButtonEvent{rawdraw::ButtonEvent::kBootLongPress});
    }
}

void Application::NoteButtonActivity() {
    Board::GetInstance().FlashActivityLed();
    if (rawdraw_ui_manager_) {
        rawdraw_ui_manager_->RequestActivePageRefresh();
    }
}

void Application::EnterWifiConfigMode() {
    wifi_connected_.store(false, std::memory_order_release);
    ESP_LOGI(kTag, "Entering WiFi config mode by long press");
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

void Application::ArmSyncSleepTimer(int interval_minutes_override) {
    Settings nvs(kSyncNamespace, false);
    const int interval_minutes = interval_minutes_override > 0
        ? interval_minutes_override
        : nvs.GetInt(kSyncIntervalKey, 30);
    if (interval_minutes <= 0) {
        ESP_LOGI(kTag, "Sync sleep interval: 关闭");
        return;
    }
    if (sleep_timer_ == nullptr) {
        esp_timer_create_args_t args = {};
        args.callback = [](void* arg) {
            static_cast<Application*>(arg)->EnterScheduledSleep();
        };
        args.arg = this;
        args.dispatch_method = ESP_TIMER_TASK;
        args.name = "app_sync_sleep";
        ESP_ERROR_CHECK(esp_timer_create(&args, &sleep_timer_));
    }
    esp_timer_stop(sleep_timer_);
    const int64_t delay_us = static_cast<int64_t>(interval_minutes) * 60 * 1000 * 1000;
    ESP_LOGI(kTag, "Sync sleep interval: %d minutes", interval_minutes);
    ESP_LOGI(kTag, "Scheduling sleep after sync interval: %d minutes", interval_minutes);
    ESP_ERROR_CHECK(esp_timer_start_once(sleep_timer_, delay_us));
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
    return GetLifecycleState() == kLifecycleSyncIdle;
}

void Application::EnterScheduledSleep() {
    // 只有 SYNC_IDLE 才允许休眠：配网/配对/连接中一律跳过。
    // （替代旧的 IsConfigMode 特判——状态机统一覆盖）
    if (!CanEnterSleepMode()) {
        // 必须重装：esp_timer_start_once 是一次性的，直接 return 会让定时器
        // 永久失效——之后即使配对成功进入 SyncIdle 也不会再有任何休眠定时器。
        ESP_LOGI(kTag, "Scheduled sleep skipped: lifecycle not SyncIdle, retry in 1 min");
        ArmSyncSleepTimer(1);
        return;
    }
    // The busy bookkeeping was always fed (audio sets Audio, the panel sets
    // Display on every queue/refresh) but nothing ever read it, so a deep sleep
    // could start mid-refresh: a four-colour refresh takes >= 15 s and the panel
    // latches garbage if the waveform is interrupted. Consult it.
    if (!SleepManager::GetInstance().CanSleepNow()) {
        ESP_LOGI(kTag, "Scheduled sleep deferred: busy=0x%x holds=%d deadline_in=%lldms",
                 (unsigned)SleepManager::GetInstance().GetBusyMask(),
                 SleepManager::GetInstance().GetHoldCount(),
                 (long long)(SleepManager::GetInstance().GetDeadlineMs() -
                             esp_timer_get_time() / 1000));
        ArmSyncSleepTimer(1);
        return;
    }
    if (notify_is_active()) {
        ESP_LOGI(kTag, "Scheduled sleep deferred: notification on screen");
        ArmSyncSleepTimer(1);
        return;
    }

    ESP_LOGI(kTag, "Entering deep sleep after sync interval; BOOT wakes device");
    TransitionLifecycle(kLifecycleSleep, "scheduled sleep");
    wifi_connected_.store(false, std::memory_order_release);
    esp_wifi_disconnect();
    esp_wifi_stop();
    esp_sleep_enable_ext0_wakeup(static_cast<gpio_num_t>(BOOT_BUTTON_GPIO), 0);
    esp_deep_sleep_start();
}

void Application::EnterManualSleep() {
    ESP_LOGI(kTag, "Entering manual deep sleep; stopping local services and WiFi");
    if (sleep_timer_ != nullptr) {
        esp_timer_stop(sleep_timer_);
    }
    wifi_connected_.store(false, std::memory_order_release);
    esp_wifi_disconnect();
    esp_wifi_stop();
    UpdateStatusBarForUi();
    vTaskDelay(pdMS_TO_TICKS(300));
    esp_sleep_enable_ext0_wakeup(static_cast<gpio_num_t>(BOOT_BUTTON_GPIO), 0);
    esp_deep_sleep_start();
}

void Application::Run() {
    while (true) {
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
    renderer->Update(status);
    rawdraw_ui_manager_->RequestActivePageRefresh();
}
