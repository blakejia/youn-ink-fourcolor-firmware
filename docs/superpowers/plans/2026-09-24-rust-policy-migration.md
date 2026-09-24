# NOTE4C Rust Policy Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move six testable NOTE4C policy/decision seams from C++ into Rust while preserving ESP-IDF/hardware mechanisms and fixing the observed `endpoint_missing` IP-cache fallback.

**Architecture:** One existing `librust_firmware.a` remains the only Rust archive. C++ calls purpose-named Rust C ABIs; Rust calls ESP-IDF only through existing `shim.cpp` or a new main-owned adapter. Wi-Fi uses a narrow component callback registration ABI because `78__esp-wifi-connect` is an independent ESP-IDF component. EPD diff reads completed read-only framebuffer snapshots outside `dirty_mutex` and returns structured refresh actions; the EPD mechanism remains C++.

**Tech Stack:** Rust 2024 `no_std` on Xtensa, `cargo test` host harness, ESP-IDF v6.0, C++17/ESP-IDF C ABI, `idf.py build`, `cargo test`, existing firmware CMake single-staticlib link.

**Spec:** `docs/superpowers/specs/2026-09-24-rust-policy-migration-design.md`

## Global Constraints

- Export PATH before sourcing ESP-IDF: `export PATH="$HOME/.cargo/bin:$PATH"` then `source ~/data/esp-idf-v6.0/export.sh`.
- Keep the current single `librust_firmware.a`; do not add a second Rust archive or `#[panic_handler]`.
- Every new `firmware/main/rust/src/*.rs` file must be added to `RUST_SOURCES` in `firmware/main/CMakeLists.txt`.
- Rust owns pure decisions; C++ owns GPIO, SPI, I2C, NVS, FreeRTOS, Wi-Fi driver, DNS/ARP/TCP, EPD waveform, and task lifecycle.
- Rust FFI uses `#[repr(C)]` PODs and `#[unsafe(no_mangle)]`; C++ headers are purpose-named and do not collide with existing headers.
- Host tests use inline `#[cfg(test)]` modules and `shim::host::lock()` where global test state is involved.
- Every task follows TDD: failing test, minimal implementation, sentinel mutation proving the test fails, restore, then full task gates.
- Do not open `/dev/ttyACM0` during device or deep-sleep validation; opening USB-Serial-JTAG resets the device.
- Do not claim hardware success from host tests; record each hardware check separately.
- Do not change EPD policy until Task 0 has positive `rr=4` causal attribution or an explicit user waiver.
- Do not push commits unless the user separately requests it.

## File Map

### Create

- `firmware/main/rust/src/charge_policy.rs` — normalized charge state transition policy and C ABI.
- `firmware/main/rust/include/charge_policy.h` — charge POD/action ABI.
- `firmware/main/rust/src/led_policy.rs` — LED action policy and C ABI.
- `firmware/main/rust/include/led_policy.h` — LED POD/action ABI.
- `firmware/main/rust/src/wifi_policy.rs` — cache codec, endpoint parsing, fast-connect/reconnect policy, C ABI.
- `firmware/main/rust/include/wifi_policy.h` — Wi-Fi POD/action ABI.
- `firmware/main/rust/src/pairing_response.rs` — parsed pairing response classification and C ABI.
- `firmware/main/rust/include/pairing_response.h` — pairing response ABI.
- `firmware/main/rust/src/epd_policy.rs` — frame diff, tiny state, and refresh action policy plus C ABI.
- `firmware/main/rust/include/epd_policy.h` — EPD policy POD/action ABI.
- `firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.h` — component-owned callback registration ABI.
- `firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.cc` — callback storage/registration and invocation guard.
- `firmware/main/wifi_policy_adapter.cc` — main-owned adapter that gathers component facts and calls Rust.
- `firmware/main/wifi_policy_adapter.h` — adapter lifecycle and action execution declarations.
- `firmware/main/boards/zectrix-s3-epaper-4.2/epd_policy_diag.h` — 不在阶段 0 创建；仅在另行批准的诊断设计中新增。

### Modify

