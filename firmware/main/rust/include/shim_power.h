/**
 * @file shim_power.h
 * @brief Duty-cycle plumbing: what is on the glass, how we woke, audio rail.
 *
 * The panel record lives in RTC memory because every deep-sleep wake is a cold
 * boot: without it the device cannot tell whether the frame it is about to
 * draw is already on the glass, and would pay a >= 15 s full refresh per wake.
 */
#ifndef SHIM_POWER_H
#define SHIM_POWER_H

#include <stdint.h>

#define RF_PANEL_MAGIC 0x50414E31u

#ifdef __cplusplus
extern "C" {
#endif

/* Layout is relied on by the host stub and the Rust reader: valid at offset
 * 4, md5 at offset 8, index at 44, sizeof == 48. Keep them in sync. */
typedef struct {
    uint32_t magic;              /* 0  */
    uint8_t  valid;              /* 4  */
    uint8_t  _pad[3];            /* 5  */
    char     displayed_md5[33];  /* 8  */
    int32_t  displayed_index;    /* 44 */
} rf_panel_record_t;             /* sizeof == 48 */

void rf_panel_record_get(rf_panel_record_t* out);
void rf_panel_mark_pending(const char* md5, int index);
void rf_panel_record_invalidate(void);
/* Re-chain the commit-on-idle hook (shim.cpp) after RawDrawUiManager::Init
 * replaced the refresh-idle slot on the promotion path. One call chains
 * exactly one trampoline; promotion is one-shot and Init wiped the previous
 * chain, so one re-chain leaves exactly one. */
void rf_panel_commit_hook_register(void);

/* 0 = 其它, 1 = timer 唤醒, 2 = ext0(BOOT), 3 = ext1(充电插入) */
int  rf_wakeup_cause(void);


/* Consecutive schedule-sync failures, persisted in RTC memory across deep
 * sleep (RAM is cleared on every wake, so a plain counter could never climb
 * the backoff ladder). The policy reads it before deciding and stores the
 * update right after: success stores 0, failure stores min(streak + 1, 8). */
uint32_t rf_fail_streak_get(void);
void rf_fail_streak_set(uint32_t streak);
void rf_rails_audio(int on);   /* 音频 + 功放 */
/* Task 1 duration ledger (shim.cpp owns the counters, Rust reads them via
 * rf_power_counters through its own FFI decl; only the count_* writers are
 * called from C++). */
void rf_power_count_wake(void);
void rf_power_add_awake_ms(uint32_t ms);
void rf_power_add_radio_ms(uint32_t ms);
void rf_power_count_http_get(void);
void rf_power_add_refresh_ms(uint32_t ms);
/* Monotonic ms clock for Rust duration bookkeeping (esp_timer ms). */
uint64_t rf_now_ms(void);

#ifdef __cplusplus
}
#endif

#endif // SHIM_POWER_H
