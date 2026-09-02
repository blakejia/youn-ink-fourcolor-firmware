"""Preview endpoint tests (web admin UI Canvas preview)."""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_preview_valid(client):
    body = {
        "canvas_json": {"default": [{"type": "div", "props": {
            "tw": "bg-black", "style": {"color": "#FFFFFF"}, "children": "preview"}}]},
    }
    r = client.post("/api/pages/preview", json=body)
    assert r.status_code == 200
    assert r.headers["Content-Type"] == "image/png"
    assert r.content[:8] == b"\x89PNG\r\n\x1a\n"  # PNG magic


def test_preview_invalid_json(client):
    r = client.post("/api/pages/preview", json={"canvas_json": {"default": [{"type": "button", "props": {}}]}})
    assert r.status_code == 400
    assert "render failed" in r.json()["detail"]


def test_preview_missing_canvas(client):
    r = client.post("/api/pages/preview", json={})
    assert r.status_code == 400


def test_preview_no_operator_token(client):
    # When OPERATOR_TOKEN is set, this must 401. In test env it's unset → allowed.
    r = client.post("/api/pages/preview", json={"canvas_json": {"default": []}})
    assert r.status_code == 200


def test_preview_debug_returns_bounds(client):
    # 8x8 red PNG — valid so the img branch actually renders.
    pixel = ("data:image/png;base64,"
             "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAFElEQVR4nGO8IyfHgA0wYRUdtBIA4FYBKCgCg6AAAAAASUVORK5CYII=")
    body = {
        "canvas_json": {"default": [
            {"type": "div", "props": {"tw": "flex flex-col bg-white", "children": [
                {"type": "div", "props": {"tw": "w-[40px] h-[40px] bg-red"}},
                {"type": "img", "props": {"src": pixel, "style": {"width": "40px", "height": "40px"}}},
            ]}}
        ]},
    }
    r = client.post("/api/pages/preview?debug=1", json=body)
    assert r.status_code == 200
    data = r.json()
    assert data["png_b64"][:4] == "iVBO"  # base64 PNG magic
    paths = [b["path"] for b in data["bounds"]]
    # Root, then indexed children — paths must be distinct and addressable.
    assert "windowData.default[0]" in paths
    assert "windowData.default[0].props.children[0]" in paths
    assert "windowData.default[0].props.children[1]" in paths
    types = {b["path"]: b["type"] for b in data["bounds"]}
    assert types["windowData.default[0].props.children[0]"] == "div"
    assert types["windowData.default[0].props.children[1]"] == "img"


def test_preview_debug_render_error(client):
    r = client.post("/api/pages/preview?debug=1", json={"canvas_json": {"default": [{"type": "video", "props": {}}]}})
    assert r.status_code == 400
    assert "render failed" in r.json()["detail"]
