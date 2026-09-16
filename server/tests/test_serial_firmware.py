"""Serial firmware store unit tests.

Covers the untrusted-input gate (magic/size), the storage split from the OTA
channel, and the path-traversal hardening of item ids.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from youn_server import serial_firmware as sf
from youn_server.config import settings

HEADER = bytes([0xE9]) + b"\x00" * 31


@pytest.fixture(autouse=True)
def _tmp_store(tmp_path, monkeypatch):
    """Point the store at a throwaway dir; never touch real data/."""
    d = tmp_path / "serial-firmware"
    monkeypatch.setattr(settings, "serial_firmware_dir", d)
    yield d


def test_rejects_a_file_that_is_not_an_esp_image(_tmp_store):
    with pytest.raises(ValueError, match="0xE9"):
        sf.validate_image(b"not-an-image")


def test_rejects_an_image_larger_than_the_app_partition(_tmp_store):
    with pytest.raises(ValueError, match="过大"):
        sf.validate_image(HEADER + b"\x00" * sf.MAX_IMAGE_BYTES)


def test_accepts_a_minimal_valid_image(_tmp_store):
    sf.validate_image(HEADER)  # 不抛即通过


def test_upload_lands_in_the_serial_dir_and_never_in_the_ota_dir(_tmp_store, tmp_path, monkeypatch):
    ota_dir = tmp_path / "ota-firmware"
    monkeypatch.setattr(settings, "firmware_dir", ota_dir)

    item = sf.save_upload("my build.bin", HEADER + b"payload")

    assert item.source == "upload"
    assert (_tmp_store / item.name).is_file()
    assert not ota_dir.exists() or list(ota_dir.glob("*")) == []


def test_upload_shares_the_content_hash_it_reported(_tmp_store):
    import hashlib

    data = HEADER + b"payload"
    item = sf.save_upload("x.bin", data)
    assert item.sha256 == hashlib.sha256(data).hexdigest()
    assert (_tmp_store / f"{item.name}.sha256").read_text().strip() == item.sha256


def test_item_id_cannot_escape_the_store(_tmp_store):
    assert sf.resolve_item("upload:../../etc/passwd") is None
    assert sf.resolve_item("upload:nope.bin") is None


def test_resolving_a_valid_upload_returns_its_path(_tmp_store):
    item = sf.save_upload("ok.bin", HEADER)
    p = sf.resolve_item(item.id)
    assert p is not None and p.name == item.name


def test_listing_includes_uploads_and_reports_image_ok(_tmp_store):
    sf.save_upload("ok.bin", HEADER)
    names = {i.name for i in sf.list_items()}
    assert any(n.endswith("-ok.bin") for n in names)
    assert all(i.image_ok for i in sf.list_items())
