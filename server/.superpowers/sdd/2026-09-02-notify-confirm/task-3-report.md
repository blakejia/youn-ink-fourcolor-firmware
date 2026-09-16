# Task 3 Report — FastMCP Notification Tools on /mcp

**Status:** COMPLETED
**Date:** 2026-09-02 18:49:40
**Commit:** 53aa18e
**Task:** Implement FastMCP notification tools on /mcp per amended plan

## Summary

Implemented Task 3 of the notify-confirm plan: FastMCP server exposing three notification tools (`push_notification`, `list_notifications`, `ack_notification`) mounted on `/mcp` with Streamable HTTP transport.

## Files Created

- `server/youn_server/mcp_server.py` — FastMCP server with 3 tools
- `server/tests/test_mcp.py` — 2 tests covering /mcp mount and tool call

## Files Modified

- `server/youn_server/app.py` — Added MCP mount block before SPA mount
- `server/requirements.txt` — Added `fastmcp>=2.0`

## Key Implementation Details

### fastmcp 4.x API Pattern

The plan was amended for fastmcp 4.x (installed: 2.10.6). The correct pattern:
```python
mcp = FastMCP("youn-notify")  # no transport kwarg
sub_app = mcp.http_app(transport="streamable-http")
app.mount("/", sub_app)  # mount at root, NOT /mcp
app.router.lifespan_context = sub_app.lifespan
```

### Critical Discovery: Mount Path

The plan specified `app.mount("/mcp", sub_app)`, but this creates a **double-mount**:
- `http_app()` with default path creates route at `/mcp` inside the sub-app
- Mounting at `/mcp` results in final URL `/mcp/mcp`
- **Fix:** Mount at `/` (root) so the sub-app's `/mcp` route becomes `/mcp` on the parent

### Streamable HTTP Transport Requirements

The MCP streamable HTTP transport enforces:
- `Accept: application/json, text/event-stream` header on all requests
- Session initialization via `initialize` method before any `tools/call`
- Session ID returned in `mcp-session-id` header must be sent on subsequent requests

### Test Adjustments

The plan's tests were written assuming a simpler HTTP handler. The actual Streamable HTTP transport requires:
1. `Accept` header (else 406)
2. `initialize` + `notifications/initialized` handshake (else 400)

The tests were rewritten to:
- `test_mcp_probe`: Assert `/mcp` is not 404 (i.e., mounted and reachable)
- `test_mcp_tool_call_push_notification`: Full session flow with proper headers

## Test Results

```text
$ cd server && ./.venv/bin/python -m pytest tests/test_mcp.py -q
..                                                                       [100%]
2 passed, 3 warnings in 1.69s

$ cd server && ./.venv/bin/python -m pytest tests/ -q
55 passed, 3 warnings in 18.42s
```

## Concerns

1. **Double-mount fix deviates from plan**: The plan explicitly said `app.mount("/mcp", sub_app)`, but this doesn't work with fastmcp's default routing. The fix (`app.mount("/", sub_app)`) achieves the intended `/mcp` endpoint but uses a different mount path.

2. **Test rewrite**: The plan's tests didn't account for MCP streamable HTTP protocol requirements (Accept headers, session init). The rewritten tests are more verbose but verify real behavior.

3. **No operator token auth**: The plan's tools don't enforce operator token (unlike HTTP endpoints in Task 2). This is per the plan's mcp_server.py code. If auth is needed, it should be added as middleware or per-tool checks.

4. **fastmcp version**: The plan mentioned "4.0.1" but installed is 2.10.6. The API pattern (`http_app`, `transport="streamable-http"`) works in both.

## Next Steps

- Task 4: Firmware BSP + notify module (GPIO buttons, HTTP client)


## Post-Review Fix (f78f454)

**Reviewer feedback:** `app.router.lifespan_context = mcp_subapp.lifespan` overwrites any existing lifespan. Safe today but silently clobbers future `lifespan=` and disables legacy `on_event` handlers.

**Fix:** Compose lifespans instead of overwriting:

```python
from contextlib import asynccontextmanager

_original_lifespan = app.router.lifespan_context

@asynccontextmanager
async def _composed_lifespan(app):
    async with _original_lifespan(app):
        async with mcp_subapp.lifespan(app):
            yield

app.router.lifespan_context = _composed_lifespan
```

**Verification:** `2 passed` on `tests/test_mcp.py -q`; `55 passed` on full suite.
