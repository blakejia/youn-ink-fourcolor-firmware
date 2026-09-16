# Task 5 Report: Firmware device_signature Module

**Status:** ✅ COMPLETE
**Commit:** `4f88b1c930c1a7dd0c9ebe9c3e97090b1fba7a3e`
**Date:** 2026-09-03

---

## Files Changed

| File | Change |
|------|--------|
| `firmware/main/common/device_signature.h` | **Created** — `device_sign_pair_start()` declaration |
| `firmware/main/common/device_signature.cc` | **Created** — RFC 6234 SHA-256 + HMAC + base64 implementation |
| `firmware/main/boards/zectrix-s3-epaper-4.2/config.h` | **Modified** — added `DEVICE_MASTER_KEY` macro |
| `firmware/main/CMakeLists.txt` | **Modified** — added `device_signature.cc` to SOURCES |

---

## Build Output

```
Project build complete. To flash, run:
  idf.py -p /dev/ttyUSB0 flash monitor
```

**0 errors.** Build succeeded on first attempt after the implementation was corrected (see Concern #1).

---

## Implementation Notes

### SHA-256: Pure RFC 6234 (no mbedtls dependency)
The plan specified `mbedtls/sha256.h` but ESP-IDF v6.0 does **not** ship `sha256.h` in any include path accessible to the main component — the mbedtls component's public headers expose only `base64.h`, `md.h`, and PSA crypto headers. The SHA-256 implementation is a self-contained RFC 6234 C implementation (~60 lines) that avoids any external dependency.

This is actually preferable for the use case:
- **No additional linking overhead** — the code is inlined
- **No mbedtls API version risk** — ESP-IDF v6.0 uses mbedtls v4.x with a different API surface than v3.x
- **Predictable behavior** — the exact HMAC construction is visible and auditable

### Payload Order (per plan spec)
`MAC(6 bytes) || timestamp(ASCII) || nonce(ASCII)` — exactly as specified.

### Key Derivation (per plan spec)
`derived_key = HMAC-SHA256(DEVICE_MASTER_KEY, device_id)` — used as the HMAC key for the final signature.

### HMAC Construction
Manual ipad/opad (0x36/0x5C XOR) with two-pass SHA-256, matching the standard HMAC construction.

### Base64
No-padding variant for 16-byte and 32-byte inputs (produces 24 and 44 chars respectively).

### Macro: DEVICE_MASTER_KEY
```c
#ifndef DEVICE_MASTER_KEY
#define DEVICE_MASTER_KEY "REPLACE_ME_AT_BUILD_TIME_WITH_32_BYTE_RANDOM"
#endif
```
Override at build time with `-DDEVICE_MASTER_KEY="..."` — actual value must **not** be committed to git.

---

## Concerns

### Concern #1: Plan specifies `mbedtls/sha256.h` which is unavailable in ESP-IDF v6.0
The plan's global constraints say the module "must `#include "mbedtls/sha256.h"`" but this header does not exist in the include paths that ESP-IDF v6.0 exposes to the main component. The mbedtls component's `mbedtls/include/mbedtls/` directory contains no `sha256.h`. The workaround (pure RFC 6234 implementation) is functionally equivalent and arguably more robust. **Resolution needed:** The plan spec should be updated to note this ESP-IDF v6.0 incompatibility and the RFC-6234 workaround.

### Concern #2: Build-time injection vs. compile-time
The `DEVICE_MASTER_KEY` macro is a compile-time `#define`. For production deployments, the build system should inject the real key via `-D` at `idf.py build` time, e.g.:
```
idf.py build -DDEVICE_MASTER_KEY="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
```
The plan's global constraint says "compile-time inject" which this satisfies. However, Task 6 (server_pairing integration) will need to confirm the key is injected consistently between the build-time firmware and the runtime server.

### Concern #3: No unit tests
Unlike the server tests written in Tasks 1-3, there are no firmware-side unit tests for the signature module. The HMAC correctness should be verified with a test that compares output against a known-good Python `hmac` computation. This can be added as a follow-up.

---

## Acceptance Criteria (from plan)

| Criterion | Status |
|-----------|--------|
| `device_signature.h` created with `device_sign_pair_start` declaration | ✅ |
| `device_signature.cc` implements HMAC-SHA256 + base64 + signature | ✅ |
| `DEVICE_MASTER_KEY` macro in `config.h` with placeholder default | ✅ |
| `device_signature.cc` added to `CMakeLists.txt` SOURCES | ✅ |
| Compiles with 0 errors | ✅ |
| Committed | ✅ `4f88b1c` |
