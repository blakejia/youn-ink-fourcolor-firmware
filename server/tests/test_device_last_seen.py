"""`last_seen` must advance on every authenticated device request.

The device polls over plain HTTP (`/api/pages/schedule`, `/api/notifications/next`)
and only opens a WebSocket for the hold-to-confirm flow. Before this fix the
registry was written *only* in the WS hello path, so a device that had been
polling for days still showed the timestamp of its last WS handshake — the admin
UI reported "4 天前" for a device that was checking in every 10 seconds.

Covers:
- the schedule GET (the endpoint every poll hits) advances `last_seen`
- the value is the request time, not merely "changed"
- an unauthenticated call must NOT advance it (no free heartbeat forge)
- a stale-by-construction token must not count as activity
"""
from __future__ import annotations

import sqlite3
import time

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, registry
from youn_server.config import settings

DEV = "NOTE4C-LASTSEEN"
_TOKEN = "l" * 64

# An unmistakably old value: 2020-01-01, far below any plausible test-run clock.
_ANCIENT = 1_577_836_800


def _register() -> None:
    registry.upsert(DEV, "NOTE4C", ip_address="127.0.0.1")
    registry.approve(DEV)
    registry.set_token(DEV, _TOKEN)


def _set_last_seen(value: int) -> None:
    """Write the column directly: `upsert` always stamps `now`, which is exactly
    the behaviour under test, so it cannot be used to construct the precondition."""
    with registry._lock:
        registry._conn.execute(
            "UPDATE devices SET last_seen = ? WHERE device_id = ?", (value, DEV)
        )


def _last_seen() -> int:
    return registry.get(DEV).last_seen


def _auth() -> dict:
    return {"Authorization": "Bearer " + _TOKEN}


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_schedule_poll_advances_last_seen(client):
    _register()
    _set_last_seen(_ANCIENT)

    before = int(time.time())
    r = client.get("/api/pages/schedule", headers=_auth())
    after = int(time.time())

    assert r.status_code == 200
    seen = _last_seen()
    assert seen >= before, f"last_seen {seen} 未推进（应 >= {before}）"
    assert seen <= after + 1, f"last_seen {seen} 超出请求时间窗口（应 <= {after}）"


def test_notifications_next_advances_last_seen(client):
    """The other endpoint every awake cycle hits, on the same code path."""
    _register()
    _set_last_seen(_ANCIENT)

    r = client.get(f"/api/notifications/next?device_id={DEV}", headers=_auth())

    assert r.status_code in (200, 204)
    assert _last_seen() > _ANCIENT


def test_unauthenticated_request_does_not_advance_last_seen(client):
    """No token -> no heartbeat. Otherwise anyone could forge liveness."""
    _register()
    _set_last_seen(_ANCIENT)

    r = client.get("/api/pages/schedule")

    assert r.status_code == 401
    assert _last_seen() == _ANCIENT, "未鉴权请求不得刷新 last_seen"


def test_wrong_token_does_not_advance_last_seen(client):
    _register()
    _set_last_seen(_ANCIENT)

    r = client.get("/api/pages/schedule", headers={"Authorization": "Bearer " + "x" * 64})

    assert r.status_code == 401
    assert _last_seen() == _ANCIENT, "错误 token 不得刷新 last_seen"


def test_revoked_device_does_not_advance_last_seen(client):
    """A revoked device is refused by `_require_device_token`; its refusal must
    not read as liveness either."""
    _register()
    _set_last_seen(_ANCIENT)
    registry.revoke(DEV)
    try:
        r = client.get("/api/pages/schedule", headers=_auth())
        assert r.status_code == 401
        assert _last_seen() == _ANCIENT
    finally:
        registry.approve(DEV)
