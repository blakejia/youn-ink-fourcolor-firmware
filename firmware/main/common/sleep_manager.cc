#include "sleep_manager.h"

#include <atomic>

#include <esp_timer.h>

#include "application.h"

namespace {
int64_t NowMs() {
    return esp_timer_get_time() / 1000;
}
}  // namespace

class SleepManagerImpl {
public:
    std::atomic<uint32_t> busy_mask{0};
    std::atomic<int64_t> deadline_ms{0};
};

static SleepManagerImpl g_sm;

SleepManager& SleepManager::GetInstance() {
    static SleepManager instance;
    return instance;
}

void SleepManager::SetBusy(SleepBusySrc src, bool busy) {
    const uint32_t bit = static_cast<uint32_t>(src);
    if (busy) {
        g_sm.busy_mask.fetch_or(bit, std::memory_order_acq_rel);
    } else {
        g_sm.busy_mask.fetch_and(~bit, std::memory_order_acq_rel);
    }
}

bool SleepManager::Busy(SleepBusySrc src) const {
    return (g_sm.busy_mask.load(std::memory_order_acquire) &
            static_cast<uint32_t>(src)) != 0;
}

void SleepManager::Kick(uint32_t delay_ms, const char* /*reason*/) {
    const int64_t new_deadline = NowMs() + static_cast<int64_t>(delay_ms);
    int64_t cur = g_sm.deadline_ms.load(std::memory_order_acquire);
    while (cur < new_deadline &&
           !g_sm.deadline_ms.compare_exchange_weak(cur, new_deadline,
                                                   std::memory_order_acq_rel,
                                                   std::memory_order_acquire)) {
    }
}

bool SleepManager::CanSleepNow() const {
    if (!Application::GetInstance().CanEnterSleepMode()) {
        return false;
    }

    if (g_sm.busy_mask.load(std::memory_order_acquire) != 0) {
        return false;
    }

    const int64_t now = NowMs();
    const int64_t deadline = g_sm.deadline_ms.load(std::memory_order_acquire);
    if (now < deadline) {
        return false;
    }

    return true;
}
