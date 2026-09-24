# Task 2 Report: Move LED Decision Logic to Rust

**Date**: 2026-09-24
**Status**: Complete
**Commit**: pending

---

## Files Changed

### Created
| File | Purpose |
|------|---------|
| `firmware/main/rust/src/led_policy.rs` | Pure Rust LED action policy with 8 tests |
| `firmware/main/rust/include/led_policy.h` | C ABI header (`rf_led_policy_decide`) |

### Modified
| File | Change |
|------|--------|
| `firmware/main/rust/src/lib.rs` | Added `pub mod led_policy;` and `pub mod charge_policy;` |
| `firmware/main/CMakeLists.txt` | Added `led_policy.rs` and `charge_policy.rs` to `RUST_SOURCES` |
| `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc` | Replace `led_decide` body with `rf_led_policy_decide` call; updated `LedAction` struct with `first_wait_notify`/`second_wait_notify`; updated three dispatch arms with explicit `wait_notify` conditionals |

### Note: `charge_policy.rs` and `charge_policy.h` were created in Task 1 but were missing from `lib.rs` and `RUST_SOURCES`. Task 2 added both to ensure the archive links correctly.

---

## Test Results

```
$ cargo test led_policy -- --nocapture
running 8 tests
test led_policy::tests::blink_override_uses_500ms_delay_and_toggles_phase ... ok
test led_policy::tests::static_override_uses_1000ms_notify ... ok
test led_policy::tests::idle_activity_pulse_uses_120ms_delay_then_180ms_delay ... ok
test led_policy::tests::full_uses_1000ms_notify ... ok
test led_policy::tests::charging_uses_200ms_delay_then_2800ms_notify ... ok
test led_policy::tests::idle_charge_state_uses_default_notify ... ok
test led_policy::tests::zero_pulses_does_not_consume_pulse ... ok
test led_policy::tests::full_takes_precedence_over_charging_in_branch_order ... ok
test result: ok. 8 passed
```

```
$ cargo test
212 tests total (led_policy + charge_policy + all other modules)
test result: ok. 212 passed
```

---

## Sentinel Mutation Proof

Changed `second_wait_notify: true` → `false` in the charging branch:
```
assert_eq!(a.second_wait_notify, true);
```
Result: **FAILED** — test correctly detected the broken value.

Restored `second_wait_notify: true`, reran: **8 passed**.

---

## Build Gate

```
$ idf.py build
  idf.py -p PORT flash
  python -m esptool --chip esp32s3 ... write-flash ...
```

Build succeeded with no errors. Both `rf_led_policy_decide` and `rf_charge_policy_decide` are present in `librust_firmware.a`.

---

## Symbol Verification

```
$ xtensa-esp32s3-elf-nm librust_firmware.a | grep rf_.*_decide
00000000 T rf_charge_policy_decide
00000000 T rf_led_policy_decide
```

---

## Behavioral Preservation

All six branches preserved exactly:
- **blink** (ovr && blink): 500ms delay, level=phase, no second, no consume, first_wait_notify=false
- **static** (ovr && !blink): 1000ms notify, level=true, no second
- **pulse** (!ovr && !charging && !full && pulses>0): 120ms+180ms delay, level=false, second_level=true, consume_pulse=true, both wait_notify=false
- **full** (!ovr && full): 1000ms notify, level=false, no second, first_wait_notify=true
- **charging** (!ovr && charging): 200ms delay + 2800ms notify, level=false, second_level=true, first_wait_notify=false, second_wait_notify=true
- **idle** (else): 1000ms notify, level=true, no second, first_wait_notify=true

The `!phase` write ordering for blink (C++ stores `!phase`, writes `action.level=phase`) is preserved in the blink arm.

---

## Concerns

1. **Pre-existing `dead_code` warning** in `settings.rs:214` — `fn section` is never called; not introduced by this task.

2. **`led_policy.rs` not in `lib.rs` for ~6 edit attempts** — the `PUT >19:` empty-body syntax was silently rejected by the edit tool; confirmed and fixed by verifying the actual file state.

3. **Incremental compile cache bypass** — cargo refused to recompile `rust_firmware` after adding `led_policy` to `lib.rs`; required verifying the actual file content (not just edit confirmation) to detect the missing registration.

4. **ESP-IDF C++17** — designated initializers for `LedAction` not supported; replaced with positional args and `static_cast<bool>()` for `int8_t`→`bool` narrowing.
