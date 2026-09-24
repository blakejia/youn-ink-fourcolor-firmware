//! Red-first integration tests for the Task 5 page content/version comparison
//! policy.
//!
//! They pin the migrated decision table against the observed production
//! behaviour of `page_sync.rs` (ABCDE design §6):
//!
//! - same hash short-circuit: cached schedule_md5 == server schedule_md5 ->
//!   SkipSame (position answer still applies, nothing rebuilt); glass md5 ==
//!   target md5 -> SkipSame (no repaint, no fetch)
//! - changed hash fetch: schedule md5 differs (or no cached schedule yet) ->
//!   Fetch + commit; glass differs -> Fetch
//! - missing metadata: body without a usable schedule_md5/pages ->
//!   UseCache (keep the old table, commit nothing, no continue)
//! - cache expiry: panel-record claim unusable (bad magic / valid == 0,
//!   e.g. after the UI handoff invalidated it) -> InvalidateCache, never
//!   SkipSame — even when the md5 bytes happen to match
//! - failure fallback: transport failure (fetch_ok == 0) -> UseCache (keep
//!   table AND glass, retry next wake); empty table after a failed sync ->
//!   UseCache with no continue (keep the glass, no hint, no cycle)
//!
//! TDD: this file is written BEFORE `page_compare_policy.rs` exists, so the
//! first `cargo test` must fail (missing module). Minimal implementation
//! follows.

use rust_firmware::page_compare_policy::*;

fn md5(tag: u8) -> [u8; 32] {
    [tag; 32]
}

fn schedule(
    fetch_ok: bool,
    usable: bool,
    has_cached: bool,
    cached_tag: u8,
    server_tag: u8,
) -> ScheduleDecision {
    decide_schedule(&ScheduleInputs {
        fetch_ok: fetch_ok as u8,
        body_usable: usable as u8,
        has_cached: has_cached as u8,
        _pad: [0; 5],
        cached_md5: md5(cached_tag),
        server_md5: md5(server_tag),
    })
}

fn page(
    resident: bool,
    trusted: bool,
    matches: bool,
    sync_ok: bool,
    has_target: bool,
) -> PageDecision {
    decide_page(&PageInputs {
        bitmap_resident: resident as u8,
        record_trusted: trusted as u8,
        glass_matches: matches as u8,
        sync_ok: sync_ok as u8,
        has_target: has_target as u8,
        _pad: [0; 3],
    })
}

// ── schedule level ────────────────────────────────────────────────────────

#[test]
fn same_schedule_hash_short_circuits() {
    // The 99% path: schedule unchanged — no rebuild, no commit (already
    // committed), but the position answer still applies (continue).
    let d = schedule(true, true, true, 0x11, 0x11);
    assert_eq!(d.action, COMPARE_SKIP_SAME);
    assert_eq!(d.commit, 0);
    assert_eq!(d.continue_sync, 1);
}

#[test]
fn changed_schedule_hash_fetches_and_commits() {
    let d = schedule(true, true, true, 0x11, 0x22);
    assert_eq!(d.action, COMPARE_FETCH);
    assert_eq!(d.commit, 1);
    assert_eq!(d.continue_sync, 1);
}

#[test]
fn cold_cache_with_usable_body_fetches() {
    // First sync ever (have_schedule_md5 == false): nothing to compare
    // against — an empty/expired cache fetches fresh and commits.
    let d = schedule(true, true, false, 0x00, 0x11);
    assert_eq!(d.action, COMPARE_FETCH);
    assert_eq!(d.commit, 1);
    assert_eq!(d.continue_sync, 1);
}

#[test]
fn missing_schedule_metadata_keeps_the_cache() {
    // Body without a usable schedule_md5/pages: keep the old table, commit
    // nothing, do not continue — exactly today's early `return false`.
    let d = schedule(true, false, true, 0x11, 0x00);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.commit, 0);
    assert_eq!(d.continue_sync, 0);
}

#[test]
fn transport_failure_keeps_the_cache() {
    // Non-200/timeout: the previous table survives, nothing commits.
    let d = schedule(false, false, true, 0x11, 0x00);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.commit, 0);
    assert_eq!(d.continue_sync, 0);
}

#[test]
fn transport_failure_dominates_matching_hashes() {
    // fetch_ok is checked first: no body means no comparison, even when the
    // (stale) facts happen to agree.
    let d = schedule(false, true, true, 0x11, 0x11);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.commit, 0);
    assert_eq!(d.continue_sync, 0);
}

// ── page level ────────────────────────────────────────────────────────────

#[test]
fn glass_match_skips_same_without_fetch_or_paint() {
    // Glass already shows the target page: no bitmap GET, no panel cycle —
    // whether or not the bitmap is also resident.
    for resident in [false, true] {
        let d = page(resident, true, true, true, true);
        assert_eq!(d.action, COMPARE_SKIP_SAME, "resident={resident}");
        assert_eq!(d.continue_paint, 0, "resident={resident}");
    }
}

#[test]
fn resident_bitmap_serves_from_cache() {
    // Slot in RAM, glass differs: blit from cache, no GET.
    let d = page(true, true, false, true, true);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.continue_paint, 1);
}

