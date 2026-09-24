/**
 * @file battery_activity_policy.h
 * @brief Battery relative-activity policy inputs and outputs (decisions live
 *         in Rust, `battery_activity_policy.rs`).
 *
 * C++ owns the ADC burst, the charge GPIO snapshot, the RTC stamps, the
 * battery_history row (server side) and the schedule GET that carries
 * `?v=&p=&c=`. This header is the ABI by which the two halves meet:
 *
 *   1. `rf_battery_activity_context`  (shim.cpp, C++) fills the CHEAP facts:
 *      charge encoding, effective clock (`time()` falling back to the
 *      PCF8563), the RTC_DATA_ATTR report stamp and the last reported
 *      sample (the activity baseline).
 *   2. `rf_battery_activity_gate_open` (Rust) opens the one-hour window
 *      WITHOUT an ADC read: first sample, unset clock, expired window or a
 *      charging<->discharging transition between the last reported sample
 *      and now. Closed => C++ never performs the 10-sample burst.
 *   3. `rf_battery_activity_read` (shim.cpp, C++) performs the ADC burst and
 *      fills `voltage_mv` / `has_sample` plus the C++-owned `p=` percent
 *      (the existing display mapping — deliberately NOT a Rust output).
 *   4. `rf_battery_activity_decide` (Rust) returns the accept / outlier /
 *      direction / relative-activity / next-gate decision.
 *   5. `rf_battery_activity_commit` (shim.cpp, C++) persists the bytes:
 *      advances the RTC report stamp and records the new baseline. The
 *      stamp write is guarded by `next >= RF_TIME_GATE_NEVER` so an unset
 *      clock can never poison the window (same rule the old
 *      `rf_battery_arm` had).
 *
 * Naming contract (design §5): the only level-shaped output is
 * `relative_activity` (0..2). There is no current sensor, so no field here
 * is or may be named `soc_percent`, `mAh`, or any absolute capacity — the
 * voltage->percent map feeding `p=` stays in C++ (display/protocol only).
 */
#ifndef BATTERY_ACTIVITY_POLICY_H
#define BATTERY_ACTIVITY_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Direction encodings — identical to the server's `c=` range (0..4). */
#define RF_BATTERY_DIRECTION_UNKNOWN      0
#define RF_BATTERY_DIRECTION_NO_POWER     1
#define RF_BATTERY_DIRECTION_CHARGING     2
#define RF_BATTERY_DIRECTION_FULL         3
#define RF_BATTERY_DIRECTION_DISCHARGING  4

/** Relative activity levels (NOT a percentage, NOT a capacity). */
#define RF_BATTERY_RELATIVE_ACTIVITY_RESTING 0
#define RF_BATTERY_RELATIVE_ACTIVITY_LOW     1
#define RF_BATTERY_RELATIVE_ACTIVITY_HIGH    2

/** Server ingest window for `v` (mirrors `server/youn_server/app.py`). */
#define RF_BATTERY_VOLTAGE_MIN_MV  2500
#define RF_BATTERY_VOLTAGE_MAX_MV  5000

/**
 * Facts for one sampling decision.
 *
 * Layout contract with `battery_activity_policy.rs` (`#[repr(C)]` + a Rust
 * test asserting offsets/size): `uint16_t voltage_mv` at 0, `uint8_t charge`
 * at 2, `uint8_t has_sample` at 3, 4 pad bytes, `int64_t now_s` at 8,
 * `int64_t last_sample_s` at 16, `uint16_t prev_mv` at 24, `uint8_t
 * prev_charge` at 26, `uint8_t prev_valid` at 27, `uint32_t min_interval_s`
 * at 28, 32 bytes total. Keep field order, types and count in step with the
 * Rust struct.
 */
