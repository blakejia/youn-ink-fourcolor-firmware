# Task 3 Report — app.py pair-start endpoint signature verification

**Date:** 2026-09-03
**Status:** ✅ Complete
**Commit:** `de1a963` — `feat(server): pair-start requires HMAC device signature`

## Changes

### `server/youn_server/app.py` (pair_start endpoint, lines 260–298)

Replaced Task 1's placeholder (MASTER_KEY presence + header presence only)
with the full authentication chain, in this order:

1. `MASTER_KEY` not configured → **401** `"device authentication failed"`
   (kept from Task 1; checked *before* header presence so an unconfigured
   server never reveals the expected request shape).
2. Missing any of `X-Device-Mac` / `X-Device-Timestamp` / `X-Device-Signature`
   → **400** `"missing device auth headers"`.
3. Non-integer timestamp → **400** `"invalid timestamp"`.
4. `PairingStore.verify_device_signature(device_id, mac, ts, nonce, sig)`
   fails (bad HMAC, ±30s window, bad MAC hex, nonce replay — all Task 2)
   → **401** `"device authentication failed"`.
5. `PairingStore.check_whitelist(device_id)` fails → **401**
   `"device not in whitelist"` (checked after auth so whitelist existence
   is not leaked to unauthenticated callers).
6. Existing `check_rate_limit(ip)` → **429** (preserved; the plan's snippet
   dropped it, but `test_pair_start_rate_limit` requires it).
7. `registry.upsert` + `create_session` → **200** `{code, expires_in}`.

### `server/tests/test_device_signature.py` (+1 integration test)

Added `test_pair_start_full_flow_401_then_200`, exercising the endpoint via
`TestClient` through four states in one flow:

1. No headers → 400 `"missing device auth headers"`.
2. All headers present but `X-Device-Signature` forged (32 zero bytes) →
   401 `"device authentication failed"`.
3. Valid signature with `ALLOWED_DEVICE_IDS=SOME-OTHER-DEVICE` → 401
   `"device not in whitelist"` (whitelist restored to `""` in `finally`).
4. Valid signature, empty whitelist → 200 with a 6-digit code.

Valid signatures are built with the existing
`server/tests/device_sig.py::signed_headers` helper (fresh random nonce per
call, so the nonce-replay cache cannot interfere); import added at the top
of the test module.

**Deviation from plan (deliberate):** the plan's Step-1 test only asserted
400 (missing headers) → 200 (valid). That sequence already passes under
Task-1 code (header-presence check, no signature verification), so it would
not have gone red in Step 2. The forged-signature 401 assertion (case 2) is
the clause that actually fails before the endpoint change
(`assert 200 == 401`) and proves `verify_device_signature` is wired in.
Case 3 likewise proves `check_whitelist` is wired in. Test name and the
400→200 spine match the plan; cases were added, not removed.

## Test evidence

Step 2 — new test before the endpoint change (failing as expected):

```
FAILED tests/test_device_signature.py::test_pair_start_full_flow_401_then_200
  - assert 200 == 401
1 failed, 3 warnings in 1.96s
```

Step 4 — signature test file after the change:

```
9 passed, 3 warnings in 1.89s
```

(3 Task-1 HTTP tests + 5 Task-2 store tests + 1 Task-3 integration test.)

Step 5 — full server suite:

```
64 passed, 3 warnings in 18.64s
```

(Plan expected ≥46; suite has grown to 64 with concurrent work — no
failures, no regressions. Warnings are pre-existing FastAPI
`on_event("shutdown")` deprecation notices, unrelated to this change.)

## Notes / concerns

- **Plan snippet vs. existing behavior:** the plan's replacement endpoint
  omitted the rate-limit check and the MASTER_KEY gate. Both were preserved:
  dropping rate limiting would regress `test_pair_start_rate_limit`, and
  dropping the key gate would make `test_master_key_required` (no headers +
  empty key, expects 401/500) fail with 400. Flag for the plan owner in case
  the firmware side or docs assume otherwise — the HTTP contract is
  unchanged: 400/401/429/200 exactly as before, with 401 now also covering
  signature and whitelist failures.
- Rate limiting is evaluated *after* signature verification, so forged
  requests don't consume a device's pairing quota; authenticated requests
  still get the same 5-per-5-min-per-IP protection as before.
- `server/data/devices.db` and `server/data/notifications.jsonl` show as
  modified/untracked after the test run — test artifacts, not committed.
- Firmware files in the working tree (`server_pairing.cc`, `application.cc`,
  etc.) belong to concurrent Task 4/5 agents and were deliberately left
  untouched and uncommitted.
