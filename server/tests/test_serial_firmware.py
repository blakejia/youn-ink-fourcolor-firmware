"""Serial firmware store unit tests.

Covers the untrusted-input gate (magic/size), the storage split from the OTA
channel, and the path-traversal hardening of item ids.
"""
from __future__ import annotations

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


def test_item_id_round_trips_for_names_that_need_sanitising(_tmp_store):
    """list_items 用磁盘原始名拼 id，resolve_item 必须原样接受它。

    仓库目录里的文件不都来自 save_upload（手工拷贝/改名/旧数据都可能），
    名单里的 id 必须能原样解析回**同一个**文件。
    反向 bug（resolve_item 二次净化）会把 `a b.bin` 解析成 `ab.bin` 或 None。
    """
    _tmp_store.mkdir(parents=True, exist_ok=True)
    spaced = _tmp_store / "a b.bin"
    plain = _tmp_store / "ab.bin"
    spaced.write_bytes(HEADER + b"SPACED")
    plain.write_bytes(HEADER + b"PLAIN")

    items = {i.name: i for i in sf.list_items()}
    assert {"a b.bin", "ab.bin"} <= set(items)

    p_spaced = sf.resolve_item(items["a b.bin"].id)
    p_plain = sf.resolve_item(items["ab.bin"].id)
    assert p_spaced == spaced and p_plain == plain
    assert p_spaced.read_bytes() != p_plain.read_bytes()
    # 列表里的 size/sha256 必须与解析到的文件一致
    assert items["a b.bin"].sha256 == sf._sha256_file(spaced)


def test_a_symlink_inside_the_store_still_resolves_outside_nothing(_tmp_store, tmp_path):
    """父目录白名单（resolve_item 的第二重校验）必须真的挡住符号链接逃逸。

    少了这行校验的实现会 follow 符号链接并返回 store 之外的路径。
    """
    _tmp_store.mkdir(parents=True, exist_ok=True)
    secret = tmp_path / "secret.bin"
    secret.write_bytes(HEADER)
    (_tmp_store / "evil.bin").symlink_to(secret)

    assert (_tmp_store / "evil.bin").is_file()  # 前提：不跟随校验时它确实“存在”
    assert sf.resolve_item("upload:evil.bin") is None


def test_listing_reports_the_build_artifact_with_its_real_size_and_hash():
    """构建产物来源（source == "build"）与磁盘上的一致；无产物时跳过。"""
    import hashlib

    if not sf.BUILD_ARTIFACT.is_file():
        pytest.skip("firmware/build/xiaozhi.bin 不存在")

    builds = [i for i in sf.list_items() if i.source == "build"]
    assert len(builds) == 1
    b = builds[0]
    assert b.id == sf.BUILD_ID
    assert b.size == sf.BUILD_ARTIFACT.stat().st_size
    assert b.sha256 == hashlib.sha256(sf.BUILD_ARTIFACT.read_bytes()).hexdigest()
    assert sf.resolve_item(sf.BUILD_ID) == sf.BUILD_ARTIFACT