- `firmware/main/CMakeLists.txt` — Rust source dependencies and main adapter source.
- `firmware/main/components/78__esp-wifi-connect/CMakeLists.txt` — component shim source.
- `firmware/main/boards/zectrix-s3-epaper-4.2/charge_status.cc/.h` — normalized GPIO facts and Rust transition.
- `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc` — Rust LED Action dispatch.
- `firmware/main/components/78__esp-wifi-connect/wifi_station.cc` — policy callback calls and Action execution.
- `firmware/main/application.cc` — adapter registration/unregistration at network lifecycle boundary.
- `firmware/main/common/server_pairing.cc` — parse facts and Rust response classification.
- `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc` — EPD Rust policy call and Action dispatch.
- `firmware/main/rust/src/lib.rs` — new module registration.

---

### Task 0: Diagnose `rr=4` Without Changing EPD Behavior

**Files:**
- Inspect only: `firmware/main/main.cc:38-102`, `firmware/main/rust/src/lib.rs:31-38`, `firmware/main/rust/shim.cpp:102-103,419-429`, `firmware/main/rust/src/page_sync.rs:167-178`, `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc:780-981`, server journal and `devices.db`.
- Create: `docs/superpowers/progress/2026-09-24-rr4-diagnosis.md`
- No production behavior change in this task.

**Interfaces:**
- Produces a written evidence record with one of: positive attribution (`custom_lcd_display`, other C++, Rust, or unknown with next bounded probe), or an explicit unresolved result that blocks Task 6.

- [ ] **Step 1: Capture the current source facts**

Record the exact reset path from source:

```text
C++ assert / ESP_ERROR_CHECK / Rust panic
→ abort/panic
→ esp_reset_reason() == ESP_RST_PANIC (4)
→ shim::rf_last_reset_reason()
→ page_sync::fetch_schedule()
→ ?rr=4
→ server journal and devices.db
```

Confirm current EPD fatal sites before writing any diagnostic plan:

```text
custom_lcd_display.cc: assert(buffer), assert(prev_buffer), assert(tx_buf),
assert(dirty_mutex), SPI polling transmit asserts, ESP_ERROR_CHECK in SPI init.
```

- [ ] **Step 2: Collect live evidence without opening serial**

Use server journal, SQLite, and a durable local capture only for the diagnostic window. The existing `devices.power_counters` row is a single latest snapshot and `battery_history` has no `reset_reason` column, so a one-time SQLite read cannot reconstruct all `w/a/r/er/eb/rr` request history.

First capture the journal stream to a dated, gitignored diagnostic file without changing production code:

```bash
mkdir -p /tmp/note4c-rr4
journalctl --user -u youn-ink-server --since '2026-09-24 12:00:00' --no-pager -q > /tmp/note4c-rr4/server-journal.txt
python3 -c "import sqlite3,json; c=sqlite3.connect('server/data/devices.db'); print(c.execute(\"SELECT device_id,power_counters FROM devices WHERE device_id='NOTE4C-3400FC'\").fetchall()); print(c.execute(\"SELECT ts,mv,pct,charge,wakes,awake_ms,radio_ms,epd_refreshes,epd_busy_ms FROM battery_history WHERE device_id='NOTE4C-3400FC' ORDER BY ts DESC LIMIT 20\").fetchall())"
```

The evidence record must preserve the exact journal text for each observation window. If the service does not log query strings, record that limitation explicitly; do not claim per-request `w/a/r/er/eb/rr` reconstruction from the latest snapshot.
Do not infer panic location from `rr=4` alone. Record whether each `w=1` follows a successful bitmap request and whether EPD counters increment before the next reset.

- [ ] **Step 3: Run a bounded reproduction checklist**

Without changing production code, document the next observation:

```text
1. Battery-only operation; USB disconnected.
2. Do not open Web Serial or any /dev/ttyACM0 reader.
3. Watch server journal for schedule → bitmap → EPD activity.
4. Record the next cold start: w=1 and rr value.
5. If a specific EPD line can be isolated, write its path and source line.
```

