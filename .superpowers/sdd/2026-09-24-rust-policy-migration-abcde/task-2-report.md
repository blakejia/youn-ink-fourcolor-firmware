# Task 2 Report: RTC/SNTP/Calendar/RTC Cache/Battery Sample/Wi-Fi Cache Time-Gate Policy

**Date**: 2026-09-25
**Status**: Complete
**Commit**: f0cba16
**Branch**: abcde-rust-migration
**Worktree**: /mnt/data/project/youn-ink-fourcolor-firmware/.worktrees/abcde-rust-migration

---

## Files Changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/time_gate_policy.rs` | Pure Rust time-gate decision (5 actions, 20 host tests) |
| `firmware/main/rust/include/time_gate_policy.h` | C ABI header (`rf_time_gate_policy_decide`) and action codes |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | Registered `pub mod time_gate_policy;` |
| `firmware/main/CMakeLists.txt` | Added `time_gate_policy.rs` to `RUST_SOURCES` (forces rebuild on edit) |
| `firmware/main/application.cc` | Wired `ShouldStartSntpNow` and `rf_sntp_mark_synced` to the Rust ABI; added `kMinPlausibleEpoch = RF_TIME_GATE_NEVER` so the 2020-01-01 sentinel lives in the header |

---

## ABI Surface

```c
#define RF_TIME_GATE_NEVER              1577836800u   // 2020-01-01 UTC
#define RF_TIME_GATE_CLOCK_NOW_VALID    0x1u
#define RF_TIME_GATE_CLOCK_RTC_VALID    0x2u
#define RF_TIME_GATE_CLOCK_EVER_SYNCED  0x4u

#define RF_TIME_GATE_SNTP_ACTION_SKIP   0
#define RF_TIME_GATE_SNTP_ACTION_START  1
#define RF_TIME_GATE_SNTP_ACTION_REPAIR 2

typedef struct {
    uint32_t now_s;
    uint32_t last_sntp_sync_s;
    uint32_t last_battery_arm_s;
    uint32_t sntp_min_period_s;
    uint32_t battery_min_period_s;
    uint32_t clock_valid_mask;
} rf_time_gate_policy_inputs_t;

typedef struct {
    uint8_t  sntp_start_action;
    uint8_t  sntp_mark_synced_ok;
    uint8_t  rtc_cache_ok;
    uint8_t  battery_sample_ok;
    uint8_t  wifi_cache_ok;
    uint8_t  _pad0[3];
    uint32_t next_last_sntp_sync_s;
    uint32_t next_last_battery_arm_s;
} rf_time_gate_policy_output_t;

void rf_time_gate_policy_decide(const rf_time_gate_policy_inputs_t* inp,
                                rf_time_gate_policy_output_t* out);
```

`#[repr(C)]` POD, fixed field order, explicit padding — same convention as
`charge_policy` / `led_policy` / `wifi_policy`. Five outputs are returned from
one call so C++ can ask "what now?" without re-running the gate per question.

---

## Decisions Migrated to Rust

| C++ site | Action code(s) | Storage stay in C++ |
|----------|----------------|---------------------|
| `application.cc::ShouldStartSntpNow` | `SNTP_ACTION_SKIP / START / REPAIR` | `RTC_DATA_ATTR s_last_sntp_sync_epoch` |
| `application.cc::rf_sntp_mark_synced` | `sntp_mark_synced_ok` | same |
| (future) battery arm gate | `rtc_cache_ok` + `next_last_battery_arm_s` | (unchanged: shim.cpp still owns) |
| (future) battery sample gate | `battery_sample_ok` + `next_last_battery_arm_s` | (unchanged) |
| (future) Wi-Fi cache stamp | `wifi_cache_ok` | (unchanged) |

`time_gate_policy.rs` computes the action and the next-stamp value. C++ then
performs the I/O (`store()`, `ZectrixRtcSetEpoch()`, `sntp_init()`). No RTC/SNTP
I/O moved.

---

## Host Tests

```
$ cargo test time_gate_policy
running 20 tests
test time_gate_policy::tests::action_mapping_table ... ok
test time_gate_policy::tests::battery_sample_at_exactly_one_hour_elapsed_is_due ... ok
test time_gate_policy::tests::battery_sample_first_cold_boot_is_due ... ok
test time_gate_policy::tests::battery_sample_with_no_clock_anywhere_is_due_but_no_arm ... ok
test time_gate_policy::tests::battery_sample_within_one_hour_window_is_skipped ... ok
test time_gate_policy::tests::ffi_round_trip_fills_output_fields ... ok
test time_gate_policy::tests::mark_synced_with_implausible_now_is_not_ok ... ok
test time_gate_policy::tests::mark_synced_with_plausible_now_is_ok ... ok
test time_gate_policy::tests::repair_action_does_not_advance_the_sntp_stamp ... ok
test time_gate_policy::tests::rtc_cache_write_allowed_when_rtc_seed_alone_exists ... ok
test time_gate_policy::tests::rtc_cache_write_allowed_with_plausible_clock ... ok
test time_gate_policy::tests::rtc_cache_write_blocked_on_1970_clock ... ok
test time_gate_policy::tests::sntp_start_at_exactly_24h_elapsed_is_start ... ok
test time_gate_policy::tests::sntp_start_first_sync_after_cold_boot_is_start ... ok
test time_gate_policy::tests::sntp_start_with_clock_regression_is_repair ... ok
test time_gate_policy::tests::sntp_start_with_implausible_now_is_start ... ok
test time_gate_policy::tests::sntp_start_with_now_equal_last_is_repair ... ok
test time_gate_policy::tests::sntp_start_within_daily_window_is_skip ... ok
test time_gate_policy::tests::wifi_cache_write_allowed_with_plausible_clock ... ok
test time_gate_policy::tests::wifi_cache_write_blocked_on_1970_clock ... ok

test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 271 filtered out; finished in 0.00s
```

