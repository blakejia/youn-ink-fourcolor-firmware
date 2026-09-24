#pragma once

#include <cstdint>

// Busy sources. Only the two that actually vote: the panel reports Display
// while a refresh is pending or in flight (CustomLcdDisplay::UpdateDisplayBusyLocked),
// the audio service reports Audio while a stream is open. Both reach the sleep
// decision through CanSleepNow() -> `busy` in rf_power_decide.
enum class SleepBusySrc : uint32_t {
    Audio   = 1u << 0,
    Display = 1u << 1,
};

// The "may I sleep" mechanism only — the policy lives in power.rs
// (rf_power_decide), which is where the decision is unit-tested.
class SleepManager {
public:
    static SleepManager& GetInstance();

    // Busy votes
    void SetBusy(SleepBusySrc src, bool busy);

    // Sleep delay deadline: extend to max(now + delay_ms).
    void Kick(uint32_t delay_ms, const char* reason = nullptr);

    // Gate: busy == 0 && now >= deadline (plus Application::CanEnterSleepMode).
    bool CanSleepNow() const;

    // Read one busy source's raw vote, without folding in the lifecycle
    // gate or the deadline (CanSleepNow folds those). For callers that need
    // "is the panel refreshing right now", e.g. the notify pull gate.
    bool Busy(SleepBusySrc src) const;

private:
    SleepManager() = default;
};

// C-style wrappers
inline void sm_set_busy(SleepBusySrc src, bool busy) {
    SleepManager::GetInstance().SetBusy(src, busy);
}

inline void sm_kick(uint32_t delay_ms, const char* reason = nullptr) {
    SleepManager::GetInstance().Kick(delay_ms, reason);
}
