//! Battery relative-activity policy for the NOTE4C firmware. C++ owns the
//! ADC, the charge GPIO snapshot, the RTC stamps and the HTTP report; this
//! module only decides — from facts C++ hands it — whether a sample is
//! accepted, whether it is filtered as an outlier, which direction the
//! battery is moving, how much it moved relative to the last reported
//! sample, and when the next sample is due.
//!
//! Compatibility port of the production sample path (`page_sync`'s v/p/c
//! block + the one-hour `rf_battery_due`/`rf_battery_arm` gate) — not a
//! redesign. New vs. the old time-only gate, per design §5:
//!
//! - **Outlier filter**: `has_sample == 0` (sensor absent / ADC failure /
//!   no battery) or a voltage outside the server ingest window
//!   (2500..=5000 mV, `server/youn_server/app.py`) rejects the sample and
//!   KEEPS the gate stamp, so one bad reading buys no silence — the next
//!   wake retries. The old code only had the `b_mv > 0` read check; the
//!   range is the server's own, so no row the device cannot store is ever
//!   sent.
//! - **Direction transition**: a charging↔discharging flip between the last
//!   reported sample and now forces the sample through even inside the
//!   window (design §5: transitions retain samples; never forge continuity
//!   across them). Same-side moves (charging↔full) are not transitions.
//! - **Relative activity**: `|mv - prev_mv|` graded into resting / low /
//!   high levels. This is the ONLY level-shaped output and it is named
//!   `relative_activity`: there is no current sensor, so nothing here is or
//!   may be named a percentage or a capacity. The voltage→percent map used
//!   by the UI and the `p=` wire field stays in C++ untouched.
//!
//! The gate itself is the old one-hour sliding window (first sample always
//! fires, `now < 0` reports without arming, `elapsed >= min_interval` opens
//! the window), evaluated here so the decision — not the C++ caller — owns
//! it. C++ fills the facts (`rf_battery_activity_context` /
//! `_read`) and persists the bytes (`rf_battery_activity_commit`).

// ── wire/staged encodings (keep in step with the header and the server) ────

/// Unknown direction (also the sink for out-of-range facts).
pub const DIRECTION_UNKNOWN: u8 = 0;
/// Battery present, no external power (server encoding 1).
pub const DIRECTION_NO_POWER: u8 = 1;
/// Charger connected, actively charging (server encoding 2).
pub const DIRECTION_CHARGING: u8 = 2;
/// Charger connected, full (server encoding 3).
pub const DIRECTION_FULL: u8 = 3;
/// Running on battery (server encoding 4).
pub const DIRECTION_DISCHARGING: u8 = 4;

/// Sample-to-sample voltage change within the ADC noise band.
pub const RELATIVE_ACTIVITY_RESTING: u8 = 0;
/// Ordinary load/charge step between reports.
pub const RELATIVE_ACTIVITY_LOW: u8 = 1;
/// Unusually large step between reports — still only relative movement.
pub const RELATIVE_ACTIVITY_HIGH: u8 = 2;

/// Lower bound of the server ingest window for `v` (2500 mV, inclusive).
pub const VOLTAGE_MIN_MV: u16 = 2500;
/// Upper bound of the server ingest window for `v` (5000 mV, inclusive).
pub const VOLTAGE_MAX_MV: u16 = 5000;
/// `|mv - prev_mv|` at or below this is treated as ADC noise.
pub const ACTIVITY_RESTING_MV: u16 = 20;
/// Above this is an unusually large step (still not a quantity estimate).
pub const ACTIVITY_HIGH_MV: u16 = 100;

// ── C ABI ─────────────────────────────────────────────────────────────────
// `#[repr(C)]` + explicit padding pins the layout against
// `rf_battery_activity_inputs_t` / `rf_battery_activity_output_t` in
// `rust/include/battery_activity_policy.h`; a layout contract test asserts
// the offsets and size.

