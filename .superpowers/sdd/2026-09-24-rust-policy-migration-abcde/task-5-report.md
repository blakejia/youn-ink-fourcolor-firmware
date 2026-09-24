# Task 5 Report: Page Content/Version Comparison Policy

**Date**: 2026-09-25
**Status**: Complete (host tests + ESP-IDF build green; device behaviour not yet observed)
**Branch**: abcde-rust-migration
**Worktree**: /mnt/data/project/youn-ink-fourcolor-firmware/.worktrees/abcde-rust-migration

---

## 1. Survey of the existing page sync comparison points (done before coding)

### 1.1 Where comparisons live

All page-comparison decisions were already inside the Rust port
`firmware/main/rust/src/page_sync.rs` (production C++ call sites route
through the `page_sync_*` C ABI; the C++ side holds no page cache of its
own). The comparison points, with pre-Task-5 line numbers:

| # | Site | Decision (pre-Task 5) | Owner after Task 5 |
|---|------|------------------------|--------------------|
| 1 | `sync_schedule` (L446) | `have_schedule_md5 && schedule_md5 == new_md5` → unchanged-md5 fast path: position/wake answer + override release, `return true`, **no commit** (already committed) | **Rust `decide_schedule`** |
| 2 | `sync_schedule` rebuild (L518) | changed md5 / cold cache → rebuild table, md5-matched bitmap carry-over, commit `schedule_md5`, `return true` | **Rust `decide_schedule`** (`action==FETCH`, `commit=1`) |
| 3 | `sync_schedule` parse-fail (L447) | body unusable → keep old table, commit nothing, `return false` | Rust (`USE_CACHE`, `continue=0`; the early return stays at the call site as mechanism) |
| 4 | `sync_once` fetch-fail (L429) | non-200/timeout → keep old table, `sync_ok=false` | Rust (`fetch_ok=0 → USE_CACHE`) — same guard shape, now one policy |
| 5 | `prepare_paint` (L637) | resident → true; trusted record + index match + md5 match → skip fetch; else `ensure_bitmap` (GET) | **Rust `decide_page`** |
| 6 | `paint_if_changed` glass check (L681) | `magic_ok && valid && md5 == target` → skip repaint (md5-keyed, index-free) | **Rust `decide_page`** |
| 7 | `paint_if_changed` empty-table (L660) | `!LAST_SYNC_OK` → keep glass (no hint, no cycle); success + hint recorded (index −1) → no cycle; else draw hint | **Rust `decide_page`** |
| 8 | `ensure_bitmap` (L555) | slot non-null → use RAM; else HTTP GET | Mechanism (C++/Rust transport), driven by the policy's Fetch/UseCache |

**C++ consumers** (transport/storage only, all deliberately untouched):
`rf_panel_record_get/mark_pending/invalidate` (shim.cpp, RTC record bytes),
`http_client_wrapper` (GET), page storage in PSRAM, rotation/`current_index`
application, `rf_request_full_refresh` (EPD submit), `application.cc:838`
`rf_panel_record_invalidate()` on UI handoff. There is **no C++ page cache
class**; `photo_storage`/`storage_manager` are unrelated photo assets.

### 1.2 Design §6 obligations mapped to code

- "outputs `SkipSame`/`Fetch`/`UseCache`/`InvalidateCache` + continuation" →
  `ScheduleDecision{action, commit, continue_sync}` and
  `PageDecision{action, continue_paint}`; four action codes shared by both.
- "same-hash short-circuit consistent with existing invalidation rules; no
  evidence → do not change invalidation conditions" → trust gate is byte-for-
  byte the old predicate (magic AND valid, md5 equality), only relocated.
- "no EPD diff/partial/full refresh migration" → verified: zero EPD/refresh
  symbols in the diff; `blit_and_refresh`/`rf_request_full_refresh` untouched.
- "C++ keeps HTTP, JSON DOM, files, page storage, rotation, EPD" → Rust
  inputs are facts only (`fetch_ok`, `body_usable`, record bytes, residency);
  all I/O stays at the call sites.

---

