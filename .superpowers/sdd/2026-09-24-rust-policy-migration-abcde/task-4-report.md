# Task 4 Report: Battery Relative-Activity Policy

**Date**: 2026-09-25
**Status**: Complete (host tests + ESP-IDF build green; device behaviour not yet observed)
**Branch**: abcde-rust-migration
**Worktree**: /mnt/data/project/youn-ink-fourcolor-firmware/.worktrees/abcde-rust-migration

---

## 1. Survey of the existing battery sample/report path (done before coding)

### 1.1 Production flow, pre-Task 4

```text
page_sync::fetch_schedule (Rust, every schedule GET)
  └─ rf_battery_due()  [shim.cpp]  ── ShimDecideBattery ── time_gate_policy.rs
       peek: one-hour sliding window on RTC_DATA_ATTR g_battery_due_at
       (time() -> PCF8563 fallback; never arms off an implausible clock)
  └─ if due: rf_battery_sample(&mv,&pct,&c)  [shim.cpp]
       └─ ZectrixReadBatterySample [board .cc]
            charge_status_.Tick + snapshot  → charge byte 0..4 (GPIO, cheap)
            no_battery → return false        (sensor-absent ⇒ omit params)
            ReadBatterySampleForTelemetry    → 10-sample ADC burst + calibration
            voltage→percent quadratic map    → the `p=` value (display/protocol)
  └─ if ok && mv>0: append "&v={mv}&p={pct}&c={c}" to /api/pages/schedule?…
  └─ rf_battery_arm()  [shim.cpp]  → g_battery_due_at = now+3600
       (never advances when time()/RTC are implausible — old NEVER guard)

server (youn_server/app.py:654): all three keys in range
  (2500≤v≤5000, 0≤p≤100, 0≤c≤4) → one battery_history row; anything less
  = no row (absent-means-absent, same as rr=).
```

Inputs that existed: raw/normalized voltage (mV, averaged over 10 ADC reads),
charge fact (0=unknown 1=no-power 2=charging 3=full 4=discharging), last-arm
RTC stamp, wall clock / PCF8563 epoch, 3600 s period.
Outputs that existed: `accept` (the time gate + `mv>0` read check), the wire
triple `v/p/c`, and the next arm stamp. **No outlier window, no direction
tracking across samples, no relative-activity notion** — `p` was, and remains,
a C++ voltage→percent display mapping.

### 1.2 Other call sites audited (all deliberately untouched)

| Site | Role | Why untouched |
|------|------|---------------|
| `application.cc:1618` `board.GetBatteryLevel()` | status-bar `battery_level/charging` | display-only voltage→percent consumer (constraint: preserve) |
| `zectrix…cc:202` `GetBatteryLevel`, `:258` factory-test percent | UI / factory percent | same display mapping, C++-owned |
| `FT/factory_test_service.cc:719` | charge/percent acceptance loop | factory mechanism, not the sample path |
| `shim.cpp` `rf_time_gate_wifi_cache_ok`, `RfFillTimeGateInputs` (application.cc) | Task 2 time-gate wiring | Task 2 scope; battery branch now subsumed (see §7) |
| server `battery_history` + `?v=&p=&c=` ingest | history/HTTP | C++ keeps HTTP; wire format unchanged |

### 1.3 Design §5 obligations mapped to code

- "outputs named `relative_activity`, never `mAh`/`soc_percent`" →
  `SampleOutput.relative_activity` (0..2) is the only level-shaped field;
  no 0-100 output exists on the Rust side; grep-verified (only prohibition
  comments mention those names).
- "charging/discharging transitions retain the sample" → `direction_transition`
  forces the gate open (new vs. the old time-only gate).
- "ADC polarity/range/calibration stay in C++" → the mV/% read lives in
  `rf_battery_activity_read` (shim.cpp + board), Rust only sees mV.

---