If the source line remains unknown, report `unresolved`; do not create a new panic metadata ABI in this task.

- [ ] **Step 4: Write the evidence record**

Write `docs/superpowers/progress/2026-09-24-rr4-diagnosis.md` with timestamps, request URLs, `w/a/r/er/eb/rr` values, and the exact conclusion:

```text
positive attribution: <path>:<line> — evidence: <request/time/log facts>
unresolved: ruled out <items>; next probe is <bounded action>
```

- [ ] **Step 5: Commit the evidence note**

```bash
git add docs/superpowers/progress/2026-09-24-rr4-diagnosis.md
git commit -m "docs: record rr=4 panic evidence"
```

Do not create a firmware change. Task 6 is blocked when the result is unresolved unless the user explicitly waives the gate.

---

### Task 1: Move `charge_status` Decision Logic to Rust

**Files:**
- Create: `firmware/main/rust/src/charge_policy.rs`
- Create: `firmware/main/rust/include/charge_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt:RUST_SOURCES`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/charge_status.cc`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/charge_status.h` only if the POD snapshot requires an explicit accessor.
- Test: inline `charge_policy.rs` tests; compile gate covers C++ ABI.

**Interfaces:**
- Consumes normalized `detect_charging` and `full_high` facts, `now_ms`, and prior timestamp state.
- Produces `rf_charge_policy_decide(const rf_charge_policy_inputs_t*, rf_charge_policy_output_t*)` and a packed snapshot/next timestamp contract consumed by `ChargeStatus::Tick`.

- [ ] **Step 1: Write the failing normalized-polarity and transition tests**

Add tests with exact source constants:

```rust
#[test]
fn low_level_charging_condition_needs_400ms_to_become_stable() { /* assert boundary at 399 and 400 */ }

#[test]
fn power_present_is_held_for_1000ms() { /* assert exactly 1000 and 1001 */ }

#[test]
fn full_takes_precedence_over_charging_when_both_are_stable() { /* assert Full */ }

#[test]
fn alternating_detect_and_full_within_1500ms_is_no_battery() { /* assert NoBattery */ }

#[test]
fn no_power_resets_after_hold_window() { /* assert NoPower */ }
```

The test input field is `detect_charging`, never a physical `detect_high`. C++ is responsible for `gpio_get_level(detect_gpio) == CHARGE_DETECT_CHARGING_LEVEL`; current config means physical low maps to true.

- [ ] **Step 2: Run the focused test to see the intended red failure**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test charge_policy -- --nocapture
```

Expected before implementation: failure because the module/function does not exist or the test is unimplemented.

- [ ] **Step 3: Define the exact POD ABI**

Use signed 64-bit timestamps because `-1` is a real sentinel. The output must preserve the existing packed snapshot bits:

```text
state bits 0..7
power_present bit 8
charging bit 9
full bit 10
no_battery bit 11
```

Return next timestamps for the C++ object to write back; Rust must not hold hidden cross-task state.

- [ ] **Step 4: Implement the minimal transition function**

Copy the current `charge_status.cc` decision order exactly:

```text
power_present = last power event within 1000 ms
stable = now - condition_start_ms >= 400 ms
alt_seen = detect/full events within 1500 ms
no_battery = alt_seen && neither stable
NoPower if !power_present
Full if stable full && !no_battery
NoBattery if no_battery
Charging otherwise
```

- [ ] **Step 5: Add C ABI contract tests**

Test numeric state/action values, `charging` for both `kCharging` and `kNoBattery`, `full`, and the packed snapshot bit layout. Keep the FFI wrapper thin; it must call the same `decide` function.

- [ ] **Step 6: Run the sentinel mutation**

Temporarily change the 400 ms stability comparison from `>=` to `>`, run the boundary test, and confirm it fails at the intended assertion. Restore the implementation and rerun green.

- [ ] **Step 7: Wire C++ without changing GPIO or callback behavior**

In `ChargeStatus::Tick`:

```text
read GPIO
normalize detect level
call Rust decide with current timestamps
write returned timestamps
publish packed snapshot through existing UpdateSnapshot
```

Keep `Init` initial `NoPower` publication, packed-change callback semantics, and `ChargeStatus::Snapshot` consumers unchanged.

- [ ] **Step 8: Run task gates and commit**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cd ../../..
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Then:

```bash
git add firmware/main/rust/src/charge_policy.rs firmware/main/rust/include/charge_policy.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/boards/zectrix-s3-epaper-4.2/charge_status.cc firmware/main/boards/zectrix-s3-epaper-4.2/charge_status.h
git commit -m "refactor(firmware): move charge state policy to Rust"
```

---

### Task 2: Move LED Decision Logic to Rust

**Files:**
- Create: `firmware/main/rust/src/led_policy.rs`
- Create: `firmware/main/rust/include/led_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt:RUST_SOURCES`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc`
- Test: inline `led_policy.rs` tests.

