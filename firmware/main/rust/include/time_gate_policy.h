/**
 * @file time_gate_policy.h
 * @brief RTC/SNTP/calendar/RTC cache/battery sample/Wi-Fi cache time-gate
 *         policy inputs and output (decision lives in Rust,
 *         `time_gate_policy.rs`).
 *
 * C++ owns RTC/SNTP/NVS access: it reads `time()`, the PCF8563, and any
 * RTC_DATA_ATTR stamps. This header defines the ABI by which C++ asks
 * Rust whether each gated operation is allowed and which degraded action
 * to take when the clock is implausible or has gone backwards.
 *
 * The five gated operations, all decided from the same snapshot of facts:
 *
 *   1. **SNTP start** (`sntp_start_action`): daily gate kept in
 *      `RTC_DATA_ATTR s_last_sntp_sync_epoch`. Returns `action`:
 *      0 = Skip (within the daily window),
 *      1 = Start (no seed / first sync / daily window elapsed),
 *      2 = Repair (RTC moved backwards).
 *   2. **SNTP mark-synced** (`sntp_mark_synced_ok`): record the new
 *      sync epoch. Always refused when `time()` itself is implausible —
 *      even if the PCF8563 fallback epoch is plausible. A fake sync
 *      record would skew the daily gate.
 *   3. **RTC cache write-back** (`rtc_cache_ok`): write a stamp into
 *      `RTC_DATA_ATTR` (e.g. `g_battery_due_at`).
 *   4. **Battery sample acceptance** (`battery_sample_ok`): the
 *      one-hour battery report gate. Falls back to the PCF8563 epoch
 *      when `time()` is implausible, mirroring the legacy BatteryClock
 *      helper, so the first battery report of a cold boot still lands.
 *   5. **Wi-Fi cache write-back** (`wifi_cache_ok`): persist an
 *      SSID/BSSID pair in RTC slow memory.
 *
 * In each branch C++ owns the actual read/write; Rust only decides.
 *
 * Clock semantics (mirrored exactly from the C++ code being migrated):
 *
 *   - `now_s` is **signed** so `time()==-1` propagates as a negative
 *     value. Narrowing to `uint32_t` before the policy call would
 *     turn `-1` into `0xFFFFFFFF` (year 2106) and silently persist
 *     it as a sync stamp. The Rust side rejects negative `now_s`.
 *   - `effective_clock_s` is the PCF8563 fallback (unsigned, only used
 *     by the battery gate; `sntp_mark_synced_ok` ignores it).
 *   - `clock_valid_mask` is the centralised "is time believable?" bitfield.
 */
#ifndef TIME_GATE_POLICY_H
#define TIME_GATE_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Sentinel epoch meaning "no prior record" (well before 2020). */
#define RF_TIME_GATE_NEVER  1577836800u  /* 2020-01-01 UTC */

/**
 * Inputs to the Rust time-gate decision.
 *
 * C++ reads `time()`, the PCF8563 (`ZectrixRtcNowEpoch`), and any
 * RTC_DATA_ATTR stamps it owns, then fills this struct. Rust is pure:
 * no cross-call state, no reads of globals.
 */
typedef struct {
    /** Current `time(nullptr)` result, SIGNED. Negative = clock not
     *  initialised. Used by the SNTP gate and the mark-synced check. */
    int64_t  now_s;
    /** PCF8563 epoch fallback (UNSIGNED). `0` = no fallback. Used by
     *  the battery sample gate; ignored by mark-synced. */
    uint32_t effective_clock_s;
    /** Last successful SNTP sync epoch (RTC_DATA_ATTR stamp). */
    uint32_t last_sntp_sync_s;
    /** Last battery-report arm epoch (RTC_DATA_ATTR stamp). */
    uint32_t last_battery_arm_s;
    /** SNTP gate minimum period in seconds (24 h typical). */
    uint32_t sntp_min_period_s;
    /** Battery-sample gate minimum period in seconds (3600 typical). */
    uint32_t battery_min_period_s;
    /**
     * Bit-packed clock validity facts computed by C++:
     *   bit 0: `time()` returned plausible (`>= 2020-01-01 UTC`).
     *   bit 1: PCF8563 holds a plausible epoch this boot.
     *   bit 2: SNTP succeeded at least once on this boot.
     *   bit 3..31: reserved, must be zero.
     */
    uint32_t clock_valid_mask;
} rf_time_gate_policy_inputs_t;

/**
 * Bit layout for `clock_valid_mask`.
 *   bit 0 = kNowValid   (time() plausible)
 *   bit 1 = kRtcValid   (PCF8563 fallback plausible)
 *   bit 2 = kEverSynced (SNTP has run on this boot)
 */
#define RF_TIME_GATE_CLOCK_NOW_VALID      0x1u
#define RF_TIME_GATE_CLOCK_RTC_VALID      0x2u
#define RF_TIME_GATE_CLOCK_EVER_SYNCED    0x4u

/** Action code: SNTP gate is closed (within the daily window). */
#define RF_TIME_GATE_SNTP_ACTION_SKIP     0
/** Action code: SNTP gate is open (start / first sync / elapsed). */
#define RF_TIME_GATE_SNTP_ACTION_START    1
/** Action code: RTC moved backwards, schedule an immediate repair. */
#define RF_TIME_GATE_SNTP_ACTION_REPAIR   2

/**
 * Outputs returned from Rust. C++ reads these and executes the action;
 * Rust does not persist or perform I/O.
 *
 * `next_last_sntp_sync_s` is signed (mirrors `now_s`). Battery arm is
 * unsigned (uses `effective_clock_s`).
 */
typedef struct {
    /** SNTP start gate result (RF_TIME_GATE_SNTP_ACTION_*). */
    uint8_t  sntp_start_action;
    /** 1 = record the new sync epoch, 0 = refuse. */
    uint8_t  sntp_mark_synced_ok;
    /** 1 = write a RTC slow-memory stamp, 0 = skip. */
    uint8_t  rtc_cache_ok;
    /** 1 = one-hour battery sample window open, 0 = skip. */
    uint8_t  battery_sample_ok;
    /** 1 = Wi-Fi cache write allowed, 0 = skip. */
    uint8_t  wifi_cache_ok;
    /** Padding to align `next_last_sntp_sync_s` on an 8-byte boundary. */
    uint8_t  _pad0[3];
    /** `now + sntp_min_period_s` (signed) — value to persist on a successful Start. */
    int64_t  next_last_sntp_sync_s;
    /** `effective_clock_s + battery_min_period_s` — value to persist on a successful arm.
     *  Returns `RF_TIME_GATE_NEVER` to signal "do not arm". */
    uint32_t next_last_battery_arm_s;
} rf_time_gate_policy_output_t;

/**
 * Decide each gated operation from `inputs`.
 *
 * Pure function: outputs depend only on the inputs.
 *
 * Thread-safety: no shared state; concurrent callers each operate on
 * their own copies of `inputs` and `outputs`.
 */
void rf_time_gate_policy_decide(const rf_time_gate_policy_inputs_t* inp,
                                rf_time_gate_policy_output_t* out);

#ifdef __cplusplus
}
#endif

#endif  /* TIME_GATE_POLICY_H */