typedef struct {
    /** Averaged battery voltage in mV (0 until `rf_battery_activity_read`). */
    uint16_t voltage_mv;
    /** Charge fact, RF_BATTERY_DIRECTION_*. */
    uint8_t  charge;
    /** 1 = C++ produced a reading; 0 = sensor absent / ADC failure. */
    uint8_t  has_sample;
    /** Padding to align `now_s` on an 8-byte boundary. */
    uint8_t  _pad[4];
    /** Effective clock, seconds: time() when plausible, else PCF8563 epoch,
     *  else negative (unset — report but never arm). */
    int64_t  now_s;
    /** Last accepted+reported sample epoch; negative = none yet. */
    int64_t  last_sample_s;
    /** Voltage of the last accepted+reported sample (activity baseline). */
    uint16_t prev_mv;
    /** Direction of that last accepted+reported sample. */
    uint8_t  prev_charge;
    /** 1 = prev_mv/prev_charge hold a real prior sample. */
    uint8_t  prev_valid;
    /** Minimum seconds between same-direction reports (C++ config, 3600). */
    uint32_t min_interval_s;
} rf_battery_activity_inputs_t;

/**
 * Decision for one sampling attempt.
 *
 * Layout contract: `uint8_t accept` at 0, `direction` at 1,
 * `relative_activity` at 2, `filtered` at 3, 4 pad bytes, `int64_t
 * next_sample_s` at 8, 16 bytes total. Rust test asserts the offsets.
 *
 * `relative_activity` is the ONLY level-shaped field: sample-to-sample
 * movement in three grades. Nothing here estimates charge state.
 */
typedef struct {
    /** 1 = report this sample (write v/p/c, then commit the new stamp). */
    uint8_t  accept;
    /** Normalized RF_BATTERY_DIRECTION_* value to report as `c=`. */
    uint8_t  direction;
    /** RF_BATTERY_RELATIVE_ACTIVITY_* movement vs. prev_mv. */
    uint8_t  relative_activity;
    /** 1 = dropped by the outlier filter (retry next wake, do NOT wait out
     *  the window); 0 = "not due yet" when accept is 0. */
    uint8_t  filtered;
    /** Padding to align `next_sample_s` on an 8-byte boundary. */
    uint8_t  _pad[4];
    /** New stamp: now_s + min_interval_s when accepted with a plausible
     *  clock; unchanged prior stamp otherwise. */
    int64_t  next_sample_s;
} rf_battery_activity_output_t;

/**
 * Fill the cheap facts (no ADC). Returns 1 on success, 0 if the board
 * cannot supply facts.
 */
int rf_battery_activity_context(rf_battery_activity_inputs_t* inp);

/**
 * Cheap pre-ADC window check (Rust, pure). Voltage fields are ignored —
 * only the clock, stamp and direction facts matter. 1 = read the ADC.
 */
uint8_t rf_battery_activity_gate_open(const rf_battery_activity_inputs_t* inp);

/**
 * The 10-sample ADC burst + voltage->mV/% (C++ mechanism). Fills
 * `voltage_mv`/`has_sample` and the existing display percent in
 * `*percent_out` (the `p=` wire field; a C++ value, not a policy output).
 * Returns 1 when a reading exists, 0 otherwise.
 */
int rf_battery_activity_read(rf_battery_activity_inputs_t* inp,
                             uint8_t* percent_out);

/** The decision (Rust, pure). */
rf_battery_activity_output_t rf_battery_activity_decide(
    const rf_battery_activity_inputs_t* inp);

/**
 * Persist the outcome (C++ mechanism): advance the RTC report stamp to
 * `next_sample_s` (only when >= RF_TIME_GATE_NEVER, guarding the unset
 * clock) and record `voltage_mv`/`direction` as the next baseline.
 * Returns 1 when the stamp advanced, 0 when it was refused/unchanged.
 */
int rf_battery_activity_commit(int64_t next_sample_s, uint16_t voltage_mv,
                               uint8_t direction);

#ifdef __cplusplus
}
#endif

#endif  /* BATTERY_ACTIVITY_POLICY_H */
