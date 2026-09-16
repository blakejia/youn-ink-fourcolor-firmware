# Task 1 Report — notify_store FIFO queue storage layer

## Status: DONE

- Commit: `fef8217` — `feat(server): FIFO notification queue store (notify_store)`
- Files:
  - Created: `server/youn_server/notify_store.py` (115 lines)
  - Created: `server/tests/test_notify_store.py` (68 lines, 7 tests)

## Test output

Command: `server/.venv/bin/python -m pytest tests/test_notify_store.py -q`

```
.......                                                                  [100%]
7 passed in 0.03s
```

Test sequence followed (TDD):
1. Wrote the 7 failing tests from the plan.
2. Confirmed failure: `ImportError: cannot import name 'notify_store'` (collection error, expected).
3. Implemented `notify_store.py` per the plan's interface spec.
4. Confirmed pass: `7 passed in 0.03s`.
5. Committed.

## Smoke verification (beyond the plan's 7 tests)

- JSONL append-only: each state transition (`pending` → `shown` → `acked`) appended as a new line; Unicode (`ensure_ascii=False`) preserved.
- Persistence: a fresh `NotifyStore` reloaded from the same file recovered `status="acked"`, `decision="agree"`.
- Singleton: `get_store() is get_store()` → `True`.
- `is_expired`: ttl=0 → `True`; ttl=300 → `False`.

## Deviation from plan (bug fix, behavior-preserving)

**The plan's sample implementation deadlocks.** In `next_for`/`ack`, the plan acquires `self._lock` and calls `self._append(n)` **inside** that lock; `_append` then acquires `self._lock` again. `threading.Lock` is **not reentrant**, so the nested acquire blocks the calling thread forever — the first `next_for` invocation hangs. Verified empirically: the first pytest run stalled until a 300s watchdog killed it, and a 60s `timeout` had to terminate a second run.

**Fix:** `threading.Lock()` → `threading.RLock()` (one-line change, line 36). `RLock` is reentrant, so the existing structure (append under the lock) is safe and correct. This is the minimal change that makes the plan's own code work; no interface or behavior changed. All 7 acceptance tests pass, plus smoke checks. The lock is held during disk append (single process, single writer — appropriate for this JSONL store; no process-crossing guarantee is claimed).

## Concerns

- **`_load` per-instance, not shared across processes.** Each `NotifyStore` loads the JSONL once at construction; only in-process mutations are visible in `_items`. If multiple processes were to open the same file, they would not see each other's in-memory state (though appends would not corrupt the file). Out of scope for Task 1: the process-local singleton (`get_store()`) is the intended usage, and later tasks (HTTP endpoints) use the same process. Flagging for awareness only.
- **Expired entries are never purged** (lazy expiry only) — the JSONL grows unboundedly in the long run. The plan explicitly specifies lazy expiry (`status="expired"` in `next_for`), so this is by design; a compaction/GC could be a follow-up.
- **`_append` is called under the lock** (via `RLock`), so disk I/O is serialized with state mutation — deliberate: guarantees the JSONL reflects the in-memory state and appends stay ordered.
- **`recent` sorts by `created_at` descending; ties are not broken deterministically** — same-millisecond enqueues (as in `test_recent_limit`, 25 rapid enqueues) may return in arbitrary-but-stable order. All acceptance tests pass; the tie-break is unspecified by the plan. Flagging only.

No new dependencies were introduced (standard library only). `server/tests/test_notify_store.py` was added under the existing `server/tests/` (plan listed both `server/tests/test_notify_store.py` and `server/youn_server/notify_store.py`; the existing test dir is `server/tests`, confirmed by conftest.py presence).