**Interfaces:**
- Consumes normalized charge snapshot, override flags, phase, and activity pulse count.
- Produces an Action with `level`, `first_ms`, `has_second`, `second_level`, `second_ms`, `consume_pulse`, `first_wait_notify`, and `second_wait_notify`.

- [ ] **Step 1: Write six branch tests**

```rust
#[test] fn blink_override_uses_500ms_delay_and_toggles_phase() {}
#[test] fn static_override_uses_1000ms_notify() {}
#[test] fn idle_activity_pulse_uses_120ms_delay_then_180ms_delay() {}
#[test] fn full_uses_1000ms_notify() {}
#[test] fn charging_uses_200ms_delay_then_2800ms_notify() {}
#[test] fn idle_charge_state_uses_default_notify() {}
```

Assert the full tuple, not just duration. The activity pulse test must assert `consume_pulse=true` and both wait modes false; charging must assert first delay/second notify.

- [ ] **Step 2: Run focused red test**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test led_policy -- --nocapture
```

Expected: missing module/function failure.

- [ ] **Step 3: Implement the pure function and fixed ABI**

Copy the six current branches exactly. Do not infer wait mechanism from duration. Use explicit booleans for the two wait modes.

- [ ] **Step 4: Run sentinel mutation**

Remove the `has_second` field or change charging's second wait to delay; the charging and activity tests must fail. Restore and rerun.

- [ ] **Step 5: Replace `led_decide` with a C++ dispatch**

Keep `PowerLedTask` and all GPIO/FreeRTOS operations in C++. Call Rust once per loop, write GPIO level, consume pulse only when Action says so, and dispatch each wait using the explicit mode. Preserve the existing `!phase` write ordering for blink.

- [ ] **Step 6: Run full Rust and firmware gates**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cd ../../..
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Commit:

```bash
git add firmware/main/rust/src/led_policy.rs firmware/main/rust/include/led_policy.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc
git commit -m "refactor(firmware): move LED action policy to Rust"
```

---

### Task 3: Add the Wi-Fi Component Callback Shim

**Files:**
- Create: `firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.h`
- Create: `firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.cc`
- Modify: `firmware/main/components/78__esp-wifi-connect/CMakeLists.txt`
- Create: `firmware/main/wifi_policy_adapter.h`
- Create: `firmware/main/wifi_policy_adapter.cc`
- Create: `firmware/main/tests/wifi_policy_shim_test.cc` (host contract fixture)
- Modify: `firmware/main/application.cc`
- Modify: `firmware/main/components/78__esp-wifi-connect/wifi_manager.h`
- Modify: `firmware/main/components/78__esp-wifi-connect/wifi_manager.cc`
- Modify: `firmware/main/CMakeLists.txt`
- Test: host fixture plus C++ compile/link contract; no production Wi-Fi behavior change in this task.

**Interfaces:**
- Produces a component C ABI with `wifi_policy_register`, `wifi_policy_unregister`, and an invocation guard.
- The adapter owns a static registration context and is initialized after board/network setup.
- `WifiManager` exposes a process-level `WifiPolicyShutdown` hook invoked only during final teardown, not on ordinary `StopStation()` calls used for paint cuts, deep-sleep entry, Wi-Fi toggles, or reconnect. The adapter registers once during startup; every normal `StartStation()` uses the existing registration. Unregister occurs only in the final manager teardown path and is idempotent.

- [ ] **Step 1: Define the component callback POD before implementation**

Use a versioned `extern "C"` interface with a fixed input POD, Action POD, and function pointers. The callback has no ownership, no allocation, no callback back into the component, and no blocking operation. Registration returns success/failure; unregister is idempotent.

- [ ] **Step 2: Add a host contract fixture**

Create `firmware/main/tests/wifi_policy_shim_test.cc` with a minimal host harness. It must exercise:

```text
register(NULL) -> failure
unregister before register -> success/no-op
register callback -> success
unregister twice -> success/no-op
invoke without callback -> documented safe fallback
callback in flight while unregister begins -> callback finishes before unregister returns
```

Run the fixture directly with the host compiler (the shim fixture must not include ESP-IDF headers):

```bash
g++ -std=c++17 -pthread \
  -Ifirmware/main/components/78__esp-wifi-connect \
  firmware/main/tests/wifi_policy_shim_test.cc \
  firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.cc \
  -o /tmp/wifi_policy_shim_test
