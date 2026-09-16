"""FastMCP server: notification + canvas (画板) tools on /mcp (streamable-http).

画板这部分**不重写任何逻辑** —— 每个工具都是 HTTP 端点背后那层模块的薄包装
（``pages`` / ``canvas_render`` / ``page_upload``），取值检查也与 HTTP 侧同款。
两条路径共用一份实现，语义就不会漂移。

挂载与鉴权在 ``app.py``：sub-app 挂在 ``/``（路由实际在 ``/mcp/``），
由一层 HTTP 中间件要求 ``X-Operator-Token``。
"""
from __future__ import annotations

import base64
import time
from dataclasses import asdict
from datetime import datetime
from zoneinfo import ZoneInfo

from fastmcp import FastMCP
# 顶层 fastmcp.Image 已弃用（会告警），媒体类型走 utilities.types
from fastmcp.utilities.types import Image

from .config import settings
from . import notify_store as ns
from . import pages as pages_mod
from . import page_upload
from .canvas_render import (CAPABILITIES, RenderError, render_canvas_to_bitmap,
                            render_canvas_to_png)
from .devices import registry

# fastmcp 4.x: no `transport=` kwarg on FastMCP(). Streamable HTTP
# transport is selected when mounting via http_app(transport=...).
mcp = FastMCP("youn-notify")


# ─── shared checks (mirror the HTTP endpoints) ────────────────────────
def _trusted_device(device_id: str) -> None:
    """Same取值检查 as app._require_known_device: 名字安全 + 存在 + 已受信。"""
    if not pages_mod.is_safe_component(device_id):
        raise ValueError(f"invalid device_id: {device_id!r}")
    dev = registry.get(device_id)
    if dev is None or not dev.trusted:
        raise ValueError(f"unknown or untrusted device: {device_id!r}")


def _checked_page_name(name: str) -> str:
    if not pages_mod.is_safe_component(name):
        raise ValueError(f"invalid page name: {name!r}")
    return name


def _render_error(e: RenderError) -> ValueError:
    return ValueError(f"render failed: {e.path}: {e.message}")


# ─── notifications (existing surface, unchanged) ──────────────────────
@mcp.tool
def push_notification(device_id: str, title: str, body: str,
                      ttl_sec: int = 300) -> dict:
    """Create a pending notification for a device."""
    if not any(d.device_id == device_id
               for d in registry.list_all(only_trusted=True)):
        raise ValueError("device not trusted")
    n = ns.get_store().enqueue(device_id, title, body, ttl_sec)
    return {"ok": True, "notification": asdict(n)}


@mcp.tool
def list_notifications(device_id: str = "", limit: int = 20) -> dict:
    """List recent notifications (all devices or filtered by device_id)."""
    items = ns.get_store().recent(device_id=device_id, limit=limit)
    return {"notifications": [asdict(n) for n in items]}


@mcp.tool
def ack_notification(notification_id: str, decision: str) -> dict:
    """Mark a notification as agreed or rejected."""
    if decision not in ("agree", "reject"):
        raise ValueError("decision must be agree|reject")
    n = ns.get_store().ack(notification_id, decision)
    if n is None:
        raise ValueError("notification not found")
    return {"ok": True, "status": n.status, "decision": n.decision}


# ─── canvas: read ─────────────────────────────────────────────────────
@mcp.tool
def list_pages(device_id: str) -> dict:
    """List a device's canvas pages: name, duration_minutes, order, bitmap md5.

    The md5 is the key the device caches the page under — the same value the
    schedule hands out — so it is what "which picture is this page" means.
    """
    _trusted_device(device_id)
    out = [{**s.to_dict(), "md5": pages_mod.page_bitmap_md5(device_id, s.name)}
           for s in pages_mod.list_pages(device_id)]
    return {"pages": out, "count": len(out)}


@mcp.tool
def get_page(device_id: str, name: str) -> dict:
    """Read one page's canvas_json (原文) plus its duration_minutes/order.

    Useful for read-modify-write: pull the JSON, edit it, hand it back to
    upsert_page.
    """
    _trusted_device(device_id)
    _checked_page_name(name)
    src = next((s for s in pages_mod.list_pages(device_id) if s.name == name), None)
    if src is None:
        raise ValueError(f"unknown page: {name!r}")
    return {**src.to_dict(), "md5": pages_mod.page_bitmap_md5(device_id, name)}


