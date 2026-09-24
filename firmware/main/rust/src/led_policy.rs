//! LED action policy. Pure: C++ gathers normalized charge snapshot, override
//! flags, phase, and activity pulse count; this decides the full action tuple.
//!
//! Policy branches (matching `board_power_bsp.cc:led_decide` exactly):
//!   ovr && ovr_blink              → 500ms delay, level = phase (toggled by C++)
//!   ovr && !ovr_blink             → 1000ms notify, level = true
//!   !ovr && !charging && !full && pulses > 0 → 120ms delay then 180ms delay,
//!                                              level = false, second_level = true,
//!                                              consume_pulse = true
//!   !ovr && full                  → 1000ms notify, level = false
//!   !ovr && charging              → 200ms delay then 2800ms notify,
//!                                    level = false, second_level = true
//!   else                          → 1000ms notify, level = true
//!
//! Wait modes (explicit booleans, not inferred from duration):
//!   first_wait_notify  = true  → ulTaskNotifyTake (event-driven)
//!   first_wait_notify  = false → vTaskDelay (regular delay)
//!   second_wait_notify = same pattern for the second segment

// ─── Public types ──────────────────────────────────────────────────────────────

/// Decision output action tuple.
///
/// All fields are explicit; C++ dispatches using the structural flags
/// (`has_second`, `consume_pulse`) and `first_wait_notify`/`second_wait_notify`,
/// never by inspecting the duration values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Action {
    /// First GPIO level (true = 1 = off, false = 0 = on for GPIO3).
    pub level: bool,
    /// Duration of the first wait segment in milliseconds.
    pub first_ms: u32,
    /// Whether a second segment follows (only for pulse and charging).
    pub has_second: bool,
    /// GPIO level for the second segment.
    pub second_level: bool,
    /// Duration of the second wait segment in milliseconds.
    pub second_ms: u32,
    /// Whether to consume one activity pulse after the first segment.
    pub consume_pulse: bool,
    /// True = use ulTaskNotifyTake (event-driven); false = use vTaskDelay.
    pub first_wait_notify: bool,
    /// True = use ulTaskNotifyTake for second segment; false = vTaskDelay.
    pub second_wait_notify: bool,
}

/// Normalized charge snapshot consumed by the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ChargeSnapshot {
    pub charging: bool,
    pub full: bool,
}

/// Rust inputs to the LED policy decision.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// LED override is active.
    pub override_enabled: bool,
    /// Override blink mode (vs static).
    pub override_blink: bool,
    /// Current blink phase (true/false); only meaningful when blink is active.
    pub phase: bool,
    /// Normalized charge snapshot.
    pub charge: ChargeSnapshot,
    /// Number of pending activity pulses.
    pub pulses: u32,
}

/// C ABI inputs — must match `rf_led_policy_inputs_t` in `led_policy.h` byte-for-byte.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CInputs {
    pub override_enabled: i8,
    pub _pad0: [u8; 7],
    pub override_blink: i8,
    pub _pad1: [u8; 7],
    pub phase: i8,
    pub _pad2: [u8; 7],
    pub charging: i8,
    pub _pad3: [u8; 7],
    pub full: i8,
    pub _pad4: [u8; 7],
    pub pulses: u32,
}

/// C ABI output — must match `rf_led_policy_output_t` in `led_policy.h`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct COutput {
    pub level: i8,
    pub _pad0: [u8; 7],
    pub first_ms: u32,
    pub has_second: i8,
    pub _pad1: [u8; 7],
    pub second_level: i8,
    pub _pad2: [u8; 7],
    pub second_ms: u32,
    pub consume_pulse: i8,
    pub _pad3: [u8; 7],
    pub first_wait_notify: i8,
    pub _pad4: [u8; 7],
    pub second_wait_notify: i8,
    pub _pad5: [u8; 7],
}

// ─── Constants ────────────────────────────────────────────────────────────────

/// Static (non-blink, non-pulse) poll interval in milliseconds.
/// Used by: override static branch, full branch, idle (no conditions) branch.
const STATIC_POLL_MS: u32 = 1000;

const BLINK_FIRST_MS: u32 = 500;
const PULSE_FIRST_MS: u32 = 120;
const PULSE_SECOND_MS: u32 = 180;
const CHARGE_FIRST_MS: u32 = 200;
const CHARGE_SECOND_MS: u32 = 2800;

