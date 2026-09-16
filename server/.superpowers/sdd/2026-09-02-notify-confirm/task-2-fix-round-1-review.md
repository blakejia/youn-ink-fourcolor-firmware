# Task 2 Fix-Round-1 Review — notification HTTP endpoints

Commit under review: `47436cc` (`fix(server): tighten /next device cross-check + persist render errors`)
Reviewing against prior findings in `task-2-review.md`.
Diff package: `.superpowers/sdd/2026-09-02-notify-confirm/task-2-fix-round-1-review-package.txt`
Files touched: `server/youn_server/app.py` (+12/-5), `server/youn_server/notify_store.py` (+18/-0), `server/tests/test_notify.py` (+19/-0).

## Finding 1 — Token/device_id cross-check on `/next`

**Original issue (`task-2-review.md` Important #1, app.py:454-456 in `4bf0dc8`):**
`_require_device_token` returned `None`, so `next_for(device_id)` used the query param directly. Any trusted device's token could drain any other device_id's queue.

**Status: ADDRESSED.**

Evidence (live `server/youn_server/app.py:454-457`):

```python
@app.get("/api/notifications/next")
async def next_notification(request: Request, device_id: str = Query(...)):
    dev = _require_device_token(request)
    if dev.device_id != device_id:
        raise HTTPException(status_code=401, detail="device mismatch")
    n = ns.get_store().next_for(device_id)
```

`_require_device_token` now returns the authenticated `Device` (signature change `-> "Device"`, body ends with `return dev`). The 401 fires before `next_for` is called, so the queue is untouched on mismatch.

Source-compat for other call sites verified: `ota_check`, `ota_download`, `ack_notification` all call `_require_device_token(request)` and discard the return — transitioning from `None` to `Device` is safe for any caller that ignored the return value.

Regression test added (`server/tests/test_notify.py:148-167`): `test_next_rejects_token_mismatch` enqueues a notification for the trusted device, attempts to pull it under `device_id="OTHER-DEVICE"`, asserts `401`, then asserts the legitimate pull returns `200`. This test would fail on `4bf0dc8` (cross-device pull returned 200) and passes on `47436cc`.

## Finding 2 — Render failure must persist status="error" to JSONL

**Original issue (`task-2-review.md` Important #2, app.py:472-475 in `4bf0dc8`):**
`ns.get_store()._items[n.id].status = "error"` mutated singleton private state without holding the lock or calling `_append`; status change lost on restart, concurrent `ack` could race.

**Status: ADDRESSED.**

Evidence — call site (live `server/youn_server/app.py:478-480`):

```python
        except Exception:
            ns.get_store().mark_error(n.id)
            raise HTTPException(500, "render failed")
```

Evidence — new public method (live `server/youn_server/notify_store.py:106-122`):

```python
    def mark_error(self, notification_id: str) -> Optional[Notification]:
        """Mark a notification as 'error' (e.g. render failed) and persist.
        ...
        """
        with self._lock:
            n = self._items.get(notification_id)
            if n is None:
                return None
            if n.status == "acked":
                return n  # terminal; don't overwrite acked state
            n.status = "error"
            self._append(n)
            return n
```

Two guarantees the new method provides that the inline pattern did not:

1. **Lock** — `with self._lock` prevents a concurrent `ack` from racing against the status flip.
2. **Persistence** — `self._append(n)` writes the error state to `notifications.jsonl`. On restart, `_load` does `self._items[d["id"]] = Notification(**d)` line-by-line, last write wins, so the reload sees `status="error"` (the second `_append` for that id, after `next_for`'s earlier `status="shown"` write). The terminal-state guard (`if n.status == "acked": return n`) correctly prevents clobbering an acked notification if `mark_error` is invoked late.

The private `_items` attribute is no longer touched from outside the store.

## New breakage check

Scanned the full diff (49 insertions, 5 deletions across 3 files). No new defects introduced:

- **No new dependencies.** Only stdlib + already-imported FastAPI primitives.
- **Signature change is source-compatible.** Other call sites (`ota_check`, `ota_download`, `ack_notification`) ignore the return value of `_require_device_token`; `None → Device` is a no-op for them.
- **Minor #3 (redundant nested `import base64`) opportunistically fixed** — the nested import was removed since `base64` is already at module top (`app.py:18`). Stylistic improvement, no behavior change.
- **Minor #4 (`ttl_sec` upper bound) deliberately deferred.** Report acknowledges it; plan scope doesn't define a bound. Acceptable scope discipline.
- **`mark_error` return path is safe.** Caller in the `except` block discards the return value; raises 500 unconditionally. `None` return (notification vanished mid-render) or "acked" return (notification was acked in a race) both correctly leave the caller raising 500 with no follow-up issue.
- **`next_for` semantics unchanged.** The existing test `test_next_returns_bitmap_and_meta` still passes per the report (8/8 with the new regression test added).
- **Test isolation preserved.** The autouse `_clean_notify_state` fixture from round 0 still applies; the new test consumes the same fixtures (`client`, `trusted_device`).

The diff is well-scoped (3 files, +49/-5) and tight. No unrelated drive-by edits.

## Verdict

**All findings addressed. Approve for integration.**

Summary:

| Severity         | Count | Status |
|------------------|-------|--------|
| CRITICAL         | 0     | pass   |
| HIGH (Important) | 0     | pass   |
| MEDIUM           | 0     | pass   |
| LOW (Minor)      | 1     | note   (deliberately deferred — `ttl_sec` upper bound) |

The two Important authz/persistence gaps from round 0 are closed with surgical fixes, a regression test covers the authz fix, and the public `mark_error` API replaces private-state mutation with a thread-safe, persist-through-restart counterpart. The fix is ready to land.
