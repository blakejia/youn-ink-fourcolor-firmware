//! Page content/version comparison policy for the NOTE4C firmware.
//!
//! C++ (and the `page_sync` module) own HTTP transport, the JSON DOM, file
//! reads/writes, page storage, rotation, the RTC panel record and the EPD;
//! this module only decides — from facts the caller already holds — whether
//! a schedule/page is unchanged, must be fetched, is served from cache, or
//! must have its claim invalidated, plus whether the sync/paint path should
//! continue.
//!
//! Compatibility port of the comparison points in `page_sync.rs` — not a
//! redesign (ABCDE design §6):
//!
//! - schedule level mirrors `sync_schedule`: fetch failure or an unusable
//!   body keeps the old table and commits nothing; an unchanged schedule_md5
//!   short-circuits to a position-only update; a changed md5 (or a cold
//!   cache) rebuilds and commits. `commit`/`continue_sync` mirror the
//!   `have_schedule_md5` write / `return true` vs `return false`.
//! - page level mirrors `prepare_paint` + `paint_if_changed` +
//!   `ensure_bitmap`: a trusted panel record whose md5 matches the target
//!   skips (no fetch, no cycle); a resident bitmap serves from RAM even when
//!   the record claim expired; otherwise the bitmap is fetched; an
//!   unusable record claim invalidates (fetch fresh, never SkipSame — the
//!   md5 bytes can match by coincidence); an empty table after a FAILED
//!   sync keeps the glass (no hint, no cycle).
//!
//! Out of scope by design: EPD diff, partial/full refresh qualification,
//! bitmap bytes, rotation. `rr=4` stays unresolved.

// ── schedule-level actions (`rf_page_compare_action_t`) ───────────────────

/// Schedule unchanged (or nothing usable): skip the rebuild.
pub const COMPARE_SKIP_SAME: u8 = 0;
/// Schedule changed (or cold cache): fetch + commit.
pub const COMPARE_FETCH: u8 = 1;
/// Keep the old table (fetch failure / unusable body).
pub const COMPARE_USE_CACHE: u8 = 2;
/// The cached claim is unusable: drop it, fetch fresh, never SkipSame.
pub const COMPARE_INVALIDATE_CACHE: u8 = 3;

// ── C ABI ─────────────────────────────────────────────────────────────────
// `#[repr(C)]` + explicit padding pins the layout against
// `rf_page_compare_*_t` in `rust/include/page_compare_policy.h`; a layout
// contract test asserts the offsets and sizes.

/// Facts for one schedule comparison. The caller fills this from the
/// transport outcome, the parse outcome and the cached schedule md5: Rust
/// never touches HTTP or the JSON DOM itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ScheduleInputs {
    /// 1 = the schedule GET returned 200 with a body (a comparison is
    /// possible at all). 0 dominates every other fact.
    pub fetch_ok: u8,
    /// 1 = the body parsed to a usable schedule_md5 (+ pages).
    pub body_usable: u8,
    /// 1 = `cached_md5` holds a previously committed schedule.
    pub has_cached: u8,
    /// Padding to align `cached_md5` on an 8-byte boundary.
    pub _pad: [u8; 5],
    /// Previously committed schedule md5 (meaningful only if `has_cached`).
    pub cached_md5: [u8; 32],
    /// Server schedule md5 from the fresh body (meaningful only if
    /// `body_usable`).
    pub server_md5: [u8; 32],
}

/// Schedule decision. `commit` mirrors the `schedule_md5` /
/// `have_schedule_md5` write; `continue_sync` mirrors `sync_schedule`'s
/// `return true` (position/table update applied) vs `return false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ScheduleDecision {
    /// `COMPARE_*` action.
    pub action: u8,
    /// 1 = commit `server_md5` as the new cached schedule.
    pub commit: u8,
    /// 1 = apply the position/table update and keep going.
    pub continue_sync: u8,
    /// Padding to a 4-byte size.
    pub _pad: u8,
}

/// Facts for one page comparison. `record_trusted` already folds the
/// magic-AND-valid gate (an all-zero power-on record is never trusted);
/// `glass_matches` is the md5 equality the caller computed; Rust never
/// reads the bitmap, the framebuffer or the RTC record itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct PageInputs {
    /// 1 = the target page's bitmap is already in RAM.
    pub bitmap_resident: u8,
    /// 1 = the panel record claim is usable (magic ok AND valid bit set).
    pub record_trusted: u8,
    /// 1 = the recorded md5 equals the target page's md5.
    pub glass_matches: u8,
    /// 1 = the last `sync_once` succeeded (gates the empty-table path).
    pub sync_ok: u8,
    /// 1 = a target page exists (non-empty schedule / manual index valid).
    pub has_target: u8,
    /// Padding to an 8-byte size.
    pub _pad: [u8; 3],
}

