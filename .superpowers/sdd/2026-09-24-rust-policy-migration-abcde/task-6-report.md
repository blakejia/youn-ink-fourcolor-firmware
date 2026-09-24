# Task 6 Report: Protocol Parsers and Status Classification

**Date**: 2026-09-25
**Status**: Complete (host tests + ESP-IDF build green; device behaviour not yet observed)
**Branch**: abcde-rust-migration
**Worktree**: /mnt/data/project/youn-ink-fourcolor-firmware/.worktrees/abcde-rust-migration

---

## 1. Survey: which parse seams remain in C++ vs already Rust

Surveyed before coding, per the assignment. Candidate seams from design §7
against the actual code:

| # | Design §7 candidate | Actual owner (pre-Task 6) | Verdict |
|---|---------------------|---------------------------|---------|
| 1 | Schedule entry fields (md5/duration classification) | `page_sync.rs::parse_schedule` (Rust, but inline predicates) | **Genuinely remaining** — migrated to `protocol_parse::decide_schedule_entry` |
| 2 | Policy `*_minutes` → seconds scaling (`minutes_to_s`) | `page_sync.rs::minutes_to_s` (Rust, inline) | **Remaining rule** — migrated to `protocol_parse::policy_minutes_to_s` |
| 3 | HTTP status → business class | Scattered inline `status == 200` / `match status` in `page_sync.rs`, `notify.rs`; classified tables in `notify_policy.rs`, `pairing_response.rs` | **Shared vocabulary added** (`classify_http_status`); per-caller responses deliberately NOT moved (see §3) |
| 4 | Battery/power query params (`v/p/c`, `w/a/r/g/f/er/eb/rr`) | Assembled in `page_sync::fetch_schedule` (Rust); values from `battery_activity_policy` (Rust) + C++ counters | **Already Rust** — no new seam; not touched |
| 5 | OTA manifest field parsing | **Does not exist**: no JSON manifest parsing anywhere in firmware; only a URL string passthrough (`wifi_station.cc::LoadHttpOtaUrl`, NVS/config → `ParseUrlAuthority`) | **Nothing to migrate**; documented, no test invented |
| 6 | Notification response fields | `notify.rs::parse_next`/`parse_binary_next` + `notify_policy::classify_response`/`decide_consume` (all Rust) | **Already Rust** — not touched |
| 7 | Wi-Fi endpoint parser (MQTT `host[:port]`, URL authority) | `wifi_policy.rs::parse_endpoint`/`parse_url_authority` + C ABI, wired in `wifi_station.cc` | **Already Rust** — not touched (no duplication) |
| 8 | Pairing response fields | `server_pairing.cc` extracts cJSON facts → `pairing_response.rs` classifies (Rust) | **Already Rust** — not touched |

Loose-compatibility semantics preserved (and pinned by tests):
MQTT `host[:port]` without scheme, URL authority, missing fields
(`unwrap_or` fallbacks), invalid JSON fallback (entries skipped / body
unusable, never fatal), old-server fallback (`notify_pending` absent →
`true`, JSON `/next` on binary 404).

---

## 2. Files changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/protocol_parse.rs` | Pure classifiers: `decide_schedule_entry`, `policy_minutes_to_s`, `classify_http_status`, C ABI `rf_protocol_schedule_entry`/`rf_protocol_policy_minutes_to_s`/`rf_protocol_http_class`, 1 module test |
| `firmware/main/rust/include/protocol_parse.h` | ABI header: `rf_protocol_schedule_entry_facts_t` (16 B), `_decision_t` (8 B), `RF_PROTOCOL_HTTP_*` codes, `RF_PROTOCOL_MAX_POLL_MINUTES` |
| `firmware/main/rust/tests/protocol_parse.rs` | 11 red-first tests: entry usable/bad-md5/missing-duration/negative-clamp/zero, minutes scale+clamp/fallback, status classes, transport-error class, C ABI + layout |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | registered `pub mod protocol_parse;` |
| `firmware/main/CMakeLists.txt` | added `protocol_parse.rs` to `RUST_SOURCES` |
| `firmware/main/rust/src/page_sync.rs` | production wiring: `minutes_to_s` routes through `protocol_parse::policy_minutes_to_s` (DOM reads stay); `parse_schedule` entry loop classifies via `protocol_parse::decide_schedule_entry` (DOM reads stay); removed now-unused local `MAX_POLL_MINUTES` (canonical home is now `protocol_parse`) |

---

## 3. ABI surface

```c
#define RF_PROTOCOL_SCHEDULE_MD5_LEN 32
#define RF_PROTOCOL_MAX_POLL_MINUTES 1440
#define RF_PROTOCOL_HTTP_OK               0  /* 200 */
#define RF_PROTOCOL_HTTP_EMPTY            1  /* 204 */
#define RF_PROTOCOL_HTTP_NOT_FOUND        2  /* 404 */
#define RF_PROTOCOL_HTTP_REJECTED         3  /* 401/429 */
#define RF_PROTOCOL_HTTP_TRANSPORT_ERROR  4  /* negative status */
#define RF_PROTOCOL_HTTP_ERROR            5  /* everything else */

typedef struct {                       /* 16 bytes */
    uint32_t md5_len;                  /* 0  */
    uint8_t  has_duration;             /* 4  */
    uint8_t  _pad[3];                  /* 5  */
    int64_t  duration_minutes;         /* 8  */
} rf_protocol_schedule_entry_facts_t;

typedef struct {                       /* 8 bytes */
    uint8_t  usable;                   /* 0  */
    uint8_t  _pad[3];                  /* 1  */
    uint32_t duration_s;               /* 4  */
} rf_protocol_schedule_entry_decision_t;

rf_protocol_schedule_entry_decision_t rf_protocol_schedule_entry(
    const rf_protocol_schedule_entry_facts_t*);
uint32_t rf_protocol_policy_minutes_to_s(uint8_t present, int64_t minutes,
                                         uint32_t fallback_s);
uint8_t rf_protocol_http_class(int32_t status);
```

