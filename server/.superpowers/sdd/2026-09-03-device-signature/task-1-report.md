# Task 1 Report — Server config + MASTER_KEY + whitelist fields

**Date:** 2026-09-03
**Plan:** `docs/superpowers/plans/2026-09-03-device-signature.md`, Task 1
**Status:** ✅ Complete
**Commit:** `87412b9f7613b609cdc069368675e2cdd02ce5a3`

## What was implemented

### Configuration (`server/youn_server/config.py`)
Added two `Settings` fields (pydantic-settings), defaulting to empty string, read
from `.env` / environment:

```python
# ── Device signature authentication (HMAC pair-start) ──
master_key: str = Field(default="")
allowed_device_ids: str = Field(default="")  # comma-separated, empty = all
```

Verified: both load from env (`MASTER_KEY=...`, `ALLOWED_DEVICE_IDS=...`) and
default to `""` when unset.

### `.env.example`
Added a documented block:
```
# ─── Device signature authentication (pair-start HMAC) ───
# Generate with: python -c "import secrets; print(secrets.token_urlsafe(32))"
MASTER_KEY=
# Comma-separated device_id whitelist; empty = all devices accepted.
ALLOWED_DEVICE_IDS=
```

### Startup fail-fast (`server/youn_server/app.py`, `create_app()`)
When `settings.master_key` is empty the server **still starts** but logs an
ERROR immediately:
```
ERROR youn_server.app: MASTER_KEY is empty: all pair-start requests will be
rejected (set MASTER_KEY in .env; generate with secrets.token_urlsafe(32))
```

### pair-start gating (`server/youn_server/app.py`)
The `/api/devices/pair-start` endpoint now:
1. Returns **401** `{"detail": "device authentication failed"}` when
   `MASTER_KEY` is empty (rejects all pairing).
2. Reads the four `X-Device-Mac / -Timestamp / -Nonce / -Signature` headers.
3. Returns **400** `{"detail": "missing device auth headers"}` when the
   required headers are absent.
4. Otherwise proceeds with the existing rate-limit → upsert → create-session
   flow (200 + 6-digit code).

HMAC signature **verification** and the whitelist check are Task 2 / Task 3.
The header names, order, and the `400`/`401` detail strings match the Task 3
endpoint spec exactly, so Task 3's `pair_start` replacement is a drop-in (the
crypto `verify_device_signature` + `check_whitelist` calls slot in between the
header-presence check and the session creation).

### Tests
- **New:** `server/tests/test_device_signature.py` — the 3 tests from the plan
  verbatim (`test_master_key_required`, `test_signature_valid`,
  `test_signature_missing_headers`).
- **New helper:** `server/tests/device_sig.py` — `signed_headers(device_id)`
  builds real, HMAC-SHA256-valid `X-Device-*` headers (byte order
  `MAC(6) || timestamp(ASCII) || nonce(ASCII)`, fresh random nonce per call).
- **New fixture:** autouse `_device_signature_key` in `conftest.py` injects a
  test `MASTER_KEY` for the whole suite; restored to `""` after each test.
- **Migrated callers:** existing pair-start calls in `tests/test_pairing.py`
  (8 sites) and `tests/test_notify.py` (1 site) now pass
  `headers=signed_headers(device_id)`.

The migration was required for a clean cutover: once pair-start enforces auth,
the old headerless calls would 401. The helper produces genuinely valid
signatures (not a bypass), so these tests keep passing unchanged after Tasks 2/3
turn on crypto verification — no production shim or test-only backdoor.

## Test output

Target command (constraint):
```
$ server/.venv/bin/python -m pytest tests/test_device_signature.py -q
...                                                                      [100%]
3 passed, 3 warnings in 1.74s
```

Full server suite (regression check):
```
$ server/.venv/bin/python -m pytest tests/ -q
..........................................................               [100%]
58 passed, 3 warnings in 18.71s
```

TDD red→green confirmed: before implementation the 3 tests errored with
`ValueError: "Settings" object has no field "master_key"`.

## Files in commit `87412b9`
- `server/youn_server/config.py` (modified — 2 fields)
- `server/youn_server/app.py` (modified — startup log + pair-start gating)
- `server/.env.example` (modified — MASTER_KEY / ALLOWED_DEVICE_IDS)
- `server/tests/test_device_signature.py` (new — 3 tests)
- `server/tests/device_sig.py` (new — signed-header test helper)
- `server/tests/conftest.py` (modified — test MASTER_KEY fixture)
- `server/tests/test_pairing.py` (modified — signed headers on pair-start)
- `server/tests/test_notify.py` (modified — signed headers on pair-start)

## Concerns / notes for downstream tasks

1. **app.py was not in the plan's Task 1 file list, but was required.** The
   plan's Task 1 "Files" listed only config.py / .env.example / the test file,
   yet its three tests exercise pair-start (401 without key, 400 without
   headers, 200 with valid signature). The minimal gating was added to app.py
   to satisfy the tests; Task 3 owns the full endpoint rewrite and extends it.
2. **`allowed_device_ids` is `str`, not `list`.** The plan's Task 1 "Interfaces"
   line says `list[str]`, but the plan's own Step 3 code and the task contract
   specify a comma-separated `str`. Implemented as `str` (default `""`); Task
   2's `check_whitelist` already splits on commas and tolerates a `list` too,
   so the test fixture's `settings.allowed_device_ids = []` / `""` both work.
3. **Production behavior change:** pair-start now hard-requires `MASTER_KEY`.
   Deployments must set it before devices can pair; unconfigured pairing fails
   closed (401). Old firmware that doesn't sign will get 401 until the signing
   firmware (Tasks 4/5) ships — intended per the security goal.
4. **Scope isolation:** sibling-owned firmware files
   (`firmware/main/...`) and runtime data (`server/data/devices.db`,
   `notifications.jsonl`) were present in the worktree but deliberately
   **not** staged or committed.
