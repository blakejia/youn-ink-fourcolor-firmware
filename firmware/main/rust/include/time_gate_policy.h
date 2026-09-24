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
 *   1. **SNTP start** (`snntp_start_ok`): daily gate kept in
 *      `RTC_DATA_ATTR s_last_sntp_sync_epoch`. Returns `action`:
 *      0 = Start (no seed/first sync/regression/last full day passed),
 *      1 = Skip (still within the daily window).
 *   2. **SNTP mark-synced** (`sntp_mark_synced_ok`): record the new sync
 *      epoch. Always true; the Rust function merely validates the input
 *      clock before letting C++ persist it.
 *   3. **RTC cache write-back** (`rtc_cache_ok`): write a stamp into
 *      `RTC_DATA_ATTR` (e.g. `g_battery_due_at`). Gated by the same clock
 *      validity check — an implausible clock arms nothing.
 *   4. **Battery sample acceptance** (`battery_sample_ok`): whether the
 *      one-hour battery report gate is open this cycle. The actual
 *      hardware call still lives in the C++ shim; this fn is the gate.
 *   5. **Wi-Fi cache write-back** (`wifi_cache_ok`): persist an SSID/BSSID
 *      pair in RTC slow memory. Same clock-validity gate as (3).
 *
 * In each branch C++ owns the actual read/write; Rust only decides.
 *
 * Clock semantics (mirrored exactly from the C++ code being migrated):
 *   - `clock_valid` is the centralized notion of "time is believable":
 *     `now >= 1577836800` (2020-01-01 UTC). RTC epoch from PCF8563 falls
 *     back to that same gate.
 *   - Regression: `now < last_event_epoch`. Rust forces an immediate
 *     repair start (SNTP can move the clock backwards when it re-syncs).
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
 *
 * Period configuration for the SNTP start gate is `sntp_min_period_s`,
 * defaulting to 24 h. The battery sample gate uses `battery_min_period_s`,
 * defaulting to 3600 s. They are explicit inputs so the same Rust
 * function remains testable without environment variables.
 */
typedef struct {
    /** Current wall-clock time in seconds since epoch (C++ `time()`). */
    uint32_t now_s;

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
     *   bit 0: `now_s` is plausible (>= 2020-01-01 UTC).
     *   bit 1: PCF8563 holds a plausible epoch this boot.
     *   bit 2: SNTP succeeded at least once on this boot (`now_s` was set
     *          via `settimeofday` from the PCF8563).
     *   bit 3..31: reserved, must be zero.
     *
     * Keeping the validity facts as a bitfield lets C++ pre-compute them
     * once per tick and pass them in; Rust does not re-read any source.
     */
    uint32_t clock_valid_mask;
} rf_time_gate_policy_inputs_t;

/**
 * Bit layout for `clock_valid_mask`.
 *   bit 0 = kNowValid
 *   bit 1 = kRtcValid
 *   bit 2 = kEverSynced
 */
#define RF_TIME_GATE_CLOCK_NOW_VALID      0x1u
#define RF_TIME_GATE_CLOCK_RTC_VALID      0x2u
#define RF_TIME_GATE_CLOCK_EVER_SYNCED    0x4u

/** Action for the SNTP start gate. */
#define RF_TIME_GATE_SNTP_ACTION_SKIP     0  /* daily gate not yet open */
#define RF_TIME_GATE_SNTP_ACTION_START    1  /* start / re-sync SNTP now */
#define RF_TIME_GATE_SNTP_ACTION_REPAIR   2  /* regression -> immediate repair */

/**
 * Outputs returned from Rust. C++ reads these and executes the action;
 * Rust does not persist or perform I/O.
 *
 * `next_last_sntp_sync_s` and `next_last_battery_arm_s` are returned for
 * C++ to write back to its RTC_DATA_ATTR fields. They are `RF_TIME_GATE_NEVER`
 * (sentinel) when the gate is closed and should not be armed.
 */
typedef struct {
    /** SNTP start gate result (RF_TIME_GATE_SNTP_ACTION_*). */
    uint8_t  sntp_start_action;
    /** SNTP mark-synced gate: 1 = record, 0 = ignore (implausible clock). */
    uint8_t  sntp_mark_synced_ok;
    /** RTC cache (battery arm, etc.) write-back gate: 1 = write, 0 = skip. */
    uint8_t  rtc_cache_ok;
    /** Battery sample gate: 1 = take sample (one-hour sliding window), 0 = skip. */
    uint8_t  battery_sample_ok;
    /** Wi-Fi cache write-back gate: 1 = write, 0 = skip. */
    uint8_t  wifi_cache_ok;
    /** Padding for the u8 cluster; ABI only requires 8-byte alignment
     *  for the next u32, but explicit padding documents the layout. */
    uint8_t  _pad0[3];
    /** `now + sntp_min_period_s` — value to persist on a successful Start. */
    uint32_t next_last_sntp_sync_s;
    /** `now + battery_min_period_s` — value to persist on a successful arm. */
    uint32_t next_last_battery_arm_s;
} rf_time_gate_policy_output_t;

/**
 * Decide each gated operation from `inputs`.
 *
 * Pure function: outputs depend only on the inputs. Always safe to call
 * the same way with a null `inp`/`out` handled by C++ (the ABI requires
 * `inp` and `out` to be non-null on entry).
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