## 2. Files changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/battery_activity_policy.rs` | Pure policy: `gate_open`, `decide` (accept / outlier filter / direction / relative activity / next gate), C ABI `rf_battery_activity_decide`, `rf_battery_activity_gate_open`, module tests |
| `firmware/main/rust/include/battery_activity_policy.h` | ABI header: `rf_battery_activity_inputs_t` (32 B), `rf_battery_activity_output_t` (16 B), direction/activity constants, the five `rf_battery_activity_*` function contracts |
| `firmware/main/rust/tests/battery_activity_policy.rs` | 15 red-first integration tests: monotonic discharge, charge/discharge transitions, same-side non-transition, outlier window, missing sample, gate expiry/skip, first sample, unset clock, activity grades, no-SOC contract, C ABI + layout tests |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | registered `pub mod battery_activity_policy;` |
| `firmware/main/CMakeLists.txt` | added `battery_activity_policy.rs` to `RUST_SOURCES` |
| `firmware/main/rust/src/page_sync.rs` | `fetch_schedule` battery block: context facts → Rust `gate_open` (pre-ADC) → ADC read → Rust `decide` → params carry Rust `direction`, commit stamp; per-sample `log_i` with dir/act/accept/filtered; 3 new/strengthened tests (ADC-skip inside window, read-on-due, direction-transition report inside window) |
| `firmware/main/rust/src/shim.rs` | externs `rf_battery_activity_{context,read,commit}`; host stubs replacing `rf_battery_sample`/`rf_battery_due`/`rf_battery_arm`; staged baseline (`set_battery_prev`); `reset_battery_staging()` now runs in `host::lock()` for order independence |
| `firmware/main/rust/shim.cpp` | new mechanism functions: cheap `context` (charge GPIO + effective clock + RTC stamps + baseline), `read` (10-sample ADC burst + `p=` percent), `commit` (baseline bytes + NEVER-guarded stamp). Removed `rf_battery_sample`, `rf_battery_due`, `rf_battery_arm`, `ShimDecideBattery` (their last caller was `page_sync`). RTC_DATA_ATTR baseline: `g_battery_prev_mv/_charge/_valid` |
| `firmware/main/boards/…/zectrix-s3-epaper-4.2.cc` | extracted `EncodeCharge(snapshot)` (byte-identical to the old inline mapping), `ZectrixReadBatterySample` now uses it; new `ZectrixReadChargeEncoding()` — same snapshot/encoding without the ADC burst, for the pre-ADC gate |

---

## 3. ABI surface

```c
#define RF_BATTERY_DIRECTION_UNKNOWN/NO_POWER/CHARGING/FULL/DISCHARGING  0..4
#define RF_BATTERY_RELATIVE_ACTIVITY_RESTING/LOW/HIGH                    0/1/2
#define RF_BATTERY_VOLTAGE_MIN_MV 2500   /* server ingest window */
#define RF_BATTERY_VOLTAGE_MAX_MV 5000

typedef struct {                       /* 32 bytes */
    uint16_t voltage_mv;               /* 0  */
    uint8_t  charge;                   /* 2  */
    uint8_t  has_sample;               /* 3  */
    uint8_t  _pad[4];                  /* 4  */
    int64_t  now_s;                    /* 8   effective clock, -1 = unset */
    int64_t  last_sample_s;            /* 16  last report stamp, -1 = none */
    uint16_t prev_mv;                  /* 24  activity baseline */
    uint8_t  prev_charge;              /* 26 */
    uint8_t  prev_valid;               /* 27 */
    uint32_t min_interval_s;           /* 28  C++ config (3600) */
} rf_battery_activity_inputs_t;

typedef struct {                       /* 16 bytes */
    uint8_t  accept;                   /* 0  */
    uint8_t  direction;                /* 1  c= value Rust reports */
    uint8_t  relative_activity;        /* 2  THE only level-shaped output */
    uint8_t  filtered;                 /* 3  outlier vs "not due" */
    uint8_t  _pad[4];                  /* 4  */
    int64_t  next_sample_s;            /* 8  now+interval / unchanged */
} rf_battery_activity_output_t;

int      rf_battery_activity_context(rf_battery_activity_inputs_t*);  /* C++ facts */
uint8_t  rf_battery_activity_gate_open(const rf_battery_activity_inputs_t*); /* Rust */
int      rf_battery_activity_read(rf_battery_activity_inputs_t*, uint8_t* /*p=*/); /* C++ ADC */
rf_battery_activity_output_t rf_battery_activity_decide(const rf_battery_activity_inputs_t*); /* Rust */
int      rf_battery_activity_commit(int64_t next_s, uint16_t mv, uint8_t direction); /* C++ bytes */
```

`#[repr(C)]`, fixed order, explicit padding; offsets/sizes asserted by
`c_structs_match_the_header_layout` (inputs 32 B, output 16 B).

### Decision ownership (production)

| Decision | Owner | Consumed at |
|----------|-------|-------------|
| accept | Rust `decide` | `page_sync::fetch_schedule` writes v/p/c only on `accept==1` |
| outlier filter | Rust `decide` (`filtered=1`, stamp kept → retry next wake) | same; `has_sample==0` also short-circuits in `read` |
| direction | Rust (`normalize_direction` → `direction`) | wire `c=`; commit baseline |
| relative activity | Rust (grade vs `prev_mv`) | firmware `log_i` per read (see §6 concern 3) |
| next-sample gate | Rust `gate_open` + `decide.next_sample_s` | pre-ADC read decision; `rf_battery_activity_commit` persists (NEVER-guarded) |

C++ owns: ADC, GPIO/charge snapshot, NVS/RTC bytes, `p=` percent mapping, HTTP.

---

## 4. TDD evidence

**RED** — test file written before the module existed:

```
error[E0432]: unresolved import `rust_firmware::battery_activity_policy`
  --> tests/battery_activity_policy.rs:21:20
error: could not compile `rust_firmware` (test "battery_activity_policy")
```

**GREEN** — minimal implementation: 15/15 integration tests + 3 module tests.

**SENTINEL** — each key decision mutated, expected failures observed, restored:

