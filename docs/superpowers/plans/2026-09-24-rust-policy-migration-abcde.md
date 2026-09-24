# NOTE4C ABCDE Rust Policy Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在保持单一 `librust_firmware.a` 和 C++ 硬件机制边界的前提下，将 RTC/SNTP 时间门、通知策略、电池相对活动估算、页面比较和协议解析五类纯策略迁入 Rust，并逐项通过 TDD、ESP-IDF 编译和 scoped review。

**Architecture:** 五个 Rust 模块分别拥有独立输入事实、固定 `#[repr(C)]` POD C ABI 和纯输出动作；C++ 继续负责 RTC/SNTP、HTTP、ADC、NVS、JSON DOM、文件、FreeRTOS、Wi-Fi 和 EPD。每个模块独立接入生产路径、独立提交和复审，禁止共享隐式 Rust 全局状态。

**Tech Stack:** Rust 2024 `no_std` on Xtensa, `cargo test` host tests, ESP-IDF v6.0, C++17 C ABI, existing single staticlib link.

**Spec:** `docs/superpowers/specs/2026-09-24-rust-policy-migration-abcde-design.md`

## Global Constraints

- `export PATH="$HOME/.cargo/bin:$PATH"` before sourcing ESP-IDF.
- Keep one `librust_firmware.a`; do not add a second archive or panic handler.
- Every new Rust source is registered in `firmware/main/CMakeLists.txt` `RUST_SOURCES`.
- Rust owns pure policy/state/parse/compare/encode decisions only.
- C++ owns GPIO, I2C, SPI, NVS, FreeRTOS, Wi-Fi driver, DNS/ARP/TCP, HTTP, JSON DOM, file I/O, EPD waveform and task lifecycle.
- Every C ABI input/output uses `#[repr(C)]` and a fixed documented layout.
- TDD is mandatory: failing test, minimal implementation, sentinel mutation, restore, full tests.
- Do not claim hardware success from host tests; record device checks separately.
- Do not migrate EPD diff/refresh policy; `rr=4` remains unresolved.
- Do not push commits without a separate user request.

---

### Task 1: Establish shared Rust policy ABI and registration

**Files:**
- Modify: `firmware/main/CMakeLists.txt` if module registration needs shared helper wiring
- Create/modify only the shared Rust module registration files already used by the repository
- Create: `firmware/main/rust/include/abcde_policy_abi.h` only if an existing shared ABI convention requires it
- Test: inline `#[cfg(test)]` tests in the first new module

**Interfaces:**
- Produces the fixed `#[repr(C)]` POD/action conventions consumed by Tasks 2–6.
- Produces no hardware or global state.

- [ ] **Step 1: Inspect existing charge/LED/Wi-Fi/pairing ABI conventions**
- [ ] **Step 2: Write a failing layout/action test for the shared ABI convention**
- [ ] **Step 3: Run the focused test and confirm the expected failure**
- [ ] **Step 4: Implement the smallest shared layout contract**
- [ ] **Step 5: Run the focused test and `cargo test`**
- [ ] **Step 6: Run `git diff --check`, commit, and obtain scoped review**

### Task 2: Migrate RTC/SNTP time-gate policy

**Files:**
- Create: `firmware/main/rust/src/time_gate_policy.rs`
- Create: `firmware/main/rust/include/time_gate_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt`
- Modify: the existing C++ time/RTC call sites identified by source search
- Test: inline Rust tests

**Interfaces:**
- Inputs: valid/invalid clock facts, last persisted timestamp, period configuration, clock regression fact.
- Outputs: allow/defer/clear/retain actions for SNTP, RTC cache, battery sample and Wi-Fi cache writes.

- [ ] **Step 1: Write failing tests for valid clock, invalid clock, elapsed gate, and clock regression**
- [ ] **Step 2: Run focused tests and confirm failures are caused by the missing decision**
- [ ] **Step 3: Implement the pure policy and C ABI**
- [ ] **Step 4: Run focused tests and sentinel mutation**
- [ ] **Step 5: Wire the ABI to existing C++ call sites without moving RTC/SNTP/NVS**
- [ ] **Step 6: Run full `cargo test` and ESP-IDF `idf.py build`**
- [ ] **Step 7: Write task report, commit, and obtain scoped review**

### Task 3: Migrate notification pull/rate policy

**Files:**
- Create: `firmware/main/rust/src/notify_policy.rs`
- Create: `firmware/main/rust/include/notify_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt`
- Modify: existing C++ notification task/transport call sites
- Test: inline Rust tests

**Interfaces:**
- Inputs: busy state, last pull/success/failure times, notification id/etag/hash, transport/status category, JSON/`.bin` response category.
- Outputs: pull/defer/deduplicate/retry/consume actions.