// ─── Core decision function ───────────────────────────────────────────────────

/// Decide the LED action tuple from the given inputs.
///
/// This is pure: no hidden cross-task state.
pub fn decide(i: Inputs) -> Action {
    if i.override_enabled && i.override_blink {
        // Branch 1: override + blink → 500ms delay, level = phase (C++ will
        // write !phase for next call), no second segment.
        // Wait mode: vTaskDelay (regular delay, not event-driven).
        Action {
            level: i.phase,
            first_ms: BLINK_FIRST_MS,
            has_second: false,
            second_level: true, // ignored when has_second=false
            second_ms: 0,
            consume_pulse: false,
            first_wait_notify: false,
            second_wait_notify: false,
        }
    } else if i.override_enabled {
        // Branch 2: override static → level=true (GPIO1=off), 1000ms notify.
        // Wait mode: event-driven (ulTaskNotifyTake).
        Action {
            level: true,
            first_ms: STATIC_POLL_MS,
            has_second: false,
            second_level: true,
            second_ms: 0,
            consume_pulse: false,
            first_wait_notify: true,
            second_wait_notify: false,
        }
    } else if !i.charge.charging && !i.charge.full && i.pulses > 0 {
        // Branch 3: activity pulse → 120ms delay then 180ms delay,
        // level=false (GPIO0=on), second_level=true (GPIO1=off),
        // consume_pulse=true.
        // Both wait modes: vTaskDelay (regular delay).
        Action {
            level: false,
            first_ms: PULSE_FIRST_MS,
            has_second: true,
            second_level: true,
            second_ms: PULSE_SECOND_MS,
            consume_pulse: true,
            first_wait_notify: false,
            second_wait_notify: false,
        }
    } else if i.charge.full {
        // Branch 4: full → level=false (GPIO0=on = full-on), 1000ms notify.
        // Wait mode: event-driven.
        Action {
            level: false,
            first_ms: STATIC_POLL_MS,
            has_second: false,
            second_level: true,
            second_ms: 0,
            consume_pulse: false,
            first_wait_notify: true,
            second_wait_notify: false,
        }
    } else if i.charge.charging {
        // Branch 5: charging → 200ms delay then 2800ms notify,
        // level=false (GPIO0=on), second_level=true (GPIO1=off).
        // First: vTaskDelay; Second: event-driven (ulTaskNotifyTake).
        Action {
            level: false,
            first_ms: CHARGE_FIRST_MS,
            has_second: true,
            second_level: true,
            second_ms: CHARGE_SECOND_MS,
            consume_pulse: false,
            first_wait_notify: false,
            second_wait_notify: true,
        }
    } else {
        // Branch 6: idle (no conditions met) → level=true (GPIO1=off),
        // 1000ms notify.
        // Wait mode: event-driven.
        Action {
            level: true,
            first_ms: STATIC_POLL_MS,
            has_second: false,
            second_level: true,
            second_ms: 0,
            consume_pulse: false,
            first_wait_notify: true,
            second_wait_notify: false,
        }
    }
}

