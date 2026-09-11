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


def _entries(durations):
    return [
        pages_mod.PageEntry(md5=f"{i:032x}", duration_minutes=d, order=i, name=f"p{i}")
        for i, d in enumerate(durations)
    ]


def test_schedule_position_walks_the_cycle():
    entries = _entries([10, 5])          # cycle = 15 min
    cycle_start = 15 * 60               # 任意 15 分钟整数倍
    assert pages_mod.schedule_position(entries, cycle_start + 0) == (0, 600)
    assert pages_mod.schedule_position(entries, cycle_start + 599) == (0, 1)
    assert pages_mod.schedule_position(entries, cycle_start + 600) == (1, 300)
    assert pages_mod.schedule_position(entries, cycle_start + 899) == (1, 1)
    assert pages_mod.schedule_position(entries, cycle_start + 900) == (0, 600)  # 绕回


def test_schedule_position_single_page_and_empty():
    assert pages_mod.schedule_position(_entries([10]), 600) == (0, 600)
    assert pages_mod.schedule_position([], 12345) == (0, None)


def test_schedule_empty_reports_position_for_no_pages(client):
    """空排期的端点契约：第 0 页、无下次翻页（None，不是 0）。"""
    body = client.get("/api/pages/schedule").json()
    assert body["current_index"] == 0
    assert body["seconds_until_next_page"] is None


def test_schedule_position_clamps_zero_duration():
    # 0 分钟页会让 cycle 为 0；必须夹到 1 分钟，否则除零
    entries = _entries([0, 5])
    assert pages_mod.schedule_position(entries, 0) == (0, 60)
    assert pages_mod.schedule_position(entries, 60) == (1, 300)


def test_schedule_md5_does_not_move_with_the_clock(client, monkeypatch):
    """设备缓存靠 md5 相等短路；位置字段一旦混进摘要，缓存会被时间推进不停击穿。"""
    # 时长必须 >= policy 下限 10，否则保存被 400 拒绝（§3.2 的契约）。
    for i, mins in enumerate((20, 10)):          # cycle = 1800 s，翻页点在 1200 s
        r = client.post("/api/pages", json={
            "name": f"clock{i}",
            "canvas_json": {"default": [{"type": "div", "props": {
                "tw": "bg-white", "style": {"color": "#000000"}, "children": "x",
            }}]},
            "duration_minutes": mins, "order": i,
        })
        assert r.status_code == 200

    # 1_000_000 % 1800 == 1000 -> 第 0 页；1_000_600 % 1800 == 1600 -> 已跨过 1200 翻页点
    monkeypatch.setattr("youn_server.app.time.time", lambda: 1_000_000.0)
    a = client.get("/api/pages/schedule").json()
    monkeypatch.setattr("youn_server.app.time.time", lambda: 1_000_600.0)  # 10 分钟后
    b = client.get("/api/pages/schedule").json()

    assert a["schedule_md5"] == b["schedule_md5"], "摘要不随时间推进而变"
    assert a["current_index"] == 0 and b["current_index"] == 1, "位置必须跟着走"
    assert a["seconds_until_next_page"] == 200 and b["seconds_until_next_page"] == 200