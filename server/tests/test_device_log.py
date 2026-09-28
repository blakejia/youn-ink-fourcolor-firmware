"""Device log upload: storage layer (devicelog.py) and HTTP endpoints.

Isolation: ``isolate_pages_dir`` swaps data_dir per test; this module points
``settings.device_log_dir`` at the same temp dir so nothing lands in the
running server's data/devicelogs.
"""
from __future__ import annotations

import base64
import tempfile
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, _pairing_store
from youn_server.config import settings
from youn_server import devicelog

from .device_sig import signed_headers


@pytest.fixture(autouse=True)
def _isolate_device_log_dir():
    """Point device_log_dir at a fresh temp dir and restore after."""
    tmp = tempfile.mkdtemp(prefix="devicelog_test_")
    orig = settings.device_log_dir
    settings.device_log_dir = Path(tmp)
    # The pairing rate-limit windows are process-local on the shared
    # PairingStore (5 pair-starts per IP per 5 min). This module pairs ten
    # times from the one test-client IP, so without a reset every test after
    # the fifth fails in the fixture with 429 — same reason test_pairing and
    # test_notify clear these.
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()
    yield
    settings.device_log_dir = orig
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()


def test_append_then_tail_round_trips():
    written = devicelog.append_lines(
        "NOTE4C-TEST", "2026-09-28T12:30:11+08:00",
        "I (27930) CustomLcdDisplay: EPD busy wait: 15000 ms\n"
        "W (27931) CustomLcdDisplay: EPD busy wait: 20000 ms\n",
        dropped=0,
    )
    assert written == 2
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert truncated is False
    assert lines[0].endswith("CustomLcdDisplay: EPD busy wait: 15000 ms")
    assert lines[0].startswith("2026-09-28T12:30:11+08:00 ")
    assert lines[1].endswith("EPD busy wait: 20000 ms")


def test_dropped_marker_is_written():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=7)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped 7 lines" in text


def test_no_dropped_marker_when_zero():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=0)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped" not in text


def test_tail_reports_truncation():
    body = "".join(f"line {i}\n" for i in range(50))
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", body, dropped=0)
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert len(lines) == 10
    assert truncated is True
    assert lines[-1].endswith("line 49")


def test_rotation_moves_the_full_file_aside(monkeypatch):
    """Design §7 lists rotation as a required case. Shrink the threshold rather
    than writing 5 MB; the constant is the module's only knob."""
    monkeypatch.setattr(devicelog, "_ROTATE_BYTES", 64)
    devicelog.append_lines("NOTE4C-TEST", "t0", "a" * 100 + "\n", dropped=0)
    devicelog.append_lines("NOTE4C-TEST", "t1", "second\n", dropped=0)
    base = settings.device_log_dir / "NOTE4C-TEST.log"
    assert base.read_text() == "t1 second\n", "current holds only the fresh append"
    assert (settings.device_log_dir / "NOTE4C-TEST.log.1").read_text().startswith("t0 aaa")


def test_rotation_keeps_newest_previous_at_1_and_caps(monkeypatch):
    monkeypatch.setattr(devicelog, "_ROTATE_BYTES", 8)
    for i in range(5):
        devicelog.append_lines("NOTE4C-TEST", f"t{i}", "x" * 20 + "\n", dropped=0)
    d = settings.device_log_dir
    assert (d / "NOTE4C-TEST.log.1").read_text().startswith("t3")
    assert (d / "NOTE4C-TEST.log.2").read_text().startswith("t2")
    assert (d / "NOTE4C-TEST.log.3").read_text().startswith("t1")
    assert not (d / "NOTE4C-TEST.log.4").exists(), "capped at .3"


def test_tail_unknown_device_is_empty():
    lines, truncated = devicelog.tail_lines("NO-SUCH-DEVICE", 10)
    assert lines == []
    assert truncated is False


def test_device_id_with_path_separator_is_rejected():
    """A device id is a filename component; traversal must not reach disk."""
    with pytest.raises(ValueError):
        devicelog.append_lines("../escape", "2026-09-28T12:30:11+08:00", "x\n", dropped=0)


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


