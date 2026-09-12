"""Upload must bind to an existing page.

Covers: the refusal when no page is named, the refusal when the named page does
not exist, the happy path replacing a page's picture while its identity
(duration, order, name) is untouched, and the fact that a bad image leaves the
page alone.
"""
from __future__ import annotations

import io

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from youn_server.app import create_app
from youn_server import pages as pages_mod


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _png(size=(800, 600), color=(10, 120, 200)) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", size, color).save(buf, format="PNG")
    return buf.getvalue()


def _make_page(name: str = "test-upload-page") -> None:
    canvas = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": []}}]}
    bitmap = pages_mod_render(canvas)
    pages_mod.upsert_page(name, canvas, 10, 3, bitmap)


def pages_mod_render(canvas):
    from youn_server.canvas_render import render_canvas_to_bitmap
    return render_canvas_to_bitmap(canvas)


def _page_source(name: str):
    for s in pages_mod.list_pages():
        if s.name == name:
            return s
    return None


def _schedule_md5_of(client, name: str) -> str:
    """Which bitmap the device is currently pointed at for this page."""
    for p in client.get("/api/pages/schedule").json()["pages"]:
        if p["name"] == name:
            return p["md5"]
    raise AssertionError(f"{name} is not in the schedule")


def test_upload_without_a_page_is_refused(client):
    before = {s.name for s in pages_mod.list_pages()}
    r = client.post("/api/uploads", files={"image": ("a.png", _png(), "image/png")})
    assert r.status_code == 400
    assert {s.name for s in pages_mod.list_pages()} == before


def test_upload_to_an_unknown_page_lists_the_pages_you_could_have_meant(client):
    _make_page("test-upload-known")
    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "no-such-page"})
    assert r.status_code == 400
    assert "test-upload-known" in r.json()["detail"]["pages"]


def test_upload_replaces_the_picture_but_not_the_page_identity(client):
    _make_page("test-upload-target")
    before = _page_source("test-upload-target")
    md5_before = _schedule_md5_of(client, "test-upload-target")

    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "test-upload-target"})
    assert r.status_code == 200
    body = r.json()
    assert body["page"] == "test-upload-target"
    assert body["duration_minutes"] == before.duration_minutes
    assert body["order"] == before.order

    after = _page_source("test-upload-target")
    # Identity untouched, content swapped.
    assert (after.duration_minutes, after.order, after.name) == \
           (before.duration_minutes, before.order, before.name)
    assert after.canvas_json != before.canvas_json
    src = after.canvas_json["default"][0]["props"]["children"][0]["props"]["src"]
    assert src.startswith("uploads://")

    # The page's new bitmap is a real 30000-byte frame, and the schedule now
    # points the slot at it.
    assert len(pages_mod.get_bitmap(body["md5"])) == 30000
    assert body["md5"] != md5_before
    assert _schedule_md5_of(client, "test-upload-target") == body["md5"]


def test_two_uploads_to_one_page_do_not_create_a_second_page(client):
    _make_page("test-upload-twice")
    n0 = len(pages_mod.list_pages())
    for _ in range(2):
        r = client.post("/api/uploads",
                        files={"image": ("a.png", _png(), "image/png")},
                        data={"page": "test-upload-twice"})
        assert r.status_code == 200
    assert len(pages_mod.list_pages()) == n0


def test_bytes_that_are_not_an_image_leave_the_page_alone(client):
    _make_page("test-upload-bad")
    before = _page_source("test-upload-bad")
    r = client.post("/api/uploads",
                    files={"image": ("a.png", b"not an image", "image/png")},
                    data={"page": "test-upload-bad"})
    assert r.status_code == 400
    after = _page_source("test-upload-bad")
    assert after.canvas_json == before.canvas_json
