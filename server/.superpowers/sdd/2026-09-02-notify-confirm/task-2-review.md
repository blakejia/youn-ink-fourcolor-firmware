# Task 2 Review — notification HTTP endpoints

Commit under review: `4bf0dc8` (on top of `fef8217`).
Files changed: `server/tests/test_notify.py` (+145), `server/youn_server/app.py` (+75 net), `server/youn_server/config.py` (+1).

## Spec compliance: ✅

All four required endpoints are present and shaped per the plan:

| Requirement | Status | Evidence |
|---|---|---|
| `POST /api/notifications` (operator, trusted device) → 201 | ✅ | `app.py:438-451` |
| Trusted-device check via `registry.list_all(only_trusted=True)` | ✅ | `app.py:447` (`any(d.device_id == device_id for d in registry.list_all(only_trusted=True))`) |
| `GET /api/notifications/next` (device token, FIFO) → 200/204 | ✅ | `app.py:454-479` |
| Atomic `mark_shown` via `next_for` (Task 1) | ✅ | `app.py:456` (`ns.get_store().next_for(device_id)`) — Task 1 review already confirmed FIFO + lock-protected mark-shown |
| Bitmap via `render_canvas_to_bitmap()` → 30000 bytes 2bpp BWRY | ✅ | `app.py:458-472` constructs flex column, base64-encoded. Smoke verifies 40000 base64 chars (= 30000 raw bytes) |
| `POST /api/notifications/{id}/ack` (device token, `agree|reject`) → 200/404 | ✅ | `app.py:482-490` |
| Idempotent `ack` | ✅ | `app.py:488` — Task 1 store already enforces first-decision-wins |
| `GET /api/notifications/history` (operator, `device_id`, `limit`) → 200 | ✅ | `app.py:493-496` |
| `config.py` `notify_default_ttl: int = Field(default=300)` | ✅ | `config.py:60` |
| `asdict` usage on `Notification` | ✅ | `app.py:451, 477, 496` |
| `settings.notify_default_ttl` consumed when `ttl_sec` not supplied | ✅ | `app.py:443` (`body.get("ttl_sec", settings.notify_default_ttl)`) |

Nothing extra leaked into the diff. The diff is bounded to: 4 endpoints, 2 imports (`asdict`, `base64`, `notify_store as ns`), 1 config field.

## Strengths

1. **Test isolation is honest and well-documented.** The `_clean_notify_state` autouse fixture calls out two real concerns (singleton staleness across rotated `data_dir`, and the in-memory 5/5min pairing rate limit) with reasoning inline. Matches the pattern in `test_pairing.py:50-52`.
2. **Plan deviation is explained, not silent.** Two deviations from the plan (the missing `pair-claim` step in the fixture, and the singleton reset) are surfaced in the report rather than buried. The `pair-claim` correction is correct: `pair-confirm` returns `{"status":"ready"}`, the token is issued by `pair-claim`.
3. **Faithful to global constraints.** All four constraints from the plan are honored at the right places: `registry.list_all(only_trusted=True)` for the trusted check, `render_canvas_to_bitmap` for the bitmap, `_require_operator`/`_require_device_token` for auth, FIFO via the Task 1 store.
4. **No new dependencies.** Only stdlib (`base64`, `dataclasses.asdict`) and already-imported FastAPI primitives.
5. **Atomicity preserved.** The base64-encoded bitmap is computed after `next_for` has already flipped status to `shown` under the store's `RLock()`, so the "poisoned render doesn't get re-served" guard (status→"error") is correctly best-effort and stays consistent.
6. **Tests cover all four endpoints' happy + error paths** (trusted-device rejection, missing-token rejection, missing-ack 404, FIFO drain via second-pull 204, idempotent ack, history filter+limit).

## Issues

### Important

1. **`/api/notifications/next` does not cross-check token's device against query `device_id`.** `app.py:454-456`.
   Any device with a valid token can pull notifications addressed to a different trusted device_id. `_require_device_token` validates the bearer; `next_for(device_id)` uses the raw query param. The implementer flagged this in "Concerns" and followed the plan verbatim, so it is not a regression, but it is a real authorization boundary gap that should be tightened before this lands for real use. Cheap fix: read `registry.get_device_by_token(token)` (already done inside `_require_device_token`), surface the device_id, and `if dev.device_id != device_id: raise HTTPException(401)`. Either return the device from a refactored `_require_device_token_for(device_id)` or duplicate the lookup inline.

2. **`render_canvas_to_bitmap` failure path touches private state on the singleton.** `app.py:472-475`.
   `ns.get_store()._items[n.id].status = "error"` mutates `_items` directly without going through `_append`, so the JSONL won't record the state change (crash recovery will treat this as still `pending`, not `error`). The implementer correctly flagged this. The plan spec writes exactly this code, so leaving it is a defensible "follow the plan," but the right shape is a public `NotifyStore.mark_error(id)` that takes the lock and `_append`s the new state — so the status change is durable across restart and the singleton's private contract isn't violated.

### Minor

3. **Redundant `import base64` inside `next_notification`.** `app.py:476`. `base64` is already at module top (`app.py:18` per the diff). The implementer noted this. Trivial cleanup; not blocking.

4. **`ttl_sec` from request body has no upper-bound validation.** `app.py:443`. A caller can pass `ttl_sec=10**9` and enqueue a notification that lives effectively forever in the FIFO. The plan doesn't define an upper bound; an "obviously too large" cap (e.g. `ttl_sec > 86400`) returning 400 would be a cheap hardening. Skip if plan scope says no.

5. **`_clean_notify_state` reaches into `_pairing_store._rate_limits` and `_claim_failures`** — private state. Mirrors the pattern in `test_pairing.py`, so it's consistent with the existing test style, but worth noting it bypasses any encapsulation `_pairing_store` might want to add later. Skip.

### Not issues (verified)

- `status_code=201` decorator on `create_notification` is correct (plan: 201).
- `Response(status_code=204)` for empty `next_for` is correct (plan: 204).
- `device_id: str = Query(...)` (required) on `/next` is correct.
- `decision in ("agree", "reject")` validation is exactly per spec.
- `ttl_sec` body field falls back to `settings.notify_default_ttl` correctly.
- `recent(device_id, limit)` call signature matches Task 1's store.
- `_clean_notify_state` `ns._store = None` is the documented reset for `get_store()`'s lazy singleton.

## Verdict: **request changes** (Important #1 is a real authz gap that should be fixed before merge)

Important #1 (token/device_id mismatch on `/next`) is the only thing standing between this commit and approval. It is a one-line addition (compare `dev.device_id` to query `device_id`, raise 401 on mismatch), the design-doc intent is clear, and shipping without it leaves a privilege escalation where any trusted device can drain any other trusted device's queue. Important #2 (public `mark_error` instead of reaching into `_items`) is also worth fixing in the same commit since both touches are in the same function, but is lower priority — the immediate bug is the missing cross-check.

After both are addressed, this is approve-grade work: faithful to spec, scoped, well-tested, and honestly documented.

## Verification

- `./.venv/bin/python -m pytest tests/test_notify.py -q` → 7 passed
- `./.venv/bin/python -m pytest tests/ -q` → 52 passed (38 pre-existing + 7 Task 1 + 7 Task 2)
- All 4 endpoints registered with the correct methods (`POST/GET/POST/GET` per spec).
