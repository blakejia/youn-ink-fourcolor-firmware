# Task 3 Fix Round 1 Review — FastMCP Lifespan Composition

**Reviewer:** Task3FixRound1Reviewer (code-reviewer)
**Date:** 2026-09-02
**Commit reviewed:** `f78f454` — "fix(server): compose FastMCP lifespan with existing app lifespan"
**Note:** The referenced `task-3-fix-round-1-review-package.txt` was never produced (only `task-3-report.md`/`task-3-review.md` exist in the SDD dir). Review performed against `git show f78f454` directly: 1 file, +12/−1, touching only the MCP mount block in `server/youn_server/app.py`.

## Finding under review

> **Important** (task-3-review.md): `app.py:674` — `app.router.lifespan_context = mcp_subapp.lifespan` overwrites any existing lifespan. Fix: compose lifespans.

## Verdict on the finding: **ADDRESSED** ✅

Evidence — `server/youn_server/app.py:674-685` (verified in working tree, matches `git show f78f454`):

```python
# Compose lifespans so we don't clobber any existing lifespan
# (or legacy on_event handlers) that may be added later.
from contextlib import asynccontextmanager
_original_lifespan = app.router.lifespan_context

@asynccontextmanager
async def _composed_lifespan(app):
    async with _original_lifespan(app):
        async with mcp_subapp.lifespan(app):
            yield

app.router.lifespan_context = _composed_lifespan
```

Checks against the original reviewer's recommended pattern and Starlette semantics:

1. **Composition, not overwrite.** `_original_lifespan` is captured *before* reassignment and entered first; `mcp_subapp.lifespan` is nested inside. Any pre-existing lifespan (including Starlette's `_DefaultLifespan`, which dispatches legacy `on_event` handlers such as `install_shutdown_handlers` at `app.py:701+`) now runs alongside the MCP session-manager lifespan instead of being clobbered. This resolves both latent traps from the original review (future `FastAPI(lifespan=...)` silently dropped; legacy `on_event` shutdown hook silently disabled).
2. **Exit ordering is correct.** Nested `async with` gives LIFO teardown: MCP task group shuts down first, then the original lifespan/on_event shutdown handlers run. Sensible ordering (per-request infrastructure down before app-level shutdown hooks).
3. **Exception propagation is safe.** If the original lifespan raises on enter, the MCP lifespan is never entered (no half-initialized MCP task group). If the MCP lifespan raises on enter, the original lifespan's `__aexit__` still runs via context-manager unwinding. No leak path.
4. **Signature matches Starlette's contract** — `_composed_lifespan(app)` takes the app instance, same as `router.lifespan_context` expects. (The parameter shadows the outer `app`; harmless inside a 3-line closure and idiomatic for lifespan functions.)
5. **Guard behavior unchanged.** The whole block is still inside `try/except ImportError`, so a missing fastmcp still degrades gracefully — the composed assignment only happens after `http_app()` succeeds.

Verification: `pytest tests/test_mcp.py -q` → **2 passed** (confirmed locally). The session-based `initialize`/`tools/call` test exercises lifespan startup on the composed context, so a broken composition would fail this test. Full-suite 55-passed claim from the report is plausible given a 1-file, 12-line, behavior-preserving change; project-wide suite run is the main agent's job per protocol.

## New breakage: **none**

- Diff is 12 lines in one function; no API or route changes.
- No new imports at module level (`asynccontextmanager` imported locally — fine inside `create_app`, consistent with the function's other local imports).
- No change to mount path, transport, or tool behavior; the `/mcp` double-mount reasoning from the original review is untouched.
- Minor nit (not a finding): the import-inside-function style and leading-underscore locals are slightly unusual but match the surrounding block's ad-hoc character and are invisible to callers. Not worth changing.

## Verdict: **All findings addressed — APPROVE**

The single Important finding from round 1 is resolved with the exact composition pattern the original review recommended, teardown ordering and exception semantics are correct, the graceful-degradation guard is preserved, and the scoped tests pass. No request for changes.
