"""Schedule API unit tests.

Covers:
- schedule_md5 stability across page mutations
- schedule_md5 changes when page content / duration / order changes
- screen_active is a server-side boolean (no client timezone math)
- bitmap endpoint returns 30000 bytes for known md5, 404 for unknown
- cleanup on refcount zero
"""
from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server import pages as pages_mod


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_schedule_empty(client):
    r = client.get("/api/pages/schedule")
    assert r.status_code == 200
    body = r.json()
    assert body["pages"] == []
    assert "schedule_md5" in body
    assert "screen_active" in body
    assert body["policy"]["min_page_duration_minutes"] == 10


def test_schedule_md5_changes_with_content(client):
    body = {
        "name": "p1",
        "canvas_json": {"default": [{"type": "div", "props": {
            "tw": "bg-white",
            "style": {"color": "#000000"},
            "children": "A",
        }}]},
        "duration_minutes": 10,
        "order": 0,
    }
    r1 = client.post("/api/pages", json=body)
    assert r1.status_code == 200
    r2 = client.get("/api/pages/schedule")
    assert r2.status_code == 200
    md5_a = r2.json()["schedule_md5"]

    # Same page name, same content → same md5 (dedup works)
    r3 = client.post("/api/pages", json=body)
    assert r3.status_code == 200
    r4 = client.get("/api/pages/schedule")
    assert r4.json()["schedule_md5"] == md5_a

    # Different content → different schedule md5
    body["canvas_json"]["default"][0]["props"]["children"] = "B"
    r5 = client.post("/api/pages", json=body)
    assert r5.status_code == 200
    r6 = client.get("/api/pages/schedule")
    assert r6.json()["schedule_md5"] != md5_a

    # Different duration → different schedule md5
    body["duration_minutes"] = 20
    r7 = client.post("/api/pages", json=body)
    assert r7.status_code == 200
    r8 = client.get("/api/pages/schedule")
    assert r8.json()["schedule_md5"] != r6.json()["schedule_md5"]


def test_bitmap_endpoint(client):
    body = {
        "name": "p2",
        "canvas_json": {"default": [{"type": "div", "props": {"tw": "bg-white", "children": "x"}}]},
        "duration_minutes": 10,
        "order": 0,
    }
    r = client.post("/api/pages", json=body)
    assert r.status_code == 200
    entry = r.json()

    r2 = client.get(f"/api/pages/bitmap/{entry['md5']}.bin")
    assert r2.status_code == 200
    assert len(r2.content) == 30000
    assert r2.headers["Content-Type"] == "application/octet-stream"

    r3 = client.get("/api/pages/bitmap/" + "0" * 32 + ".bin")
    assert r3.status_code == 404


def test_delete_page_cleans_bitmap(client):
    body = {
        "name": "p3",
        "canvas_json": {"default": [{"type": "div", "props": {"tw": "bg-black", "children": "x"}}]},
        "duration_minutes": 10,
        "order": 0,
    }
    r1 = client.post("/api/pages", json=body)
    entry = r1.json()
    md5 = entry["md5"]

    r2 = client.delete("/api/pages/p3")
    assert r2.status_code == 200

    r3 = client.get(f"/api/pages/bitmap/{md5}.bin")
    assert r3.status_code == 404


def test_min_duration_validation(client):
    body = {
        "name": "p4",
        "canvas_json": {"default": []},
        "duration_minutes": 1,
        "order": 0,
    }
    r = client.post("/api/pages", json=body)
    assert r.status_code == 400
    assert "min_page_duration_minutes" in r.json()["detail"]


def test_render_error_returns_400_with_path(client):
    body = {
        "name": "bad",
        "canvas_json": {"default": [{"type": "button", "props": {"children": "x"}}]},
        "duration_minutes": 10,
        "order": 0,
    }
    r = client.post("/api/pages", json=body)
    assert r.status_code == 400
    detail = r.json()["detail"]
    assert "windowData.default[0]" in detail
    assert "unsupported element type" in detail


def test_screen_active_is_bool(client):
    r = client.get("/api/pages/schedule")
    assert r.status_code == 200
    body = r.json()
    assert isinstance(body["screen_active"], bool)


def test_policy_matches_config(client):
    r = client.get("/api/pages/schedule")
    assert r.status_code == 200
    body = r.json()
    policy = body["policy"]
    assert policy["sleep_window"]["start"] == "00:00"
    assert policy["sleep_window"]["end"] == "06:00"
    assert policy["sleep_window"]["tz"] == "Asia/Shanghai"
    assert policy["poll_interval_minutes"] == 10
    assert policy["sleep_poll_interval_minutes"] == 60