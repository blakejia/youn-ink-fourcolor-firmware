# Task 3 Report: Notification Pull/Rate/Consume Policy

**Date**: 2026-09-25
**Status**: Complete (host tests + ESP-IDF build green; device behaviour not yet observed)
**Branch**: abcde-rust-migration
**Worktree**: /mnt/data/project/youn-ink-fourcolor-firmware/.worktrees/abcde-rust-migration

---

## Files Changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/notify_policy.rs` | Pure Rust pull gate / response classification / consume decision / backoff ladder (20 host tests) |
| `firmware/main/rust/include/notify_policy.h` | C ABI header: `rf_notify_pull_inputs_t`, `rf_notify_pull_output_t`, `rf_notify_response_facts_t`, action/class constants, gate-stats storage ABI |
| `firmware/main/rust/tests/notify_policy.rs` | Integration tests: idle pull, EPD-busy defer, duplicate skip, success consume, failure backoff, JSON/.bin classification, C ABI + layout contract |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | Registered `pub mod notify_policy;` |
| `firmware/main/CMakeLists.txt` | Added `notify_policy.rs` to `RUST_SOURCES` (edit triggers rebuild) |
| `firmware/main/rust/src/notify.rs` | Fetch paths now classify/consume through the policy; duplicate-id dedup (`LAST_ID`, survives dismiss); outcome recording; new `notify_state()` C ABI; 2 new unit tests |
| `firmware/main/rust/src/shim.rs` | Externs + host fakes for `rf_time_now_s`, `rf_notify_gate_stats`, `rf_notify_gate_record` |
| `firmware/main/rust/shim.cpp` | RTC-backed gate stamps (`last_pull`, `last_failure`, streak) + the three shim functions |
| `firmware/main/rust/include/notify.h` | Declares `notify_state()` (+ `<stdint.h>`) |
| `firmware/main/application.cc` | Both pull call sites (`RouteInput` BOOT fetch, `RunPowerCycle` pending pull) now fill facts, call `rf_notify_pull_decide`, and pull only on `PULL`; gate constants |
| `firmware/main/common/sleep_manager.h/.cc` | Read-only `Busy(SleepBusySrc)` peek — raw busy vote without the lifecycle/deadline folding of `CanSleepNow()` (mechanism only, no EPD policy moved) |

---

## ABI Surface

```c
#define RF_NOTIFY_STATE_IDLE/FETCHING/NOTIFYING          0/1/2
#define RF_NOTIFY_PULL_ACTION_PULL/DEFER_BUSY/SKIP_NOT_IDLE/BACKOFF   0/1/2/3
#define RF_NOTIFY_RESPONSE_BINARY_NOTIFY/JSON_NOTIFY/EMPTY/JSON_FALLBACK/
             TRANSPORT_ERROR/STATUS_ERROR                0..5
#define RF_NOTIFY_CONSUME_SHOW/SKIP_DUPLICATE/RETRY_BACKOFF/
             NONE_EMPTY/FETCH_JSON_FALLBACK              0..4

typedef struct {            /* 48 bytes */
    uint8_t  state;         /* 0  */
    uint8_t  epd_busy;      /* 1  */
    uint8_t  _pad[6];       /* 2  */
    int64_t  now_s;         /* 8  */
    int64_t  last_pull_s;   /* 16 */
    int64_t  last_failure_s;/* 24 */
    uint32_t fail_streak;   /* 32 */
    uint32_t min_interval_s;/* 36 */
    uint32_t base_backoff_s;/* 40 */
    uint32_t max_backoff_s; /* 44 */
} rf_notify_pull_inputs_t;

typedef struct {            /* 8 bytes */
    uint8_t  action; uint8_t _pad[3]; uint32_t wait_s;
} rf_notify_pull_output_t;

typedef struct {            /* 8 bytes */
    int32_t status; uint8_t binary_path; uint8_t _pad[3];
} rf_notify_response_facts_t;

void     rf_notify_pull_decide(const rf_notify_pull_inputs_t*, rf_notify_pull_output_t*);
uint8_t  rf_notify_classify_response(const rf_notify_response_facts_t*);
uint8_t  rf_notify_decide_consume(uint8_t response_class, uint8_t parsed, uint8_t duplicate);
uint32_t rf_notify_backoff_delay_s(uint32_t streak, uint32_t base_s, uint32_t max_s);
uint32_t rf_notify_record_result(uint8_t ok, uint32_t streak);
void     rf_notify_gate_stats(int64_t* last_pull_s, int64_t* last_failure_s, uint32_t* streak);
void     rf_notify_gate_record(int64_t now_s, uint8_t failed, uint32_t streak);
```

`#[repr(C)]` POD, fixed field order, explicit padding; offsets and sizes asserted by
the layout test (same convention as `pairing_response`). Gate stamps/streak live in
RTC slow memory in `shim.cpp` (RAM clears on every duty-cycle wake — same reason as
`g_fail_streak`); Rust computes every transition, C++ only stores bytes.

---

## Decisions Migrated to Rust

