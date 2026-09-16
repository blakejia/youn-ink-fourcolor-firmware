# Task 2 Review — `PairingStore.verify_device_signature` + `check_whitelist` + nonce replay

**Reviewer:** Task2Reviewer
**Date:** 2026-09-03
**Commit:** `366c8937ff24ffd6834b20124ed2be82c8c1f1ef`
**Files:** `server/youn_server/pairing.py` (+109), `server/tests/test_device_signature.py` (+95)

> Note: `task-2-review-package.txt` was not present in the SDD directory; this
> review was performed against the live commit (`git show 366c893`), the current
> `pairing.py` / test file, the plan brief, and the task report. Tests were
> re-run: **8 passed** (`tests/test_device_signature.py`).

## Spec compliance: ✅

| Requirement (brief/global constraints) | Status | Evidence |
|---|---|---|
| `verify_device_signature(device_id, mac, timestamp:int, nonce, signature_b64) -> bool` | ✅ | `pairing.py:208-210` signature matches plan interface |
| `_nonce_cache` field on store | ✅ | `pairing.py:70` (`dict[str,float]`, `"device_id:nonce"` → first-seen) |
| `derived_key = HMAC-SHA256(MASTER_KEY, device_id)` | ✅ | `pairing.py:236-238` — key=MASTER_KEY, msg=device_id |
| Signature over `MAC(6) \|\| timestamp(ASCII) \|\| nonce(ASCII)` | ✅ | `pairing.py:256` `mac_bytes + str(timestamp).encode() + nonce.encode()` |
| HMAC-SHA256, base64 signature | ✅ | `pairing.py:259,265` |
| MASTER_KEY ≥ 32 bytes, empty → verify fails | ✅ | `pairing.py:220-224` (empty or `< 32` → `False`) |
| Timestamp window ±30s | ✅ | `pairing.py:227-233`, `TIMESTAMP_WINDOW_SEC = 30` (`pairing.py:46`); `abs()` covers stale **and** far-future |
| Nonce replay 5 min, in-memory | ✅ | `pairing.py:272-283`, `NONCE_CACHE_TTL_SEC = 300` (`pairing.py:47`); eviction + insert under `self._lock` |
| MAC: 12 hex → `bytes.fromhex` → must be 6 bytes | ✅ | `pairing.py:241-253` — catches `ValueError`/`TypeError`, then enforces `len == 6` |
| Whitelist: comma-separated **str**, empty = all | ✅ | `pairing.py:295-302` — splits on `,`, strips, drops empties; `not allowed → True`; also tolerates list/tuple |
| 5 new meaningful tests | ✅ | wrong key, stale ts, nonce replay (True→False), bad MAC, whitelist (empty/list/unlisted) |

All plan-mandated behaviors are present and correct. No missing requirements.

## Strengths

- **Constant-time comparison** via `hmac.compare_digest` (`pairing.py:266`),
  with both operands as `bytes` (no str/bytes foot-gun).
- **Fails closed at every step**: bad key length, timestamp, MAC hex, MAC
  length, base64, HMAC mismatch, and replay each return `False` with a
  distinct, searchable log line — no secret/key material is logged.
- **Replay cache is bounded and correct**: lazy eviction of entries older than
  300s runs under the store lock before the membership check; the nonce is
  recorded **only after** a valid signature, so a forged request can't burn a
  legitimate device's nonce. Cache key is per-device scoped.
- **MAC validation is two-stage** (decodable hex *and* exactly 6 bytes),
  matching the firmware's 6-byte `esp_read_mac`.
- **Tests are isolated** on a `tmp_path` DB via the `store` fixture
  (`test_device_signature.py:79-84`); the whitelist test mutates global
  `settings` and restores it in `finally` (`:151-162`).
- **Report deviations are sound and well-documented**: the 5 tests exercise
  `PairingStore` directly rather than over HTTP, because the endpoint wiring
  (401s, `detail` messages) is explicitly Task 3's scope and does not exist
  yet — the plan's own Step-2 expectation (`AttributeError` on the method)
  only fires at unit level. Task 3's plan calls these methods with the exact
  signatures implemented here.

## Issues

### Critical
None.

### Important
None.

### Minor

1. **Key-length check counts characters, not bytes** — `pairing.py:220`
   `len(master_key) < 32` measures `str` length. The constraint says "≥ 32
   bytes". For ASCII keys (the normal case, e.g. `secrets.token_urlsafe(32)`)
   chars == bytes; a non-ASCII key would be measured short. Consider
   `len(master_key.encode()) < 32` for byte-accuracy.

2. **`base64.b64decode` without `validate=True`** — `pairing.py:259`. With the
   default, characters outside the alphabet are silently discarded before the
   padding check. Not exploitable (the HMAC must still match, so forgery is
   impossible), but `base64.b64decode(signature_b64, validate=True)` is
   stricter and rejects malformed input outright.

3. **"Never raises" docstring vs. un-coerced timestamp** — `pairing.py:217`
   promises "never raises", but `abs(now - timestamp)` at `pairing.py:228`
   raises `TypeError` if a non-numeric `timestamp` reaches it. The method is
   typed `int` and Task 3's endpoint does `int(ts_str)` before calling (report
   notes this explicitly), so the contract holds in practice; worth a
   defensive `int(timestamp)` or a guard so the guarantee is self-contained.

4. **Nonce cache key uses `:` delimiter** — `pairing.py:272`
   `f"{device_id}:{nonce}"`. A `device_id` containing `:` could theoretically
   collide with another `(device, nonce)` pair. Impact is a *false replay
   rejection* (availability), never an auth bypass, and device_id format is
   controlled. Acceptable; noting for completeness.

5. **Test coverage gaps (edge cases)** — no unit test for: far-future
   timestamp (the `abs()` branch at `pairing.py:228` is code-claimed but
   untested), a MAC that is valid hex but wrong length (e.g. 10 chars → 5
   bytes; the `len != 6` branch at `pairing.py:248`), or undecodable base64
   (`pairing.py:259-264`). The happy path *is* covered (first call of the
   replay test asserts `True`). These are cheap additions for Task 3 if it
   touches the file.

6. **Autouse fixture teardown doesn't restore the whitelist** —
   `test_device_signature.py:19` sets `settings.allowed_device_ids = []` but
   the teardown (`:21`) only resets `master_key`. The whitelist test's
   `finally` leaves it `""`, so current file-order execution is correct, but
   the fixture's restore is asymmetric and slightly fragile. (Assignment of a
   `list` to a `str` field works only because pydantic's
   `validate_assignment` is off — confirmed by the passing suite.)

## Verdict: **Approve**

Spec-compliant, cryptographically correct (per-device derived key, fixed
payload byte order, constant-time compare), replay window and timestamp window
match the brief, whitelist parsing handles empty/whitespace/list cases, and the
5 tests are meaningful and isolated. All findings are Minor hardening/coverage
nits that can be picked up opportunistically in Task 3 — none block this task.
