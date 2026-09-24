//! Protocol field classification for the NOTE4C firmware (Task 6,
//! ABCDE design §7). Rust receives C++-extracted facts / short strings and
//! returns fixed parsed structs; it never touches a JSON DOM, HTTP/TLS, or
//! files.
//!
//! Scope is the genuinely remaining seams (see the survey in the Task 6
//! report): schedule entry field classification (`md5` length +
//! `duration_minutes` presence/value), policy `*_minutes` scaling, and HTTP
//! status → business-class mapping. Everything else the design lists is
//! already Rust-owned and deliberately untouched:
//!
//! - wifi endpoint parsing → `wifi_policy.rs`
//! - pairing response classification → `pairing_response.rs`
//! - notify response classification + body parsing → `notify_policy.rs` /
//!   `notify.rs`
//! - battery relative-activity policy → `battery_activity_policy.rs`
//! - OTA manifest → no JSON manifest parsing exists anywhere in the
//!   firmware (only a URL string passthrough), so there is nothing to move.
//!
//! Compatibility: entries without a usable md5/duration are skipped, never
//! fatal; minutes fall back instead of stopping polling; negative status is
//! a transport error, never a server answer.

/// md5 length a schedule entry must carry (32 hex chars).
pub const SCHEDULE_MD5_LEN: u32 = 32;

/// Upper bound for policy minutes (mirrors `page_sync::MAX_POLL_MINUTES`).
pub const MAX_POLL_MINUTES: i64 = 1440;

/// HTTP status → business classes (mirrors the production call sites:
/// `page_sync` treats only 200 as success, `notify_policy` adds the
/// 204-empty / 404-fallback splits, `pairing_response` adds 401/429).
pub const HTTP_CLASS_OK: u8 = 0;
pub const HTTP_CLASS_EMPTY: u8 = 1;
pub const HTTP_CLASS_NOT_FOUND: u8 = 2;
pub const HTTP_CLASS_REJECTED: u8 = 3;
pub const HTTP_CLASS_TRANSPORT_ERROR: u8 = 4;
pub const HTTP_CLASS_ERROR: u8 = 5;

/// Facts C++ extracted for one schedule `pages[]` entry: the md5 byte
/// length (32 = usable) plus whether `duration_minutes` was present and its
/// value. `#[repr(C)]` + explicit padding pins the layout against
/// `rf_protocol_schedule_entry_facts_t` in `rust/include/protocol_parse.h`;
/// a layout contract test asserts the offsets and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ScheduleEntryFacts {
    /// Byte length of the entry's md5 value.
    pub md5_len: u32,
    /// 1 = `duration_minutes` was present with an integer value.
    pub has_duration: u8,
    /// Padding to align `duration_minutes` on an 8-byte boundary.
    pub _pad: [u8; 3],
    /// The `duration_minutes` value (meaningful only if `has_duration`).
    pub duration_minutes: i64,
}

/// Entry decision. `usable == 0` means skip this entry, never fail the
/// whole response. `#[repr(C)]` against
/// `rf_protocol_schedule_entry_decision_t` (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ScheduleEntryDecision {
    /// 1 = the entry is usable (md5 well-formed AND duration present).
    pub usable: u8,
    /// Padding to align `duration_s`.
    pub _pad: [u8; 3],
    /// `duration_minutes.max(0) * 60` (meaningful only if `usable`).
    pub duration_s: u32,
}

/// Classify one schedule entry. Pure: no I/O, no globals.
///
/// Mirrors `page_sync::parse_schedule` exactly: an entry is usable only
/// with a 32-byte md5 AND a present `duration_minutes`; the duration maps
/// to `max(0) * 60` seconds.
pub fn decide_schedule_entry(f: &ScheduleEntryFacts) -> ScheduleEntryDecision {
    if f.md5_len != SCHEDULE_MD5_LEN || f.has_duration == 0 {
        return ScheduleEntryDecision { usable: 0, _pad: [0; 3], duration_s: 0 };
    }
    ScheduleEntryDecision {
        usable: 1,
        _pad: [0; 3],
        duration_s: f.duration_minutes.max(0) as u32 * 60,
    }
}

/// Scale a policy `*_minutes` value to seconds. Pure.
///
/// Mirrors `page_sync::minutes_to_s` exactly: positive minutes scale and
/// clamp at 1440 min; anything else (missing/non-positive) falls back so a
/// server that stops sending policy never stops the device polling.
pub fn policy_minutes_to_s(present: u8, minutes: i64, fallback_s: u32) -> u32 {
    if present != 0 && minutes > 0 {
        (minutes.min(MAX_POLL_MINUTES) as u32) * 60
    } else {
        fallback_s
    }
}

/// Map an HTTP status to a business class. Pure.
///
/// 200 = ok, 204 = empty, 404 = not-found (old-server fallback signal),
/// 401/429 = rejected, negative = transport failure, everything else =
/// error. This is the shared vocabulary; each caller keeps its own
/// response to the class (the notify 404→JSON-fallback split stays in
/// `notify_policy`, the 200-only schedule gate stays in `page_sync`).
pub fn classify_http_status(status: i32) -> u8 {
    if status < 0 {
        return HTTP_CLASS_TRANSPORT_ERROR;
    }
    match status {
        200 => HTTP_CLASS_OK,
        204 => HTTP_CLASS_EMPTY,
        404 => HTTP_CLASS_NOT_FOUND,
        401 | 429 => HTTP_CLASS_REJECTED,
        _ => HTTP_CLASS_ERROR,
    }
}

/// Classify one schedule entry (C ABI).
#[unsafe(no_mangle)]
pub extern "C" fn rf_protocol_schedule_entry(f: *const ScheduleEntryFacts) -> ScheduleEntryDecision {
    // SAFETY: the caller passes a valid, correctly aligned struct.
    decide_schedule_entry(unsafe { &*f })
}

/// Scale policy minutes (C ABI). `present` is a 0/1 flag.
#[unsafe(no_mangle)]
pub extern "C" fn rf_protocol_policy_minutes_to_s(present: u8, minutes: i64, fallback_s: u32) -> u32 {
    policy_minutes_to_s(present, minutes, fallback_s)
}

/// Map an HTTP status (C ABI).
#[unsafe(no_mangle)]
pub extern "C" fn rf_protocol_http_class(status: i32) -> u8 {
    classify_http_status(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_entries_without_md5_or_duration() {
        let no_md5 = decide_schedule_entry(&ScheduleEntryFacts {
            md5_len: 0,
            has_duration: 1,
            _pad: [0; 3],
            duration_minutes: 10,
        });
        assert_eq!(no_md5.usable, 0);
        let no_dur = decide_schedule_entry(&ScheduleEntryFacts {
            md5_len: 32,
            has_duration: 0,
            _pad: [0; 3],
            duration_minutes: 10,
        });
        assert_eq!(no_dur.usable, 0);
    }
}