/// Facts for one sampling decision. C++ fills this: `rf_battery_activity_context`
/// supplies the cheap facts (charge encoding, effective clock, RTC stamps),
/// `rf_battery_activity_read` supplies the ADC burst (only after
/// [`gate_open`] says the window is open, so inside the window the 10-sample
/// read never happens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SampleInputs {
    /// Averaged battery voltage in mV (0 until `read` fills it).
    pub voltage_mv: u16,
    /// Charge fact, `DIRECTION_*` encoding.
    pub charge: u8,
    /// 1 = C++ produced a reading; 0 = sensor absent / ADC failure.
    pub has_sample: bool,
    /// Padding to align `now_s` on an 8-byte boundary.
    pub _pad: [u8; 4],
    /// Effective clock in seconds: `time()` when plausible, else the
    /// PCF8563 epoch, else negative (unset — report but never arm).
    pub now_s: i64,
    /// Last accepted+reported sample epoch; negative = none yet.
    pub last_sample_s: i64,
    /// Voltage of the last accepted+reported sample (activity baseline).
    pub prev_mv: u16,
    /// Direction of that last accepted+reported sample.
    pub prev_charge: u8,
    /// 1 = `prev_mv`/`prev_charge` hold a real prior sample.
    pub prev_valid: bool,
    /// Minimum seconds between same-direction reports (C++ config, 3600).
    pub min_interval_s: u32,
}

/// Decision for one sampling attempt. Field names are the contract: this is
/// a relative-activity estimate and nothing else — no percentage, no
/// capacity (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SampleOutput {
    /// 1 = report this sample (C++ writes v/p/c and advances the stamp).
    pub accept: u8,
    /// Normalized `DIRECTION_*` value to report as `c=`.
    pub direction: u8,
    /// `RELATIVE_ACTIVITY_*` movement vs. the last reported sample.
    pub relative_activity: u8,
    /// 1 = the sample was dropped by the outlier filter (as opposed to
    /// "not due yet" — both give `accept = 0`, but only this one must be
    /// retried on the next wake without waiting out the window).
    pub filtered: u8,
    /// Padding to align `next_sample_s` on an 8-byte boundary.
    pub _pad: [u8; 4],
    /// New stamp for `last_sample_s`: `now_s + min_interval_s` when the
    /// sample is accepted and the clock is set; the unchanged prior stamp
    /// otherwise (so a rejected/skipped sample never opens a silent hour).
    pub next_sample_s: i64,
}

/// Clamp a fact byte into the documented wire encoding.
pub fn normalize_direction(charge: u8) -> u8 {
    if charge <= DIRECTION_DISCHARGING {
        charge
    } else {
        DIRECTION_UNKNOWN
    }
}

/// Which side of the charger a direction sits on. `DIRECTION_UNKNOWN` is
/// its own side: a fact that was never meaningful cannot transition.
fn side(direction: u8) -> u8 {
    match direction {
        DIRECTION_CHARGING | DIRECTION_FULL => 1,
        DIRECTION_NO_POWER | DIRECTION_DISCHARGING => 2,
        _ => 0,
    }
}

fn voltage_ok(mv: u16) -> bool {
    mv >= VOLTAGE_MIN_MV && mv <= VOLTAGE_MAX_MV
}

fn grade_activity(delta_mv: u16) -> u8 {
    if delta_mv <= ACTIVITY_RESTING_MV {
        RELATIVE_ACTIVITY_RESTING
    } else if delta_mv <= ACTIVITY_HIGH_MV {
        RELATIVE_ACTIVITY_LOW
    } else {
        RELATIVE_ACTIVITY_HIGH
    }
}

/// Charging↔discharging flip vs. the last reported sample, with both sides
/// known. Same-side moves (charging↔full, discharging↔unknown) are not.
fn direction_transition(i: &SampleInputs) -> bool {
    if !i.prev_valid {
        return false;
    }
    let prev = side(normalize_direction(i.prev_charge));
    let now = side(normalize_direction(i.charge));
    prev != 0 && now != 0 && prev != now
}

