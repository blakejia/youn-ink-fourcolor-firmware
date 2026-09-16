# Task 3 Review — app.py pair-start endpoint signature verification

**Reviewer:** Task3Reviewer (code-reviewer)
**Date:** 2026-09-03
**Commit reviewed:** `de1a963` — `feat(server): pair-start requires HMAC device signature`

> Note: `task-3-review-package.txt` was not present in the SDD directory.
> Reviewed against the actual commit `de1a963` diff plus the current tree
> (`server/youn_server/app.py`, `server/tests/test_device_signature.py`,
> `server/tests/device_sig.py`), and re-ran the scoped tests independently.

## Spec compliance: ✅

| Requirement (global constraints) | Status | Evidence |
|---|---|---|
| pair-start calls `verify_device_signature` | ✅ | `app.py:282-285` |
| pair-start calls `check_whitelist` | ✅ | `app.py:289-290` |
| Missing headers → 400 `"missing device auth headers"` | ✅ | `app.py:273-274` |
| Invalid signature → 401 `"device authentication failed"` | ✅ | `app.py:285` |
| Not in whitelist → 401 `"device not in whitelist"` | ✅ | `app.py:290` |
| MASTER_KEY empty gate preserved (`test_master_key_required`) | ✅ | `app.py:265-266`; test passes |
| Rate-limit check preserved (`test_pair_start_rate_limit`) | ✅ | `app.py:292-294`; test passes |

Endpoint order in `server/youn_server/app.py:261-298`:
MASTER_KEY gate → header presence → timestamp parse → HMAC verify →
whitelist → rate limit → registry.upsert + create_session. This matches the
plan's Task 3 snippet and keeps the pre-existing HTTP contract
(400/401/429/200).

Independent verification: `pytest tests/test_device_signature.py
tests/test_pairing.py` → **24 passed** (9 signature + 15 pairing), including
`test_master_key_required` and `test_pair_start_rate_limit`.

## Strengths

1. **Caught two plan omissions.** The plan's replacement snippet dropped both
   the MASTER_KEY gate and the rate-limit check. The implementer preserved
   both (report lines 26-28, 86-93); dropping either would have failed
   `test_master_key_required` (empty key + no headers expected 401/500, would
   have become 400) and `test_pair_start_rate_limit`.
2. **Security-conscious check ordering.**
   - MASTER_KEY checked *before* header presence (`app.py:262-266`) so an
     unconfigured server never reveals the expected request shape.
   - Whitelist checked *after* signature verification (`app.py:287-290`) so
     whitelist existence/membership is not leaked to unauthenticated callers.
   - Rate limit checked *after* auth (`app.py:292-294`) so forged requests
     cannot burn a device's per-IP pairing quota.
3. **The new test actually goes red.** The plan's 400→200 spine already passes
   under Task-1 code (header-presence check, no signature verification), so it
   would not have failed in Step 2. The added forged-signature 401 assertion
   (test case 2) is the clause that fails pre-change (`assert 200 == 401`,
   evidence in report lines 58-64), proving `verify_device_signature` is
   wired in; case 3 likewise proves `check_whitelist` is wired in. Good
   deviation, well documented in the report.
4. **Correct nonce-cache reasoning across the 4 cases.** `signed_headers()`
   (`device_sig.py:29`) uses a fresh random nonce per call. Case 2's forged
   request fails HMAC comparison *before* the nonce is cached (Task 2 code
   caches only after `compare_digest` succeeds), so the reused `headers`
   nonce in case 4 is still unspent → deterministic 200. Case 3's valid-sig /
   whitelist-fail request caches a *different* nonce. No replay-cache
   interference; no flakiness.
5. Error strings match the global constraints verbatim, including the
   uniform-401 design (`"device authentication failed"` for bad key/bad
   HMAC/old timestamp/replay/bad MAC, per spec line 25).

## Issues

### Critical
None.

### Important
None.

### Minor

1. **Missing-only-`X-Device-Nonce` yields 401, not 400** —
   `server/youn_server/app.py:273`. The presence gate checks
   `mac and ts_str and sig` but not `nonce` (verbatim from the plan snippet,
   plan line 433). A request with mac/timestamp/signature but no nonce passes
   the 400 gate, fails HMAC verification (empty nonce → mismatch), and
   returns 401 `"device authentication failed"`. This is safe (uniform auth
   failure, no hint about which header is missing) and the firmware always
   sends all four headers, so no action required; noting for awareness.
2. **Rate limit no longer throttles unauthenticated volume** —
   `server/youn_server/app.py:282-294`. Because the rate limiter runs after
   signature verification, floods of forged/header-less requests get 400/401
   without consuming quota *and* without ever being rate-limited. This is the
   deliberate, documented trade-off (report lines 94-96): forged requests
   can't burn a legitimate device's quota, HMAC verification is cheap, and
   the stateful/expensive operation (session creation) remains protected.
   Acceptable; flag only if unauthenticated request-volume shielding becomes
   a requirement.
3. **Fixture type inconsistency (pre-existing, not introduced here)** —
   `server/tests/test_device_signature.py:21` sets
   `settings.allowed_device_ids = []` (a list) while the config field and the
   Task-3 test use comma-separated strings (`"SOME-OTHER-DEVICE"` → restored
   to `""`). `check_whitelist` defensively handles both types, and tests pass,
   so this is cosmetic; a future cleanup could make the fixture use `""`.

## Test hygiene

- Integration test is meaningful: it drives the real HTTP stack via
  `TestClient` through four states (400 no-headers → 401 forged sig →
  401 whitelist → 200 valid), asserting both status codes and response
  detail strings, not just status codes.
- Global state mutation (`settings.allowed_device_ids`) is wrapped in
  try/finally with restoration (`test_device_signature.py:185-196`).
- Import style `from .device_sig import signed_headers` matches
  `conftest.py:16`, `test_notify.py:27`, `test_pairing.py:25`; reuse of the
  shared helper avoids duplicating signature construction.
- Report's full-suite evidence (64 passed) corroborated by my scoped re-run
  (24 passed across the two affected files).

## Verdict

**Approve.** All seven spec requirements are met, both preserved gates
(MASTER_KEY, rate limit) are verified in source and by passing tests, the
test genuinely fails before the change and passes after, and the deviations
from the plan snippet are correct, deliberate, and documented. The three
minor items are observations/non-blocking cleanups, not change requests.
