# Task 2 Report — Server `verify_device_signature` + nonce replay + whitelist

**Date:** 2026-09-03
**Commit:** `366c8937ff24ffd6834b20124ed2be82c8c1f1ef`
**Status:** ✅ Complete — all tests pass.

## What was implemented

### `server/youn_server/pairing.py`
- Imports: added `base64`, `hashlib`, `hmac`, and `from .config import settings`.
- Constants:
  - `TIMESTAMP_WINDOW_SEC = 30` (±30s clock-skew window)
  - `NONCE_CACHE_TTL_SEC = 300` (5-minute in-memory replay window)
- `PairingStore.__init__`: added `self._nonce_cache: dict[str, float] = {}`
  (`"device_id:nonce"` → first-seen epoch seconds).
- `PairingStore.verify_device_signature(device_id, mac, timestamp, nonce, signature_b64) -> bool`
  1. Rejects if `MASTER_KEY` empty or `< 32` bytes (returns `False`, logs error).
  2. Rejects if `abs(now - timestamp) > 30` (covers stale **and** far-future).
  3. Derives `derived_key = HMAC-SHA256(MASTER_KEY, device_id)`.
  4. Parses MAC via `bytes.fromhex`; requires exactly **6 bytes** (else `False`).
  5. Reconstructs payload `MAC(6) || timestamp(ASCII) || nonce(ASCII)`.
  6. Base64-decodes the provided signature (any decode error → `False`).
  7. Constant-time compare via `hmac.compare_digest` (mismatch → `False`).
  8. Nonce replay: under `self._lock`, evicts entries older than 300s, then
     rejects if `"device_id:nonce"` is already present; otherwise records it.
  - Never raises at the auth boundary; logs the reason for each rejection.
- `PairingStore.check_whitelist(device_id) -> bool`: parses
  `settings.allowed_device_ids` (comma-separated **string** per Task 1),
  strips whitespace, ignores empty entries; empty whitelist ⇒ all accepted.
  Also tolerates a list/tuple value defensively.

### `server/tests/test_device_signature.py`
Added 5 tests (unit-level, calling `PairingStore` directly via a `store`
fixture on an isolated tmp DB), plus a shared `_sign(...)` helper:

| Test | Asserts |
|------|---------|
| `test_verify_signature_invalid_key` | signature from a wrong MASTER_KEY → `False` |
| `test_verify_signature_timestamp_out_of_window` | timestamp 60s old → `False` |
| `test_verify_signature_nonce_replay` | same (device,nonce): 1st `True`, 2nd `False` |
| `test_verify_signature_invalid_mac` | non-hex MAC string → `False` |
| `test_check_whitelist_rejects_unlisted` | empty wl allows all; populated wl allows only listed ids |

## Test output

```
tests/test_device_signature.py::test_master_key_required PASSED                         [ 12%]
tests/test_device_signature.py::test_signature_valid PASSED                             [ 25%]
tests/test_device_signature.py::test_signature_missing_headers PASSED                   [ 37%]
tests/test_device_signature.py::test_verify_signature_invalid_key PASSED                [ 50%]
tests/test_device_signature.py::test_verify_signature_timestamp_out_of_window PASSED    [ 62%]
tests/test_device_signature.py::test_verify_signature_nonce_replay PASSED               [ 75%]
tests/test_device_signature.py::test_verify_signature_invalid_mac PASSED                [ 87%]
tests/test_device_signature.py::test_check_whitelist_rejects_unlisted PASSED            [100%]
======================== 8 passed, 3 warnings in 1.76s =========================
```

TDD step-2 (pre-implementation) result was the exact expected failure:
`AttributeError: 'PairingStore' object has no attribute 'verify_device_signature'`
(and `... 'check_whitelist'`) — **5 failed, 3 passed**.

Regression: `tests/test_pairing.py` → **15 passed** (module I modified).
(Full suite intentionally NOT run — mid-flight sibling tasks; main agent runs it.)

## Deviations from the plan's literal Step-1 snippet (and why)

1. **Tests are unit tests on `PairingStore`, not HTTP calls.** The plan's
   Step-2 expects `AttributeError: 'PairingStore' object has no attribute
   'verify_device_signature'` — that error only surfaces when tests call the
   store directly; HTTP-level tests against the current (Task 1) endpoint
   would return **200**, not AttributeError. Task 3 explicitly owns the
   `app.py` endpoint wiring (`verify_device_signature` + `check_whitelist`
   calls), and my assigned file scope is `pairing.py` + tests only. So the 5
   tests call the store directly; the HTTP-level behavior (401s, whitelist
   `detail` message) is covered by Task 3's endpoint + integration test.

2. **Whitelist test combined and renamed.** The plan's
   `test_signature_whitelist_reject` asserts the HTTP response contains
   `"whitelist"` — but that detail message is produced by the Task 3
   endpoint (`detail="device not in whitelist"`), which does not exist yet.
   I implemented `test_check_whitelist_rejects_unlisted` at unit level
   (empty→allow-all, listed→allowed, unlisted→denied), which directly
   validates `check_whitelist`. The HTTP `"whitelist"` assertion belongs to
   Task 3.

3. **Reused inline signing helper rather than `tests/device_sig.py`.**
   `signed_headers()` in that helper always generates a **fresh random
   nonce** (deliberately, to dodge the replay cache) and returns header
   dicts — it cannot express Task 2's needs (a fixed/replayed nonce, a
   forced old timestamp, a wrong key, a bad MAC). I added a local `_sign()`
   helper in the test file that builds the raw signature over
   `MAC||timestamp||nonce` with those knobs. The existing Task-1 tests are
   untouched.

4. **`settings.allowed_device_ids` is a `str`** (Task 1 config) — handled in
   `check_whitelist`; the stale `settings.allowed_device_ids = []` line in
   the Task 1 fixture would raise on pydantic v2 but is out of my scope
   (conftest.py's autouse fixture already manages `master_key`; the
   Task-1-fixture `= []` assignment is harmless to Task 2 tests since they
   set the whitelist explicitly and restore it).

## Concerns / notes for Task 3

- **Endpoint wiring is pending.** `pair_start` in `app.py` still only checks
  `settings.master_key` + header presence (Task 1 behavior) and does **not**
  call `verify_device_signature` / `check_whitelist` yet. Until Task 3 lands,
  a correctly-*shaped* but invalidly-*signed* request will still 200 at HTTP
  level. Task 3's plan code already calls both methods with the exact
  signatures implemented here:
  `verify_device_signature(device_id, mac, timestamp:int, nonce, sig)` and
  `check_whitelist(device_id)`.
- **Nonce cache is per-store-instance, in-memory.** Cleared on restart
  (acceptable per plan). It is bounded lazily on each successful verification
  (entries evicted after 300s). The shared module-level `_pairing_store`
  makes replay protection effective across requests.
- **Timestamp parsed in the endpoint** (`int(ts_str)`) per Task 3; the store
  method receives an `int`.
- MAC must be exactly 6 bytes; both `fromhex` failure and wrong length
  return `False` (logged distinctly).