/tmp/wifi_policy_shim_test
```

- [ ] **Step 3: Implement callback storage in the component**

The component shim owns only the callback pointer/context, an invocation counter, and a short-lived guard. It must not include `main` headers or Rust headers. It must not call back into the Wi-Fi driver.

The adapter exposes `wifi_policy_adapter_register()` and `wifi_policy_adapter_unregister()`. Register once after the network/board initialization boundary. Do not unregister on ordinary `StopStation()` calls; they are used for paint cuts, deep-sleep entry, Wi-Fi toggles, and reconnects. Add a separate final-teardown hook on `WifiManager` that unregisters after no further Wi-Fi callback can run. Keep the adapter free of Rust object pointers.

- [ ] **Step 5: Add sources to CMake**

Add the component shim to `78__esp-wifi-connect/CMakeLists.txt`; add `wifi_policy_adapter.cc` to the main source list. Keep the host fixture outside the firmware link unless the existing test convention requires a separate test target; it must still be a tracked, runnable fixture.

- [ ] **Step 6: Run host fixture and firmware gates**

Run the exact host C++ fixture command after reading the repository test convention, then:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Confirm no undefined references and the registration/unregistration symbols are in the final link. Commit the fixture and all seam files:

```bash
git add firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.h firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.cc firmware/main/components/78__esp-wifi-connect/CMakeLists.txt firmware/main/wifi_policy_adapter.h firmware/main/wifi_policy_adapter.cc firmware/main/tests/wifi_policy_shim_test.cc firmware/main/application.cc firmware/main/components/78__esp-wifi-connect/wifi_manager.h firmware/main/components/78__esp-wifi-connect/wifi_manager.cc firmware/main/CMakeLists.txt
git commit -m "refactor(firmware): add WiFi policy callback seam"
```


---

### Task 4: Move Wi-Fi Cache, Fast Reconnect, and Endpoint Policy to Rust

**Files:**
- Create: `firmware/main/rust/src/wifi_policy.rs`
- Create: `firmware/main/rust/include/wifi_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt:RUST_SOURCES`
- Modify: `firmware/main/components/78__esp-wifi-connect/wifi_station.cc`
- Modify: `firmware/main/wifi_policy_adapter.cc`
- Test: inline Rust tests plus C++ compile/link gate.

**Interfaces:**
- Consumes cache bytes, age, SSID/BSSID/channel, endpoint target facts, probe result, reconnect count, and force-scan facts.
- Produces cache validity and actions `DirectConnect`, `Probe`, `Scan`, `Retry`, `Stop`, `DeferProbe { retain_ip: true }`, plus cache-clear flags.

- [ ] **Step 1: Write cache codec red tests**

Cover the exact current RTC layout:

```text
magic 0x52465731
ssid[33], bssid[6], channel
```

Tests must cover wrong magic, empty SSID, missing NUL at byte 32, zero BSSID, all-FF BSSID, valid record, and a fixed byte-layout round trip.

- [ ] **Step 2: Write endpoint parsing red tests**

Use the exact existing split:

```text
MQTT ParseEndpoint accepts plain host[:port], default 8883
MQTT keeps a nonnumeric suffix as part of the host when the colon is not followed by digits
WebSocket/OTA ParseUrlAuthority requires http://, https://, ws://, or wss://
http/ws default port 80
https/wss default port 443
URL mode keeps a nonnumeric authority suffix as part of the host, as the current parser does
URL mode rejects only a missing/unsupported scheme or empty authority/host
accept IPv4 literal where the existing parser accepts it
accept explicit/default numeric port
```

Do not add general URL validation, scheme rejection to MQTT, or numeric-port validation to URL mode.

- [ ] **Step 3: Write policy transition red tests**

Cover direct connect, stale cache, scan, retry, stop, association clear, IP clear, and endpoint missing:

```rust
assert_eq!(decide(input_with_missing_endpoint()).action, DeferProbe { retain_ip: true });
assert!(!decide(input_with_missing_endpoint()).clear_ip_cache);
assert!(decide(input_with_dns_failure()).clear_ip_cache);
```

Cover exact age boundary `kIpFastMaxAgeMs = 3_600_000` and all current fast-connect constants from `wifi_station.cc`.

- [ ] **Step 4: Run focused red tests**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test wifi_policy -- --nocapture
```

