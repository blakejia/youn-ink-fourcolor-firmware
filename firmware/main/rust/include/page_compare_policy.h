/**
 * @file page_compare_policy.h
 * @brief Page content/version comparison inputs and outputs (decisions live
 *         in Rust, `page_compare_policy.rs`).
 *
 * C++ (and `page_sync`) own HTTP transport, the JSON DOM, file reads/writes,
 * page storage, rotation, the RTC panel record and the EPD. This header is
 * the ABI by which a caller asks Rust whether a schedule/page is unchanged,
 * must be fetched, is served from cache, or must have its claim invalidated,
 * plus whether the sync/paint path should continue.
 *
 * The decision table is a compatibility port of the comparison points in
 * `page_sync.rs` — not a redesign:
 *
 *   - schedule level mirrors `sync_schedule`: fetch failure or an unusable
 *     body keeps the old table and commits nothing; an unchanged
 *     schedule_md5 short-circuits to a position-only update; a changed md5
 *     (or a cold cache) rebuilds and commits.
 *   - page level mirrors `prepare_paint` + `paint_if_changed` +
 *     `ensure_bitmap`: a trusted panel record (magic AND valid) whose md5
 *     matches the target skips; a resident bitmap serves from RAM even when
 *     the record claim expired; otherwise the bitmap is fetched; an
 *     unusable record claim invalidates, never SkipSame; an empty table
 *     after a FAILED sync keeps the glass (no hint, no cycle).
 *
 * Out of scope by design: EPD diff, partial/full refresh qualification,
 * bitmap bytes, rotation. `rr=4` stays unresolved.
 */
#ifndef PAGE_COMPARE_POLICY_H
#define PAGE_COMPARE_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Comparison actions (`rf_page_compare_*_decision_t.action`). */
#define RF_PAGE_COMPARE_SKIP_SAME        0 /**< unchanged: skip, nothing to commit/fetch */
#define RF_PAGE_COMPARE_FETCH            1 /**< changed/missing: fetch + commit/proceed */
#define RF_PAGE_COMPARE_USE_CACHE        2 /**< keep the old table/bitmap/glass */
#define RF_PAGE_COMPARE_INVALIDATE_CACHE 3 /**< drop the stale claim, fetch fresh */

/**
 * Facts for one schedule comparison.
 *
 * Layout contract with `page_compare_policy.rs` (`#[repr(C)]` + a Rust
 * test asserting offsets/size): `uint8_t` fetch_ok at 0, body_usable at 1,
 * has_cached at 2, 5 pad bytes, `uint8_t cached_md5[32]` at 8,
 * `uint8_t server_md5[32]` at 40, 72 bytes total. Keep field order, types
 * and count in step with the Rust struct.
 */
typedef struct {
    /** 1 = the schedule GET returned 200 with a body. 0 dominates. */
    uint8_t  fetch_ok;
    /** 1 = the body parsed to a usable schedule_md5 (+ pages). */
    uint8_t  body_usable;
    /** 1 = cached_md5 holds a previously committed schedule. */
    uint8_t  has_cached;
    /** Padding to align `cached_md5` on an 8-byte boundary. */
    uint8_t  _pad[5];
    /** Previously committed schedule md5. */
    uint8_t  cached_md5[32];
    /** Server schedule md5 from the fresh body. */
    uint8_t  server_md5[32];
} rf_page_compare_schedule_inputs_t;

/**
 * Schedule decision.
 *
 * Layout contract: `uint8_t` action at 0, commit at 1, continue_sync at 2,
 * 1 pad byte, 4 bytes total. Rust test asserts the offsets.
 *
 * `commit` mirrors the schedule_md5/have_schedule_md5 write;
 * `continue_sync` mirrors `sync_schedule`'s `return true` (position/table
 * update applied) vs `return false`.
 */
typedef struct {
    /** RF_PAGE_COMPARE_* action. */
    uint8_t  action;
    /** 1 = commit server_md5 as the new cached schedule. */
    uint8_t  commit;
    /** 1 = apply the position/table update and keep going. */
    uint8_t  continue_sync;
    uint8_t  _pad;
} rf_page_compare_schedule_decision_t;

/**
 * Facts for one page comparison. `record_trusted` already folds the
 * magic-AND-valid gate; `glass_matches` is the recorded-md5 == target-md5
 * equality the caller computed.
 *
 * Layout contract: `uint8_t` bitmap_resident at 0, record_trusted at 1,
 * glass_matches at 2, sync_ok at 3, has_target at 4, 3 pad bytes, 8 bytes
 * total. Rust test asserts the offsets.
 */
typedef struct {
    /** 1 = the target page's bitmap is already in RAM. */
    uint8_t  bitmap_resident;
    /** 1 = the panel record claim is usable (magic ok AND valid set). */
    uint8_t  record_trusted;
    /** 1 = the recorded md5 equals the target page's md5. */
    uint8_t  glass_matches;
    /** 1 = the last sync_once succeeded (gates the empty-table path). */
    uint8_t  sync_ok;
    /** 1 = a target page exists (non-empty schedule / manual index). */
    uint8_t  has_target;
    uint8_t  _pad[3];
} rf_page_compare_page_inputs_t;

/**
 * Page decision.
 *
 * Layout contract: `uint8_t` action at 0, continue_paint at 1, 2 pad
 * bytes, 4 bytes total. Rust test asserts the offsets.
 */
typedef struct {
    /** RF_PAGE_COMPARE_* action. */
    uint8_t  action;
    /** 1 = fetch (when missing) then paint; 0 = leave the glass alone. */
    uint8_t  continue_paint;
    uint8_t  _pad[2];
} rf_page_compare_page_decision_t;

/** Schedule-level comparison (Rust, pure). */
rf_page_compare_schedule_decision_t rf_page_compare_schedule(
    const rf_page_compare_schedule_inputs_t* inp);

/** Page-level comparison (Rust, pure). */
rf_page_compare_page_decision_t rf_page_compare_page(
    const rf_page_compare_page_inputs_t* inp);

#ifdef __cplusplus
}
#endif

#endif  /* PAGE_COMPARE_POLICY_H */