@pytest.fixture()
def trusted_device(client):
    """Register + trust a device through the real pairing flow."""
    r = client.post("/api/devices/pair-start",
                    json={"device_id": "NOTE4C-TEST", "board_type": "NOTE4C"},
                    headers=signed_headers("NOTE4C-TEST"))
    assert r.status_code == 200
    code = r.json()["code"]
    client.post("/api/devices/pair-confirm",
                json={"device_id": "NOTE4C-TEST", "code": code},
                headers={"X-Operator-Token": ""})
    r = client.post("/api/devices/pair-claim",
                    json={"device_id": "NOTE4C-TEST", "code": code})
    return "NOTE4C-TEST", r.json()["token"]


def test_log_upload_defaults_to_no_opinion(client, trusted_device):
    """Both sides silent => off. The stored value must be NULL, not 0: a
    DEFAULT 0 would read as an opinion and void the local switch."""
    device_id, _ = trusted_device
    r = client.get("/api/devices", headers={"X-Operator-Token": ""})
    dev = next(d for d in r.json()["devices"] if d["device_id"] == device_id)
    assert dev["log_upload"] is None


def test_operator_enable_reaches_schedule_policy(client, trusted_device):
    device_id, token = trusted_device
    r = client.post(f"/api/devices/{device_id}/log-upload",
                    json={"value": 1},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    assert r.json()["log_upload"] == 1

    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["policy"]["log_upload"] == 1


def test_operator_can_clear_back_to_no_opinion(client, trusted_device):
    """Clearing the service setting hands control back to the device."""
    device_id, token = trusted_device
    client.post(f"/api/devices/{device_id}/log-upload",
                json={"value": 1}, headers={"X-Operator-Token": ""})
    r = client.post(f"/api/devices/{device_id}/log-upload",
                    json={"value": None}, headers={"X-Operator-Token": ""})
    assert r.json()["log_upload"] is None

    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["policy"]["log_upload"] is None


def test_operator_explicit_off_is_distinct_from_no_opinion(client, trusted_device):
    """0 and NULL are different states: 0 overrides the local switch."""
    device_id, token = trusted_device
    client.post(f"/api/devices/{device_id}/log-upload",
                json={"value": 0}, headers={"X-Operator-Token": ""})
    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["policy"]["log_upload"] == 0


def test_schedule_echoes_local_opinion(client, trusted_device):
    """The uplink carries the device's own switch so the UI can explain why an
    enabled service setting has no effect."""
    device_id, token = trusted_device
    r = client.get("/api/pages/schedule?lo=2",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["local_log_upload"] == 2
    # Absent means "no opinion", and it must not be confused with 0.
    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["local_log_upload"] == 0


def test_device_log_requires_token(client, trusted_device):
    r = client.post("/api/device-log", json={"seq_hi": 1, "dropped": 0, "lines": ""})
    assert r.status_code == 401


def test_device_log_writes_base64_lines(client, trusted_device):
    device_id, token = trusted_device
    payload = base64.b64encode(b"I (1) Tag: hello\nW (2) Tag: world\n").decode()
    r = client.post("/api/device-log",
                    json={"seq_hi": 2, "dropped": 0, "lines": payload},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 201
    assert r.json()["written"] == 2
    text = (settings.device_log_dir / f"{device_id}.log").read_text()
    assert "Tag: hello" in text and "Tag: world" in text


def test_device_log_rejects_oversize_body(client, trusted_device):
    device_id, token = trusted_device
    big = base64.b64encode(b"x" * (4096 + 1)).decode()
    r = client.post("/api/device-log",
                    json={"seq_hi": 1, "dropped": 0, "lines": big},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 400


def test_device_log_rejects_bad_base64(client, trusted_device):
    device_id, token = trusted_device
    r = client.post("/api/device-log",
                    json={"seq_hi": 1, "dropped": 0, "lines": "!!!not base64!!!"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 400


def test_logs_tail_endpoint_returns_lines(client, trusted_device):
    device_id, token = trusted_device
    payload = base64.b64encode(b"a\nb\nc\n").decode()
    client.post("/api/device-log",
                json={"seq_hi": 3, "dropped": 0, "lines": payload},
                headers={"Authorization": f"Bearer {token}"})
    r = client.get(f"/api/devices/{device_id}/logs",
                   params={"tail": 2},
                   headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    body = r.json()
    assert len(body["lines"]) == 2
    assert body["lines"][-1].endswith("c")
    assert body["truncated"] is True
