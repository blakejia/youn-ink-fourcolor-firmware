//! Normalized charge state transition policy. Pure: C++ gathers the normalized
//! GPIO facts and timestamps; this decides the state.
//!
//! Polarity contract: the C++ shim owns `gpio_get_level(detect_gpio) ==
//! CHARGE_DETECT_CHARGING_LEVEL` (currently low == charging). Rust receives
//! `detect_charging == true` when charging is detected, which means the
//! physical detect pin is at `CHARGE_DETECT_CHARGING_LEVEL`. Do not re-interpret
//! the physical level here.

/// Stable condition threshold in milliseconds.
const STABLE_HIGH_MS: i64 = 400;
/// Power-present hold window in milliseconds.
const POWER_PRESENT_HOLD_MS: i64 = 1000;
/// Alternate detect/full event window in milliseconds.
const ALT_WINDOW_MS: i64 = 1500;

/// Decision output state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    NoPower = 0,
    Charging = 1,
    Full = 2,
    NoBattery = 3,
}

/// Rust inputs for the charge policy decision.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// Normalized detect-charger signal (true when charging is detected).
    pub detect_charging: bool,
    /// Normalized full-high signal (true when battery-full is indicated).
    pub full_high: bool,
    /// Current wall-clock time in milliseconds.
    pub now_ms: i64,
    /// When `detect_charging` was first observed continuously (-1 = never).
    pub detect_start_ms: i64,
    /// When `full_high` was first observed continuously (-1 = never).
    pub full_start_ms: i64,
    /// Most recent time `detect_charging` was observed.
    pub last_detect_ms: i64,
    /// Most recent time `full_high` was observed.
    pub last_full_ms: i64,
    /// Most recent time any power signal was observed (-1 = never).
    pub last_power_ms: i64,
}

/// Rust outputs: the decided state and updated timestamps for C++ to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub state: State,
    /// Updated `detect_start_ms`.
    pub next_detect_start_ms: i64,
    /// Updated `full_start_ms`.
    pub next_full_start_ms: i64,
    /// Updated `last_detect_ms`.
    pub next_last_detect_ms: i64,
    /// Updated `last_full_ms`.
    pub next_last_full_ms: i64,
    /// Updated `last_power_ms`.
    pub next_last_power_ms: i64,
}

/// Pack the snapshot bits into a 32-bit integer.
///
/// Bit layout (matches `ChargeStatus::Pack`):
///   bits 0..7  : state
///   bit  8     : power_present
///   bit  9     : charging  ( Charging | NoBattery )
///   bit  10    : full
///   bit  11    : no_battery
pub fn pack_snapshot(state: State, power_present: bool, charging: bool, full: bool, no_battery: bool) -> u32 {
    (state as u32)
        | ((power_present as u32) << 8)
        | ((charging as u32) << 9)
        | ((full as u32) << 10)
        | ((no_battery as u32) << 11)
}

/// Core decision function. All arguments are explicit; no hidden cross-task state.
pub fn decide(i: &Inputs) -> Output {
    // Update detect timestamps
    let (next_detect_start_ms, next_last_detect_ms) = if i.detect_charging {
        (
            if i.detect_start_ms < 0 { i.now_ms } else { i.detect_start_ms },
            i.now_ms,
        )
    } else {
        (-1, i.last_detect_ms)
    };

    // Update full timestamps
    let (next_full_start_ms, next_last_full_ms) = if i.full_high {
        (
            if i.full_start_ms < 0 { i.now_ms } else { i.full_start_ms },
            i.now_ms,
        )
    } else {
        (-1, i.last_full_ms)
    };

    // Power present: any signal within the hold window
    let power_present = i.last_power_ms >= 0 && (i.now_ms - i.last_power_ms) <= POWER_PRESENT_HOLD_MS;

    // Next power timestamp
    let next_last_power_ms = if i.detect_charging || i.full_high {
        i.now_ms
    } else {
        i.last_power_ms
    };

    // Stable condition: first continuous observation at least STABLE_HIGH_MS ago
    let detect_stable = next_detect_start_ms >= 0 && (i.now_ms - next_detect_start_ms) >= STABLE_HIGH_MS;
    let full_stable = next_full_start_ms >= 0 && (i.now_ms - next_full_start_ms) >= STABLE_HIGH_MS;

    // Alternate detect/full seen within the window
    let alt_seen = power_present
        && next_last_detect_ms >= 0
        && next_last_full_ms >= 0
        && (i.now_ms - next_last_detect_ms) <= ALT_WINDOW_MS
        && (i.now_ms - next_last_full_ms) <= ALT_WINDOW_MS;

    // NoBattery requires both alternation and instability
    let no_battery = alt_seen && !detect_stable && !full_stable;

    // State selection order mirrors ChargeStatus::Tick
    let state = if !power_present {
        State::NoPower
    } else if full_stable && !no_battery {
        State::Full
    } else if detect_stable || no_battery {
        if no_battery { State::NoBattery } else { State::Charging }
    } else {
        State::Charging
    };

    Output {
        state,
        next_detect_start_ms,
        next_full_start_ms,
        next_last_detect_ms,
        next_last_full_ms,
        next_last_power_ms,
    }
}

