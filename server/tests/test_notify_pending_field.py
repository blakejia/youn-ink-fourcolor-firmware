"""Task 4 (power): /api/pages/schedule carries `notify_pending`.

The device polls /api/notifications/next unconditionally on every wake;
when the queue is empty that is a wasted radio round-trip. The schedule
response therefore carries whether anything is pending so the device can
skip the empty poll.
"""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server.devices import registry
from youn_server import notify_store as ns

DEV = "NOTE4C-NP"
_TOKEN = "n" * 64


@pytest.fixture(autouse=True)
def _fresh_notify_store():
    """Point the notify singleton at this test's temp data_dir (see conftest)."""
    ns._store = None
    yield
    ns._store = None


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _register() -> None:
    registry.upsert(DEV, "NOTE4C", ip_address="127.0.0.1")
    registry.approve(DEV)
    registry.set_token(DEV, _TOKEN)


def _auth() -> dict:
    return {"Authorization": "Bearer " + _TOKEN}


def test_schedule_reports_pending_notification(client):
    """Empty queue => notify_pending=false; after enqueue => true."""
    _register()
    body = client.get("/api/pages/schedule", headers=_auth()).json()
    assert body["notify_pending"] is False
    ns.get_store().enqueue(DEV, "t", "b")
    body = client.get("/api/pages/schedule", headers=_auth()).json()
    assert body["notify_pending"] is True


def test_has_pending_is_read_only(client):
    """has_pending must not consume: next_for still delivers afterwards."""
    _register()
    store = ns.get_store()
    store.enqueue(DEV, "t", "b")
    assert store.has_pending(DEV) is True
    assert store.has_pending(DEV) is True
    assert store.next_for(DEV) is not None
    assert store.has_pending(DEV) is False
