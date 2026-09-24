//! Red-first integration tests for the Task 4 battery relative-activity policy.
//!
//! They pin the migrated decision table against the observed production
//! behaviour plus the new gates from the ABCDE design (§5):
//!
//! - monotonic discharge accepted: steady discharge + expired gate -> Accept,
//!   direction follows the charge fact, next gate armed
//! - charge-direction transition preserves sample: charging -> discharging
//!   inside the window still reports (design §5: transitions retain samples,
//!   never forge a continuous percentage)
//! - outlier voltage filtered: mv==0 / below 2500 / above 5000 (the server
//!   ingest window) -> Reject, gate stamp untouched so the next wake retries
//! - gate expiry allows sample / non-expired gate skips: the time gate rules
//! - outputs never resemble SOC: direction <= 4, relative_activity <= 2 —
//!   no 0-100 percent, no mAh, by construction
//!
//! TDD: this file is written BEFORE `battery_activity_policy.rs` exists, so
//! the first `cargo test` must fail (missing module). Minimal implementation
//! follows.

use rust_firmware::battery_activity_policy::*;

fn sample(
    mv: u16,
    charge: u8,
    has_sample: bool,
    now: i64,
    last: i64,
    prev_mv: u16,
    prev_charge: u8,
    prev_valid: bool,
    interval: u32,
) -> SampleOutput {
    decide(&SampleInputs {
        voltage_mv: mv,
        charge,
        has_sample,
        _pad: [0; 4],
        now_s: now,
        last_sample_s: last,
        prev_mv,
        prev_charge,
        prev_valid,
        min_interval_s: interval,
    })
}

/// Steady discharge, gate long expired: the common hourly report.
fn discharging(mv: u16, now: i64, last: i64, prev_mv: u16) -> SampleOutput {
    sample(
        mv,
        DIRECTION_DISCHARGING,
        true,
        now,
        last,
        prev_mv,
        DIRECTION_DISCHARGING,
        true,
        3600,
    )
}

// ── acceptance ────────────────────────────────────────────────────────────

#[test]
fn monotonic_discharge_accepted() {
    // prev 4000 -> now 3950 discharging, last report an hour+ ago.
    let out = discharging(3950, 5000, 1000, 4000);
    assert_eq!(out.accept, 1);
    assert_eq!(out.direction, DIRECTION_DISCHARGING);
    assert_eq!(out.filtered, 0);
    assert_eq!(out.next_sample_s, 5000 + 3600);
}

#[test]
fn charge_direction_transition_preserves_sample() {
    // Unplugged mid-window: charging -> discharging with only 100 s elapsed.
    // The old time-only gate would skip this wake; the policy preserves the
    // transition sample instead of forging continuity across it.
    let out = sample(
        4050,
        DIRECTION_DISCHARGING,
        true,
        2000,
        1900,
        4100,
        DIRECTION_CHARGING,
        true,
        3600,
    );
    assert_eq!(out.accept, 1);
    assert_eq!(out.direction, DIRECTION_DISCHARGING);
}

#[test]
fn reverse_transition_preserves_sample() {
    // Plugged mid-window: discharging -> charging is preserved too.
    let out = sample(
        4110,
        DIRECTION_CHARGING,
        true,
        2000,
        1900,
        3980,
        DIRECTION_DISCHARGING,
        true,
        3600,
    );
    assert_eq!(out.accept, 1);
    assert_eq!(out.direction, DIRECTION_CHARGING);
}

#[test]
fn same_side_full_to_charging_is_not_a_transition() {
    // Full <-> charging stays on the charging side: no forced accept, the
    // gate still skips inside the window.
    let out = sample(
        4180, DIRECTION_FULL, true, 2000, 1900, 4170, DIRECTION_CHARGING, true, 3600,
    );
    assert_eq!(out.accept, 0);
    assert_eq!(out.next_sample_s, 1900);
}

// ── outlier filter ────────────────────────────────────────────────────────