/// Page decision. `continue_paint` mirrors "proceed to fetch/blit" (the
/// paint path's job) vs "leave the glass alone".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct PageDecision {
    /// `COMPARE_*` action.
    pub action: u8,
    /// 1 = fetch (when missing) then paint; 0 = glass stays as is.
    pub continue_paint: u8,
    /// Padding to a 4-byte size.
    pub _pad: [u8; 2],
}

/// Schedule-level comparison. Pure: no I/O, no globals.
pub fn decide_schedule(i: &ScheduleInputs) -> ScheduleDecision {
    let out = |action: u8, commit: u8, continue_sync: u8| ScheduleDecision {
        action,
        commit,
        continue_sync,
        _pad: 0,
    };
    // No body, no comparison: the previous table survives untouched —
    // exactly today's fetch-failure path.
    if i.fetch_ok == 0 || i.body_usable == 0 {
        return out(COMPARE_USE_CACHE, 0, 0);
    }
    // 99% path: unchanged schedule — position-only update, nothing to
    // commit (already committed), but the answer still applies.
    if i.has_cached != 0 && i.cached_md5 == i.server_md5 {
        return out(COMPARE_SKIP_SAME, 0, 1);
    }
    // Changed md5, or a cold cache with nothing to compare against.
    out(COMPARE_FETCH, 1, 1)
}

/// Page-level comparison. Pure: no I/O, no globals.
pub fn decide_page(i: &PageInputs) -> PageDecision {
    let out = |action: u8, continue_paint: u8| PageDecision {
        action,
        continue_paint,
        _pad: [0; 2],
    };
    // Empty table after a FAILED sync means "schedule unknown", not "the
    // server says there are no pages": keep the glass, no hint, no cycle.
    if i.has_target == 0 && i.sync_ok == 0 {
        return out(COMPARE_USE_CACHE, 0);
    }
    // shows it — no fetch, no panel cycle. Trust requires magic AND valid
    // AND match (the caller folds the first two into `record_trusted`), so
    // coincidence/stale bytes can never short-circuit here. With an empty
    // schedule this is the recorded hint (index -1): no second cycle.
    if i.record_trusted != 0 && i.glass_matches != 0 {
        return out(COMPARE_SKIP_SAME, 0);
    }
    // Successful empty sync but the glass still shows an old page (or an
    // untrusted record): the old content is no longer authoritative — drop
    // the claim and draw the hint. Nothing is downloaded (there is no page
    // to GET), so this is InvalidateCache, not Fetch.
    if i.has_target == 0 {
        return out(COMPARE_INVALIDATE_CACHE, 1);
    }
    // RAM truth beats a stale claim: the bitmap is provably the target
    // page's, so serve it without a GET even when the record expired.
    if i.bitmap_resident != 0 {
        return out(COMPARE_USE_CACHE, 1);
    }
    // No trusted match and nothing in RAM: an unusable claim invalidates
    // (drop it, fetch fresh); otherwise fetch the missing bitmap.
    if i.record_trusted == 0 {
        return out(COMPARE_INVALIDATE_CACHE, 1);
    }
    out(COMPARE_FETCH, 1)
}

/// # Safety
/// `inp` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_page_compare_schedule(inp: *const ScheduleInputs) -> ScheduleDecision {
    decide_schedule(unsafe { &*inp })
}

/// # Safety
/// `inp` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_page_compare_page(inp: *const PageInputs) -> PageDecision {
    decide_page(unsafe { &*inp })
}

// ─── Tests ────────────────────────────────────────────────────────────────
// Red-first coverage lives in `tests/page_compare_policy.rs`; the
// layout/ABI contract is asserted there too (same style as notify_policy /
// battery_activity_policy).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_hash_short_circuits_without_commit() {
        let d = decide_schedule(&ScheduleInputs {
            fetch_ok: 1,
            body_usable: 1,
            has_cached: 1,
            _pad: [0; 5],
            cached_md5: [0x11; 32],
            server_md5: [0x11; 32],
        });
        assert_eq!(d.action, COMPARE_SKIP_SAME);
        assert_eq!((d.commit, d.continue_sync), (0, 1));
    }
}
