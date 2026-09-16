"""MCP 画板工具的端到端测试。

走**真实** streamable-http 协议（initialize → notifications/initialized →
tools/call），与 tests/test_mcp.py 同一套路：不抄私有近路，这样挂载/lifespan
出问题能被抓到。响应是 SSE，从 `data:` 行里取 JSON。
"""
from __future__ import annotations

import base64
import io
import json

import pytest
from fastapi.testclient import TestClient
from PIL import Image as PILImage

from youn_server import pages as pages_mod
from youn_server.app import create_app
from youn_server.devices import registry

DEV = "DEV-MCP-CANVAS"

CANVAS = {"default": [{"type": "div", "props": {
    "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
    "children": [{"type": "span", "props": {
        "tw": "text-[28px]", "style": {"color": "#000000"},
        "children": "MCP 画板"}}]}}]}


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


@pytest.fixture(autouse=True)
def _trusted_device():
    registry.upsert(DEV, "NOTE4C")
    registry.approve(DEV)
    yield


def _headers(sid=None):
    h = {"Accept": "application/json, text/event-stream",
         "Content-Type": "application/json"}
    if sid:
        h["mcp-session-id"] = sid
    return h


def _sse_json(text: str) -> dict:
    """从 SSE 里取出最后一个 data: 行的 JSON（没有则当纯 JSON）。"""
    for line in reversed(text.splitlines()):
        if line.startswith("data:"):
            return json.loads(line[len("data:"):].strip())
    return json.loads(text)


def _session(client) -> str:
    r = client.post("/mcp", headers=_headers(), json={
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "canvas-test", "version": "1.0"}},
    })
    assert r.status_code == 200, f"initialize failed: {r.status_code} {r.text}"
    sid = r.headers.get("mcp-session-id")
    assert sid, "initialize did not return a session id"
    r = client.post("/mcp", headers=_headers(sid),
                    json={"jsonrpc": "2.0", "method": "notifications/initialized"})
    assert r.status_code in (200, 202), f"initialized got {r.status_code}"
    return sid


def _call_raw(client, sid, name, args=None) -> dict:
    r = client.post("/mcp", headers=_headers(sid), json={
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": name, "arguments": args or {}},
    })
    assert r.status_code == 200, f"tools/call {name} -> {r.status_code}: {r.text}"
    return _sse_json(r.text)


def _call(client, sid, name, args=None):
    """调用工具并返回其 JSON 载荷；工具报错时断言失败并带上原文。"""
    payload = _call_raw(client, sid, name, args)
    result = payload.get("result", {})
    assert not result.get("isError"), f"{name} errored: {result}"
    content = result.get("content") or []
    assert content, f"{name} returned no content: {payload}"
    assert content[0].get("type") == "text", f"{name} content type: {content[0].get('type')}"
    return json.loads(content[0]["text"])


def _call_error(client, sid, name, args=None) -> str:
    payload = _call_raw(client, sid, name, args)
    result = payload.get("result", {})
    assert result.get("isError"), f"{name} was expected to fail: {payload}"
    return json.dumps(result)


