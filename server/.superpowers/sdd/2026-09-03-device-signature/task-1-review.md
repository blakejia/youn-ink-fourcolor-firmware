# Task 1 Review — Server config + MASTER_KEY + whitelist

**Reviewer:** code review (GrotesqueConstrictor)
**Date:** 2026-09-03
**Commit reviewed:** `87412b9f7613b609cdc069368675e2cdd02ce5a3`
**Method:** plan + report + full commit diff; every changed file read; scoped tests
run; behavior probed empirically via `TestClient`.

> Note: the assigned diff file `task-1-review-package.txt` does not exist in the
> SDD directory. The diff was reconstructed from git (`git show 87412b9`) and the
> live working tree, which is the authoritative source anyway.

---

## 1. Spec compliance: ✅ (with one vacuous test — see Issues)

| Requirement (plan Task 1 + global constraints) | Status | Evidence |
|---|---|---|
| `config.py`: `master_key: str`, default `""`, from `.env` | ✅ | `config.py:70` |
| `config.py`: `allowed_device_ids`, default `""`, comma-separated | ✅ | `config.py:72` |
| `allowed_device_ids` is **str** (contract: comma-separated; Task 2 tolerates str+list) | ✅ | chose `str` per global constraint + Step 3 code, not the plan's stale `list[str]` Interfaces line — correct call |
| `.env.example`: `MASTER_KEY=` + `ALLOWED_DEVICE_IDS=` documented | ✅ | `.env.example:28-34` (includes generation command + fail-closed note) |
| `app.py`: startup validation when key empty | ✅ | `app.py:180-184` logs ERROR; server still starts |
| 3 tests present verbatim from plan | ✅ | `test_device_signature.py:24,35,61` |
| `test_master_key_required`: empty key → pair-start rejected | ✅ | returns **401** `device authentication failed` (assertion allows 401/500) |
| `test_signature_missing_headers`: no headers → 400 | ✅ | returns **400** `missing device auth headers` |
| `test_signature_valid`: signed request → 200 + 6-digit code | ⚠️ passes but **vacuous** | see I-1 |

Test runs (verified, not just claimed):
- `pytest tests/test_device_signature.py -q` → **3 passed**
- `pytest tests/ -q` → **58 passed** (full server suite, no regressions)

---

## 2. Boundary deviation assessment: **Acceptable** (forced cascade, not scope creep)

The implementer touched files beyond the nominal Task-1 config/.env/test trio:
`app.py` (pair-start gating), `conftest.py`, `device_sig.py` (new),
`test_pairing.py` (8 sites), `test_notify.py` (1 site).

This is **correct and safe**, and largely *required*:

1. **app.py was already in the plan's Task 1 file list** (`plan:34` —
   "Modify: `app.py` (+startup 校验)"). The report's Concern #1 misstates this as
   "app.py was not in the plan's Task 1 file list" — it was, for startup
   validation. The report's underlying point still stands: the plan scoped app.py
   to *startup* validation, but the plan's **own three Task-1 tests** exercise
   *endpoint* behavior (401 without key, 400 without headers, 200 signed). Those
   tests cannot pass on a startup log alone — they force the pair-start gate.
2. **The caller migration is a forced consequence.** Once pair-start returns 400
   for headerless requests (required by `test_signature_missing_headers`), the
   nine existing headerless pair-start calls in `test_pairing.py`/`test_notify.py`
   go red. Migrating them is the only way to keep the suite green; leaving them
   broken would be the worse outcome.
3. **No production shim or test-only backdoor.** `device_sig.signed_headers()`
   builds a *genuinely valid* HMAC-SHA256 signature with the spec byte order
   (`MAC(6) || ts(ASCII) || nonce(ASCII)`, `derived = HMAC(MASTER_KEY, device_id)`),
   fresh `os.urandom` nonce per call to dodge the Task-2 replay cache. These
   tests stay green unchanged when Tasks 2/3 turn on real crypto verification.
4. **Clean cutover.** All 9 pair-start call sites are migrated (grep-verified);
   no headerless caller remains. Full suite 58 passed.

**Design concern about doing Task 3's work early:** the gate shipped in Task 1
checks *header presence only*, not signature validity. This is an intentional,
honestly-disclosed staging (report line 51), but it opens a transient window —
see I-1. Acceptable **only because** Tasks 2/3 land in the same SDD sequence and
no signing firmware exists until Tasks 4/5. It must not be deployed with
`MASTER_KEY` set before Task 2 lands.

---

## 3. Strengths

