#ifndef _APPLICATION_H_
#define _APPLICATION_H_

#include <atomic>
#include <cstdint>
#include <string>
#include <functional>
#include <memory>
#include <string_view>

#include "audio_service.h"
#include "device_state.h"

namespace ui {
class RawDrawUiManager;
}
class CustomLcdDisplay;

class Application {
public:
    static Application& GetInstance() {
        static Application instance;
        return instance;
    }

    Application(const Application&) = delete;
    Application& operator=(const Application&) = delete;

    // Quiet boots (timer-wake duty cycle) skip the UI manager, the audio
    // service and the panel bring-up; anything that touches those must
    // null-check. Honoured only when provisioned AND paired (see .cc).
    void Initialize(bool quiet);
    // True on a timer-wake duty-cycle boot. Set before Board::GetInstance()
    // so the board ctor can skip the panel bring-up.
    bool IsQuietBoot() const { return quiet_boot_.load(std::memory_order_acquire); }
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
    AudioService& GetAudioService() { return audio_service_; }
    ui::RawDrawUiManager* GetRawDrawUiManager() { return rawdraw_ui_manager_.get(); }
    void UpdateStatusBarForUi();
    void UpdateWifiStatusForProvisioning(const std::string& state, int reason);
    bool CanEnterSleepMode() const;
     void OnUpClick();
    void OnUpDoubleClick();
    void OnDownClick();
    void OnUpLongPress();
    void OnDownLongPress();
    void OnWifiConfigComboLongPress();
    void OnBootClick();
    void OnBootLongPress();

    // One-shot duty-cycle step both boot paths share (Task 6 quiet path calls
    // this once per wake): page_sync_sync_once() -> notify_request_next() ->
    // page_sync_paint_if_changed() (unconditional) -> ServicePowerPolicy().
    void RunPowerCycle();
    // Timer-wake (quiet) boots are not user activity: backdate the idle clock
    // so the policy computes the duty-cycle wake instead of holding the
    // device inside the interactive grace window. (A fresh boot otherwise
    // looks "recently active" because the idle clock starts at zero — which
    // is exactly what gives BOOT wakes their grace.)
    void NoteQuietWake();
    // Evaluate the Rust power policy now: sleep until the server's next page
    // change (bounded by the poll cap and sleep window), or rearm the timer
    // for the stay-awake interval. Never sleeps on mains.
    void ServicePowerPolicy();

private:
    Application();
    ~Application();
    std::atomic<DeviceState> state_{kDeviceStateUnknown};
    std::atomic<LifecycleState> lifecycle_{kLifecycleUnknown};

    std::atomic<bool> wifi_connected_{false};
    AudioService audio_service_;
    std::unique_ptr<ui::RawDrawUiManager> rawdraw_ui_manager_;
    esp_timer_handle_t sleep_timer_ = nullptr;
    // Timer-wake duty cycle (provisioned AND paired only); the board ctor
    // reads this via IsQuietBoot() to skip the panel bring-up. Atomic: set
    // once in Initialize, read from the WiFi/esp_timer tasks afterwards.
    std::atomic<bool> quiet_boot_{false};
    // esp_timer ms of the last button activity; the policy's idle clock reads
    // this. Zero at boot (fresh boots land inside the grace window, which is
    // what keeps BOOT wakes awake); negative = quiet wake, grace suppressed.
    // A member, not a timer-local static, so it survives across callbacks.
    int64_t last_activity_ms_ = 0;
    // esp_timer ms of the last RunPowerCycle start; -1 = none yet this boot.
    // The power-timer dispatcher runs a cycle when this is unset or the
    // server's poll interval has elapsed since, and a bare policy check
    // otherwise. Written and read in the esp_timer task only.
    int64_t last_cycle_ms_ = -1;
    // True while RunPowerCycle (including its terminal policy call) runs in
    // the esp_timer task. The WiFi connected path consults it before arming
    // the timer: an arm from that task mid-cycle would fire mid-cycle, which
    // the cycle's entry-stop cannot cover.
    std::atomic<bool> cycle_in_progress_{false};
    // Quiet→interactive promotion latch. Requested from the policy (mains),
    // button activity, or config-AP entry — all outside the main-loop task,
    // which alone owns UI construction (Run() consumes the request).
    // promoted_ makes it one-shot per boot; both atomics, no lock needed.
    std::atomic<bool> promote_requested_{false};
    std::atomic<bool> promoted_{false};
    // (The sync-failure backoff streak is NOT here: it lives in RTC memory
    // via rf_fail_streak_*, because RAM is cleared on every deep-sleep wake.)
    // Set by RunPowerCycle immediately before page_sync_sync_once(); consumed
    // (cleared) by ServicePowerPolicy. The streak advances at most once per
    // real sync attempt — timer re-arms (mains/grace/busy) must not ratchet
    // it. Per-boot RAM by design; only the counter outlives sleep.
    bool sync_attempted_ = false;
    // Snapshot of the last page_sync_sync_once() outcome, written only by
    // RunPowerCycle alongside sync_attempted_. The policy reads this — not a
    // fresh page_sync_sync_ok() — so a timer-side evaluation cannot observe
    // a half-finished cycle's state.
    bool sync_result_ok_ = false;

    void RearmPowerTimer(uint32_t delay_ms);
    // esp_timer callback: run the one-shot cycle when one is due, else just
    // re-evaluate the policy (see .cc for the two reasons).
    void OnPowerTimer();
    // Build the UI manager and register the settings page (interactive only).
    void BuildRawDrawUi(CustomLcdDisplay* lcd);
    // Ask the main loop to promote a quiet boot (build UI + bring up panel).
    // Quiet-only and idempotent; safe from any task.
    void RequestPromotion();
    // Main-loop side: consume a pending promotion. Owns UI construction.
    void ServicePromotion();
    void EnterManualSleep();
    void NoteButtonActivity();
    void EnterWifiConfigMode();
    void StartServerPairingOnce();
    static void ServerPairingTaskEntry(void* arg);
};

#endif  // _APPLICATION_H_
