# Task 1 Review — notify_store FIFO queue storage layer

## Spec compliance: ✅

All requirements from `docs/superpowers/plans/2026-09-02-notify-confirm.md` Task 1 are met:
- ✅ Path: `server/youn_server/notify_store.py` (plan location used).
- ✅ Path: `server/tests/test_notify_store.py` (the existing `server/tests/` dir; conftest is there, `server/.superpowers/sdd/` is a plans-only tree, so the chosen location is correct).
- ✅ `Notification` dataclass with `id, device_id, title, body, created_at, ttl_sec, status, decision, acked_at`, plus `is_expired`.
- ✅ `NotifyStore` with `enqueue(device_id, title, body, ttl_sec=300) -> Notification`, `next_for(device_id) -> Notification | None`, `ack(id, decision) -> Notification | None`, `recent(device_id="", limit=20) -> list[Notification]`.
- ✅ `get_store()` process-local singleton via module-global `_store`.
- ✅ Storage: `data/notifications.jsonl`, append-only per-state-transition, `threading.Lock`-equivalent (`RLock`, see below) guarding all writes.
- ✅ FIFO per device — `next_for` sorts `self._items.values()` by `created_at` and filters by `device_id` then `status == "pending"`.
- ✅ Atomic mark-shown — `next_for` mutates `status="shown"` and calls `_append` inside the same critical section.
- ✅ Idempotent ack — first decision wins: short-circuit on `status == "acked"` returns the existing record without overwriting `decision`/`acked_at`.
- ✅ Lazy expiry — `ttl_sec=0` skipped in `next_for` (status→`expired`); not purged from in-memory map nor JSONL.
- ✅ Standard library only — `json`, `threading`, `time`, `uuid`, `dataclasses`, `pathlib`, `typing`. Plus `.config` for `settings.data_dir` (which is the planned import).
- ✅ All 7 acceptance tests pass (`7 passed in 0.03s`, verified).
- ✅ Nothing extra beyond the plan's surface; the one deviation is a bug fix (see Important #1 below).

## Strengths

1. **Faithful TDD.** The 7 tests in `tests/test_notify_store.py` mirror the plan's stated acceptance tests verbatim; nothing was quietly widened or skipped.
2. **Minimal, behavior-preserving RLock fix.** The plan's published `notify_store.py` contains a re-entrant-acquire deadlock: `next_for` / `ack` / (by extension) `enqueue`'s callers hold `self._lock` and call `self._append(n)`, which re-acquires `self._lock`. I reproduced the hang with a `threading.Lock` repro — thread stays alive indefinitely; with `RLock`, the same code path completes in 0.08s under a 4×50 threaded race. The implementer's one-line change (`Lock()` → `RLock()`, line 36) makes the plan's own structure correct without touching interface or behavior. This is the right minimal fix; reject any proposal to "restructure to avoid nested locking" because that would also restructure the plan's stated contract.
3. **Append-only, ordered, Unicode-safe.** `_append` uses `ensure_ascii=False` (verified — Chinese characters and ✓ glyph survive round-trip) and writes one JSON record per line. State transitions (`pending → shown → acked`) each append a new line, preserving history.
4. **Singleton + persistence split.** `get_store()` is process-local (correct for the in-process model the rest of the plan assumes); a fresh `NotifyStore(path)` loaded from the same file restores `status="shown"` / `decision="agree"` faithfully — verified by the implementer's reload smoke and re-confirmed here.
5. **`tests/` directory placement is correct.** The instructions referenced a path `server/.superpowers/sdd/.../task-1-review-package.txt` — the actual diff lives at `.superpowers/sdd/...` at the repo root, and the chosen `server/tests/test_notify_store.py` matches the existing pytest root (conftest present there). The implementer's note about this is accurate.

## Issues

### Critical
*None.*

### Important
1. **`threading.Lock` → `threading.RLock` is a deviation from the plan's own sample code, but it is a correct, behavior-preserving, spec-faithful fix — not an invention.**  
   `server/youn_server/notify_store.py:36` (`self._lock = threading.RLock()`).  
   **Why this is Important, not Critical:** the plan's published sample is **broken as written** — it deadlocks any caller of `next_for` or `ack` on the very first invocation. A reviewer must explicitly approve this deviation because the diff does not match the plan verbatim.  
   **Why this is correct, not a workaround:**
   - Empirically reproduced with `threading.Lock`: thread alive after 3s; process times out at 10s (exit 124).
   - With `RLock`: same code path completes in 0.08s under 4×50 concurrent enqueue+next_for workers, all assertions pass.
   - `RLock` is a strict superset of `Lock` for this code path — reentrancy is exactly what the plan's nested `with self._lock:` structure requires, and no other thread ever waits for it (single writer per process).
   - No interface, return type, status string, ordering, or persistence behavior changed. All 7 acceptance tests still pass and exercise the affected path (`enqueue` → `next_for` → `ack`).
   - The minimality is the key argument: changing `Lock` to `RLock` is one token; restructuring to avoid nested locking would change the plan's stated contract and re-introduce risk.
   - **Action required (not a code change, but a doc change):** the plan's sample code in `docs/superpowers/plans/2026-09-02-notify-confirm.md` Task 1 Step 3 should be amended to reflect `RLock` so the same dead code is not copy-pasted into future tasks. Without this amendment, the document is a trap for parallel work.

