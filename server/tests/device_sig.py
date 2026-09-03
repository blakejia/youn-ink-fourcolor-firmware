"""Test helper for device-signature pair-start requests.

Builds the four ``X-Device-*`` headers with a valid HMAC-SHA256 signature
derived from ``TEST_MASTER_KEY``, so existing pairing/notify tests exercise
the authenticated pair-start endpoint. Uses a fresh random nonce per call to
avoid the (Task 2) nonce-replay cache.
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import os
import time

# Mirrors the key injected by the autouse fixture in conftest.py and the
# enable_pairing fixture in test_device_signature.py.
TEST_MASTER_KEY = "test_master_key_at_least_32_bytes_long_xx"


def signed_headers(
    device_id: str,
    *,
    mac: str = "AABBCCDDEEFF",
    timestamp: int | None = None,
) -> dict[str, str]:
    """Return X-Device-* headers carrying a correct signature for device_id."""
    ts = int(time.time()) if timestamp is None else timestamp
    nonce_b64 = base64.b64encode(os.urandom(16)).decode()
    derived = hmac.new(
        TEST_MASTER_KEY.encode(), device_id.encode(), hashlib.sha256
    ).digest()
    payload = bytes.fromhex(mac) + str(ts).encode() + nonce_b64.encode()
    sig = base64.b64encode(
        hmac.new(derived, payload, hashlib.sha256).digest()
    ).decode()
    return {
        "X-Device-Mac": mac,
        "X-Device-Timestamp": str(ts),
        "X-Device-Nonce": nonce_b64,
        "X-Device-Signature": sig,
    }
