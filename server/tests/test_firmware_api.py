"""Firmware repository endpoint tests.

The gate is fail-closed *by design*: unlike every other operator endpoint in
this app, an unconfigured OPERATOR_TOKEN must not open firmware read/write.
"""
from __future__ import annotations

import hashlib
import os

import pytest
from fastapi.testclient import TestClient

from youn_server import serial_firmware as sf
from youn_server.app import create_app
from youn_server.config import settings

HEADER = bytes([0xE9]) + b"\x00" * 31
TOKEN = "test-operator-token"


@pytest.fixture(autouse=True)
def _store(tmp_path, monkeypatch):
    monkeypatch.setattr(settings, "serial_firmware_dir", tmp_path / "serial-firmware")
    os.environ["OPERATOR_TOKEN"] = TOKEN
    yield
    os.environ.pop("OPERATOR_TOKEN", None)


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_listing_requires_the_operator_token(client):
    assert client.get("/api/firmware").status_code == 401


def test_download_requires_the_operator_token(client):
    assert client.get("/api/firmware/build:xiaozhi.bin/download").status_code == 401


def test_upload_requires_the_operator_token(client):
    r = client.post("/api/firmware", files={"file": ("x.bin", HEADER)})
    assert r.status_code == 401


def test_endpoints_refuse_when_no_token_is_configured(client, monkeypatch):
    monkeypatch.delenv("OPERATOR_TOKEN", raising=False)
    monkeypatch.setattr(settings, "operator_token", "")
    assert client.get("/api/firmware").status_code == 503
    assert client.post("/api/firmware", files={"file": ("x.bin", HEADER)}).status_code == 503


def test_upload_then_list_then_download_round_trip(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("demo.bin", HEADER + b"payload")},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 200
    item = r.json()
    assert item["source"] == "upload"
    assert item["size"] == len(HEADER) + 7

    listing = client.get("/api/firmware", headers={"X-Operator-Token": TOKEN}).json()
    assert any(i["id"] == item["id"] for i in listing["items"])

    d = client.get(f"/api/firmware/{item['id']}/download", headers={"X-Operator-Token": TOKEN})
    assert d.status_code == 200
    assert d.content == HEADER + b"payload"
    assert d.headers["x-sha256"] == hashlib.sha256(HEADER + b"payload").hexdigest()


def test_upload_rejects_a_non_esp_image(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("bad.bin", b"nope")},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 400
    assert "0xE9" in r.json()["detail"]


def test_upload_rejects_an_oversized_image(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("big.bin", HEADER + b"\x00" * sf.MAX_IMAGE_BYTES)},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 400


def test_download_rejects_traversal_and_unknown_ids(client):
    h = {"X-Operator-Token": TOKEN}
    for bad in ("upload:..%2F..%2Fetc%2Fpasswd", "upload:nope.bin", "bogus:x"):
        assert client.get(f"/api/firmware/{bad}/download", headers=h).status_code == 404
