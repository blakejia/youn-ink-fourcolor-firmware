"""The schedule GET carries `?rr=<esp_reset_reason>`; the server must persist it.

Remote-device forensics: a device that reboots every ~40 s after its poll is
either browning out (battery) or watchdog-resetting (firmware). The serial
console is not available for a device deployed off-site, so the reset reason
rides the schedule query string next to the power counters and is stored with
them. Covers:
- a poll with rr stores the value (0xf = BROWNOUT on S3)
- a poll without rr (old firmware) must NOT clobber a previously stored reason
"""
from __future__ import annotations

import json

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, registry

DEV = "NOTE4C-RR"
_TOKEN = "r" * 64


def _register() -> None:
    registry.upsert(DEV, "NOTE4C", ip_address="127.0.0.1")
    registry.approve(DEV)
    registry.set_token(DEV, _TOKEN)


def _auth() -> dict:
    return {"Authorization": "Bearer " + _TOKEN}


def _stored() -> dict:
    return json.loads(registry.get_power_counters(DEV) or "{}")


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_reset_reason_is_stored(client):
    _register()
    r = client.get("/api/pages/schedule?w=2&a=156815&r=156815&g=2&f=1&rr=15",
                   headers=_auth())
    assert r.status_code == 200
    assert _stored()["reset_reason"] == 15, "rr=15 must land in the stored snapshot"


def test_missing_rr_keeps_the_previous_reason(client):
    """Old firmware sends no rr; it must not erase a reason seen earlier."""
    _register()
    r1 = client.get("/api/pages/schedule?w=1&a=0&r=0&g=0&f=0&rr=8", headers=_auth())
    assert r1.status_code == 200
    assert _stored()["reset_reason"] == 8

    r2 = client.get("/api/pages/schedule?w=2&a=100&r=100&g=1&f=0", headers=_auth())
    assert r2.status_code == 200
    assert _stored()["reset_reason"] == 8, "a poll without rr must not clobber the stored reason"
    assert _stored()["wakes"] == 2, "the counters themselves still update"