- [ ] **Step 1: Write failing tests for idle, busy, duplicate, success, failure backoff, binary/JSON fallback**
- [ ] **Step 2: Confirm red tests, implement minimal policy**
- [ ] **Step 3: Add sentinel mutation and restore**
- [ ] **Step 4: Wire C++ to pass facts and execute actions while retaining HTTP/file/task ownership**
- [ ] **Step 5: Run `cargo test`, ESP-IDF build, and task report**
- [ ] **Step 6: Obtain scoped review and fix any Critical/Important findings**

### Task 4: Migrate battery relative-activity policy

**Files:**
- Create: `firmware/main/rust/src/battery_activity_policy.rs`
- Create: `firmware/main/rust/include/battery_activity_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt`
- Modify: existing battery sampling/reporting call sites
- Test: inline Rust tests

**Interfaces:**
- Inputs: normalized voltage, charge direction, previous sample, sampling gate facts.
- Outputs: accept/filter/direction/relative-activity and next-gate actions.

- [ ] **Step 1: Write failing tests for monotonic samples, charge-direction change, invalid/outlier voltage, and gate expiry**
- [ ] **Step 2: Confirm red tests; implement minimal policy**
- [ ] **Step 3: Add sentinel mutation and restore**
- [ ] **Step 4: Wire C++ while retaining ADC/NVS/history/HTTP ownership**
- [ ] **Step 5: Assert no output name or contract implies SOC/mAh**
- [ ] **Step 6: Run full host/ESP-IDF gates, report, commit, review**

### Task 5: Migrate page content/version comparison

**Files:**
- Create: `firmware/main/rust/src/page_compare_policy.rs`
- Create: `firmware/main/rust/include/page_compare_policy.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt`
- Modify: existing page sync/cache call sites
- Test: inline Rust tests

**Interfaces:**
- Inputs: local metadata, server metadata, binding/rotation facts, cache validity and transport result.
- Outputs: `Fetch`, `SkipSame`, `UseCache`, `InvalidateCache` and continuation action.

- [ ] **Step 1: Write failing tests for same hash, changed hash, missing metadata, cache expiry, and failure fallback**
- [ ] **Step 2: Confirm red tests; implement minimal comparison policy**
- [ ] **Step 3: Add sentinel mutation and restore**
- [ ] **Step 4: Wire C++ while retaining HTTP/JSON/file/EPD ownership**
- [ ] **Step 5: Verify no EPD diff/refresh policy was moved**
- [ ] **Step 6: Run full gates, report, commit, review**

### Task 6: Migrate protocol parsers and status classification

**Files:**
- Create: `firmware/main/rust/src/protocol_parse.rs`
- Create: `firmware/main/rust/include/protocol_parse.h`
- Modify: `firmware/main/rust/src/lib.rs`
- Modify: `firmware/main/CMakeLists.txt`
- Modify: existing schedule/OTA/notification/battery parsing call sites
- Test: inline Rust tests

**Interfaces:**
- Inputs: C++-extracted short strings/JSON facts and HTTP status/transport categories.
- Outputs: fixed parsed structs and business classifications.

- [ ] **Step 1: Write failing tests for schedule fields, battery query fields, OTA manifest, notification response, invalid JSON, and status mapping**
- [ ] **Step 2: Confirm red tests; implement minimal parsers preserving loose compatibility**
- [ ] **Step 3: Add sentinel mutation and restore**
- [ ] **Step 4: Wire C++ without moving JSON DOM/HTTP/TLS/file behavior**
- [ ] **Step 5: Verify MQTT host[:port], URL authority, invalid JSON and old JSON fallback semantics**
- [ ] **Step 6: Run full gates, report, commit, review**

### Task 7: Final cross-module verification and review

**Files:**
- Modify only if a review finding requires a scoped fix
- Do not add unrelated refactors

- [ ] **Step 1: Run full `cargo test`**
- [ ] **Step 2: Run `idf.py build` with required PATH order**
- [ ] **Step 3: Verify all new Rust symbols are in the linked archive/ELF**
- [ ] **Step 4: Run `git diff --check` and inspect untracked runtime artifacts without staging them**
- [ ] **Step 5: Dispatch final whole-branch reviewer with all task reports and deferred findings**
- [ ] **Step 6: Fix Critical/Important findings, re-review, and only then mark complete**

## Completion Criteria

- Tasks 1–6 each have an implementer report and scoped reviewer approval.
- Full Rust host tests and ESP-IDF build pass at final tip.
- A–E production paths call Rust policy ABIs; no policy remains only in C++ without an explicit documented reason.
- Battery output remains relative activity only.
- EPD policy remains untouched and `rr=4` remains a documented blocker.
- No push occurs without a separate user instruction.
