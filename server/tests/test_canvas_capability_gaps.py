"""Six low-cost capability additions, all with observable geometry/ink contracts:

- four-direction padding / margin tokens (pt-/pr-/pb-/pl-, mt-…-ml-)
  + style paddingTop/paddingRight/... marginTop/...
- min-w-*/max-w-*/min-h-*/max-h-* tokens (style keys existed; token spellings did not)
- text-decoration: underline / line-through (style key) + underline token
- per-corner radius (style borderRadius: "tl,tr,br,bl")

Each test renders a real page and asserts geometry/ink, never Spec fields.
"""
from __future__ import annotations

import pytest
from PIL import Image

from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.image_conv import IDX_BLACK, SCREEN_H, SCREEN_W


def _decode(data: bytes) -> Image.Image:
    """Same 2bpp unpack as test_canvas_render._decode_2bpp."""
    img = Image.new("RGB", (SCREEN_W, SCREEN_H))
    px = img.load()
    for y in range(SCREEN_H):
        for xb in range(SCREEN_W // 4):
            b = data[y * (SCREEN_W // 4) + xb]
            for i in range(4):
                idx = (b >> (6 - 2 * i)) & 0x03
                if idx == 0:
                    px[xb * 4 + i, y] = (0, 0, 0)          # black
                elif idx == 1:
                    px[xb * 4 + i, y] = (255, 255, 255)    # white
                elif idx == 2:
                    px[xb * 4 + i, y] = (255, 0, 0)        # red
                else:
                    px[xb * 4 + i, y] = (255, 255, 0)      # yellow
    return img


def _ink(img: Image.Image, box: tuple[int, int, int, int]) -> int:
    x0, y0, x1, y1 = box
    px = img.load()
    n = 0
    for y in range(y0, y1):
        for x in range(x0, x1):
            r, g, b = px[x, y]
            if r < 60 and g < 60 and b < 60:
                n += 1
    return n


def _dark_cols(img: Image.Image, y0: int, y1: int) -> list[int]:
    px = img.load()
    cols = []
    for x in range(SCREEN_W):
        for y in range(y0, y1):
            r, g, b = px[x, y]
            if r < 60 and g < 60 and b < 60:
                cols.append(x)
                break
    return cols


# ── four-direction padding ──────────────────────────────────────────

def test_padding_left_token_shifts_the_child():
    a = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px]",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px]",
            "children": [],
        }}],
    }}]})
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px] pl-[30px]",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px]",
            "children": [],
        }}],
    }}]})
    ia, ib = _decode(a), _decode(b)
    ca = _dark_cols(ia, 0, SCREEN_H)
    cb = _dark_cols(ib, 0, SCREEN_H)
    assert min(cb) - min(ca) == 26, f"pl-[30px] 应把块右移 26px（30-4）：{min(ca)}→{min(cb)}"


def test_padding_top_style_key_shifts_down():
    a = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px]",
            "children": [],
        }}],
    }}]})
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white",
        "style": {"paddingTop": 25},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px]",
            "children": [],
        }}],
    }}]})
    ib = _decode(b)
    ia = _decode(a)
    rows = [y for y in range(SCREEN_H)
            if any(_ink(ib, (x, y, x + 1, y + 1)) for x in range(0, 60))]
    rows_a = [y for y in range(SCREEN_H)
              if any(_ink(ia, (x, y, x + 1, y + 1)) for x in range(0, 60))]
    assert rows[0] - rows_a[0] == 25, f"paddingTop:25 应把块下移 25px: {rows_a[0]}→{rows[0]}"


# ── four-direction margin ───────────────────────────────────────────

def test_margin_left_token_shifts_in_a_row():
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white flex flex-row",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px] ml-[50px]",
            "children": [],
        }}],
    }}]})
    cols = _dark_cols(_decode(b), 0, SCREEN_H)
    assert min(cols) == 50, f"ml-[50px] 应让块起点在 x=50: {min(cols)}"


def test_margin_top_style_key_shifts_in_a_column():
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white flex flex-col",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] h-[20px]",
            "style": {"marginTop": 33},
            "children": [],
        }}],
    }}]})
    ib = _decode(b)
    rows = [y for y in range(SCREEN_H)
            if any(_ink(ib, (x, y, x + 1, y + 1)) for x in range(0, 60))]
    assert rows[0] == 33, f"marginTop:33 应让块顶在 y=33: {rows[0]}"


# ── min/max size tokens ─────────────────────────────────────────────

def test_min_w_token_floors_the_width():
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white flex flex-row",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[40px] min-w-[120px] h-[20px]",
            "children": [],
        }}],
    }}]}))
    cols = _dark_cols(img, 0, SCREEN_H)
    assert max(cols) - min(cols) + 1 == 120, f"min-w-[120px] 应把 40px 撑到 120px: {max(cols)-min(cols)+1}"


def test_max_w_token_caps_the_width():
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white flex flex-row",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "bg-black w-[300px] max-w-[100px] h-[20px]",
            "children": [],
        }}],
    }}]}))
    cols = _dark_cols(img, 0, SCREEN_H)
    assert max(cols) - min(cols) + 1 == 100, f"max-w-[100px] 应把 300px 压到 100px: {max(cols)-min(cols)+1}"


# ── text decoration ─────────────────────────────────────────────────

def test_underline_style_key_draws_a_line():
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px]",
        "style": {"color": "#000000", "textDecoration": "underline"},
        "children": "下划",
    }}]}))
    # 下划线是 1-2px 高的水平连续段：找一行墨数远超字形（"下划"@16px ≈ 30px 宽，
    # 字形墨分布稀疏；下划线行应接近 32px 连续黑）
    rows = [(_ink(img, (10, y, 60, y + 1)), y) for y in range(40)]
    best_n, best_y = max(rows)
    assert best_n >= 20, f"应有接近整宽的水平下划线: y={best_y} ink={best_n}"


def test_underline_token_draws_a_line():
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px] underline",
        "style": {"color": "#000000"},
        "children": "下划",
    }}]}))
    rows = [(_ink(img, (10, y, 60, y + 1)), y) for y in range(40)]
    best_n, best_y = max(rows)
    assert best_n >= 20, f"underline 令牌同样要画线: y={best_y} ink={best_n}"


def test_line_through_draws_a_mid_line():
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px]",
        "style": {"color": "#000000", "textDecoration": "line-through"},
        "children": "删除",
    }}]}))
    rows = [(_ink(img, (10, y, 60, y + 1)), y) for y in range(40)]
    best_n, best_y = max(rows)
    assert best_n >= 20, f"line-through 应有贯穿线: y={best_y} ink={best_n}"


# ── per-corner radius ───────────────────────────────────────────────

def test_per_corner_radius_cuts_only_the_named_corner():
    """borderRadius '20,0,0,0'（tl）只应削左上角：右上角应是实心直角。"""
    img = _decode(render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-black w-[60px] h-[40px]",
        "style": {"borderRadius": "20,0,0,0"},
        "children": [],
    }}]}))
    px = img.load()
    # 左上角 (0,0) 附近应无墨（被削）；右上角 (59,0) 应有墨
    tl = _ink(img, (0, 0, 8, 8))
    tr = _ink(img, (52, 0, 60, 8))
    assert tl < tr, f"左上角被削({tl})应明显小于右上直角({tr})"