# ── 工具面 ────────────────────────────────────────────────────────────
def test_tools_list_exposes_the_canvas_surface(client):
    sid = _session(client)
    r = client.post("/mcp", headers=_headers(sid),
                    json={"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
    names = {t["name"] for t in _sse_json(r.text)["result"]["tools"]}
    assert {"list_pages", "get_page", "upsert_page", "delete_page", "get_schedule",
            "preview_canvas", "upload_image_as_page", "reorder_pages",
            "describe_canvas_schema"} <= names


def test_a_write_tool_requires_a_trusted_device(client):
    sid = _session(client)
    err = _call_error(client, sid, "list_pages", {"device_id": "NOT-A-DEVICE"})
    assert "untrusted" in err or "unknown" in err


# ── 读写往返 ──────────────────────────────────────────────────────────
def test_upsert_then_list_then_get_round_trip(client):
    sid = _session(client)
    created = _call(client, sid, "upsert_page", {
        "device_id": DEV, "name": "mcp-page", "canvas_json": CANVAS,
        "duration_minutes": 10, "order": 0})
    assert created["name"] == "mcp-page"
    assert len(created["md5"]) == 32

    listed = _call(client, sid, "list_pages", {"device_id": DEV})
    assert [p["name"] for p in listed["pages"]] == ["mcp-page"]
    assert listed["pages"][0]["md5"] == created["md5"]

    got = _call(client, sid, "get_page", {"device_id": DEV, "name": "mcp-page"})
    assert got["canvas_json"] == CANVAS
    assert got["duration_minutes"] == 10


def test_upsert_rejects_a_render_error_with_its_path(client):
    sid = _session(client)
    bad = {"default": [{"type": "video", "props": {}}]}
    err = _call_error(client, sid, "upsert_page",
                      {"device_id": DEV, "name": "bad", "canvas_json": bad})
    assert "render failed" in err


def test_upsert_rejects_a_duration_below_the_minimum(client):
    sid = _session(client)
    err = _call_error(client, sid, "upsert_page", {
        "device_id": DEV, "name": "short", "canvas_json": CANVAS,
        "duration_minutes": 1})
    assert "duration_minutes" in err


def test_schedule_reports_the_current_page(client):
    sid = _session(client)
    for i, n in enumerate(("p1", "p2")):
        _call(client, sid, "upsert_page", {"device_id": DEV, "name": n,
                                           "canvas_json": CANVAS,
                                           "duration_minutes": 10, "order": i})
    sched = _call(client, sid, "get_schedule", {"device_id": DEV})
    assert [p["name"] for p in sched["pages"]] == ["p1", "p2"]
    assert 0 <= sched["current_index"] < 2
    assert "sleep_window" in sched["policy"]
    assert isinstance(sched["screen_active"], bool)


def test_delete_unknown_page_errors_and_known_page_disappears(client):
    sid = _session(client)
    _call(client, sid, "upsert_page", {"device_id": DEV, "name": "gone",
                                       "canvas_json": CANVAS})
    err = _call_error(client, sid, "delete_page",
                      {"device_id": DEV, "name": "never-existed"})
    assert "unknown page" in err
    assert _call(client, sid, "delete_page",
                 {"device_id": DEV, "name": "gone"})["deleted"] == "gone"
    assert _call(client, sid, "list_pages", {"device_id": DEV})["count"] == 0


# ── 重排 ──────────────────────────────────────────────────────────────
def test_reorder_moves_the_listed_pages_and_keeps_the_rest(client):
    sid = _session(client)
    for i, n in enumerate(("a", "b", "c")):
        _call(client, sid, "upsert_page", {"device_id": DEV, "name": n,
                                           "canvas_json": CANVAS, "order": i})
    out = _call(client, sid, "reorder_pages", {"device_id": DEV,
                                               "ordered_names": ["c", "a"]})
    assert out["order"] == ["c", "a", "b"]
    listed = _call(client, sid, "list_pages", {"device_id": DEV})
    assert [p["name"] for p in listed["pages"]] == ["c", "a", "b"]
    # 时长与画布不该被重排改动
    assert all(p["duration_minutes"] == 10 for p in listed["pages"])


def test_reorder_rejects_unknown_and_duplicate_names(client):
    sid = _session(client)
    _call(client, sid, "upsert_page", {"device_id": DEV, "name": "a",
                                       "canvas_json": CANVAS})
    assert "unknown pages" in _call_error(client, sid, "reorder_pages",
                                          {"device_id": DEV, "ordered_names": ["nope"]})
    assert "duplicates" in _call_error(client, sid, "reorder_pages",
                                       {"device_id": DEV, "ordered_names": ["a", "a"]})


# ── 预览 / 图片成页 ───────────────────────────────────────────────────
def test_preview_returns_a_png_image_block(client):
    sid = _session(client)
    payload = _call_raw(client, sid, "preview_canvas", {"canvas_json": CANVAS})
    block = payload["result"]["content"][0]
    assert block["type"] == "image", block
    assert block.get("mimeType") in ("image/png", None) or block.get("mime_type") == "image/png"
    raw = base64.b64decode(block["data"])
    assert raw[:8] == b"\x89PNG\r\n\x1a\n"
    # 预览不得落盘
    assert _call(client, sid, "list_pages", {"device_id": DEV})["count"] == 0


def test_upload_image_creates_a_page_and_letterboxes_it(client):
    sid = _session(client)
    img = PILImage.new("RGB", (120, 60), (10, 200, 30))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    out = _call(client, sid, "upload_image_as_page", {
        "device_id": DEV, "name": "from-image",
        "image_base64": base64.b64encode(buf.getvalue()).decode()})
    assert out["page"] == "from-image"
    # 归一化后应是一张面板尺寸的图，并被画进 canvas_json
    src = _call(client, sid, "get_page", {"device_id": DEV, "name": "from-image"})
    node = src["canvas_json"]["default"][0]["props"]["children"][0]
    assert node["props"]["src"].startswith("uploads://")


def test_upload_rejects_bad_base64(client):
    sid = _session(client)
    err = _call_error(client, sid, "upload_image_as_page", {
        "device_id": DEV, "name": "x", "image_base64": "!!!not base64!!!"})
    assert "base64" in err or "usable image" in err


# ── schema 工具不得撒谎 ───────────────────────────────────────────────
def test_describe_schema_example_actually_renders(client):
    sid = _session(client)
    schema = _call(client, sid, "describe_canvas_schema")
    assert schema["panel"]["bitmap_bytes"] == 30000
    # 它给的 example 必须真的能过渲染器 —— 否则工具在撒谎
    _call(client, sid, "upsert_page", {"device_id": DEV, "name": "from-schema",
                                       "canvas_json": schema["example"]})
    assert pages_mod.page_bitmap_md5(DEV, "from-schema") is not None
