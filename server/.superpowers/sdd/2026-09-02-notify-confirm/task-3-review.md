# Task 3 Review — FastMCP Notification Tools on /mcp

**Reviewer:** Task3Reviewer (code-reviewer)
**Date:** 2026-09-02
**Commit reviewed:** `53aa18e` — "feat(server): FastMCP notification tools on /mcp"
**Note:** The referenced `task-3-review-package.txt` was never produced; review performed against the working tree (commit 53aa18e verified via `git show --stat`: 4 files, +157).

## Spec compliance: ✅

| Requirement | Status | Evidence |
|---|---|---|
| `requirements.txt` 新增 `fastmcp>=2.0` | ✅ | `server/requirements.txt:10` |
| 3 tools: `push_notification` / `list_notifications` / `ack_notification` | ✅ | `server/youn_server/mcp_server.py:19,32,39` — signatures match plan (device trust check, `agree\|reject` validation, not-found handling all present) |
| Tools call `notify_store` directly (in-process, no HTTP) | ✅ | `mcp_server.py:26,34,44` — `ns.get_store().enqueue/recent/ack`, zero HTTP |
| fastmcp 4.x pattern: `FastMCP("name")`, no transport kwarg | ✅ | `mcp_server.py:13` |
| `http_app(transport="streamable-http")` | ✅ | `app.py:672` |
| Lifespan propagated | ✅ | `app.py:674` — `app.router.lifespan_context = mcp_subapp.lifespan` |
| 2 tests pass | ✅ | Verified locally: `pytest tests/test_mcp.py -q` → 2 passed in 1.80s |

The one spec deviation — mounting at `/` instead of `/mcp` — was investigated and is **correct**. `http_app()` with default path already creates a `Mount('/mcp')` inside the sub-app (verified by introspecting `sub.routes` on installed fastmcp 2.10.6); mounting that sub-app at `/mcp` would yield the double path `/mcp/mcp`. This matches the plan amendment committed after the initial dispatch. Tests confirm the endpoint responds at exactly `/mcp`.

## Strengths

1. **Tests exercise the real protocol.** Rather than shortcutting the streamable-HTTP handshake, the test performs `initialize` → `notifications/initialized` → `tools/call` with correct `Accept`/`mcp-session-id` headers (`test_mcp.py:38-52,58-74`). This genuinely verifies mount wiring, lifespan init (a broken lifespan would make `initialize` fail), and end-to-end tool execution including device trust registration (`registry.upsert` + `approve` before the call).
2. **The double-mount trap was caught and documented.** Both the report and the in-code comment (`app.py:671`) explain why `/` is the right mount point — exactly the kind of non-obvious framework behavior future maintainers would otherwise re-derive painfully.
3. **Graceful degradation preserved.** The `try/except ImportError` guard (`app.py:669-676`) keeps the app bootable when fastmcp is absent.
4. **Test docstring documents the protocol contract** (Accept header, session handshake), making the test self-explanatory.

## Issues

### Critical
None.

### Important

1. **`app.py:674` — overwriting `app.router.lifespan_context` breaks any future (or pre-existing) explicit lifespan.** `create_app()` currently constructs `FastAPI(...)` with no `lifespan=` kwarg (`app.py:180-184`), so the overwrite is harmless **today** — I verified empirically that with a bare FastAPI app the MCP lifespan works and `initialize` returns 200. But two latent traps:
   - If anyone later adds `FastAPI(lifespan=...)` to `create_app()`, the MCP mount block silently clobbers it. The robust pattern is composition:
     ```python
     from contextlib import asynccontextmanager
     prev = app.router.lifespan_context
     @asynccontextmanager
     async def lifespan(a):
         async with prev(a), mcp_subapp.lifespan(a):
             yield
     app.router.lifespan_context = lifespan
     ```
   - The module-level `install_shutdown_handlers(app)` (`app.py:701-713`, registered via deprecated `@app.on_event`) is currently unaffected only because of Starlette's legacy `on_event` backstop — I verified `on_event` startup handlers stop firing once `router.lifespan_context` is replaced by a non-`_DefaultLifespan` contextmanager. In this app the shutdown hook only best-effort-closes httpx clients (the function docstring itself says "uvicorn handles asyncio cleanup"), so the blast radius is minor — but it's a real behavior change invisible in the diff. At minimum, add a comment; ideally compose lifespans.

### Minor

2. **`mcp_server.py:8` — unused import `settings`.** `from .config import settings` is imported but never referenced (the plan's pseudo-code also had this; the tools never check `operator_token` — see next item). Flake8 `F401` would flag it.

3. **`/mcp` endpoint is unauthenticated while the Task 2 HTTP endpoints require Bearer tokens.** The plan's Interfaces section lists `settings.operator_token` as consumed by Task 3, but no tool checks it. The report flags this as concern #3 and attributes it to the plan's own code — accurate: the plan's `mcp_server.py` sample has no auth either. Since `/mcp` shares port 9002 with the operator UI, any network peer that can reach the port can push/list/ack notifications on trusted devices. Low immediate risk on a LAN appliance, but it should be a conscious decision recorded in the plan (or a follow-up adding bearer middleware on the sub-app), not an accident.

4. **`test_mcp.py:55` — `test_mcp_probe` asserts only `status != 404`.** Fine as a mount smoke check, but the assertion would also pass on 500. The docstring already notes a bare GET should yield 406; asserting `r.status_code == 406` would be tighter and self-documenting. Non-blocking.

## Verdict: **APPROVE**

Spec is fully met, the plan amendment (mount at `/`) is correctly applied and justified, and the 2 tests pass against the real streamable-HTTP protocol (verified locally). The Important item (lifespan composition) is a latent-robustness fix, not a defect in current behavior — recommend addressing it in a small follow-up or folding the composition pattern in now while the context is fresh. The auth gap on `/mcp` originates in the plan itself and should be settled at plan level, not in this task.
