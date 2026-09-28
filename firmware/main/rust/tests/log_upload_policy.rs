//! Log-upload decision table + C layout contract.
//!
//! The layout test mirrors the style of the notify_policy / page_compare_policy
//! tests: field order, offsets and size must agree with
//! `rust/include/log_upload_policy.h` or the FFI reads garbage.

use rust_firmware::log_upload_policy::*;
fn base() -> Inputs {
    Inputs {
        local_set: OPINION_NONE,
        server_set: OPINION_ON,
        has_pending: 1,
        wifi_ready: 1,
        // Above MAX_UPLOAD_BYTES so the happy path actually exercises the cap;
        // `max_bytes_is_capped_by_pending_bytes` pins the other bound.
        pending_bytes: 1500,
        pending_lines: 2,
        fail_streak: 0,
        _pad: [0; 4],
        last_fail_s: -1,
        now_s: 1000,
    }
}

#[test]
fn uploads_when_enabled_and_pending_and_online() {
    let d = decide(&base());
    assert_eq!(d.action, UPLOAD);
    assert_eq!(d.max_bytes, MAX_UPLOAD_BYTES);
}

#[test]
fn service_switch_alone_enables() {
    // Neither side needs the other: the service can turn it on by itself.
    let i = Inputs { local_set: OPINION_NONE, server_set: OPINION_ON, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn local_switch_alone_enables() {
    // And so can the device holder, with no service involvement.
    let i = Inputs { local_set: OPINION_ON, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn neither_side_speaking_leaves_it_off() {
    let i = Inputs { local_set: OPINION_NONE, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn service_off_overrides_local_on() {
    // The documented conflict rule: the service wins.
    let i = Inputs { local_set: OPINION_ON, server_set: OPINION_OFF, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn service_on_overrides_local_off() {
    let i = Inputs { local_set: OPINION_OFF, server_set: OPINION_ON, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn local_off_alone_disables() {
    let i = Inputs { local_set: OPINION_OFF, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn resolve_enabled_is_the_conflict_rule() {
    assert!(resolve_enabled(OPINION_NONE, OPINION_ON));
    assert!(resolve_enabled(OPINION_ON, OPINION_NONE));
    assert!(!resolve_enabled(OPINION_NONE, OPINION_NONE));
    assert!(!resolve_enabled(OPINION_ON, OPINION_OFF), "service off wins");
    assert!(resolve_enabled(OPINION_OFF, OPINION_ON), "service on wins");
}

#[test]
fn disabled_switch_skips() {
    let i = Inputs { server_set: OPINION_OFF, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn nothing_pending_skips() {
    let i = Inputs { has_pending: 0, pending_bytes: 0, pending_lines: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_EMPTY);
}

#[test]
fn offline_skips() {
    let i = Inputs { wifi_ready: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_NO_NET);
}

#[test]
fn disabled_beats_everything() {
    // Precedence: the composed switch is checked before pending/network so a
    // disabled device never even inspects the buffer.
    let i = Inputs {
        server_set: OPINION_OFF, has_pending: 0, wifi_ready: 0, ..base()
    };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn backoff_holds_until_window_elapses() {
    let i = Inputs { fail_streak: 2, last_fail_s: 900, now_s: 1000, ..base() };
    assert_eq!(decide(&i).action, SKIP_BACKOFF);
}

#[test]
fn backoff_releases_after_window() {
    let i = Inputs { fail_streak: 2, last_fail_s: 0, now_s: 100_000, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn unset_clock_does_not_gate_on_backoff() {
    // now_s < 0 means the wall clock is unset (cold boot): the device must not
    // wedge behind a comparison against a bogus stamp.
    let i = Inputs { fail_streak: 3, last_fail_s: 0, now_s: -1, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn max_bytes_is_capped_by_pending_bytes() {
    let i = Inputs { pending_bytes: 10, ..base() };
    assert_eq!(decide(&i).max_bytes, 10);
}

#[test]
fn backoff_delay_grows_then_caps() {
    assert_eq!(backoff_delay_s(0, 60, 900), 0);
    assert_eq!(backoff_delay_s(1, 60, 900), 60);
    assert_eq!(backoff_delay_s(2, 60, 900), 120);
    assert_eq!(backoff_delay_s(4, 60, 900), 480);
    assert_eq!(backoff_delay_s(5, 60, 900), 900, "capped");
    assert_eq!(backoff_delay_s(99, 60, 900), 900, "no overflow at high streaks");
}

#[test]
fn decide_is_idempotent() {
    let i = base();
    assert_eq!(decide(&i), decide(&i));
}

#[test]
fn c_structs_match_the_header_layout() {
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<Inputs>(), 40);
    assert_eq!(offset_of!(Inputs, local_set), 0);
    assert_eq!(offset_of!(Inputs, server_set), 1);
    assert_eq!(offset_of!(Inputs, has_pending), 2);
    assert_eq!(offset_of!(Inputs, wifi_ready), 3);
    assert_eq!(offset_of!(Inputs, pending_bytes), 4);
    assert_eq!(offset_of!(Inputs, pending_lines), 8);
    assert_eq!(offset_of!(Inputs, fail_streak), 12);
    assert_eq!(offset_of!(Inputs, last_fail_s), 24);
    assert_eq!(offset_of!(Inputs, now_s), 32);

    assert_eq!(size_of::<Decision>(), 8);
    assert_eq!(offset_of!(Decision, action), 0);
    assert_eq!(offset_of!(Decision, max_bytes), 4);
}