@mcp.tool
def get_schedule(device_id: str) -> dict:
    """What the device is showing now: ordered pages, current index, policy.

    Mirrors the payload the device itself polls (GET /api/pages/schedule),
    including the sleep window and whether the screen is active right now.
    """
    _trusted_device(device_id)
    entries = pages_mod.build_schedule_from_disk(device_id)
    index, seconds_left = pages_mod.schedule_position(entries, time.time())
    return {
        "schedule_md5": pages_mod.compute_schedule_md(entries),
        "server_time": datetime.now(ZoneInfo(settings.canvas_timezone)).isoformat(),
        "current_index": index,
        "seconds_until_next_page": seconds_left,
        "policy": {
            "sleep_window": {
                "start": settings.canvas_sleep_start,
                "end": settings.canvas_sleep_end,
                "tz": settings.canvas_timezone,
            },
            "poll_interval_minutes": settings.canvas_poll_interval_minutes,
            "sleep_poll_interval_minutes": settings.canvas_sleep_poll_interval_minutes,
            "min_page_duration_minutes": settings.canvas_min_page_duration_minutes,
        },
        "pages": [e.to_dict() for e in entries],
        "screen_active": pages_mod.screen_active_now(),
    }


@mcp.tool
def describe_canvas_schema() -> dict:
    """The canvas JSON subset this server actually renders (with a worked example).

    Read it before writing canvas_json by hand: anything outside this subset
    makes the renderer raise, and the page is then rejected.
    """
    return {
        "panel": {"width": page_upload.PANEL_WIDTH, "height": page_upload.PANEL_HEIGHT,
                  "colors": ["white", "black", "red", "yellow"],
                  "bitmap_bytes": 30000},
        "root": {"windowData_key": "default", "value": "array of nodes"},
        "node_types": ["div", "span", "img"],
        "text": "a node whose props.children is a plain string, or children[] entries that are strings",
        # 以下三组都来自 canvas_render.CAPABILITIES（唯一真相）。历史上前端一份、
        # 渲染器一份，新增 w-full/justify-between 时只改了渲染器，MCP 就报了假账。
        "tw_tokens": [tok for group in CAPABILITIES["tw_tokens"].values() for tok in group],
        "tw_tokens_by_group": {k: list(v) for k, v in CAPABILITIES["tw_tokens"].items()},
        "style_keys": list(CAPABILITIES["style_keys"]),
        "img_props": {"src": "see image_src",
                      "style": "width/height in px (an unsized img draws 0x0)",
                      "dither": "omit (default) → auto: flat art is mapped tone-by-tone onto the palette (luminance-monotone, keeps distinct tones apart and uses black for dark tones), photos/gradients get Floyd-Steinberg. true ⇒ force dither, false ⇒ force nearest-color snap"},
        "image_src": [
            "data:image/...;base64,...",
            "http(s)://… (must be anonymously fetchable)",
            "uploads://<32 lowercase hex> (bound by upload_image_as_page)",
        ],
        "example": {"default": [{"type": "div", "props": {
            "tw": "flex flex-col w-full h-full items-center justify-center bg-white gap-[8px]",
            "children": [
                {"type": "span", "props": {"tw": "text-[28px]", "style": {"color": "#000000"},
                                           "children": "Hello 电子墨水"}},
            ]}}]},
        "not_supported": list(CAPABILITIES["not_supported"]) + [
            "templates ($for/$ifAny/{{get}})", "z-index", "3D transforms",
            "calc()", "RTL", "animation", "css classes"],
    }


