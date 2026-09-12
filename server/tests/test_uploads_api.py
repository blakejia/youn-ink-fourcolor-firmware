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
from youn_server.config import settings
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


def _canvas(children: list | None = None) -> dict:
    return {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": children or []}}]}


def pages_mod_render(canvas):
    from youn_server.canvas_render import render_canvas_to_bitmap
    return render_canvas_to_bitmap(canvas)


def _diff_from_blank(bitmap: bytes) -> int:
    """How many frame bytes differ from an empty white page.

    A page that drew the picture as a single pixel differs in one byte; a page
    that actually shows the picture differs in most of them.
    """
    return sum(a != b for a, b in zip(bitmap, pages_mod_render(_canvas())))


def _upload_dir_files() -> list[str]:
    if not settings.uploads_dir.exists():
        return []
    return sorted(p.name for p in settings.uploads_dir.iterdir())


def _make_page(name: str = "test-upload-page",
               duration_minutes: int = 10, order: int = 3) -> None:
    canvas = _canvas()
    bitmap = pages_mod_render(canvas)
    pages_mod.upsert_page(name, canvas, duration_minutes, order, bitmap)


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
    files_before = _upload_dir_files()
    r = client.post("/api/uploads", files={"image": ("a.png", _png(), "image/png")})
    assert r.status_code == 400
    assert {s.name for s in pages_mod.list_pages()} == before
    # Refused before anything was written to disk.
    assert _upload_dir_files() == files_before


def test_upload_to_an_unknown_page_lists_the_pages_you_could_have_meant(client):
    _make_page("test-upload-known")
    files_before = _upload_dir_files()
    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "no-such-page"})
    assert r.status_code == 400
    assert "test-upload-known" in r.json()["detail"]["pages"]
    # Refused before anything was written to disk.
    assert _upload_dir_files() == files_before


def test_upload_replaces_the_picture_but_not_the_page_identity(client):
    # Distinctive identity on the target, different values on a decoy page:
    # an endpoint that invents constants or grabs the wrong page fails here.
    _make_page("test-upload-target", duration_minutes=17, order=42)
    _make_page("test-upload-other", duration_minutes=5, order=1)
    target_before = _page_source("test-upload-target")
    other_before = _page_source("test-upload-other")
    md5_before = _schedule_md5_of(client, "test-upload-target")

    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "test-upload-target"})
    assert r.status_code == 200
    body = r.json()
    assert body["page"] == "test-upload-target"
    assert body["duration_minutes"] == 17
    assert body["order"] == 42

    after = _page_source("test-upload-target")
    # Identity untouched, content swapped.
    assert (after.duration_minutes, after.order, after.name) == (17, 42, "test-upload-target")
    assert after.canvas_json != target_before.canvas_json
    src = after.canvas_json["default"][0]["props"]["children"][0]["props"]["src"]
    assert src.startswith("uploads://")

    # Both promised files exist for the id the page references.
    upload_id = src.split("://", 1)[1]
    assert (settings.uploads_dir / f"{upload_id}.png").exists()
    assert (settings.uploads_dir / f"{upload_id}.src.png").exists()

    # The page's new bitmap is a real 30000-byte frame that actually shows the
    # picture, and the schedule now points the slot at it.
    bitmap = pages_mod.get_bitmap(body["md5"])
    assert len(bitmap) == 30000
    assert _diff_from_blank(bitmap) > 25000
    assert body["md5"] != md5_before
    assert _schedule_md5_of(client, "test-upload-target") == body["md5"]

    # The decoy page is exactly as it was.
    other_after = _page_source("test-upload-other")
    assert (other_after.duration_minutes, other_after.order,
            other_after.canvas_json) == (5, 1, other_before.canvas_json)


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


def test_a_transparent_upload_does_not_paint_black(client):
    _make_page("test-upload-alpha")
    buf = io.BytesIO()
    Image.new("RGBA", (800, 600), (0, 0, 0, 0)).save(buf, format="PNG")
    r = client.post("/api/uploads",
                    files={"image": ("a.png", buf.getvalue(), "image/png")},
                    data={"page": "test-upload-alpha"})
    assert r.status_code == 200
    # Alpha must be composited onto white, so a fully transparent picture leaves
    # the page blank. Dropping alpha instead makes it black: all 30000 differ.
    assert _diff_from_blank(pages_mod.get_bitmap(r.json()["md5"])) == 0