#[test]
fn resident_bitmap_serves_even_when_the_record_expired() {
    // RAM truth beats a stale claim: the bitmap is provably the target
    // page's (keyed by md5 at carry time), so an expired record must not
    // force a re-download.
    let d = page(true, false, false, true, true);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.continue_paint, 1);
}

#[test]
fn glass_mismatch_fetches() {
    let d = page(false, true, false, true, true);
    assert_eq!(d.action, COMPARE_FETCH);
    assert_eq!(d.continue_paint, 1);
}

#[test]
fn expired_record_invalidates_the_cache() {
    // Bad magic / valid == 0 (e.g. after the UI handoff invalidated the
    // claim): the glass contents are unknown — drop the claim and fetch
    // fresh, never SkipSame.
    let d = page(false, false, false, true, true);
    assert_eq!(d.action, COMPARE_INVALIDATE_CACHE);
    assert_eq!(d.continue_paint, 1);
}

#[test]
fn expired_record_never_skips_even_when_the_md5_matches() {
    // The md5 bytes can match by coincidence (or staleness) while the claim
    // itself is unusable. Trust requires magic AND valid AND match.
    let d = page(false, false, true, true, true);
    assert_eq!(d.action, COMPARE_INVALIDATE_CACHE);
    assert_eq!(d.continue_paint, 1);
}

#[test]
fn failed_sync_with_no_target_keeps_the_glass() {
    // Empty table after a FAILED sync means "schedule unknown", not "no
    // pages": no hint, no cycle, the glass keeps the last good page.
    let d = page(false, false, false, false, false);
    assert_eq!(d.action, COMPARE_USE_CACHE);
    assert_eq!(d.continue_paint, 0);
}

#[test]
fn empty_schedule_with_hint_recorded_skips() {
    // Successful empty sync, hint already recorded as index -1: the glass
    // already shows what the canvas chose — no second panel cycle.
    let d = page(false, true, true, true, false);
    assert_eq!(d.action, COMPARE_SKIP_SAME);
    assert_eq!(d.continue_paint, 0);
}

#[test]
fn empty_schedule_with_stale_glass_invalidates() {
    // Server says "no pages" but the glass still shows an old page (or an
    // untrusted record): the old content is no longer authoritative —
    // replace it with the hint.
    let d = page(false, true, false, true, false);
    assert_eq!(d.action, COMPARE_INVALIDATE_CACHE);
    assert_eq!(d.continue_paint, 1);
}

// ── C ABI ─────────────────────────────────────────────────────────────────

#[test]
fn c_abi_returns_the_same_decisions() {
    let s = unsafe {
        rf_page_compare_schedule(&ScheduleInputs {
            fetch_ok: 1,
            body_usable: 1,
            has_cached: 1,
            _pad: [0; 5],
            cached_md5: md5(0x11),
            server_md5: md5(0x11),
        })
    };
    assert_eq!(s.action, COMPARE_SKIP_SAME);
    assert_eq!(s.commit, 0);
    assert_eq!(s.continue_sync, 1);

    let p = unsafe {
        rf_page_compare_page(&PageInputs {
            bitmap_resident: 0,
            record_trusted: 0,
            glass_matches: 0,
            sync_ok: 1,
            has_target: 1,
            _pad: [0; 3],
        })
    };
    assert_eq!(p.action, COMPARE_INVALIDATE_CACHE);
    assert_eq!(p.continue_paint, 1);
}

#[test]
fn c_structs_match_the_header_layout() {
    // Layout contract with rust/include/page_compare_policy.h: field order,
    // offsets and sizes must agree or the FFI reads garbage. Same style as
    // the notify_policy / battery_activity_policy layout tests.
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<ScheduleInputs>(), 72);
    assert_eq!(offset_of!(ScheduleInputs, fetch_ok), 0);
    assert_eq!(offset_of!(ScheduleInputs, body_usable), 1);
    assert_eq!(offset_of!(ScheduleInputs, has_cached), 2);
    assert_eq!(offset_of!(ScheduleInputs, cached_md5), 8);
    assert_eq!(offset_of!(ScheduleInputs, server_md5), 40);

    assert_eq!(size_of::<ScheduleDecision>(), 4);
    assert_eq!(offset_of!(ScheduleDecision, action), 0);
    assert_eq!(offset_of!(ScheduleDecision, commit), 1);
    assert_eq!(offset_of!(ScheduleDecision, continue_sync), 2);

    assert_eq!(size_of::<PageInputs>(), 8);
    assert_eq!(offset_of!(PageInputs, bitmap_resident), 0);
    assert_eq!(offset_of!(PageInputs, record_trusted), 1);
    assert_eq!(offset_of!(PageInputs, glass_matches), 2);
    assert_eq!(offset_of!(PageInputs, sync_ok), 3);
    assert_eq!(offset_of!(PageInputs, has_target), 4);

    assert_eq!(size_of::<PageDecision>(), 4);
    assert_eq!(offset_of!(PageDecision, action), 0);
    assert_eq!(offset_of!(PageDecision, continue_paint), 1);
}