- **Fail-closed default is correct.** Empty `MASTER_KEY` → server starts (so ops
  can diagnose) but *every* pair-start gets 401, plus a loud startup ERROR with
  the fix command. Unconfigured pairing cannot issue codes.
- **Test helper is forward-compatible, not a bypass.** Real crypto, per-device
  derived key, random nonce — it exercises the true authenticated path and needs
  zero changes when verification lands.
- **Header names / order / detail strings match the Task-3 endpoint spec**
  (`401 "device authentication failed"`, `400 "missing device auth headers"`),
  so Task 3's crypto slots in between the presence check and session creation.
- **Config comments and `.env.example` are thorough** — generation command,
  fail-closed semantics, whitelist format all documented.
- **Migration is complete and the full suite is green** (58 passed).

---

## 4. Issues

### Important

- **I-1. The gate accepts forged signatures; `test_signature_valid` is vacuous in Task 1.**
  `app.py:266-273` gates on `MASTER_KEY` non-empty and header *presence* only; it
  never verifies the HMAC. Empirically confirmed with `TestClient`:
  - Forged/garbage `X-Device-Signature` (all 4 headers present) → **200, pairing
    code issued**, device registered (`device_id=EVIL`).
  - The plan's `test_signature_valid` builds a correct signature and asserts 200,
    but the code would return 200 for *any* signature string — the test passes
    without the security it names. Real verification is Task 2 (`pairing.py`
    `verify_device_signature`), so this is expected staging, **not a Task-1
    defect** — but two hard requirements follow:
    1. Task 2 (crypto verification) is a **hard prerequisite** before
       `MASTER_KEY` is set in any reachable/deployed environment. Until then a
       deployment that sets `MASTER_KEY` has *false confidence*: any client
       sending four plausible headers gets a pairing code and a registry entry.
    2. Add a Task-2 test that asserts a **forged** signature → 401 (the plan's
       `test_signature_invalid_key` covers this — ensure it lands and fails
       red against the Task-1 code before Task 2 is implemented).

### Minor

- **M-1. Missing `nonce` in the header-presence check.** `app.py:272`:
  `if not (mac and ts_str and sig):` omits `nonce`, even though `nonce` is read at
  `app.py:270`. Empirically: a request with mac/ts/sig but **no** `X-Device-Nonce`
  → **200, code issued**. Inconsistent with the "four required headers" contract
  and will hand Task 2 an empty nonce for its replay cache. Add `nonce` to the
  condition (Task 2 should own this, or fix now): `if not (mac and ts_str and nonce and sig):`.
- **M-2. Report Concern #1 misstates the plan.** It claims app.py "was not in the
  plan's Task 1 file list"; `plan:34` lists it (`+startup 校验`). Accurate framing:
  startup validation was in scope; the *endpoint gating* exceeded "+startup 校验"
  but was forced by the plan's own tests. Cosmetic, but the SDD record should be
  correct for downstream reviewers.
- **M-3. Unused `monkeypatch` fixture params.** `test_device_signature.py:16`
  (`enable_pairing`) and `:24` (`test_master_key_required`) take `monkeypatch`
  but never use it. Copied verbatim from the plan, so not a deviation — just dead
  params worth dropping if the file is touched again.
- **M-4. Redundant dual autouse key fixtures.** Both `conftest.py:22`
  (`_device_signature_key`) and `test_device_signature.py:15` (`enable_pairing`)
  are autouse and set/reset `settings.master_key`. They set the same value so
  there's no conflict, but the module-level one duplicates the conftest one.
  Harmless; could be collapsed later. (The module fixture also sets
  `allowed_device_ids = []`, a list, which is inert in Task 1 and tolerated by
  Task 2 — fine.)

---

## 5. Verdict: **Approve** (proceed to Task 2, with I-1/M-1 carried forward)

The Task-1 deliverable — config fields, `.env.example`, startup fail-closed
validation, and the three tests — is correct, spec-compliant, and verified (3/3
target tests, 58/58 full suite). The out-of-trio file changes are a **forced,
coherent cascade** (the plan's own tests require endpoint gating, which requires
caller migration), contain **no production shim or backdoor**, and keep the
suite green.

Conditions for the downstream tasks (not blockers for merging Task 1):
- **Task 2 must land and be deployed before `MASTER_KEY` is set anywhere
  reachable** (I-1); add/keep the forged-signature → 401 test.
- Add `nonce` to the presence check (M-1), ideally in Task 2 where the value is
  consumed.
- Correct the report's app.py-scope statement (M-2) for the SDD record.