# ─── canvas: write ────────────────────────────────────────────────────
@mcp.tool
def upsert_page(device_id: str, name: str, canvas_json: dict,
                duration_minutes: int = 10, order: int = 0) -> dict:
    """Create or replace a page; the server renders canvas_json to the panel bitmap.

    Same validation as POST /api/pages: the render must succeed and the
    duration must be at least min_page_duration_minutes.
    """
    _trusted_device(device_id)
    _checked_page_name(name)
    if not isinstance(canvas_json, dict):
        raise ValueError("canvas_json must be an object")
    if duration_minutes < settings.canvas_min_page_duration_minutes:
        raise ValueError("duration_minutes must be >= "
                         f"min_page_duration_minutes={settings.canvas_min_page_duration_minutes}")
    try:
        bitmap = render_canvas_to_bitmap(canvas_json)
    except RenderError as e:
        raise _render_error(e) from e
    entry = pages_mod.upsert_page(device_id, name, canvas_json,
                                  int(duration_minutes), int(order), bitmap)
    return entry.to_dict()


@mcp.tool
def delete_page(device_id: str, name: str) -> dict:
    """Delete a page (its bitmap is dropped when no page references it anymore)."""
    _trusted_device(device_id)
    _checked_page_name(name)
    if not pages_mod.delete_page(device_id, name):
        raise ValueError(f"unknown page: {name!r}")
    return {"deleted": name}


@mcp.tool
def reorder_pages(device_id: str, ordered_names: list[str]) -> dict:
    """Set the display order of a device's pages in one call.

    Pages you do not list keep their previous relative order and are placed
    after the listed ones. Each page's duration and canvas are preserved.
    """
    _trusted_device(device_id)
    sources = {s.name: s for s in pages_mod.list_pages(device_id)}
    for n in ordered_names:
        _checked_page_name(n)
    missing = [n for n in ordered_names if n not in sources]
    if missing:
        raise ValueError(f"unknown pages: {missing}; existing: {sorted(sources)}")
    if len(set(ordered_names)) != len(ordered_names):
        raise ValueError("ordered_names contains duplicates")
    rest = [n for n, _ in sorted(sources.items(), key=lambda kv: (kv[1].order, kv[1].name))
            if n not in ordered_names]
    final = list(ordered_names) + rest
    for idx, n in enumerate(final):
        src = sources[n]
        if src.order == idx:
            continue
        # 位图 md5 只由内容决定 ⇒ 只改 order 时重渲染结果不变，引用计数不受影响
        try:
            bitmap = render_canvas_to_bitmap(src.canvas_json)
        except RenderError as e:
            raise _render_error(e) from e
        pages_mod.upsert_page(device_id, n, src.canvas_json, src.duration_minutes, idx, bitmap)
    return {"device_id": device_id, "order": final}


@mcp.tool
def upload_image_as_page(device_id: str, name: str, image_base64: str,
                         duration_minutes: int = 0, order: int = -1) -> dict:
    """Turn an image into a page (server letterboxes it to the panel).

    Replace-or-create: an existing page keeps its own duration/order; a new one
    uses duration_minutes (default 10, still subject to the minimum) and, when
    order < 0, is appended after the last page. image_base64 accepts anything
    Pillow reads (PNG/JPEG/...); it is decoded, never written to the data dir
    as-is (the original is kept for re-cropping under uploads/).
    """
    _trusted_device(device_id)
    _checked_page_name(name)
    try:
        data = base64.b64decode(image_base64, validate=True)
    except Exception as e:  # noqa: BLE001 - binascii.Error and friends
        raise ValueError(f"image_base64 is not valid base64: {e}") from e
    if not data:
        raise ValueError("empty image")
    try:
        return page_upload.bind_image_to_page(
            device_id, name, data,
            create=True,
            duration_minutes=duration_minutes or None,
            order=None if order < 0 else int(order),
        )
    except ValueError as e:
        raise ValueError(str(e)) from e


# ─── canvas: preview ──────────────────────────────────────────────────
@mcp.tool
def preview_canvas(canvas_json: dict) -> Image:
    """Render canvas_json and return the PNG (nothing is stored or changed).

    Use this to check what a page will look like before upsert_page.
    """
    if not isinstance(canvas_json, dict):
        raise ValueError("canvas_json must be an object")
    try:
        png = render_canvas_to_png(canvas_json)
    except RenderError as e:
        raise _render_error(e) from e
    return Image(data=png, format="png")