#[test]
fn outlier_voltage_filtered() {
    // mv==0 keeps the legacy `b_mv > 0` read check; the 2500..5000 window
    // mirrors the server ingest range, so the device never spends radio on
    // a row the server would drop.
    for mv in [0u16, 1, 2499, 5001, 6000, u16::MAX] {
        let out = discharging(mv, 5000, 1000, 4000);
        assert_eq!(out.accept, 0, "mv={mv}");
        // The filter, not the gate, is what dropped it.
        assert_eq!(out.filtered, 1, "mv={mv}");
        // A filtered outlier must not buy silence: the gate stamp is kept so
        // the next wake retries instead of waiting out a full window.
        assert_eq!(out.next_sample_s, 1000, "mv={mv}");
    }
}

#[test]
fn missing_sample_filtered() {
    // Sensor absent / ADC failure / no-battery: no sample at all.
    let out = sample(3950, DIRECTION_DISCHARGING, false, 5000, 1000, 4000,
                     DIRECTION_DISCHARGING, true, 3600);
    assert_eq!(out.accept, 0);
    assert_eq!(out.filtered, 1);
    assert_eq!(out.next_sample_s, 1000);
}

#[test]
fn outlier_does_not_poison_the_baseline() {
    // The outlier carries no usable direction/activity either: resting
    // levels, direction still resolved from the (suspect) fact byte.
    let out = discharging(0, 5000, 1000, 4000);
    assert_eq!(out.relative_activity, RELATIVE_ACTIVITY_RESTING);
}

// ── time gate ─────────────────────────────────────────────────────────────

#[test]
fn gate_expiry_allows_sample() {
    // elapsed (3600) >= interval (3600): boundary is inclusive, due.
    let out = discharging(3950, 4600, 1000, 4000);
    assert_eq!(out.accept, 1);
    assert_eq!(out.next_sample_s, 4600 + 3600);
}

#[test]
fn non_expired_gate_skips() {
    // Same direction, small delta, 1000 s elapsed of a 3600 s window.
    let out = discharging(3990, 2000, 1000, 4000);
    assert_eq!(out.accept, 0);
    assert_eq!(out.direction, DIRECTION_DISCHARGING);
    assert_eq!(out.filtered, 0, "not due is not the same as filtered");
    assert_eq!(out.next_sample_s, 1000);
}

#[test]
fn first_sample_accepts_and_arms() {
    // No prior report (last < 0): accept and arm off the wall clock.
    let out = sample(3950, DIRECTION_DISCHARGING, true, 5000, -1, 0, 0, false, 3600);
    assert_eq!(out.accept, 1);
    assert_eq!(out.next_sample_s, 5000 + 3600);
    // No baseline yet: no movement observed -> resting.
    assert_eq!(out.relative_activity, RELATIVE_ACTIVITY_RESTING);
}

#[test]
fn unset_clock_accepts_without_arming() {
    // Cold boot before SNTP (`now_s < 0`, notify-style unset): must not wedge
    // behind the wall clock, and must never persist a stamp off it.
    let out = sample(3950, DIRECTION_DISCHARGING, true, -1, 1000, 4000,
                     DIRECTION_DISCHARGING, true, 3600);
    assert_eq!(out.accept, 1);
    assert_eq!(out.next_sample_s, 1000);
}

// ── relative activity ─────────────────────────────────────────────────────

#[test]
fn activity_grades_sample_to_sample_movement() {
    // |d| <= 20 mV: ADC noise band -> resting.
    let resting = discharging(3990, 5000, 1000, 4000);
    assert_eq!(resting.relative_activity, RELATIVE_ACTIVITY_RESTING);
    // 21..=100 mV: ordinary load step.
    let low = discharging(3950, 5000, 1000, 4000);
    assert_eq!(low.relative_activity, RELATIVE_ACTIVITY_LOW);
    // > 100 mV between reports: worth a closer look, still just relative.
    let high = discharging(3800, 5000, 1000, 4000);
    assert_eq!(high.relative_activity, RELATIVE_ACTIVITY_HIGH);
    // Rising deltas grade the same: charging ramps are movement too.
    let rise = sample(4150, DIRECTION_CHARGING, true, 5000, 1000, 4000,
                      DIRECTION_CHARGING, true, 3600);
    assert_eq!(rise.relative_activity, RELATIVE_ACTIVITY_HIGH);
}

