"""battery_history table + schedule GET ?v=&p=&c= ingest (spec 2026-09-22)."""
from __future__ import annotations

import json
import time

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, registry

DEV = "NOTE4C-BATTERY"
_TOKEN = "t" * 64


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


@pytest.fixture(autouse=True)
def _clean_history():
    """Isolate the append-only history per test (shared process registry)."""
    _register()
    try:
        with registry._lock:
            registry._conn.execute(
                "DELETE FROM battery_history WHERE device_id = ?", (DEV,)
            )
    except Exception:
        pass  # red phase: table does not exist yet
    yield


def _get_schedule(client, query=""):
    return client.get(f"/api/pages/schedule{query}", headers=_auth())


def test_full_battery_params_insert_one_row(client):
    now = int(time.time())
    r = _get_schedule(client, "?w=3&a=900&r=400&g=1&f=0&rr=3&v=3980&p=76&c=4")
    assert r.status_code == 200
    rows = registry.battery_history(DEV, since_ts=now - 60)
    assert len(rows) == 1
    row = rows[0]
    assert (row["mv"], row["pct"], row["charge"]) == (3980, 76, 4)
    assert row["wakes"] == 3 and row["awake_ms"] == 900 and row["radio_ms"] == 400


def test_missing_battery_params_change_nothing(client):
    now = int(time.time())
    assert _get_schedule(client, "?w=1&a=2&r=3&g=4&f=5&rr=3").status_code == 200
    assert registry.battery_history(DEV, since_ts=now - 60) == []


def test_partial_battery_params_change_nothing(client):
    now = int(time.time())
    assert _get_schedule(client, "?w=1&v=3900&p=70").status_code == 200  # 缺 c
    assert registry.battery_history(DEV, since_ts=now - 60) == []


def test_out_of_range_values_dropped(client):
    now = int(time.time())
    for bad in ("?v=100&p=50&c=4", "?v=6000&p=50&c=4", "?v=3900&p=101&c=4",
                "?v=3900&p=50&c=9"):
        assert _get_schedule(client, bad).status_code == 200
    assert registry.battery_history(DEV, since_ts=now - 60) == []


def test_purge_drops_rows_older_than_90_days(client):
    now = int(time.time())
    old = now - 91 * 86400
    registry.add_battery_sample(DEV, old, 4000, 90, 4, {})
    _get_schedule(client, "?w=1&a=1&r=1&g=1&f=0&v=3980&p=76&c=4")
    rows = registry.battery_history(DEV, since_ts=now - 100 * 86400)
    assert rows
    assert old not in [r["ts"] for r in rows]
    assert all(r["ts"] >= now - 90 * 86400 - 5 for r in rows)


def test_snapshot_gains_battery_fields_only_with_params(client):
    _get_schedule(client, "?w=1&a=2&r=3&g=4&f=5&v=3980&p=76&c=4")
    snap = json.loads(registry.get_power_counters(DEV))
    assert snap["battery_mv"] == 3980 and snap["battery_pct"] == 76
    assert snap["battery_charge"] == 4


def test_snapshot_without_params_has_no_battery_fields(client):
    _get_schedule(client, "?w=1&a=2&r=3&g=4&f=5")
    snap = json.loads(registry.get_power_counters(DEV))
    assert "battery_mv" not in snap
    assert "battery_pct" not in snap
    assert "battery_charge" not in snap