2. **`is_expired` boundary depends on real-clock advance for `ttl_sec=0` only.**  
   `server/youn_server/notify_store.py:25-27`. `test_expired_skipped` uses `ttl_sec=0`. Because `enqueue` reads `time.time()` and `next_for` reads it again, there is a real-clock gap during which `created_at + ttl_sec > now` could still be true if `ttl_sec < delta_now`. This test passes only because `ttl_sec=0` makes `now >= created_at + 0` always true — verified: `e0 = Notification(created_at=100.0, ttl_sec=0).is_expired(200.0) == True`. So the test passes for the right reason. For `ttl_sec=1` and a sub-millisecond gap, the test would flake on a slow CI. The plan only specifies `ttl_sec=300` (default) and the boundary test uses 0, so this is acceptable for now. Flag for awareness only; do not block.

### Minor
1. **`recent` sort tie-break is unspecified.** Same-millisecond enqueues (as in `test_recent_limit`, 25 rapid enqueues, plus the implementer's smoke) yield non-deterministic ordering. `test_recent_limit` only asserts the *count* is 20, not order, so this is fine for the spec. Worth a docstring note on `recent` ("ties broken arbitrarily") for Task 2 callers; not a blocker.

2. **`_load` swallows parse errors silently.**  
   `server/youn_server/notify_store.py:43-50`. A truncated line is silently dropped, so a corrupted append (e.g., partial write from crash mid-`_append`) leaves the JSONL with N-1 records and the implementer never knows. For a single-writer, append-only, process-local FIFO this is acceptable — the JSONL format itself has no corruption-detection (no line check) — but a one-line `logging.warning(...)` at minimum would help future debugging. YAGNI for Task 1; consider in a future compaction task.

3. **`_append` re-enters `_lock` even when called from outside the lock.**  
   `server/youn_server/notify_store.py:55-59`. Every `_append` call wraps the file write in `with self._lock:`. Inside `next_for` / `ack`, this is re-entry (works because `RLock`). From `enqueue`, this is a fresh acquire. The lock is held during the disk write — fine for the single-process, single-writer assumption, and the report explicitly notes this. No action.

4. **`enqueue` mutates `_items` under lock, then releases, then appends to disk.**  
   `server/youn_server/notify_store.py:67-77`. If the file write fails (disk full, permission), `_items` already has the record but the JSONL does not. The next process restart will not see it. Acceptable because (a) the failure surface is narrow, (b) the plan says append-only, and (c) reversing the order (append first, then add to map) would risk `_items` having no record for a file-only entry. Worth a one-line comment noting "in-memory and on-disk may diverge on write failure" — purely documentary.

5. **`get_store()` is not thread-safe at construction.**  
   `server/youn_server/notify_store.py:108-113`. Two threads calling `get_store()` simultaneously could each instantiate and assign `_store`, leaving the last writer's instance as the singleton while the first caller's instance is orphaned (but not leaked — garbage-collected). In practice, `get_store()` is called from FastAPI startup and test fixtures, single-threaded. Not a bug for the planned use. Minor.

## Test hygiene

- All 7 tests target observable behavior (return values, status transitions, idempotency), not source-text or implementation detail.
- `tempfile.mkdtemp` per test gives clean isolation; no cross-test pollution.
- No mocks, no `unittest.mock` overreach — the store is exercised against a real JSONL on disk.
- Test names are descriptive (`test_enqueue_and_fifo_order`, `test_ack_idempotent`, etc.).
- TDD discipline preserved (failing test → implement → green).

## Verification performed

- Ran `cd server && ./.venv/bin/python -m pytest tests/test_notify_store.py -q` → `7 passed in 0.03s` (matches report).
- Smoke-checked all 4 claims from the implementer's report:
  - Unicode (`你好`, `✓`) round-trips through JSONL. ✅
  - Fresh `NotifyStore(path)` reload returns `status="shown"`. ✅
  - `get_store() is get_store()` → `True`. ✅
  - `is_expired` boundaries at ttl=0 and ttl=300. ✅
- Independent deadlock repro with plan's `Lock`: thread hangs at `next_for` (timeout exit 124). ✅ Confirms plan-as-written is broken.
- 4-thread × 50-iteration `enqueue`+`next_for` race on the shipped code completes in 0.08s with `RLock`. ✅

## Verdict: **APPROVE with plan-amendment follow-up**

The code is correct, minimal, faithful to the plan's interface, passes all acceptance tests, and the one deviation (`Lock` → `RLock`) is a necessary bug fix that makes the plan's own structure work rather than a workaround or scope creep. The implementer's bug discovery and one-line fix are exactly the kind of well-justified deviation this review process exists to catch.

**Mandatory follow-up (do not block merge, but flag explicitly to main agent):**
- `docs/superpowers/plans/2026-09-02-notify-confirm.md` Task 1 Step 3 sample code shows `threading.Lock()`. That sample is **deadlock-prone as written** and must be amended to `threading.RLock()` before any future agent copies it for another module (e.g., `mcp_server.py`, paired-task code in the same plan). Without this, the bug will silently re-appear in parallel work.
- The review-package path mismatch (`server/.superpowers/...` vs `.superpowers/...` at repo root) should be reconciled so the next reviewer's instructions don't re-route to a non-existent file.

No re-implementation required. Ship Task 1 as `fef8217`.