| Mutation | Result |
|----------|--------|
| M1 outlier: voltage range removed | `outlier_voltage_filtered`, `outlier_does_not_poison_the_baseline` FAILED (13 passed/2 failed) |
| M2 direction transition disabled | `charge_direction_transition_preserves_sample`, `reverse_transition_preserves_sample` FAILED; **production** `page_sync::tests::a_direction_transition_reports_inside_the_window` FAILED (0 passed/1 failed) |
| M3 gate always open | `non_expired_gate_skips`, `same_side_full_to_charging…`, `c_abi…` FAILED; **production** `battery_params_wait_outside_the_one_hour_gate` FAILED (2 passed/1 failed) |
| M4 activity threshold collapsed | `activity_grades_sample_to_sample_movement`, `c_abi…` FAILED |
| M5 next-gate never advances | `monotonic_discharge_accepted`, `gate_expiry_allows_sample`, `first_sample_accepts_and_arms`, `c_abi…` FAILED |

**RESTORE** — file byte-identical (`cmp`), full suite green again.

---

## 5. Verification

| Gate | Command | Result |
|------|---------|--------|
| Full host suite | `cargo test` (`PATH=$HOME/.cargo/bin:$PATH`) | **PASS** — 344 tests: lib 305 (incl. 3 module + 3 production battery tests), battery integration 15, device_signature 4, notify 20 |
| ESP-IDF build | `source ~/data/esp-idf-v6.0/export.sh; IDF_TARGET=esp32s3 idf.py build` | **PASS** — "Project build complete", `xiaozhi.bin` 0x2c8650 B; only pre-existing fatfs-Kconfig and GNU-stack notes |
| Symbols in archive | `xtensa-esp32s3-elf-nm main/rust/target/…/librust_firmware.a` | `T rf_battery_activity_decide`, `T rf_battery_activity_gate_open`; `U` context/read/commit (resolved from shim.cpp) |
| Symbols in ELF | `nm build/xiaozhi.elf` | `T rf_battery_activity_{context,read,commit}` @ 0x4200fe88+, `T ZectrixReadChargeEncoding` @ 0x420186d4 |
| Dangling refs | grep old symbols | only doc comments mention `rf_battery_due/arm`; no code references |
| Whitespace | `git diff --check` | clean |
| No-SOC contract | grep `soc_percent\|mAh` in new files | only prohibition statements (header §"naming contract", test doc); no field carries either name; `outputs_never_resemble_soc_or_mah` asserts direction≤4, activity≤2, accept≤1 over 73×7 fact combos |

Device behaviour: **not observed** (host tests + build only; report/transition
/ADC-skip behaviour needs a real wake to confirm).

---

## 6. Behavioural deltas vs. pre-Task 4 (intentional, design §5)

1. **Direction transitions report inside the one-hour window** (old: skipped
   until expiry). Same-side moves (charging↔full) still respect the window.
2. **Outlier window 2500..5000 mV** added (old: only `mv>0`); a rejected
   sample does not advance the stamp, so the next wake retries — strictly
   tighter against the server's own ingest range (no row ever sent that the
   server would drop).
3. **`c=` now comes from Rust's normalized `direction`** (byte-identical for
   all facts the board can produce: full→3, charging/no-battery→2, else→4;
   out-of-range bytes collapse to 0 instead of leaking).
4. Old `rf_battery_due`/`rf_battery_arm`/`rf_battery_sample` bridges removed
   (last caller was `page_sync`); their semantics live in `gate_open` +
   `decide` + `commit`, including the NEVER guard.

Everything else — ADC averaging, percent map, RTC persistence mechanism, wire
format, server ingest, display paths — is unchanged.

---

## 7. Concerns / notes for review

1. **Task 2's battery branch is now consumer-less in production.**
   `time_gate_policy.rs` still returns `battery_sample_ok` /
   `next_last_battery_arm_s` (tested), but the production battery window moved
   to `battery_activity_policy` (a strict superset: same window + first-sample
   /unset-clock/transition rules). The module and its Task 2 wiring in
   `application.cc` were left untouched (out of Task 4 scope); Task 7's ABI
   audit can decide whether to prune the branch.
2. **C ABI `rf_battery_activity_decide`/`gate_open` are not in the final ELF**
   (present in `librust_firmware.a`, host-tested through the ABI): on device
   `page_sync` calls the same pure functions directly, so the linker drops the
   unreferenced exports — identical to Task 3's `rf_notify_classify_response`
   family. The header declares them for any future C++ caller.
3. **`relative_activity` is consumed only by the firmware log line** (and by
   `direction`'s baseline logic indirectly). Sending it to the server would be
   a wire/protocol change the design did not authorise (§9), so observability
   is the honest sink; the value is computed and emitted every sample.
4. **ADC-skip nuance:** a direction transition inside the window now *does*
   trigger the 10-sample burst (it must, to preserve the sample); inside the
   window with no direction change the burst is still skipped (pinned by
   `battery_params_wait_outside_the_one_hour_gate`).
5. **`p=` percent** remains the C++ quadratic map (protocol/display value);
   Rust never sees or produces it beyond relaying `percent_out`.
