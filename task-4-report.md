# Task 4a Report

- Replaced the `wifi_policy_adapter.cc` TODO callback with fixed-POD conversion to `rf_wifi_policy_inputs_t` / `rf_wifi_policy_output_t`, the existing `rf_wifi_policy_decide` C ABI call, and shim action mapping.
- Unregistered callbacks return `false`; the shim therefore retains its zero-initialized no-op fallback.
- Added the minimal endpoint-missing policy decision invocation in `wifi_station.cc`; the existing `IpFastFallback("endpoint_missing")` path remains preserved regardless of policy availability.
- Rust policy C ABI and CMake source registration are included in the Task 4a firmware changes.

## Verification

- `cargo test`: BLOCKED — `cargo` is not installed / not on PATH in this environment (`command not found: cargo`).
- `idf.py build`: BLOCKED — `idf.py` is not installed / not on PATH in this environment (`which idf.py` returned no path).
- `git diff --check`: PASS.
