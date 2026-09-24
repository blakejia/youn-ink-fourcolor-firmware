/**
 * @file charge_policy.h
 * @brief Normalized charge state transition policy inputs and output
 *         (decision lives in Rust, `charge_policy.rs`).
 *
 * C++ owns GPIO read and polarity normalization. This header defines the
 * ABI between the normalized facts and the Rust decision function.
 *
 * The C++ shim is responsible for:
 *   detect_charging = (gpio_get_level(detect_gpio) == CHARGE_DETECT_CHARGING_LEVEL)
 *   full_high       = (gpio_get_level(full_gpio) == 1)
 *
 * Polarity: CHARGE_DETECT_CHARGING_LEVEL is currently 0 (physical low = charging).
 * Rust receives the normalized boolean; it does not re-interpret the physical level.
 */
#ifndef CHARGE_POLICY_H
#define CHARGE_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Inputs to the Rust charge policy decision.
 *
 * Timestamps use signed 64-bit integers; -1 is the sentinel meaning "never".
 *
 * C++ is responsible for maintaining and passing these timestamps across calls.
 * Rust does not hold any cross-call state.
 */
typedef struct {
    /** Normalized charging-detect signal (true when charging is detected). */
    int8_t   detect_charging;
    int8_t   _pad0[7];
    /** Normalized full-high signal (true when battery-full is indicated). */
    int8_t   full_high;
    int8_t   _pad1[7];
    /** Current wall-clock time in milliseconds (provided by C++). */
    int64_t  now_ms;
    /** When `detect_charging` was first continuously observed (-1 = never). */
    int64_t  detect_start_ms;
    /** When `full_high` was first continuously observed (-1 = never). */
    int64_t  full_start_ms;
    /** Most recent time `detect_charging` was observed (-1 = never). */
    int64_t  last_detect_ms;
    /** Most recent time `full_high` was observed (-1 = never). */
    int64_t  last_full_ms;
    /** Most recent time any power signal was observed (-1 = never). */
    int64_t  last_power_ms;
} rf_charge_policy_inputs_t;

/**
 * Output from the Rust charge policy decision.
 *
 * These timestamps are the values C++ should write back to its state fields
 * before the next Tick call.
 */
typedef struct {
    /** Decided state (0=NoPower, 1=Charging, 2=Full, 3=NoBattery). */
    int32_t  state;
    int32_t  _pad0;
    /** Updated `detect_start_ms` for C++ to persist. */
    int64_t  next_detect_start_ms;
    /** Updated `full_start_ms` for C++ to persist. */
    int64_t  next_full_start_ms;
    /** Updated `last_detect_ms` for C++ to persist. */
    int64_t  next_last_detect_ms;
    /** Updated `last_full_ms` for C++ to persist. */
    int64_t  next_last_full_ms;
    /** Updated `last_power_ms` for C++ to persist. */
    int64_t  next_last_power_ms;
} rf_charge_policy_output_t;

/**
 * Decide the normalized charge state.
 *
 * @param in   Pointer to inputs (must not be null).
 * @param out  Pointer to output struct (must not be null).
 *
 * Thread-safety: the Rust side is pure and stateless; the caller guarantees
 * that concurrent calls do not race on the same output struct.
 */
void rf_charge_policy_decide(const rf_charge_policy_inputs_t* in,
                             rf_charge_policy_output_t* out);

#ifdef __cplusplus
}
#endif

#endif  // CHARGE_POLICY_H