/// Is the one-hour window open for this sample? Exposed so the caller runs
/// the cheap gate before the ADC burst; pure, no I/O.
pub fn gate_open(i: &SampleInputs) -> bool {
    // First sample (no prior report): always.
    if !i.prev_valid || i.last_sample_s < 0 {
        return true;
    }
    // Clock unset: report every wake (the old gate did too) but never arm.
    if i.now_s < 0 {
        return true;
    }
    // A direction flip must not wait out the window — preserving the
    // transition sample is a design §5 requirement.
    if direction_transition(i) {
        return true;
    }
    // Sliding window; a regressed clock saturates to 0 elapsed → closed.
    i.now_s.saturating_sub(i.last_sample_s) >= i.min_interval_s as i64
}

/// Core decision. Pure: no I/O, no globals.
pub fn decide(i: &SampleInputs) -> SampleOutput {
    let mut o = SampleOutput {
        accept: 0,
        direction: normalize_direction(i.charge),
        relative_activity: RELATIVE_ACTIVITY_RESTING,
        filtered: 0,
        _pad: [0; 4],
        next_sample_s: i.last_sample_s,
    };

    // Outlier filter first: a sensor failure or an out-of-window voltage
    // carries no movement information at all, and the stamp must stay put
    // so the next wake retries.
    if !i.has_sample || !voltage_ok(i.voltage_mv) {
        o.filtered = 1;
        return o;
    }

    // Relative movement vs. the last reported sample (resting when there is
    // no baseline yet).
    if i.prev_valid {
        let delta = (i.voltage_mv as i32).abs_diff(i.prev_mv as i32) as u16;
        o.relative_activity = grade_activity(delta);
    }

    if !gate_open(i) {
        return o;
    }

    o.accept = 1;
    // Arm only off a plausible clock; an unset clock reports but never
    // advances the stamp (C++ additionally guards the write with the
    // NEVER bound, mirroring the old `rf_battery_arm`).
    if i.now_s >= 0 {
        o.next_sample_s = i.now_s.saturating_add(i.min_interval_s as i64);
    }
    o
}

/// # Safety
/// `inp` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_battery_activity_decide(inp: *const SampleInputs) -> SampleOutput {
    decide(unsafe { &*inp })
}

/// Cheap pre-ADC gate for callers that have not read the cell yet (voltage
/// fields ignored — only the clock/stamp/direction facts matter).
///
/// # Safety
/// `inp` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_battery_activity_gate_open(inp: *const SampleInputs) -> u8 {
    gate_open(unsafe { &*inp }) as u8
}

// ─── Tests ────────────────────────────────────────────────────────────────
// Red-first coverage lives in `tests/battery_activity_policy.rs`; the
// layout/ABI contract is asserted there too (same style as notify_policy).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_encoding_matches_the_wire_contract() {
        assert_eq!(DIRECTION_UNKNOWN, 0);
        assert_eq!(DIRECTION_NO_POWER, 1);
        assert_eq!(DIRECTION_CHARGING, 2);
        assert_eq!(DIRECTION_FULL, 3);
        assert_eq!(DIRECTION_DISCHARGING, 4);
        // Out-of-range facts collapse to unknown instead of leaking a 5/6.
        assert_eq!(normalize_direction(5), DIRECTION_UNKNOWN);
        assert_eq!(normalize_direction(255), DIRECTION_UNKNOWN);
    }

    #[test]
    fn activity_thresholds_grade_boundary_deltas() {
        assert_eq!(grade_activity(0), RELATIVE_ACTIVITY_RESTING);
        assert_eq!(grade_activity(ACTIVITY_RESTING_MV), RELATIVE_ACTIVITY_RESTING);
        assert_eq!(grade_activity(ACTIVITY_RESTING_MV + 1), RELATIVE_ACTIVITY_LOW);
        assert_eq!(grade_activity(ACTIVITY_HIGH_MV), RELATIVE_ACTIVITY_LOW);
        assert_eq!(grade_activity(ACTIVITY_HIGH_MV + 1), RELATIVE_ACTIVITY_HIGH);
    }

    #[test]
    fn voltage_window_is_the_server_ingest_range() {
        assert!(!voltage_ok(VOLTAGE_MIN_MV - 1));
        assert!(voltage_ok(VOLTAGE_MIN_MV));
        assert!(voltage_ok(VOLTAGE_MAX_MV));
        assert!(!voltage_ok(VOLTAGE_MAX_MV + 1));
    }
}