`#[repr(C)]`, fixed order, explicit padding; offsets/sizes asserted by
`c_structs_match_the_header_layout` (16/8).

### Decision ownership (production)

| Decision | Owner | Consumed at |
|----------|-------|-------------|
| Schedule entry usable + duration_s | Rust `decide_schedule_entry` | `page_sync::parse_schedule` loop |
| Policy minutes → seconds + clamp + fallback | Rust `policy_minutes_to_s` | `page_sync::minutes_to_s` |
| HTTP status → business class | Rust `classify_http_status` (shared vocabulary) | Header-available; call sites keep their existing responses |
| JSON DOM walks, HTTP transport, files, tasks | `page_sync` / C++ (untouched) | — |
| Notify 404→JSON-fallback split | `notify_policy` (unchanged) | `notify::fetch_once` |
| Schedule 200-only gate | `page_sync::fetch_schedule` (unchanged) | `sync_once` |

`classify_http_status` is intentionally vocabulary-only: unifying every
caller's `match status` behind one function would change the notify
binary-vs-JSON split and the schedule 200-only gate — behaviour changes
with no mandate. The header declares the mapping for future callers.

---

## 4. TDD evidence

**RED** — test file written before the module existed:

```
error[E0432]: unresolved import `rust_firmware::protocol_parse`
```

**GREEN** — minimal implementation: 11/11 integration tests + 1 module test.

**SENTINEL** — each key decision mutated, expected failures observed, file
restored byte-identical (`cmp` after each):

| Mutation | Result |
|----------|--------|
| M1 entry gate removed (`if ...` → `if false`) | `entry_with_bad_md5_len_is_skipped`, `entry_with_missing_duration_is_skipped` FAILED (9/11) |
| M2 minutes clamp removed (`.min(MAX)` → raw) | `policy_minutes_scale_and_clamp` FAILED (10/11) |
| M3 401/429 → ERROR instead of REJECTED | `http_status_maps_to_business_classes` FAILED (10/11) |
| **M4 (production)** `decide_schedule_entry` call renamed | **lib test compile error E0425** — proves production consumes the Rust decision |
| Dead-code cleanup | removed shadowed `MAX_POLL_MINUTES` from `page_sync.rs` after Xtensa build flagged it; host suite re-green |

**RESTORE** — `cmp` byte-identical after every mutation; full suite green.

---

## 5. Verification

| Gate | Command | Result |
|------|---------|--------|
| Full host suite | `cargo test` (`PATH=$HOME/.cargo/bin:$PATH`) | **PASS** — 374 tests: lib 307, battery 15, device_signature 4, notify 20, page_compare 17, protocol_parse 11, doc 0 |
| Xtensa Rust build | `cargo +esp build --release --target xtensa-esp32s3-none-elf -Zbuild-std=core` | **PASS** (after removing the shadowed constant) |
| ESP-IDF build | `cd firmware; source ~/data/esp-idf-v6.0/export.sh; IDF_TARGET=esp32s3 idf.py build` (with cargo on PATH first) | **PASS** — `xiaozhi.bin` 2918320 B; note: the first attempt failed on `cargo +esp: no such file or directory` because the build shell lacked `$HOME/.cargo/bin` on PATH — environment, not code; rerun with the required PATH order succeeded |
| Symbols in archive | `xtensa-esp32s3-elf-nm librust_firmware.a \| grep rf_protocol` | `T rf_protocol_http_class`, `T rf_protocol_policy_minutes_to_s`, `T rf_protocol_schedule_entry` |
| Whitespace | `git diff --check` | clean |

Device behaviour: **not observed** (host tests + build only).

---

## 6. Behavioural deltas vs. pre-Task 6

**None observable.** Every branch is the old predicate relocated behind the
policy ABI, pinned by the existing production tests (`parses_md5_pages...`,
`negative_duration_clamps_to_zero`, `caps_at_max_pages...`,
`rejects_unusable_responses`, `policy_defaults...`,
`nonsense_policy_values...` — all green without modification). The entry
loop performs the md5 DOM read twice (once for `md5_len`, once for
`md5_from`); the second is authoritative and the first only feeds the
length fact — same accept/reject outcomes as before.

## 7. Concerns / notes for review

1. **`classify_http_status` has no production caller yet** (vocabulary +
   header, like Tasks 3–5's exported-but-linker-dropped symbols). Wiring
   each `match status` through it is a follow-up with its own behaviour
   proof, not this task.
2. **OTA manifest is absent, not deferred**: `grep` for
   `ota_manifest|/api/ota|/api/firmware|firmware_version|update_available|ota_url`
   finds only a URL string passthrough and build-script config. If a
   manifest protocol appears later, it gets its own red-first module.
3. **Mid-task incident (process, not code)**: one `edit` call landed in
   the main repo's `lib.rs` instead of the worktree (absolute-path
   resolution). Detected via `grep` on both trees, reverted immediately
   (`git checkout`), verified zero diff. No main-repo files remain touched.
4. **Build PATH ordering matters**: the plan's global constraint
   (`export PATH="$HOME/.cargo/bin:$PATH"` before sourcing ESP-IDF) is
   load-bearing — without it `cargo +esp` fails inside the IDF build.
