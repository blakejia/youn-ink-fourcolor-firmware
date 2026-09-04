#ifndef _APPLICATION_H_
#define _APPLICATION_H_

#include <atomic>
#include <string>
#include <functional>
#include <memory>
#include <string_view>

#include "audio_service.h"
#include "device_state.h"

namespace ui {
class RawDrawUiManager;
}

class Application {
public:
    static Application& GetInstance() {
        static Application instance;
        return instance;
    }

    Application(const Application&) = delete;
    Application& operator=(const Application&) = delete;

    void Initialize();
    void Run();

    DeviceState GetDeviceState() const { return state_.load(std::memory_order_acquire); }
    bool SetDeviceState(DeviceState state);
    LifecycleState GetLifecycleState() const { return lifecycle_.load(std::memory_order_acquire); }
    void TransitionLifecycle(LifecycleState next, const char* reason);

    void Schedule(std::function<void()>&& callback);
    void PlaySound(const std::string_view& sound);
    void PlaySound(const std::string_view& sound, int duration_ms);
    void MuteSound();
    void StopSound();
    bool CanEnterSleepMode() const;

    AudioService& GetAudioService() { return audio_service_; }
    ui::RawDrawUiManager* GetRawDrawUiManager() { return rawdraw_ui_manager_.get(); }
    void UpdateStatusBarForUi();
    void UpdateWifiStatusForProvisioning(const std::string& state, int reason);

     void OnUpClick();
    void OnDownClick();
    void OnUpLongPress();
    void OnDownLongPress();
    void OnWifiConfigComboLongPress();
    void OnBootClick();
    void OnBootLongPress();

private:
    Application();
    ~Application();
    std::atomic<DeviceState> state_{kDeviceStateUnknown};
    std::atomic<LifecycleState> lifecycle_{kLifecycleUnknown};

    std::atomic<bool> wifi_connected_{false};
    AudioService audio_service_;
    std::unique_ptr<ui::RawDrawUiManager> rawdraw_ui_manager_;
    esp_timer_handle_t sleep_timer_ = nullptr;

    void ArmSyncSleepTimer();
    void EnterScheduledSleep();
    void EnterManualSleep();
    void NoteButtonActivity();
    void EnterWifiConfigMode();
    void StartServerPairingOnce();
    static void ServerPairingTaskEntry(void* arg);
};

#endif  // _APPLICATION_H_
