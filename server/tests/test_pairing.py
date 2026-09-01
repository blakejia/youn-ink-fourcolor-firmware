"""Device pairing & auth tests.

Covers:
- Happy path: pair-start → pair-confirm → pair-claim → token auth
- Code expiry
- Code wrong 5 times → 10-min lockout
- Claim reuse after successful claim → 401
- Token reverse lookup (require_device_token)
- Revoke → 401
- WS no token → 401
- schedule/bitmap public access (no token → 200)
- Rate limiting on pair-start
"""
from __future__ import annotations

import time

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, _pairing_store
from youn_server.devices import registry
from youn_server.pairing import PairingStore


import os


@pytest.fixture(autouse=True)
def _clean_pairing():
    """Reset pairing store and device registry state between tests."""
    # Set operator token for tests
    os.environ["OPERATOR_TOKEN"] = "test-operator-token"
    # Clean any leftover pairing sessions
    _pairing_store.cleanup_expired()
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()
    yield
    # Cleanup after test
    _pairing_store.cleanup_expired()
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()
    os.environ.pop("OPERATOR_TOKEN", None)


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


# ── Happy path ───────────────────────────────────────────────────────


def test_pair_full_happy_path(client):
    """Start → confirm → claim → use token on protected endpoint."""
    device_id = "TEST-HAPPY-001"

    # 1. pair-start (public)
    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id,
        "board_type": "NOTE4C",
    })
    assert r.status_code == 200
    body = r.json()
    assert len(body["code"]) == 6
    assert body["expires_in"] == 300
    code = body["code"]

    # 2. pair-confirm (operator)
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id,
        "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    assert r.status_code == 200
    assert r.json()["status"] == "ready"

    # 3. pair-claim (public)
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id,
        "code": code,
    })
    assert r.status_code == 200
    token = r.json()["token"]
    assert len(token) == 64  # secrets.token_hex(32)

    # 4. Verify token grants access to device-protected endpoint
    r = client.get("/api/ota/check", headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200

    # 5. Verify token works for device
    dev = registry.get(device_id)
    assert dev is not None
    assert dev.trusted is True


# ── Code expiry ──────────────────────────────────────────────────────


def test_pair_claim_expired_code(client):
    """Claim with an expired code returns 401."""
    device_id = "TEST-EXPIRE-001"

    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id,
        "board_type": "NOTE4C",
    })
    code = r.json()["code"]

    # Confirm it
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id,
        "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    assert r.status_code == 200

    # Manually expire the session by manipulating the store
    with _pairing_store._lock:
        _pairing_store._conn.execute(
            "UPDATE pairing_sessions SET expires_at = ? WHERE device_id = ?",
            (int(time.time()) - 10, device_id),
        )

    # Claim should fail
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id,
        "code": code,
    })
    assert r.status_code == 401


# ── Wrong code lockout ───────────────────────────────────────────────


def test_pair_claim_wrong_code_lockout(client):
    """5 wrong claim attempts → locked for 10 minutes."""
    device_id = "TEST-LOCK-001"

    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id,
        "board_type": "NOTE4C",
    })
    code = r.json()["code"]

    # Confirm it
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id,
        "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    assert r.status_code == 200

    # 5 wrong claims
    for i in range(5):
        r = client.post("/api/devices/pair-claim", json={
            "device_id": device_id,
            "code": "000000",
        })
        assert r.status_code == 401, f"attempt {i+1} should be 401"

    # 6th attempt should be 429 (locked)
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id,
        "code": "000000",
    })
    assert r.status_code == 429


# ── Claim reuse ──────────────────────────────────────────────────────


def test_pair_claim_reuse_401(client):
    """Successfully claiming then trying again with same code → 401."""
    device_id = "TEST-REUSE-001"

    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id,
        "board_type": "NOTE4C",
    })
    code = r.json()["code"]

    # Confirm
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id,
        "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    assert r.status_code == 200

    # First claim — success
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id,
        "code": code,
    })
    assert r.status_code == 200

    # Second claim — session deleted, should 401
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id,
        "code": code,
    })
    assert r.status_code == 401