Expected: missing module/function failures.

- [ ] **Step 5: Implement the pure policy and fixed POD ABI**

Rust owns the codec and decision table. C++ still owns the actual `FastRcCache`/RTC storage and ESP-IDF calls. The Action must distinguish `retain_ip` from all probe failures.

- [ ] **Step 6: Prove the endpoint sentinel test bites**

Change the missing-endpoint action to `clear_ip_cache`; the `retain_ip` test must fail. Restore and rerun green.

- [ ] **Step 7: Wire the main adapter and component seam**

The adapter maps component facts to Rust input and returns Action fields. `wifi_station.cc` calls the registered callback at the existing decision points, then executes the Action using existing mechanisms.

Replace the current `endpoint_missing → IpFastFallback("endpoint_missing")` path with:

```text
DeferProbe:
  stop this IP fast attempt
  restart DHCP
  retain IP fast cache
  retain association cache/RTC mirror
  return without DNS/TCP probe
```

Do not clear IP cache for missing endpoint. All other `IpFastFallback` reasons preserve their current clear behavior.

Endpoint configuration is not hot-reloaded in this task. The next existing STA connect/fast-attempt reads NVS/config again; if endpoint is then present and IP cache is still fresh, probe resumes.


- [ ] **Step 8: Run full gates and commit the 3a policy migration**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cd ../../..
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Commit the language-only policy/cache migration first:

```bash
git add firmware/main/rust/src/wifi_policy.rs firmware/main/rust/include/wifi_policy.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/components/78__esp-wifi-connect/wifi_station.cc firmware/main/wifi_policy_adapter.cc
git commit -m "refactor(firmware): move WiFi cache and reconnect policy to Rust"
```

- [ ] **Step 9: Add and verify the 3b endpoint_missing behavior fix separately**

Add the endpoint-missing host test first, then change only the missing-endpoint branch to return `DeferProbe { retain_ip: true }` while preserving all other `IpFastFallback` clear behavior. Run the focused test, full `cargo test`, and `idf.py build`, then commit separately:

```bash
git add firmware/main/rust/src/wifi_policy.rs firmware/main/rust/include/wifi_policy.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/components/78__esp-wifi-connect/wifi_station.cc firmware/main/wifi_policy_adapter.cc
git commit -m "fix(firmware): retain IP cache when endpoint is missing"
```

### Task 5: Classify Pairing Responses in Rust

**Files:**
- Create: `firmware/main/rust/src/pairing_response.rs`
- Create: `firmware/main/rust/include/pairing_response.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt:RUST_SOURCES`
- Modify: `firmware/main/common/server_pairing.cc`
- Test: inline Rust tests and `cargo test`; compile gate for C++ adapter.