| Decision | Old owner | New owner | Consumer site |
|----------|-----------|-----------|---------------|
| Pull gate: IDLE-only, EPD-busy defer, min-interval rate limit, failure backoff ladder (`base*2^(streak-1)` capped) | `notify.rs::request_next` state guard only (no rate/backoff) | `notify_policy::decide_pull` | `application.cc` `RouteInput` (BOOT fetch) and `RunPowerCycle` (pending pull) — both now gate `notify_request_next()` |
| Response classification: 200 `.bin`/200 JSON/204/404→JSON-fallback/transport/status error | `fetch_once` + `handle_next` inline `match status` | `notify_policy::classify_response` | `notify.rs::fetch_once` (binary path) and `handle_next` (JSON path) |
| Consume: show fresh parsed body, skip duplicate id/etag, retry unparseable with backoff, empty→no-op, fallback→GET JSON | ad-hoc `match` arms (dedup did not exist) | `notify_policy::decide_consume` | `notify.rs::consume_parsed` (both wire formats) |
| Failure streak step (success resets, failure increments) | not present | `notify_policy::record_result` | `notify.rs::fetch_once` → `rf_notify_gate_record` |

**Preserved semantics** (compatibility port, not a tightening):
- `/next.bin` 404 → JSON endpoint fallback, one GET, shared parse/ack — untouched (existing host test still green).
- 204 = empty queue, non-200/204 = failure, transport (negative status) = failure — same mapping as the old `match status`.
- State machine IDLE→FETCHING→NOTIFYING unchanged: `request_next` still no-ops unless IDLE; every `fetch_once` terminal path still leaves FETCHING.
- EPD diff/refresh policy untouched (`rr=4` still unresolved); `SleepManager::Busy()` is a read-only peek.
- Old servers without `.bin` keep working; no protocol/NVS/API tightening.

**New behaviour** (additive, per design §B):
- Rate limit (5 s) and failure backoff (60 s → ×2 → cap 900 s) now gate pulls. Defaults chosen conservatively; C++ owns the values (`kNotify*` constants), Rust owns the decision. `now_s < 0` skips the time gates so cold boot still pulls.
- Duplicate id skip: `notify.rs` remembers the last shown id per boot (separate slot, survives dismiss); a re-offered id is fetched but not repainted.
- EPD-busy defer: a pull requested while the panel's Display vote is set is held (was: unconditional GET).

---

## TDD Evidence

1. **RED** — `firmware/main/rust/tests/notify_policy.rs` written first; `cargo test --test notify_policy` failed with `E0432 unresolved import rust_firmware::notify_policy` (missing module). Two struct literals then failed on the missing `_pad` field.
2. **GREEN** — minimal `notify_policy.rs` + registration: 20/20 passed.
3. **SENTINEL 1** — `DEFER_BUSY` branch mutated to return `PULL` → `epd_busy_defers_the_pull` FAILED (0 passed). Restored → 20/20.
4. **SENTINEL 2** — duplicate branch of `decide_consume` mutated to `CONSUME_SHOW` → integration test `duplicate_id_or_etag_is_skipped_never_reshown` FAILED **and** the wiring unit test `a_re_offered_notification_is_skipped_after_dismiss` FAILED (the mutation proves both layers are pinned). Restored → full suite green.
5. New unit tests in `notify.rs` prove the production wiring, not just the policy:
   - `a_re_offered_notification_is_skipped_after_dismiss` — fetch runs, panel untouched, no popup.
   - `pull_outcomes_feed_the_failure_streak` — 500 → `(attempt=1000, failure=1000, streak=1)`; next 204 → `(2000, 1000, 0)`.

## Gates

- `cargo test` (host): **green** — 301 unit + 4 (device_signature) + 20 (notify_policy) + 0, 0 failed.
- `export PATH="$HOME/.cargo/bin:$PATH"; source ~/data/esp-idf-v6.0/export.sh; IDF_TARGET=esp32s3 idf.py build`: **green** — `xiaozhi.bin 0x2c8380 bytes`, 29% free, all links resolved.
- `git diff --check`: clean.
- Symbols (nm): archive `librust_firmware.a` exports all six Rust C ABI fns
  (`rf_notify_pull_decide`, `rf_notify_classify_response`, `rf_notify_decide_consume`,
  `rf_notify_backoff_delay_s`, `rf_notify_record_result`, `notify_state`); the linked
  ELF carries the ones C++ actually calls (`rf_notify_pull_decide`, `notify_state`,
  `rf_notify_gate_stats`, `rf_notify_gate_record`, `rf_time_now_s`) — LTO drops the
  C ABI fns no C++ caller references yet, and they are exercised on host instead.
- Device hardware behaviour: **not observed** (host tests + link only, per plan).

## Concerns for Review

1. **Rate/backoff are new enforcement.** A BOOT press inside a 5 s window or a
   60–900 s backoff window now logs "held" instead of pulling. On an outage the
   ladder climbs 60/120/…/900 s exactly like the sync ladder; the pending
   notification stays server-side (TTL 300 s keeps offering it) and the next
   cycle/press after the window pulls again.
2. **Stamp timing**: `last_pull` records the fetch *completion* (single writer:
   `fetch_once`), so the rate window measures completion-to-completion. Early
   aborts (no device_id / endpoint build / alloc failure) record nothing — no
   attempt happened.
3. **EPD-busy defer drops the request silently for this cycle** (logged at INFO);
   no deferred-retry queue was added, because adding one would change task
   lifecycle (C++ owns it). Retries ride the existing cycle/press paths.
4. `SleepManager::Busy()` reads the raw vote — a Display busy that overlaps a
   cycle defers that cycle's pull (the overlap window is real: 15–25 s waves vs
   ≥15 s poll).
5. Duplicate dedup memory is boot-local (RTC not used); after a deep-sleep wake
   the same id could be re-shown once — matches "RAM clears on wake" semantics
   of every other notify state (`ID`, `STATE`).