# ── Token reverse lookup ─────────────────────────────────────────────


def test_require_device_token_valid(client):
    """Valid trusted device token → 200 on /api/ota/check."""
    device_id = "TEST-TOKEN-001"

    # Full pairing flow
    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id, "board_type": "NOTE4C",
    })
    code = r.json()["code"]
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id, "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id, "code": code,
    })
    token = r.json()["token"]

    r = client.get("/api/ota/check", headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200


def test_require_device_token_invalid(client):
    """Invalid token → 401."""
    r = client.get("/api/ota/check", headers={"Authorization": "Bearer badtoken"})
    assert r.status_code == 401
    assert r.json()["detail"] == "unauthorized"


def test_require_device_token_no_header(client):
    """No Authorization header → 401."""
    r = client.get("/api/ota/check")
    assert r.status_code == 401


# ── Revoke → 401 ────────────────────────────────────────────────────


def test_revoke_then_token_401(client):
    """After revoking a device, its token should 401."""
    device_id = "TEST-REVOKE-001"

    # Full pairing
    r = client.post("/api/devices/pair-start", json={
        "device_id": device_id, "board_type": "NOTE4C",
    })
    code = r.json()["code"]
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": device_id, "code": code,
    }, headers={"X-Operator-Token": "test-operator-token"})
    r = client.post("/api/devices/pair-claim", json={
        "device_id": device_id, "code": code,
    })
    token = r.json()["token"]

    # Verify token works
    r = client.get("/api/ota/check", headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200

    # Revoke device
    r = client.post(f"/api/devices/{device_id}/revoke", headers={
        "X-Operator-Token": "test-operator-token",
    })
    assert r.status_code == 200

    # Token should now fail
    r = client.get("/api/ota/check", headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 401


# ── WS no token → 401 ───────────────────────────────────────────────


def test_ws_no_token_closes(client):
    """WebSocket without Authorization header → close after error."""
    with client.websocket_connect("/ws") as ws:
        # Send hello
        ws.send_json({"deviceId": "TEST-WS-001", "boardType": "NOTE4C"})
        # Should get error and close
        data = ws.receive_text()
        import json
        msg = json.loads(data)
        assert msg.get("type") == "error" or "unauthorized" in str(msg).lower()


# ── Public endpoints stay public ─────────────────────────────────────


def test_schedule_no_token_200(client):
    """GET /api/pages/schedule works without any auth."""
    r = client.get("/api/pages/schedule")
    assert r.status_code == 200


def test_bitmap_no_token_200(client):
    """GET /api/pages/bitmap/{md5}.bin works without auth (404 for unknown is fine)."""
    r = client.get("/api/pages/bitmap/00000000000000000000000000000000.bin")
    # 404 is expected for non-existent bitmap — the point is no 401
    assert r.status_code == 404


def test_health_no_token_200(client):
    """GET /api/health works without auth."""
    r = client.get("/api/health")
    assert r.status_code == 200


# ── Rate limiting ────────────────────────────────────────────────────


def test_pair_start_rate_limit(client):
    """6th pair-start from same IP within 5 min → 429."""
    # 5 allowed
    for i in range(5):
        r = client.post("/api/devices/pair-start", json={
            "device_id": f"TEST-RL-{i:03d}",
            "board_type": "NOTE4C",
        })
        assert r.status_code == 200, f"request {i+1} should be 200"

    # 6th blocked
    r = client.post("/api/devices/pair-start", json={
        "device_id": "TEST-RL-005",
        "board_type": "NOTE4C",
    })
    assert r.status_code == 429


# ── OTA download auth ────────────────────────────────────────────────


def test_ota_download_no_token_401(client):
    """GET /api/ota/download without token → 401."""
    r = client.get("/api/ota/download/firmware.bin")
    assert r.status_code == 401


# ── Confirm without operator token → 401 ─────────────────────────────


def test_pair_confirm_no_operator_token_401(client):
    """pair-confirm without operator token → 401."""
    r = client.post("/api/devices/pair-confirm", json={
        "device_id": "TEST-CONFIRM-001",
        "code": "123456",
    })
    assert r.status_code == 401