**Interfaces:**
- C++ parses cJSON and passes status plus facts: JSON-valid, code-is-string, expires-is-number, token-is-string, token-nonempty.
- Rust returns a response classification mapped to existing `rf_pairing_outcome_t`; it does not replace `pairing.rs::decide`.

- [ ] **Step 1: Write pair-start classification tests**

```rust
#[test] fn pair_start_requires_200_valid_string_code_and_number_expiry() {}
#[test] fn pair_start_type_mismatch_is_failure() {}
#[test] fn pair_start_non_200_or_transport_error_is_failure() {}
```

- [ ] **Step 2: Write pair-claim compatibility tests**

```rust
#[test] fn claim_200_with_any_nonempty_token_is_granted() {}
#[test] fn claim_200_without_token_or_with_invalid_json_is_pending() {}
#[test] fn claim_401_or_429_is_rejected() {}
#[test] fn other_statuses_and_transport_errors_are_network_errors() {}
```

Do not add token length/charset validation.

- [ ] **Step 3: Run focused red tests**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test pairing_response -- --nocapture
```

- [ ] **Step 4: Implement the classifier and fixed POD ABI**

Keep the result numeric mapping explicit and test the mapping. `cJSON_IsString(token_item)` alone is insufficient for the `token_nonempty` fact; C++ must pass the first-byte/nonempty result separately.

- [ ] **Step 5: Run a sentinel mutation**

Change the HTTP 200 empty-token result from `PENDING` to `NETWORK_ERROR`; the compatibility test must fail. Restore and rerun.

- [ ] **Step 6: Wire C++ response facts without moving transport**

Change only response classification after HTTP/cJSON parsing. Preserve `server_pairing_run`’s existing `rf_pairing_decide` loop, NVS token write, UI callback, and delay execution.

- [ ] **Step 7: Run gates and commit**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cd ../../..
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Commit:

```bash
git add firmware/main/rust/src/pairing_response.rs firmware/main/rust/include/pairing_response.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/common/server_pairing.cc
git commit -m "refactor(firmware): classify pairing responses in Rust"
```

---

### Task 6: Move EPD Diff and Refresh Eligibility to Rust (Blocked by Task 0)

**Files:**
- Create: `firmware/main/rust/src/epd_policy.rs`
- Create: `firmware/main/rust/include/epd_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt:RUST_SOURCES`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc`
- Test: inline Rust tests and `cargo test`; compile gate for C++.

**Interfaces:**
- C++ completes `tx_buf` snapshot and releases `dirty_mutex` before calling Rust.
- Rust receives read-only `prev_buffer` and `tx_buffer` pointers, lengths, width/height, `bytes_per_row`, and the current policy state.
- Rust returns `SyncBaselineAndSkip`, `SkipNoDiff`, `SkipTiny`, `RefreshFull`, or `RefreshPartial`, plus required `next_tiny_*` fields.

- [ ] **Step 1: Stop if Task 0 is unresolved**

- [ ] **Step 2: Write frame diff red tests**

Cover current constants exactly. The canonical ABI unit is **milliseconds**:

```text
kMinDiffBitRatio = 0.001
kForceFullDiffRatio = 0.30
kTinyMaxStreak = 4
kTinyMaxAccumBits = 512
kTinyMaxHoldMs = 1200 (current source: pdMS_TO_TICKS(1200) at HZ=100)
```

C++ converts the current FreeRTOS tick difference to milliseconds before calling Rust. The persisted ABI field is `next_tiny_first_ms`, not a raw tick value. Host tests cover 1199/1200/1201 ms and the exact C++ conversion is covered by a contract assertion.
Required precedence, matching current C++ order, is:

```text
wake-baseline candidate -> SyncBaselineAndSkip
diff_bits == 0 && !force_full -> SkipNoDiff
tiny-diff gate -> SkipTiny or normal refresh
!prev_buffer_synced || !prev_buffer -> RefreshFull
force_full -> RefreshFull
is_four_color -> RefreshFull
diff_ratio >= 0.30 -> RefreshFull
partial_since_full >= 10 -> RefreshFull
otherwise -> RefreshPartial
```

