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


# ── Task 2: verify_device_signature / check_whitelist (unit level) ──────
# Task 2 adds the PairingStore methods; Task 3 wires them into the
# pair-start HTTP endpoint. These tests call the store directly so the
# expected pre-implementation failure is AttributeError on the method.

MAC_HEX = "AABBCCDDEEFF"
NONCE_B64 = base64.b64encode(b"\x00" * 16).decode()


@pytest.fixture
def store(tmp_path):
    """Fresh PairingStore on an isolated DB with an empty nonce cache."""
    s = pairing_mod.PairingStore(tmp_path / "pair.db")
    yield s
    s.close()


def _sign(device_id, ts, nonce_b64, *, mac_hex=MAC_HEX, key=None):
    """Build a signature over MAC(6) || timestamp || nonce.

    Uses settings.master_key by default; pass ``key`` to sign with a
    different MASTER_KEY (wrong-key rejection test).
    """
    master = (settings.master_key if key is None else key).encode()
    derived = hmac.new(master, device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(ts).encode() + nonce_b64.encode()
    return base64.b64encode(
        hmac.new(derived, payload, hashlib.sha256).digest()
    ).decode()


def test_verify_signature_invalid_key(store):
    """Signature produced with a wrong MASTER_KEY is rejected."""
    device_id = "DEV-KEY"
    now = int(time.time())
    sig = _sign(device_id, now, NONCE_B64, key="wrong_key_32_bytes_pad_pad_pad_pad")
    assert store.verify_device_signature(
        device_id, MAC_HEX, now, NONCE_B64, sig
    ) is False


def test_verify_signature_timestamp_out_of_window(store):
    """Timestamp older than the ±30s window is rejected."""
    device_id = "DEV-OLD"
    old_ts = int(time.time()) - 60
    sig = _sign(device_id, old_ts, NONCE_B64)
    assert store.verify_device_signature(
        device_id, MAC_HEX, old_ts, NONCE_B64, sig
    ) is False


def test_verify_signature_nonce_replay(store):
    """Same (device_id, nonce) succeeds once, then is rejected as replay."""
    device_id = "DEV-REPLAY"
    now = int(time.time())
    sig = _sign(device_id, now, NONCE_B64)
    assert store.verify_device_signature(
        device_id, MAC_HEX, now, NONCE_B64, sig
    ) is True
    assert store.verify_device_signature(
        device_id, MAC_HEX, now, NONCE_B64, sig
    ) is False


def test_verify_signature_invalid_mac(store):
    """A non-hex MAC string is rejected before signature comparison."""
    device_id = "DEV-BADMAC"
    now = int(time.time())
    # Sign over the raw non-hex bytes; the server must fail at bytes.fromhex.
    derived = hmac.new(
        settings.master_key.encode(), device_id.encode(), hashlib.sha256
    ).digest()
    payload = b"not_hex!!" + str(now).encode() + NONCE_B64.encode()
    sig = base64.b64encode(
        hmac.new(derived, payload, hashlib.sha256).digest()
    ).decode()
    assert store.verify_device_signature(
        device_id, "not_hex!!", now, NONCE_B64, sig
    ) is False


def test_check_whitelist_rejects_unlisted(store):
    """Empty whitelist allows all; a populated whitelist allows only listed ids."""
    try:
        settings.allowed_device_ids = ""
        assert store.check_whitelist("ANY-DEVICE") is True

        settings.allowed_device_ids = "ALLOWED-DEV, SECOND-DEV"
        assert store.check_whitelist("ALLOWED-DEV") is True
        assert store.check_whitelist("SECOND-DEV") is True
        assert store.check_whitelist("NOT-ALLOWED") is False
    finally:
        settings.allowed_device_ids = ""