## 2. Files changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/page_compare_policy.rs` | Pure policy: `decide_schedule`, `decide_page`, 4 action codes, C ABI `rf_page_compare_schedule`/`rf_page_compare_page`, 1 module test |
| `firmware/main/rust/include/page_compare_policy.h` | ABI header: `rf_page_compare_schedule_inputs_t` (72 B), `_schedule_decision_t` (4 B), `_page_inputs_t` (8 B), `_page_decision_t` (4 B), `RF_PAGE_COMPARE_*` codes |
| `firmware/main/rust/tests/page_compare_policy.rs` | 17 red-first tests: same-hash short-circuit, changed-hash fetch, cold cache, missing metadata, transport failure (incl. dominance), glass-match skip, resident UseCache (± trusted record), glass-mismatch fetch, expired-record InvalidateCache (incl. md5-coincidence), failed-sync fallback, hint skip/stale-glass, C ABI + layout tests |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | registered `pub mod page_compare_policy;` |
| `firmware/main/CMakeLists.txt` | added `page_compare_policy.rs` to `RUST_SOURCES` (rebuild dependency) |
| `firmware/main/rust/src/page_sync.rs` | production wiring: `sync_schedule` branch now `decide_schedule` (SkipSame → position-only update; Fetch → rebuild+commit); `prepare_paint` branches on `decide_page` (SkipSame/UseCache → no download); `paint_if_changed` glass check → `decide_page` SkipSame; `paint_if_changed` empty-table failure/hint branch → `decide_page` (UseCache keep-glass / SkipSame hint-recorded / InvalidateCache draw-hint). New `record_trusted`, `prepare_decision`, `paint_decision` fact-translators. Log lines stay at call sites (mechanism). |

---

## 3. ABI surface

```c
#define RF_PAGE_COMPARE_SKIP_SAME        0
#define RF_PAGE_COMPARE_FETCH            1
#define RF_PAGE_COMPARE_USE_CACHE        2
#define RF_PAGE_COMPARE_INVALIDATE_CACHE 3

typedef struct {                       /* 72 bytes */
    uint8_t  fetch_ok;                 /* 0  */
    uint8_t  body_usable;              /* 1  */
    uint8_t  has_cached;               /* 2  */
    uint8_t  _pad[5];                  /* 3  */
    uint8_t  cached_md5[32];           /* 8  */
    uint8_t  server_md5[32];           /* 40 */
} rf_page_compare_schedule_inputs_t;

typedef struct {                       /* 4 bytes */
    uint8_t  action;                   /* 0  */
    uint8_t  commit;                   /* 1  */
    uint8_t  continue_sync;            /* 2  */
    uint8_t  _pad;                     /* 3  */
} rf_page_compare_schedule_decision_t;

typedef struct {                       /* 8 bytes */
    uint8_t  bitmap_resident;          /* 0 */
    uint8_t  record_trusted;           /* 1  magic AND valid, folded by caller */
    uint8_t  glass_matches;            /* 2 */
    uint8_t  sync_ok;                  /* 3 */
    uint8_t  has_target;               /* 4 */
    uint8_t  _pad[3];                  /* 5 */
} rf_page_compare_page_inputs_t;

typedef struct {                       /* 4 bytes */
    uint8_t  action;                   /* 0 */
    uint8_t  continue_paint;           /* 1 */
    uint8_t  _pad[2];                  /* 2 */
} rf_page_compare_page_decision_t;

rf_page_compare_schedule_decision_t
    rf_page_compare_schedule(const rf_page_compare_schedule_inputs_t*);
rf_page_compare_page_decision_t
    rf_page_compare_page(const rf_page_compare_page_inputs_t*);
```

`#[repr(C)]`, fixed order, explicit padding; offsets/sizes asserted by
`c_structs_match_the_header_layout` (72/4/8/4).

### Decision ownership (production)

| Decision | Owner | Consumed at |
|----------|-------|-------------|
| schedule same-hash short-circuit | Rust `decide_schedule` | `page_sync::sync_schedule` |
| schedule changed-hash fetch + commit | Rust (`FETCH`,`commit=1`) | same |
| fetch-failure / missing metadata fallback | Rust (`USE_CACHE`,`continue=0`) | `sync_once`/`sync_schedule` guards |
| glass already shows target (skip fetch/paint) | Rust `decide_page` (`SKIP_SAME`) | `prepare_paint`, `paint_if_changed` |
| resident bitmap serves from cache | Rust (`USE_CACHE`,`continue=1`) | `prepare_paint` (fetch decision), `ensure_bitmap` still resolves the pointer |
| expired/missing record claim | Rust (`INVALIDATE_CACHE`) | `prepare_paint`/`paint_if_changed` fall-through to fetch/hint |
| failed-sync empty-table keep-glass | Rust (`USE_CACHE`,`continue=0`) | `paint_if_changed` empty branch |
| EPD diff/refresh, HTTP, JSON, files, rotation | C++ (untouched) | — |

---

## 4. TDD evidence

**RED** — test file written before the module existed:

```
error[E0432]: unresolved import `rust_firmware::page_compare_policy`
  --> tests/page_compare_policy.rs:25:20
error: could not compile `rust_firmware` (test "page_compare_policy")
```