This explicitly means an unsynced baseline with zero diff returns `SkipNoDiff` before the later full-refresh predicates, because the current C++ checks no-diff first. Add a test for `prev_buffer_synced=false` plus `diff_bits=0` that expects `SkipNoDiff`; this is a preservation requirement, not an inferred improvement.
Additional action assertions:

```text
tiny below threshold -> SkipTiny + next state
tiny at streak/accum/hold threshold -> RefreshFull/RefreshPartial + tiny zero
urgent/force -> bypass tiny skip
```

- [ ] **Step 4: Run focused red tests**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test epd_policy -- --nocapture
```

- [ ] **Step 5: Implement the fixed ABI and pure policy**

Use an explicit tick unit in the ABI. Pass elapsed tiny time as milliseconds or a converted tick duration; do not make host tests depend on FreeRTOS `TickType_t`. Rust returns the three `next_tiny_*` fields on every Action. C++ writes them after the EPD call returns, matching current behavior when the C++ EPD methods return `void` and may log timeout but continue.

- [ ] **Step 6: Prove state transitions bite**

Temporarily clear tiny state on `SyncBaselineAndSkip`; the preservation test must fail. Restore and rerun. Then temporarily remove the `is_four_color -> RefreshFull` branch; the four-color test must fail. Restore and rerun.

- [ ] **Step 7: Replace the C++ decision block**

Keep debounce, sampling interval, mutex snapshot, `sm_kick`, EPD calls, framebuffer ownership, and callback/idle handling in C++. Replace only `analyze_frame_diff` plus the diff/tiny/full/partial decision block with one Rust call outside `dirty_mutex`. Keep:

```text
RefreshFull → EPD_Init(); EPD_Display(); partial_since_full=0
RefreshPartial → EPD_Init(); EPD_DisplayPart(); partial_since_full++
```

Both branches write back the Rust-returned zero tiny state after the call returns. `SyncBaselineAndSkip` writes back its returned current state without clearing it.

- [ ] **Step 8: Run full gates and commit**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cd ../../..
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

Commit:

```bash
git add firmware/main/rust/src/epd_policy.rs firmware/main/rust/include/epd_policy.h firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc
git commit -m "refactor(firmware): move EPD refresh policy to Rust"
```

---

### Task 7: Final Cross-Layer Verification

**Files:**
- No source changes unless a task's own gate exposes a defect.
- Verify: Rust host suite, firmware build, ELF symbols, service binary/artifact, and device evidence.

- [ ] **Step 1: Run Rust host suite**

```bash
cd firmware/main/rust
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
```

- [ ] **Step 2: Run the final firmware build in the main session**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```

- [ ] **Step 3: Verify symbols and artifact**

```bash
xtensa-esp32s3-elf-nm firmware/build/xiaozhi.elf | grep -E 'charge_policy|led_policy|wifi_policy|pairing_response|epd_policy|rf_last_reset_reason'
md5sum firmware/build/xiaozhi.bin
cp firmware/build/xiaozhi.bin server/data/firmware/xiaozhi.bin
cmp firmware/build/xiaozhi.bin server/data/firmware/xiaozhi.bin
```

- [ ] **Step 4: Perform hardware checks by task**

Record each as observed or not observed:

```text
Charge: insert/remove/charging/full transitions
LED: activity pulse, charging blink/static behavior
Wi-Fi: disconnect/reconnect, endpoint_missing retention/recovery
Pairing: pair-start success, pending, rejected, network retry
EPD: unchanged bitmap, changed bitmap, full/partial, rr=4 status
```

Do not open serial during battery/deep-sleep checks. Do not claim EPD success if Task 0 remains unresolved.

- [ ] **Step 5: Report unverified boundaries explicitly**

The final report must distinguish:

```text
host-tested Rust policy
compile-linked C++/Rust ABI
server-observed device requests
device-observed charge/LED/WiFi/pairing/EPD behavior
unverified hardware or blocked rr=4 attribution
```
