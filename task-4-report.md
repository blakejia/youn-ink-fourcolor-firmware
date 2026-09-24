# Task 4a Report

- Replaced the `wifi_policy_adapter.cc` TODO callback with fixed-POD conversion to `rf_wifi_policy_inputs_t` / `rf_wifi_policy_output_t`, the existing `rf_wifi_policy_decide` C ABI call, and shim action mapping.
- Unregistered callbacks return `false`; the shim therefore retains its zero-initialized no-op fallback.
- Added the minimal endpoint-missing policy decision invocation in `wifi_station.cc`; the existing `IpFastFallback("endpoint_missing")` path remains preserved regardless of policy availability.
- Rust policy C ABI and CMake source registration are included in the Task 4a firmware changes.
- Added production C ABI exports for RTC cache encode/decode/validate; production WifiStation cache save/seed now calls Rust while C++ owns RTC storage.
- Endpoint resolution now delegates MQTT and URL parsing to the Rust C ABI, preserving existing lax parser semantics.

## Verification

- `cargo test` (with `PATH=$HOME/.cargo/bin:$PATH`): PASS — 260 tests passed across 3 suites.
- `idf.py build` (with ESP-IDF v6.0 export): PASS — project build complete; `xiaozhi.bin` generated.
- Build emitted existing ESP-SR/Kconfig and linker notes only; no Task 4a compile errors.
- `git diff --check`: PASS.
- Final P2: Rust URL parser now preserves case-insensitive HTTP/HTTPS/WS/WSS schemes with an allocation-free ASCII lowering check; added host contract coverage for all four uppercase schemes. Post-fix `cargo test`: 261 passed; `idf.py build`: PASS.
- Final review fixes: explicit `retain_ip` action bit is preserved by the adapter; endpoint presence is checked before ARP/GW/DNS/TCP and endpoint-missing defer retains IP/association/RTC cache while restarting DHCP; reconnect decisions now flow through Rust Retry/Stop/clear-Wi-Fi-cache actions. Added reconnect host coverage. `cargo test`: 275 passed; full `idf.py build`: PASS.