**GREEN** — minimal implementation: 17/17 integration tests + 1 module test.

**SENTINEL** — each key decision mutated, expected failures observed, file
restored byte-identical (`cmp` after each):

| Mutation | Result |
|----------|--------|
| M1 fetch-failure guard removed (`fetch_ok/body_usable` check → `false`) | `transport_failure_keeps_the_cache`, `transport_failure_dominates_matching_hashes`, `missing_schedule_metadata_keeps_the_cache` FAILED (14/17) |
| M2 schedule short-circuit inverted (`==` → `!=`) | `same_schedule_hash_short_circuits`, `changed_schedule_hash_fetches_and_commits`, `cold_cache...` FAILED (14/17) |
| M3 record-trust gate removed (match alone → SkipSame) | `expired_record_never_skips_even_when_the_md5_matches` FAILED |
| M4 `sync_ok` gate removed from empty-table fallback | `empty_schedule_with_hint_recorded_skips`, `empty_schedule_with_stale_glass_invalidates` FAILED (15/17) |
| **M5 (production)** glass-match skip removed | **`page_sync::tests::a_glass_that_already_shows_the_target_page_fetches_nothing`, `prepare_paint_is_true_when_nothing_needs_downloading`, `skips_the_repaint_when_the_glass_already_shows_it` FAILED** — proves production consumes the Rust decision |
| **M6 (production)** empty-table failure fallback removed | **`page_sync::tests::failed_sync_with_a_page_on_the_glass_paints_nothing` FAILED** |
| **M7 (production)** schedule short-circuit removed | `page_compare_policy::tests::same_hash_short_circuits_without_commit` FAILED (305/306) |

**RESTORE** — `cmp` byte-identical after every mutation; full suite green.

---

## 5. Verification

| Gate | Command | Result |
|------|---------|--------|
| Full host suite | `cargo test` (`PATH=$HOME/.cargo/bin:$PATH`) | **PASS** — 362 tests: lib 306, battery 15, device_signature 4, notify 20, page_compare 17, doc 0 |
| ESP-IDF build | `cd firmware; source ~/data/esp-idf-v6.0/export.sh; IDF_TARGET=esp32s3 idf.py build` | **PASS** — "Project build complete", `xiaozhi.bin` 0x2c86c0 B (29% free); only pre-existing fatfs-Kconfig and GNU-stack notes |
| Symbols in archive | `xtensa-esp32s3-elf-nm librust_firmware.a` | `T rf_page_compare_page`, `T rf_page_compare_schedule` |
| EPD untouched | `git diff` grep for refresh/waveform/epd/rr=4 additions | zero hits (only out-of-scope doc lines in new files) |
| Whitespace | `git diff --check` | clean |

`rf_page_compare_*` symbols do not appear in the final ELF: the production
`page_sync` paths call the pure functions directly, so the linker drops the
unreferenced exports (same as Tasks 3/4 — the header declares them for any
future C++ caller).

Device behaviour: **not observed** (host tests + build only).

---

## 6. Behavioural deltas vs. pre-Task 5

**None observable.** Every branch is the old predicate relocated behind the
policy ABI, pinned by the existing production tests (`skips_the_repaint...`,
`an_unchanged_schedule_does_not_re_fetch...`, `failed_sync_with_a_page_on...`,
`a_record_with_wrong_magic_is_not_trusted`, empty-hint tests) — all green
without modification. One nuance: `prepare_paint`'s untrusted-record case now
classifies as `INVALIDATE_CACHE` (was: fall through to fetch) — same
behaviour, richer label. The old inline `record_magic_ok && valid` checks at
the two call sites were deleted (no duplicate truth).

## 7. Concerns / notes for review

1. **`record_trusted`/`glass_matches` are computed by the Rust caller**
   (`record_trusted()`, the two decision helpers), not C++. This matches the
   Task 3/4 pattern (page_sync is already the Rust production call path);
   the header documents the fields for a future C++ caller.
2. **The empty-table hint key stays index-based** (`record_index == -1`)
   folded into `glass_matches`, exactly as before — md5 can never match the
   hint (32 zero bytes stored as an empty string).
3. **`ensure_bitmap`'s pointer resolution remains mechanism** (transport +
   ownership), not comparison: it is reached only after the policy says
   Fetch/Invalidate.
4. **`commit`/`continue_sync` on the schedule decision** currently drive the
   `SkipSame` early-return vs rebuild; the `commit=0` path in the `USE_CACHE`
   branch coincides with the existing early `return false` before any commit
   — no double-commit window was introduced.
