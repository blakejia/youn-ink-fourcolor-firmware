"""Device signature authentication tests."""
from __future__ import annotations
import base64
import hmac
import hashlib
import time
import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server.config import settings
from youn_server import pairing as pairing_mod


@pytest.fixture(autouse=True)
def enable_pairing(monkeypatch):
    """Inject MASTER_KEY for the test session."""
    settings.master_key = "test_master_key_at_least_32_bytes_long_xx"
    settings.allowed_device_ids = []
    yield
    settings.master_key = ""


def test_master_key_required(monkeypatch):
    """Without MASTER_KEY, pair-start returns 500 (or 401)."""
    settings.master_key = ""
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start", json={
            "device_id": "DEV-1", "board_type": "NOTE4C"
        })
        assert r.status_code in (401, 500)


def test_signature_valid():
    """Correctly signed pair-start returns 200 + code."""
    device_id = "NOTE4C-TEST"
    now = int(time.time())
    mac_hex = "AABBCCDDEEFF"
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    master = settings.master_key.encode()
    derived = hmac.new(master, device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()

    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={
                       "X-Device-Mac": mac_hex,
                       "X-Device-Timestamp": str(now),
                       "X-Device-Nonce": nonce_b64,
                       "X-Device-Signature": sig,
                   })
        assert r.status_code == 200
        assert "code" in r.json()
        assert len(r.json()["code"]) == 6


def test_signature_missing_headers():
    """Missing signature headers returns 400."""
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": "DEV-1", "board_type": "NOTE4C"})
        assert r.status_code == 400
