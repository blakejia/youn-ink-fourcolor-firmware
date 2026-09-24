/**
 * @file led_policy.h
 * @brief LED action policy inputs and output (decision lives in Rust, `led_policy.rs`).
 *
 * C++ owns GPIO, FreeRTOS task lifecycle, and pulse counter management.
 * This header defines the ABI between the normalized facts and the Rust decision
 * function.
 *
 * The C++ shim is responsible for:
 *   override_enabled = led_override_enabled_.load()
 *   override_blink  = led_override_blink_.load()
 *   phase           = led_override_phase_.load()
 *   snap.charging   = charge_status->Get().charging
 *   snap.full       = charge_status->Get().full
 *   pulses          = led_activity_pulses_.load()
 */

#ifndef LED_POLICY_H
#define LED_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Inputs to the Rust LED policy decision.
 *
 * Booleans are represented as int8_t (0 = false, 1 = true) for ABI safety.
 * The _pad fields ensure consistent struct layout across ABIs.
 */
typedef struct {
    /** LED override is active. */
    int8_t   override_enabled;
    int8_t   _pad0[7];
    /** Override blink mode (vs static). */
    int8_t   override_blink;
    int8_t   _pad1[7];
    /** Current blink phase (true/false); only meaningful when blink is active. */
    int8_t   phase;
    int8_t   _pad2[7];
    /** Normalized charge.charging signal. */
    int8_t   charging;
    int8_t   _pad3[7];
    /** Normalized charge.full signal. */
    int8_t   full;
    int8_t   _pad4[7];
    /** Number of pending activity pulses. */
    uint32_t pulses;
} rf_led_policy_inputs_t;

/**
 * Output from the Rust LED policy decision.
 *
 * C++ dispatches using the structural flags (`has_second`, `consume_pulse`,
 * `first_wait_notify`, `second_wait_notify`) and `level`/`second_level`:
 *   consume_pulse=true → pulse branch (both waits are vTaskDelay, then decrement)
 *   has_second=true   → second segment present
 *   first_wait_notify/second_wait_notify: true = ulTaskNotifyTake, false = vTaskDelay
 */
typedef struct {
    /** First GPIO level (true=1=off, false=0=on for GPIO3). */
    int8_t   level;
    int8_t   _pad0[7];
    /** Duration of the first wait segment in milliseconds. */
    uint32_t first_ms;
    /** Whether a second segment follows (only for pulse and charging). */
    int8_t   has_second;
    int8_t   _pad1[7];
    /** GPIO level for the second segment. */
    int8_t   second_level;
    int8_t   _pad2[7];
    /** Duration of the second wait segment in milliseconds. */
    uint32_t second_ms;
    /** Whether to consume one activity pulse after the first segment. */
    int8_t   consume_pulse;
    int8_t   _pad3[7];
    /** True = ulTaskNotifyTake for first segment; false = vTaskDelay. */
    int8_t   first_wait_notify;
    int8_t   _pad4[7];
    /** True = ulTaskNotifyTake for second segment; false = vTaskDelay. */
    int8_t   second_wait_notify;
    int8_t   _pad5[7];
} rf_led_policy_output_t;

/**
 * Decide the LED action tuple.
 *
 * @param in   Pointer to inputs (must not be null).
 * @param out  Pointer to output struct (must not be null).
 *
 * Thread-safety: the Rust side is pure and stateless; the caller guarantees
 * that concurrent calls do not race on the same output struct.
 */
void rf_led_policy_decide(const rf_led_policy_inputs_t* in,
                          rf_led_policy_output_t* out);

#ifdef __cplusplus
}
#endif

#endif  // LED_POLICY_H