// ─── C ABI ───────────────────────────────────────────────────────────────────

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_led_policy_decide(
    inp: *const CInputs,
    out: *mut COutput,
) {
    let inp = unsafe { &*inp };
    let out = unsafe { &mut *out };

    let i = Inputs {
        override_enabled: inp.override_enabled != 0,
        override_blink: inp.override_blink != 0,
        phase: inp.phase != 0,
        charge: ChargeSnapshot {
            charging: inp.charging != 0,
            full: inp.full != 0,
        },
        pulses: inp.pulses,
    };

    let a = decide(i);

    out.level = if a.level { 1 } else { 0 };
    out.first_ms = a.first_ms;
    out.has_second = if a.has_second { 1 } else { 0 };
    out.second_level = if a.second_level { 1 } else { 0 };
    out.second_ms = a.second_ms;
    out.consume_pulse = if a.consume_pulse { 1 } else { 0 };
    out.first_wait_notify = if a.first_wait_notify { 1 } else { 0 };
    out.second_wait_notify = if a.second_wait_notify { 1 } else { 0 };
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(charging: bool, full: bool) -> ChargeSnapshot {
        ChargeSnapshot { charging, full }
    }

    /// Branch 1: ovr && ovr_blink → 500ms delay, level=phase, no second,
    /// consume_pulse=false, first_wait_notify=false.
    #[test]
    fn blink_override_uses_500ms_delay_and_toggles_phase() {
        let a = decide(Inputs {
            override_enabled: true,
            override_blink: true,
            phase: true,
            charge: snap(false, false),
            pulses: 0,
        });
        assert_eq!(a.level, true); // level == phase
        assert_eq!(a.first_ms, 500);
        assert_eq!(a.has_second, false);
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.first_wait_notify, false);
        assert_eq!(a.second_wait_notify, false);
    }

    /// Branch 2: ovr && !ovr_blink → 1000ms notify, level=true.
    #[test]
    fn static_override_uses_1000ms_notify() {
        let a = decide(Inputs {
            override_enabled: true,
            override_blink: false,
            phase: true,
            charge: snap(true, true), // should be ignored
            pulses: 10,
        });
        assert_eq!(a.level, true);
        assert_eq!(a.first_ms, 1000);
        assert_eq!(a.has_second, false);
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.first_wait_notify, true);
        assert_eq!(a.second_wait_notify, false);
    }

    /// Branch 3: !ovr && !charging && !full && pulses > 0 →
    /// 120ms delay then 180ms delay, level=false, consume_pulse=true.
    #[test]
    fn idle_activity_pulse_uses_120ms_delay_then_180ms_delay() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: false,
            charge: snap(false, false),
            pulses: 1,
        });
        assert_eq!(a.level, false);
        assert_eq!(a.first_ms, 120);
        assert_eq!(a.has_second, true);
        assert_eq!(a.second_level, true);
        assert_eq!(a.second_ms, 180);
        assert_eq!(a.consume_pulse, true);
        assert_eq!(a.first_wait_notify, false);
        assert_eq!(a.second_wait_notify, false);
    }

    /// Branch 4: !ovr && full → 1000ms notify, level=false.
    #[test]
    fn full_uses_1000ms_notify() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: false,
            charge: snap(false, true),
            pulses: 5,
        });
        assert_eq!(a.level, false);
        assert_eq!(a.first_ms, 1000);
        assert_eq!(a.has_second, false);
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.first_wait_notify, true);
        assert_eq!(a.second_wait_notify, false);
    }

    /// Branch 5: !ovr && charging → 200ms delay then 2800ms notify,
    /// level=false, has_second=true, consume_pulse=false,
    /// first_wait_notify=false, second_wait_notify=true.
    #[test]
    fn charging_uses_200ms_delay_then_2800ms_notify() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: true,
            charge: snap(true, false),
            pulses: 0,
        });
        assert_eq!(a.level, false);
        assert_eq!(a.first_ms, 200);
        assert_eq!(a.has_second, true);
        assert_eq!(a.second_level, true);
        assert_eq!(a.second_ms, 2800);
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.first_wait_notify, false);
        assert_eq!(a.second_wait_notify, true);
    }

    /// Branch 6: idle (none of the above) → 1000ms notify, level=true.
    #[test]
    fn idle_charge_state_uses_default_notify() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: false,
            charge: snap(false, false),
            pulses: 0,
        });
        assert_eq!(a.level, true);
        assert_eq!(a.first_ms, 1000);
        assert_eq!(a.has_second, false);
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.first_wait_notify, true);
        assert_eq!(a.second_wait_notify, false);
    }

    /// Sentinels: pulses=0 must NOT enter the pulse branch.
    #[test]
    fn zero_pulses_does_not_consume_pulse() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: false,
            charge: snap(false, false),
            pulses: 0,
        });
        assert_eq!(a.consume_pulse, false);
        assert_eq!(a.has_second, false);
    }

    /// full takes precedence over charging when both are true (never happens in
    /// practice but matches the branch order in led_decide).
    #[test]
    fn full_takes_precedence_over_charging_in_branch_order() {
        let a = decide(Inputs {
            override_enabled: false,
            override_blink: false,
            phase: false,
            charge: snap(true, true), // both true — full branch wins
            pulses: 0,
        });
        // Full branch: level=false, has_second=false, first_wait_notify=true
        assert_eq!(a.level, false);
        assert_eq!(a.has_second, false);
        assert_eq!(a.first_wait_notify, true);
    }
}
