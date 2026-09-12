"""Pages belong to a device: storage, schedule and the admin API.

Ownership is the directory (data/pages/<device_id>/<name>.json); bitmaps stay
global and content-addressed, keyed in their meta by "<device>/<name>".
"""
from __future__ import annotations

import io
import json

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from youn_server import pages as pages_mod
from youn_server.app import create_app, registry
from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.config import settings

DEV_A = "NOTE4C-AAAAAA"
DEV_B = "NOTE4C-BBBBBB"


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _canvas(label: str) -> dict:
    return {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": label}}]}


def _make(device: str, name: str, order: int = 0) -> None:
    canvas = _canvas(name)
    pages_mod.upsert_page(device, name, canvas, 10, order, render_canvas_to_bitmap(canvas))


def _png() -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", (800, 600), (10, 120, 200)).save(buf, format="PNG")
    return buf.getvalue()


def _register(device_id: str, token: str, trusted: bool = True) -> None:
    registry.upsert(device_id, "NOTE4C", ip_address="127.0.0.1")
    if trusted:
        registry.approve(device_id)
    registry.set_token(device_id, token)


# ── storage ────────────────────────────────────────────────────────────

def test_pages_belong_to_the_device_that_created_them():
    _make(DEV_A, "a-page")
    _make(DEV_B, "b-page")
    assert [p.name for p in pages_mod.list_pages(DEV_A)] == ["a-page"]
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["b-page"]
    assert pages_mod._page_path(DEV_A, "a-page").parent.name == DEV_A


def test_two_devices_can_use_the_same_page_name():
    _make(DEV_A, "home")
    _make(DEV_B, "home")
    assert [p.name for p in pages_mod.list_pages(DEV_A)] == ["home"]
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["home"]


def test_a_schedule_never_contains_another_devices_page():
    _make(DEV_A, "only-a", order=0)
    _make(DEV_B, "only-b", order=0)
    assert [e.name for e in pages_mod.build_schedule_from_disk(DEV_A)] == ["only-a"]
    assert [e.name for e in pages_mod.build_schedule_from_disk(DEV_B)] == ["only-b"]


def test_a_page_on_the_flat_old_layout_is_ignored():
    """No compatibility read: two layouts must never both be live."""
    pages_dir = settings.data_dir / "pages"
    pages_dir.mkdir(parents=True, exist_ok=True)
    (pages_dir / "legacy.json").write_text(json.dumps({
        "name": "legacy", "canvas_json": _canvas("x"),
        "duration_minutes": 10, "order": 0}))
    assert pages_mod.list_pages(DEV_A) == []


def test_the_bitmap_meta_keys_the_source_by_device_slash_name():
    _make(DEV_A, "same")
    metas = list((settings.data_dir / "pages").glob("*.bmp.json"))
    assert metas, "expected a bitmap meta"
    sources = [s for f in metas for s in json.loads(f.read_text())["sources"]]
    assert sources == [f"{DEV_A}/same"]


def test_replacing_a_page_leaves_it_claimed_by_one_bitmap_only():
    """The invariant from the earlier fix, now expressed per device."""
    _make(DEV_A, "twice")
    _make(DEV_A, "twice")
    metas = list((settings.data_dir / "pages").glob("*.bmp.json"))
    claimers = [f.name for f in metas if f"{DEV_A}/twice" in json.loads(f.read_text())["sources"]]
    assert len(claimers) == 1


def test_deleting_one_devices_page_leaves_the_other_alone():
    _make(DEV_A, "shared-name")
    _make(DEV_B, "shared-name")
    assert pages_mod.delete_page(DEV_A, "shared-name") is True
    assert pages_mod.list_pages(DEV_A) == []
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["shared-name"]


def test_a_schedule_entry_carries_the_pages_own_duration_and_order():
    _make(DEV_A, "dur", order=7)
    entry = pages_mod.build_schedule_from_disk(DEV_A)[0]
    assert (entry.name, entry.order, entry.duration_minutes) == ("dur", 7, 10)
def test_a_page_whose_bitmap_is_gone_is_not_advertised():
    """A meta that claims a page whose .bin is absent (refcount GC dropped the
    bytes while the meta survived) must not put that md5 on the schedule."""
    _make(DEV_A, "ghost")
    md5 = pages_mod.build_schedule_from_disk(DEV_A)[0].md5
    assert pages_mod.get_bitmap(md5) is not None
    pages_mod._bitmap_path(md5).unlink()
    assert pages_mod.get_bitmap(md5) is None
    assert pages_mod.build_schedule_from_disk(DEV_A) == []



# ── the admin API ──────────────────────────────────────────────────────

def test_pages_require_a_device(client):
    assert client.get("/api/pages").status_code == 400
    assert client.post("/api/pages", json={"name": "x", "canvas_json": {}}).status_code == 400


def test_a_device_that_is_unknown_or_untrusted_is_refused(client):
    assert client.get("/api/pages", params={"device": "NOTE4C-NOSUCH"}).status_code == 400
    _register("NOTE4C-UNTRUSTED", "e" * 64, trusted=False)
    assert client.get("/api/pages", params={"device": "NOTE4C-UNTRUSTED"}).status_code == 400


def test_pages_are_listed_per_device(client):
    _register(DEV_A, "a" * 64)
    _register(DEV_B, "b" * 64)
    r = client.post("/api/pages", json={
        "device": DEV_A, "name": "a-only", "canvas_json": _canvas("a"),
        "duration_minutes": 10, "order": 0})
    assert r.status_code == 200
    assert [p["name"] for p in
            client.get("/api/pages", params={"device": DEV_A}).json()["pages"]] == ["a-only"]
    assert client.get("/api/pages", params={"device": DEV_B}).json()["pages"] == []


def test_an_upload_must_name_a_device_and_a_page_of_that_device(client):
    _register(DEV_A, "a" * 64)
    _register(DEV_B, "b" * 64)
    assert client.post("/api/uploads", files={"image": ("a.png", b"x", "image/png")}).status_code == 400
    r = client.post("/api/uploads", files={"image": ("a.png", b"x", "image/png")},
                    data={"device": DEV_A, "page": "not-here"})
    assert r.status_code == 400
    assert not any(settings.uploads_dir.iterdir())
    # A page of the OTHER device is not good enough either. b"x" would also
    # 400 on image parsing, so use a real picture: with a global page list
    # the endpoint would accept the name and upsert into DEV_A's directory.
    _make(DEV_B, "b-only")
    uploads_before = sorted(p.name for p in settings.uploads_dir.iterdir())
    r = client.post("/api/uploads", files={"image": ("a.png", _png(), "image/png")},
                    data={"device": DEV_A, "page": "b-only"})
    assert r.status_code == 400
    assert sorted(p.name for p in settings.uploads_dir.iterdir()) == uploads_before
    assert [p.name for p in pages_mod.list_pages(DEV_A)] == []


def test_the_schedule_requires_the_device_token(client):
    _register("NOTE4C-TESTC", "c" * 64)
    assert client.get("/api/pages/schedule").status_code == 401
    r = client.get("/api/pages/schedule", headers={"Authorization": "Bearer " + "c" * 64})
    assert r.status_code == 200
    assert r.json()["pages"] == []


def test_an_untrusted_device_token_cannot_read_a_schedule(client):
    _register("NOTE4C-TESTD", "d" * 64, trusted=False)
    r = client.get("/api/pages/schedule", headers={"Authorization": "Bearer " + "d" * 64})
    assert r.status_code == 401


def test_a_page_name_with_a_space_is_a_400_not_a_500(client):
    """Names that mix valid and invalid characters must be refused as 400
    before anything touches disk — never escape as a 500."""
    _register(DEV_A, "a" * 64)
    files_before = sorted(p.relative_to(settings.data_dir)
                          for p in settings.data_dir.rglob("*") if p.is_file())
    r = client.post("/api/pages", json={
        "device": DEV_A, "name": "home page", "canvas_json": _canvas("x"),
        "duration_minutes": 10, "order": 0})
    assert r.status_code == 400
    files_after = sorted(p.relative_to(settings.data_dir)
                         for p in settings.data_dir.rglob("*") if p.is_file())
    assert files_after == files_before


def test_a_dotdot_device_is_a_400_not_an_escape(client):
    """"." and ".." pass a naive alnum/._- whitelist; _page_dir would then
    resolve outside pages/. Even a registered ".." device must be refused
    before anything touches disk."""
    assert not pages_mod.is_safe_component(".")
    assert not pages_mod.is_safe_component("..")
    _register("..", "dd0t" * 16)
    files_before = sorted(p.relative_to(settings.data_dir)
                          for p in settings.data_dir.rglob("*") if p.is_file())
    r = client.post("/api/pages", json={
        "device": "..", "name": "x", "canvas_json": _canvas("x"),
        "duration_minutes": 10, "order": 0})
    assert r.status_code == 400
    files_after = sorted(p.relative_to(settings.data_dir)
                         for p in settings.data_dir.rglob("*") if p.is_file())
    assert files_after == files_before
    assert not (settings.data_dir / "x.json").exists()
