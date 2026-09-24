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

def test_refresh_activity_counters_are_saved_with_battery_sample(client):
    now = int(time.time())
    r = _get_schedule(
        client,
        "?w=3&a=900&r=400&g=1&f=0&er=2&eb=3456&rr=3&v=3980&p=76&c=4",
    )
    assert r.status_code == 200
    row = registry.battery_history(DEV, since_ts=now - 60)[0]
    assert row["epd_refreshes"] == 2
    assert row["epd_busy_ms"] == 3456
    snap = json.loads(registry.get_power_counters(DEV))
    assert snap["epd_refreshes"] == 2
    assert snap["epd_busy_ms"] == 3456


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
    snap = json.loads(registry.get_power_counters(DEV))
    assert "battery_mv" not in snap
    assert "battery_pct" not in snap
    assert "battery_charge" not in snap


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


# ── power-history endpoint ──────────────────────────────────────────────
_OP_TOKEN = "op_test_token"


@pytest.fixture(autouse=True)
def _operator_token_for_power_history():
    """Enable operator auth for power-history tests (conftest clears it)."""
    import youn_server.config as config_mod
    orig = config_mod.settings.operator_token
    config_mod.settings.operator_token = _OP_TOKEN
    yield
    config_mod.settings.operator_token = orig


def _op_headers() -> dict:
    return {"X-Operator-Token": _OP_TOKEN}


def test_power_history_requires_operator_token(client):
    """No X-Operator-Token header → 401 when operator token is configured."""
    r = client.get(f"/api/devices/{DEV}/power-history")
    assert r.status_code == 401


def test_power_history_returns_points_ascending(client):
    """Points come back sorted by ts ascending; counters preserved."""
    now = int(time.time())
    for i, (mv, p) in enumerate([(4000, 90), (3900, 80), (3800, 70)]):
        registry.add_battery_sample(DEV, now - 300 + i * 60, mv, p, 4,
                                   {"wakes": i, "awake_ms": 100 * i,
                                    "epd_refreshes": i + 2,
                                    "epd_busy_ms": 500 * (i + 1)})
    r = client.get(f"/api/devices/{DEV}/power-history?hours=1",
                   headers=_op_headers())
    assert r.status_code == 200
    pts = r.json()["points"]
    assert [p["mv"] for p in pts] == [4000, 3900, 3800]
    assert pts[0]["awake_ms"] == 0 and pts[2]["awake_ms"] == 200
    assert pts[0]["epd_refreshes"] == 2 and pts[2]["epd_refreshes"] == 4
    assert pts[0]["epd_busy_ms"] == 500 and pts[2]["epd_busy_ms"] == 1500

def test_power_history_downsamples_over_500_points(client):
    """600 raw rows over 12 h → capped at 500 buckets by time-bucket average."""
    now = int(time.time())
    for i in range(600):
        registry.add_battery_sample(DEV, now - 600 * 60 + i * 60,
                                    4000 - i // 10, 90, 4, {})
    r = client.get(f"/api/devices/{DEV}/power-history?hours=12",
                   headers=_op_headers())
    assert r.status_code == 200
    assert len(r.json()["points"]) <= 500


def test_power_history_hours_cap_and_default(client):
    """hours=99999 is silently clamped to 2160; returns 200, not 422."""
    r = client.get(f"/api/devices/{DEV}/power-history?hours=99999",
                   headers=_op_headers())
    assert r.status_code == 200  # 超上限钳到 2160，不报错


def test_devices_list_carries_power_snapshot(client):
    """/api/devices must carry each row's power snapshot: the devices page
    renders the battery badge and the charge-state curve straight from
    `device.power` — without it the cell is a dash and no curve renders."""
    _register()
    assert _get_schedule(
        client, "?w=1&a=2&r=3&g=4&f=5&v=3980&p=76&c=4"
    ).status_code == 200
    r = client.get("/api/devices", headers=_op_headers())
    assert r.status_code == 200
    dev = next(d for d in r.json()["devices"] if d["device_id"] == DEV)
    assert dev["power"]["battery_mv"] == 3980
    assert dev["power"]["battery_pct"] == 76
    assert dev["power"]["battery_charge"] == 4
