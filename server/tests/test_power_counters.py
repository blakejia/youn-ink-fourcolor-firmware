"""Power-counter piggyback on the schedule GET (Task 1).

The device rides its duration ledger on the schedule poll query string
(?w=&a=&r=&g=&f=); the server stores the snapshot and echoes it back as
"power". Follows the existing test_schedule_api.py client/device style.
"""
from __future__ import annotations

import json

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, registry

DEV = "NOTE4C-POWER"
_TOKEN = "p" * 64


def _register() -> None:
    registry.upsert(DEV, "NOTE4C", ip_address="127.0.0.1")
    registry.approve(DEV)
    registry.set_token(DEV, _TOKEN)


def _auth() -> dict:
    return {"Authorization": "Bearer " + _TOKEN}


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_schedule_echoes_power_counters(client):
    _register()
    r = client.get(
        "/api/pages/schedule?w=7&a=1234&r=567&g=2&f=890", headers=_auth()
    )
    assert r.status_code == 200
    body = r.json()
    assert body["power"] == {
        "wakes": 7,
        "awake_ms": 1234,
        "radio_ms": 567,
        "http_gets": 2,
        "refresh_ms": 890,
    }


def test_schedule_without_counters_reads_as_zero(client):
    _register()
    r = client.get("/api/pages/schedule", headers=_auth())
    assert r.status_code == 200
    body = r.json()
    assert body["power"] == {
        "wakes": 0,
        "awake_ms": 0,
        "radio_ms": 0,
        "http_gets": 0,
        "refresh_ms": 0,
    }


def test_power_snapshot_is_persisted(client):
    _register()
    r = client.get(
        "/api/pages/schedule?w=3&a=100&r=50&g=1&f=20", headers=_auth()
    )
    assert r.status_code == 200
    stored = registry.get_power_counters(DEV)
    assert stored is not None
    assert json.loads(stored) == {
        "wakes": 3,
        "awake_ms": 100,
        "radio_ms": 50,
        "http_gets": 1,
        "refresh_ms": 20,
    }
