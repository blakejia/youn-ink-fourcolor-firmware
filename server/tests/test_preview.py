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
