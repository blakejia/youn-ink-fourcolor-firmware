"""HTTP endpoint tests for the notification queue.

Covers:
- POST /api/notifications        (operator, trusted-device check)
- GET  /api/notifications/next   (device token, FIFO + bitmap)
- POST /api/notifications/{id}/ack (device token, agree|reject)
- GET  /api/notifications/history (operator)

Test isolation notes:
- ``isolate_pages_dir`` (autouse, conftest) swaps ``settings.data_dir`` to a
  fresh temp dir each test. The ``notify_store`` module-level singleton
  therefore needs to be reset too, so each test starts with an empty queue.
- Pairing rate limits (``_rate_limits`` / ``_claim_failures``) are also
  process-local on the PairingStore instance used by app.py; we clear them
  between tests so the 6th pair-start in a row doesn't hit the in-memory
  5/5min limit.
"""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app, _pairing_store
from youn_server.config import settings
from youn_server import notify_store as ns

from .device_sig import signed_headers


@pytest.fixture(autouse=True)
def _clean_notify_state():
    """Reset notify_store singleton + pairing rate-limit windows between tests."""
    # Reset notify_store singleton so each test sees an empty queue pointing
    # at the (just-rotated) temp data_dir from isolate_pages_dir.
    ns._store = None
    # Clear in-memory pairing rate-limit windows on the shared PairingStore.
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()
    yield
    ns._store = None
    _pairing_store._rate_limits.clear()
    _pairing_store._claim_failures.clear()


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


@pytest.fixture()
def trusted_device(client):
    # Register a trusted device via pairing flow (reuse existing pairing)
    r = client.post("/api/devices/pair-start", json={"device_id": "NOTE4C-TEST", "board_type": "NOTE4C"},
                    headers=signed_headers("NOTE4C-TEST"))
    assert r.status_code == 200
    code = r.json()["code"]
    r = client.post("/api/devices/pair-confirm",
                    json={"device_id": "NOTE4C-TEST", "code": code},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    # Plan spec wrote ``token = r.json()["token"]`` here, but pair-confirm only
    # marks the session ready. The token actually comes from pair-claim.
    r = client.post("/api/devices/pair-claim",
                    json={"device_id": "NOTE4C-TEST", "code": code})
    assert r.status_code == 200
    token = r.json()["token"]
    return "NOTE4C-TEST", token


def test_create_notification(client, trusted_device):
    device_id, _ = trusted_device
    r = client.post("/api/notifications",
                    json={"device_id": device_id, "title": "t", "body": "b"},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 201
    assert r.json()["notification"]["status"] == "pending"


def test_create_requires_trusted_device(client):
    r = client.post("/api/notifications",
                    json={"device_id": "UNKNOWN", "title": "t", "body": "b"})
    assert r.status_code == 400
    assert "not trusted" in r.json()["detail"].lower()


def test_next_returns_bitmap_and_meta(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next",
                   params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    data = r.json()
    assert "bitmap_base64" in data
    assert data["notification"]["title"] == "t"
    # Second pull: no pending left
    r2 = client.get("/api/notifications/next",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 204


def test_next_requires_device_token(client, trusted_device):
    device_id, _ = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"})
    r = client.get("/api/notifications/next", params={"device_id": device_id})
    assert r.status_code == 401


def test_ack_notification(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"})
    r = client.get("/api/notifications/next", params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    nid = r.json()["notification"]["id"]
    r = client.post(f"/api/notifications/{nid}/ack",
                    json={"decision": "agree"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["status"] == "acked"
    assert r.json()["decision"] == "agree"


def test_ack_404_for_unknown(client, trusted_device):
    _, token = trusted_device
    r = client.post("/api/notifications/unknown-id/ack",
                    json={"decision": "agree"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 404


def test_ack_rejects_another_devices_notification(client, trusted_device):
    """A paired device's token cannot ack a notification owned by another device.

    Regression: ack discarded the Device returned by ``_require_device_token``,
    so any trusted device could agree/reject any notification id (the ``next``
    endpoint does cross-check device_id, ack did not).
    """
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"})
    r = client.get("/api/notifications/next", params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    nid = r.json()["notification"]["id"]

    # Pair a second device and take its token.
    other_id = "NOTE4C-OTHER"
    r = client.post("/api/devices/pair-start",
                    json={"device_id": other_id, "board_type": "NOTE4C"},
                    headers=signed_headers(other_id))
    code = r.json()["code"]
    client.post("/api/devices/pair-confirm",
                json={"device_id": other_id, "code": code},
                headers={"X-Operator-Token": ""})
    other_token = client.post("/api/devices/pair-claim",
                              json={"device_id": other_id, "code": code}
                              ).json()["token"]

    r = client.post(f"/api/notifications/{nid}/ack",
                    json={"decision": "reject"},
                    headers={"Authorization": f"Bearer {other_token}"})
    assert r.status_code == 403

    # The owner's decision is what lands.
    r = client.post(f"/api/notifications/{nid}/ack",
                    json={"decision": "agree"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["decision"] == "agree"


def test_history(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t1", "body": "b"})
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t2", "body": "b"})
    r = client.get("/api/notifications/history",
                   params={"device_id": device_id},
                   headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    assert len(r.json()["notifications"]) == 2


def test_next_rejects_token_mismatch(client, trusted_device):
    """Trusted device's token cannot drain a different device_id's queue."""
    device_id, token = trusted_device
    # Create a notification for our own device.
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    # Try to drain it under a different device_id — must 401, queue must be intact.
    r = client.get("/api/notifications/next",
                   params={"device_id": "OTHER-DEVICE"},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 401
    # Queue for the legitimate device is still pending and pulls cleanly.
    r2 = client.get("/api/notifications/next",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 200


def test_next_bin_returns_id_plus_bitmap(client, trusted_device):
    """200 = id(32B hex ascii) || bitmap(30000B), total 30032 bytes."""
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next.bin",
                   params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    body = r.content
    assert len(body) == 32 + 30000
    nid = body[:32].decode("ascii")
    assert len(nid) == 32 and all(c in "0123456789abcdef" for c in nid)
    # Second pull: queue drained -> 204 (same atomic mark_shown semantics).
    r2 = client.get("/api/notifications/next.bin",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 204


def test_next_bin_requires_device_token(client, trusted_device):
    device_id, _ = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next.bin", params={"device_id": device_id})
    assert r.status_code == 401


def test_next_bin_rejects_token_mismatch(client, trusted_device):
    """Cross-device drain must 401 and leave the queue intact (same rule as
    the JSON endpoint — one auth boundary, not two)."""
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next.bin",
                   params={"device_id": "OTHER-DEVICE"},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 401
    r2 = client.get("/api/notifications/next.bin",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 200


def test_next_bin_and_json_share_queue_semantics(client, trusted_device):
    """A pull through either endpoint consumes the same FIFO entry exactly
    once — the two endpoints are views over one store, not parallel queues."""
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next",
                   params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    nid = r.json()["notification"]["id"]
    # The binary endpoint sees an empty queue afterwards.
    r2 = client.get("/api/notifications/next.bin",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 204
    # And ack on the id pulled from JSON still works.
    r3 = client.post(f"/api/notifications/{nid}/ack",
                     json={"decision": "agree"},
                     headers={"Authorization": f"Bearer {token}"})
    assert r3.status_code == 200