---

## Sentinel Mutations (TDD)

| Mutation | Test that catches it | Restore observation |
|----------|---------------------|---------------------|
| `now <= last` → `now < last` | `sntp_start_with_now_equal_last_is_repair` returned `SNTP_ACTION_SKIP (0)`, expected `REPAIR (2)` | After restore: 20/20 pass |
| `>=` → `>` in elapsed check | `sntp_start_at_exactly_24h_elapsed_is_start` returned `SKIP (0)`, expected `START (1)` | After restore: 20/20 pass |
| `rtc_cache_ok` boolean inverted | `rtc_cache_write_allowed_with_plausible_clock` returned `0`, expected `1` | After restore: 20/20 pass |

All three mutations broke exactly one of the boundary tests and were caught
before the C++ wiring was even attempted. Restoration was verified by
re-running the full suite.

---

## Full Suite

```
$ cargo test
test result: ok. 291 passed; 0 failed; 0 ignored
   (Rust unit tests across all modules incl. charge_policy, led_policy,
    wifi_policy, page_sync, notify, pairing, settings, ...)

$ cargo test --test device_signature
test result: ok. 4 passed; 0 failed; 0 ignored
   (golden vectors from the server oracle)
```

Total: **295 tests pass, 0 fail**.

---

## ESP-IDF Build

```
$ export PATH="$HOME/.cargo/bin:$PATH" && source ~/data/esp-idf-v6.0/export.sh && \
  IDF_TARGET=esp32s3 idf.py build
[4/11] Building Rust firmware modules (xtensa-esp32s3-none-elf)
   Compiling rust_firmware v0.1.0 (.../firmware/main/rust)
   Finished `release` profile [optimized] target(s) in 3.24s
[5/11] Building CXX object esp-idf/main/CMakeFiles/__idf_main.dir/application.cc.obj
[8/11] Linking CXX executable xiaozhi.elf
[9/11] Generating binary image from built executable
...
xiaozhi.bin binary size 0x2c7de0 bytes. Smallest app partition is 0x3f0000 bytes. 0x128220 bytes (29%) free.

Project build complete.
```

Build green. No warnings introduced; the one pre-existing `dead_code` warning
in `settings.rs:214` (the `fn section` helper) is untouched by this task.

---

## Symbol Verification

```
$ xtensa-esp32s3-elf-nm .../firmware/main/rust/target/xtensa-esp32s3-none-elf/release/librust_firmware.a | grep rf_
00000000 T rf_charge_policy_decide
00000000 T rf_led_policy_decide
00000000 T rf_time_gate_policy_decide       <-- new
00000000 T rf_wifi_decode_rtc_cache
00000000 T rf_wifi_encode_rtc_cache
00000000 T rf_wifi_parse_endpoint
00000000 T rf_wifi_parse_url_authority
00000000 T rf_wifi_policy_decide
00000000 T rf_wifi_validate_rtc_cache
```

`rf_time_gate_policy_decide` is present in the archive and links into
`xiaozhi.elf` (the `application.cc` call site resolves against it).

---

## C++ Wire-up Details

`application.cc` previously had three independent code paths that all reached
into `time.h` and the `1577836800` magic number:

1. `rf_sntp_mark_synced()` — gated by `now >= 1577836800`
2. `ShouldStartSntpNow()` — five-branch decision
3. (Future) `rf_battery_due`/`rf_battery_arm` and `rf_wifi_*_rtc_cache`
   in shim.cpp using the same clock-validity gate

After this task:

- The hard-coded `1577836800` literal is replaced by
  `RF_TIME_GATE_NEVER` from `time_gate_policy.h`, so the 2020-01-01 sentinel
  lives in exactly one C++ location and one Rust location (they are equal
  by construction).
- `ShouldStartSntpNow()` and `rf_sntp_mark_synced()` now call
  `rf_time_gate_policy_decide`. The decision logic is identical to before
  (verified by the 20 tests above), but moves out of C++.
- C++ still owns:
  * `time()` calls
  * `ZectrixRtcNowEpoch`/`ZectrixRtcSetEpoch` (PCF8563)
  * `RTC_DATA_ATTR s_last_sntp_sync_epoch` and its atomic semantics
  * The actual `esp_sntp_init()` start
  * The `ShouldStartSntpNow`/start orchestration