// ── no-SOC contract ───────────────────────────────────────────────────────

#[test]
fn outputs_never_resemble_soc_or_mah() {
    // The policy owns acceptance/direction/activity only. Direction stays in
    // the 0..=4 wire encoding, activity in 0..=2 levels: neither can ever be
    // mistaken for a 0-100 percent or a capacity figure.
    for mv in (2400u16..=5100).step_by(37) {
        for charge in 0u8..=6 {
            let out = sample(mv, charge, true, 5000, 1000, 4000,
                             DIRECTION_DISCHARGING, true, 3600);
            assert!(out.direction <= 4, "mv={mv} charge={charge}");
            assert!(out.relative_activity <= 2, "mv={mv} charge={charge}");
            assert!(out.accept <= 1, "mv={mv} charge={charge}");
        }
    }
}

// ── C ABI ─────────────────────────────────────────────────────────────────

#[test]
fn c_abi_returns_the_same_decisions() {
    let inp = SampleInputs {
        voltage_mv: 3950,
        charge: DIRECTION_DISCHARGING,
        has_sample: true,
        _pad: [0; 4],
        now_s: 5000,
        last_sample_s: 1000,
        prev_mv: 4000,
        prev_charge: DIRECTION_DISCHARGING,
        prev_valid: true,
        min_interval_s: 3600,
    };
    // SAFETY: pointer is to a local that outlives the call.
    let out = unsafe { rf_battery_activity_decide(&inp) };
    assert_eq!(out.accept, 1);
    assert_eq!(out.direction, DIRECTION_DISCHARGING);
    assert_eq!(out.relative_activity, RELATIVE_ACTIVITY_LOW);
    assert_eq!(out.filtered, 0);
    assert_eq!(out.next_sample_s, 8600);

    // The cheap pre-ADC gate is the same decision C++ consults before the
    // 10-sample burst: window open here, closed one second later.
    assert_eq!(unsafe { rf_battery_activity_gate_open(&inp) }, 1);
    let mut closed = inp;
    closed.now_s = 2000;
    closed.last_sample_s = 1900;
    assert_eq!(unsafe { rf_battery_activity_gate_open(&closed) }, 0);
}

#[test]
fn c_structs_match_the_header_layout() {
    // Layout contract with rust/include/battery_activity_policy.h: field
    // order, offsets and sizes must agree or the FFI reads garbage. Same
    // style as the notify_policy layout test.
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<SampleInputs>(), 32);
    assert_eq!(offset_of!(SampleInputs, voltage_mv), 0);
    assert_eq!(offset_of!(SampleInputs, charge), 2);
    assert_eq!(offset_of!(SampleInputs, has_sample), 3);
    assert_eq!(offset_of!(SampleInputs, now_s), 8);
    assert_eq!(offset_of!(SampleInputs, last_sample_s), 16);
    assert_eq!(offset_of!(SampleInputs, prev_mv), 24);
    assert_eq!(offset_of!(SampleInputs, prev_charge), 26);
    assert_eq!(offset_of!(SampleInputs, prev_valid), 27);
    assert_eq!(offset_of!(SampleInputs, min_interval_s), 28);

    assert_eq!(size_of::<SampleOutput>(), 16);
    assert_eq!(offset_of!(SampleOutput, accept), 0);
    assert_eq!(offset_of!(SampleOutput, direction), 1);
    assert_eq!(offset_of!(SampleOutput, relative_activity), 2);
    assert_eq!(offset_of!(SampleOutput, filtered), 3);
    assert_eq!(offset_of!(SampleOutput, next_sample_s), 8);
}