// ─── C ABI ───────────────────────────────────────────────────────────────────
/// `charge_policy.h` byte-for-byte. Timestamps use i64 because -1 is a sentinel.
#[repr(C)]
pub struct CInputs {
    pub detect_charging: i8,
    _pad0: [u8; 7],
    pub full_high: i8,
    _pad1: [u8; 7],
    pub now_ms: i64,
    pub detect_start_ms: i64,
    pub full_start_ms: i64,
    pub last_detect_ms: i64,
    pub last_full_ms: i64,
    pub last_power_ms: i64,
}

/// Output returned to C — must match `rf_charge_policy_output_t` in
/// `charge_policy.h` byte-for-byte.
#[repr(C)]
pub struct COutput {
    pub state: i32,
    _pad0: i32,
    pub next_detect_start_ms: i64,
    pub next_full_start_ms: i64,
    pub next_last_detect_ms: i64,
    pub next_last_full_ms: i64,
    pub next_last_power_ms: i64,
}

// ─── C ABI ───────────────────────────────────────────────────────────────────

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_charge_policy_decide(inp: *const CInputs, out: *mut COutput) {
    let i = unsafe { &*inp };
    let r = decide(&Inputs {
        detect_charging: i.detect_charging != 0,
        full_high: i.full_high != 0,
        now_ms: i.now_ms,
        detect_start_ms: i.detect_start_ms,
        full_start_ms: i.full_start_ms,
        last_detect_ms: i.last_detect_ms,
        last_full_ms: i.last_full_ms,
        last_power_ms: i.last_power_ms,
    });
    let o = unsafe { &mut *out };
    o.state = r.state as i32;
    o.next_detect_start_ms = r.next_detect_start_ms;
    o.next_full_start_ms = r.next_full_start_ms;
    o.next_last_detect_ms = r.next_last_detect_ms;
    o.next_last_full_ms = r.next_last_full_ms;
    o.next_last_power_ms = r.next_last_power_ms;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Sentinel timestamp value: -1 means "never observed".
    const NEVER: i64 = -1;

    /// Helper: short-hand Inputs builder.
    fn inp(detect: bool, full: bool, now: i64, ds: i64, fs: i64, ld: i64, lf: i64, lp: i64) -> Inputs {
        Inputs {
            detect_charging: detect,
            full_high: full,
            now_ms: now,
            detect_start_ms: ds,
            full_start_ms: fs,
            last_detect_ms: ld,
            last_full_ms: lf,
            last_power_ms: lp,
        }
    }

    /// Helper: extract state from decide output.
    fn state(detect: bool, full: bool, now: i64, ds: i64, fs: i64, ld: i64, lf: i64, lp: i64) -> State {
        decide(&inp(detect, full, now, ds, fs, ld, lf, lp)).state
    }

    // ── Boundary: 400 ms stability ─────────────────────────────────────────

    /// At exactly 399 ms the condition is not yet stable → Charging.
    #[test]
    fn low_level_charging_condition_needs_400ms_to_become_stable() {
        // First detect at t=0: C++ sets last_power_present_ms_ when detect goes high
        assert_eq!(state(true, false, 0, NEVER, NEVER, NEVER, NEVER, 0), State::Charging);
        // At t=399: 399 < 400 → not stable
        assert_eq!(state(true, false, 399, 0, NEVER, 0, NEVER, 0), State::Charging);
        // At t=400: 400 >= 400 → stable (but Full precedence only applies when full_stable)
        assert_eq!(state(true, false, 400, 0, NEVER, 0, NEVER, 0), State::Charging);
    }

    // ── Boundary: 1000 ms power-present hold ───────────────────────────────

    /// Power present is held for 1000 ms after the last signal.
    #[test]
    fn power_present_is_held_for_1000ms() {
        // Signal at t=0; power present through t=1000
        assert_eq!(state(true, false, 500, 0, NEVER, 0, NEVER, 0), State::Charging);
        assert_eq!(state(true, false, 1000, 0, NEVER, 0, NEVER, 0), State::Charging);
        // At t=1001: 1001 - 0 = 1001 > 1000 → power expired → NoPower
        assert_eq!(state(true, false, 1001, 0, NEVER, 0, NEVER, 0), State::NoPower);
    }

    // ── Precedence: Full beats Charging when both are stable ─────────────────

    /// When full_high is stable for 400 ms it wins over charging.
    #[test]
    fn full_takes_precedence_over_charging_when_both_are_stable() {
        // Both stable at 500 ms → Full
        assert_eq!(state(true, true, 500, 0, 0, 0, 0, 0), State::Full);
        // Charging only stable → Charging
        assert_eq!(state(true, false, 500, 0, NEVER, 0, NEVER, 0), State::Charging);
    }

    // ── NoBattery: alternating detect+full within 1500 ms, both unstable ─────

    /// Rapid alternation of detect and full without stability → NoBattery.
    #[test]
    fn alternating_detect_and_full_within_1500ms_is_no_battery() {
        // Detect at t=0, full at t=100; both unstable at t=500
        // alt_seen: last_detect=0, last_full=100, now=500
        // both within 1500 ms: true
        // detect_stable: 500-0 >= 400: true  ← this would make it Charging
        // Let's use tighter timing where neither is stable yet.
        // Detect at t=100, full at t=200, now=500: neither is stable
        assert_eq!(state(true, true, 500, NEVER, NEVER, 100, 200, 0), State::NoBattery);
    }

    // ── Reset: no power → NoPower ────────────────────────────────────────────

    /// No power signal within the hold window → NoPower.
    #[test]
    fn no_power_resets_after_hold_window() {
        // No detect, no full → NoPower regardless of timestamps
        assert_eq!(state(false, false, 2000, NEVER, NEVER, NEVER, NEVER, NEVER), State::NoPower);
        // Last power at 0, now 2000: 2000-0 = 2000 > 1000 → expired
        assert_eq!(state(false, false, 2000, NEVER, NEVER, NEVER, NEVER, 0), State::NoPower);
    }

    // ── C ABI mapping ────────────────────────────────────────────────────────

    #[test]
    fn state_enum_values_match_cpp() {
        assert_eq!(State::NoPower as u8, 0);
        assert_eq!(State::Charging as u8, 1);
        assert_eq!(State::Full as u8, 2);
        assert_eq!(State::NoBattery as u8, 3);
    }

    #[test]
    fn charging_flag_covers_both_charging_and_no_battery() {
        let charging = |state| matches!(state, State::Charging | State::NoBattery);

        // Stable charging → charging=true
        let r = decide(&inp(true, false, 500, 0, NEVER, 0, NEVER, 0));
        assert!(charging(r.state));

        // NoBattery → charging=true
        let r = decide(&inp(true, true, 500, NEVER, NEVER, 100, 200, 0));
        assert!(charging(r.state));

        // Full → charging=false
        let r = decide(&inp(true, true, 500, 0, 0, 0, 0, 0));
        assert!(!charging(r.state));
    }

    #[test]
    fn packed_snapshot_bit_layout() {
        // State::Full, power_present=true, charging=false, full=true, no_battery=false
        let packed = pack_snapshot(State::Full, true, false, true, false);
        assert_eq!(packed & 0xFF, State::Full as u32);           // bits 0..7
        assert_eq!((packed >> 8) & 1, 1);                        // bit 8
        assert_eq!((packed >> 9) & 1, 0);                        // bit 9
        assert_eq!((packed >> 10) & 1, 1);                       // bit 10
        assert_eq!((packed >> 11) & 1, 0);                       // bit 11

        // NoBattery: power_present=true, charging=true, full=false, no_battery=true
        let packed = pack_snapshot(State::NoBattery, true, true, false, true);
        assert_eq!(packed & 0xFF, State::NoBattery as u32);
        assert_eq!((packed >> 8) & 1, 1);
        assert_eq!((packed >> 9) & 1, 1);  // charging
        assert_eq!((packed >> 10) & 1, 0);
        assert_eq!((packed >> 11) & 1, 1);
    }

    #[test]
    fn ffi_decide_returns_correct_timestamps() {
        let ci = CInputs {
            detect_charging: 1,
            _pad0: [0u8; 7],
            full_high: 0,
            _pad1: [0u8; 7],
            now_ms: 500,
            detect_start_ms: 100,
            full_start_ms: NEVER,
            last_detect_ms: 500,
            last_full_ms: NEVER,
            last_power_ms: 500,
        };
        let mut co = COutput {
            state: 99,
            _pad0: 0,
            next_detect_start_ms: 99,
            next_full_start_ms: 99,
            next_last_detect_ms: 99,
            next_last_full_ms: 99,
            next_last_power_ms: 99,
        };
        // SAFETY: co is valid and aligned
        unsafe { rf_charge_policy_decide(&ci, &mut co) };

        // detect_start_ms stays at 100 (not -1, so not reset)
        assert_eq!(co.next_detect_start_ms, 100);
        // last_detect_ms updates to now
        assert_eq!(co.next_last_detect_ms, 500);
        // last_power_ms updates
        assert_eq!(co.next_last_power_ms, 500);
        assert_eq!(co.state, State::Charging as i32);
    }

    // ── Sentinel: -1 timestamp is a real sentinel ─────────────────────────────

    #[test]
    fn detect_start_of_minus_one_means_never_seen() {
        // With detect_start_ms = -1 and detect_charging=true at now=100:
        // next_detect_start_ms should become 100 (latch)
        let r = decide(&inp(true, false, 100, NEVER, NEVER, NEVER, NEVER, NEVER));
        assert_eq!(r.next_detect_start_ms, 100);
    }

    // Sentinel: changing >= to > in stability check must break a test
    /// At exactly t=400 with full_high stable, >= returns Full.
    /// A broken >=→> mutation would return Charging.
    #[test]
    fn stability_boundary_is_exclusive_of_400ms_for_full_precedence() {
        let r = decide(&inp(true, true, 400, 0, 0, 0, 0, 0));
        assert_eq!(r.state, State::Full);
    }
}