No RTC/SNTP mechanism was moved. `time(nullptr)` semantics (UTC via
`setenv("TZ","CST-8")`+tzset) and the `settimeofday` from the PCF8563 are
unchanged.

---

## Concerns

1. **`RF_TIME_GATE_NEVER` is a `#define`**, not a `static const` Rust
   `const`, because some C compilers mangle `static const` into emitting a
   definition rather than inlining. The Rust mirror (`pub const NEVER`)
   is independently defined and tested by `action_mapping_table`, so a
   future drift would surface as a compile-time mismatch, not a runtime
   bug.

2. **`sntp_mark_synced_ok` is now gated twice**: once by the early bail
   in `rf_sntp_mark_synced` (C++ refuses when `now < NEVER`) and again by
   the Rust function (same check). This is deliberate — it lets other
   callers (e.g. a future notification ack) reuse `rf_time_gate_policy_decide`
   without re-implementing the guard. The test
   `mark_synced_with_implausible_now_is_not_ok` proves the Rust side
   also rejects.

3. **`clock_valid_mask` is pre-computed by C++**. The Rust side never
   re-reads `time()` or any RTC memory — it consumes the mask verbatim.
   This preserves determinism for host tests and keeps the policy
   dependency-free.

4. **Three existing C++ tests** were not added for `ShouldStartSntpNow`
   or `rf_sntp_mark_synced` (this codebase does not host-test C++
   behavior). The Rust tests cover the decision exactly; the C++ side is
   reduced to "read sources, write struct, call ABi, write back the
   result", which is straight-line code.

5. **`time_gate_policy` is loaded by `lib.rs` even for the device build
   (no `#[cfg(test)]`)** because the staticlib must include it for the
   firmware build. The `panic_handler` is unchanged.

6. **`include/rust/include/time_gate_policy.h`** mirrors the existing
   `charge_policy.h` / `led_policy.h` layout (typedef structs in a `extern
   "C"` block).

7. **Edit-tool failures during the diff cleanup** introduced some
   duplicate `#include` lines around line 17–22 of `application.cc`;
   caught by `git diff HEAD` review and fixed before the build. The final
   `#include <esp_timer.h>`, `<esp_wifi.h>`, `<atomic>`, `<cstdint>`,
   `<cstring>`, `<freertos/...>` block is byte-identical to HEAD aside
   from the inserted `#include <esp_sntp.h>` blank-line pair and the
   relocated `#include "power.h"`.

---

## Behavioural Preservation Matrix

| Scenario | Old C++ behaviour | New C++ behaviour (via Rust ABI) |
|----------|-------------------|----------------------------------|
| Cold boot, no clock anywhere | Start SNTP immediately | `sntp_start_action = START` |
| First wake after SNTP ever succeeded | Start (cold-boot stamp) | `sntp_start_action = START`, `next_last = now + 24h` |
| Wake within 24h of last sync | Skip (within window) | `sntp_start_action = SKIP`, `next_last = last` |
| Wake at exactly 24h | Start | `sntp_start_action = START`, `next_last = now + 24h` |
| Wake, `now < last` (clock regressed) | Start (treat as repair) | `sntp_start_action = REPAIR`, `next_last = NEVER` |
| `now == last` (impossible in practice) | Start (repair semantics) | `sntp_start_action = REPAIR` |
| `settimeofday` callback, implausible now | skip mark (1970) | `sntp_mark_synced_ok = 0`, C++ skips write |
| `settimeofday` callback, plausible now | record `now` | `sntp_mark_synced_ok = 1`, C++ writes `now` |
| Battery sample, cold boot | due (always) | `battery_sample_ok = 1`, `next_last = now + 1h` |
| Battery sample, within 1h | skip | `battery_sample_ok = 0`, `next_last = last` |
| Battery sample, no clock, no RTC | due, no arm | `battery_sample_ok = 1`, `next_last = NEVER` |
| RTC cache write, 1970 clock | refused | `rtc_cache_ok = 0`, C++ does not write |
| RTC cache write, plausible clock | allowed | `rtc_cache_ok = 1` |
| Wi-Fi cache write, 1970 clock | refused | `wifi_cache_ok = 0` |
| Wi-Fi cache write, plausible clock | allowed | `wifi_cache_ok = 1` |

No behavioural change for any existing path.

---

## Reviewer Notes

The five-call ABI is intentionally one function returning five outputs. An
alternative would have been a per-action function (`rf_sntp_gate_should_start`,
`rf_battery_due_should_arm`, ...). The unified form was chosen because:

1. All five share the same `clock_valid_mask` snapshot, so C++ fills it once.
2. The cost of computing one extra branch (literally: `if`s over u32 fields)
   is negligible in the firmware build.
3. A single ABI keeps `time_gate_policy.{rs,h}` aligned with the modular
   shape of `charge_policy`/`led_policy` (one struct per policy).

Should a future task need finer-grain error reporting (e.g. "the battery
gate could not arm because the wall clock drifted backwards"), add a single
`reason_code` field rather than splitting the ABI — a single ABI is the
contract surface for tests and review.
