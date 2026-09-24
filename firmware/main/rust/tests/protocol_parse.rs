//! Red-first integration tests for the Task 6 protocol parser migration
//! (ABCDE design §7).
//!
//! They pin the genuinely remaining parse seams against the observed
//! production behaviour of `page_sync.rs`:
//!
//! - schedule entry fields: an entry is usable only with a 32-byte md5 AND
//!   a present `duration_minutes`; the duration maps to
//!   `max(0) * 60` seconds (negative clamps, zero stays usable)
//! - policy minutes: positive minutes scale to seconds clamped at 1440 min,
//!   anything else falls back (mirrors `minutes_to_s`)
//! - HTTP status mapping: 200 ok / 204 empty / 404 not-found / 401+429
//!   rejected / negative transport-error / everything else error
//!
//! Deliberately NOT covered here (already Rust-owned, do not touch):
//! wifi endpoint parsing (`wifi_policy.rs`), pairing classification
//! (`pairing_response.rs`), notify response classification + body parsing
//! (`notify_policy.rs` / `notify.rs`), battery activity policy
//! (`battery_activity_policy.rs`). OTA manifest: no JSON manifest parsing
//! exists anywhere in the firmware (only a URL string passthrough), so
//! there is nothing to migrate and no test to write.
//!
//! TDD: this file is written BEFORE `protocol_parse.rs` exists, so the
//! first `cargo test` must fail (missing module). Minimal implementation
//! follows.

use rust_firmware::protocol_parse::*;

fn entry(md5_len: u32, has_duration: bool, duration_minutes: i64) -> ScheduleEntryDecision {
    decide_schedule_entry(&ScheduleEntryFacts {
        md5_len,
        has_duration: has_duration as u8,
        _pad: [0; 3],
        duration_minutes,
    })
}

// ── schedule entry fields ───────────────────────────────────────────────

#[test]
fn valid_entry_is_usable_with_minutes_as_seconds() {
    let d = entry(32, true, 10);
    assert_eq!(d.usable, 1);
    assert_eq!(d.duration_s, 600);
}

#[test]
fn entry_with_bad_md5_len_is_skipped() {
    // Mirrors `md5_from`: anything but 32 bytes drops the entry, never the
    // whole response.
    for len in [0, 1, 31, 33, 64] {
        assert_eq!(entry(len, true, 10).usable, 0, "md5_len={len}");
    }
}

#[test]
fn entry_with_missing_duration_is_skipped() {
    assert_eq!(entry(32, false, 0).usable, 0);
    assert_eq!(entry(32, false, 10).usable, 0);
}

#[test]
fn negative_duration_clamps_to_zero_but_stays_usable() {
    // Mirrors `dur_min.max(0) as u32 * 60`: the entry parses, the wait is 0.
    let d = entry(32, true, -5);
    assert_eq!(d.usable, 1);
    assert_eq!(d.duration_s, 0);
}

#[test]
fn zero_duration_is_usable_with_zero_seconds() {
    let d = entry(32, true, 0);
    assert_eq!(d.usable, 1);
    assert_eq!(d.duration_s, 0);
}

// ── policy minutes ──────────────────────────────────────────────────────

#[test]
fn policy_minutes_scale_and_clamp() {
    assert_eq!(policy_minutes_to_s(1, 10, 600), 600);
    // Clamped at 1440 min, mirroring MAX_POLL_MINUTES.
    assert_eq!(policy_minutes_to_s(1, 10_000, 600), 1440 * 60);
}

#[test]
fn policy_minutes_fall_back() {
    for (present, minutes) in [(0, 10), (1, 0), (1, -5)] {
        assert_eq!(
            policy_minutes_to_s(present, minutes, 600),
            600,
            "present={present} minutes={minutes}"
        );
    }
}

// ── HTTP status mapping ─────────────────────────────────────────────────

#[test]
fn http_status_maps_to_business_classes() {
    assert_eq!(classify_http_status(200), HTTP_CLASS_OK);
    assert_eq!(classify_http_status(204), HTTP_CLASS_EMPTY);
    assert_eq!(classify_http_status(404), HTTP_CLASS_NOT_FOUND);
    assert_eq!(classify_http_status(401), HTTP_CLASS_REJECTED);
    assert_eq!(classify_http_status(429), HTTP_CLASS_REJECTED);
    assert_eq!(classify_http_status(500), HTTP_CLASS_ERROR);
    assert_eq!(classify_http_status(301), HTTP_CLASS_ERROR);
}

#[test]
fn transport_errors_are_their_own_class() {
    // The HTTP wrapper returns -1 for network/connection failure; that must
    // never read as a server answer.
    assert_eq!(classify_http_status(-1), HTTP_CLASS_TRANSPORT_ERROR);
    assert_eq!(classify_http_status(-100), HTTP_CLASS_TRANSPORT_ERROR);
}

// ── C ABI ───────────────────────────────────────────────────────────────

#[test]
fn c_abi_matches_the_pure_decisions() {
    let d = unsafe {
        rf_protocol_schedule_entry(&ScheduleEntryFacts {
            md5_len: 32,
            has_duration: 1,
            _pad: [0; 3],
            duration_minutes: 10,
        })
    };
    assert_eq!(d.usable, 1);
    assert_eq!(d.duration_s, 600);

    assert_eq!(unsafe { rf_protocol_http_class(200) }, HTTP_CLASS_OK);
    assert_eq!(unsafe { rf_protocol_http_class(-1) }, HTTP_CLASS_TRANSPORT_ERROR);
    assert_eq!(unsafe { rf_protocol_policy_minutes_to_s(1, 10, 600) }, 600);
    assert_eq!(unsafe { rf_protocol_policy_minutes_to_s(0, 10, 600) }, 600);
}

#[test]
fn c_structs_match_the_header_layout() {
    // Layout contract with rust/include/protocol_parse.h: field order,
    // offsets and sizes must agree or the FFI reads garbage. Same style as
    // the page_compare / notify_policy / battery_activity_policy tests.
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<ScheduleEntryFacts>(), 16);
    assert_eq!(offset_of!(ScheduleEntryFacts, md5_len), 0);
    assert_eq!(offset_of!(ScheduleEntryFacts, has_duration), 4);
    assert_eq!(offset_of!(ScheduleEntryFacts, duration_minutes), 8);

    assert_eq!(size_of::<ScheduleEntryDecision>(), 8);
    assert_eq!(offset_of!(ScheduleEntryDecision, usable), 0);
    assert_eq!(offset_of!(ScheduleEntryDecision, duration_s), 4);
}
