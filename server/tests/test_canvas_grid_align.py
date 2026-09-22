"""Grid alignment (`justify-items`) and axis-split gap (`gap-x-` / `gap-y-`).

Geometry contracts on rendered ink:
- justify-items:center must centre a narrower child inside its grid column
  (previously the column gap was ignored for the inline axis: always stretch).
- gap-x-/gap-y- tokens (and style rowGap/columnGap) split the single gap into
  per-axis values; gap-[N] still sets both.
"""
from __future__ import annotations

from PIL import Image

from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.image_conv import SCREEN_H, SCREEN_W


def _decode(data: bytes) -> Image.Image:
    img = Image.new("RGB", (SCREEN_W, SCREEN_H))
    px = img.load()
    for y in range(SCREEN_H):
        for xb in range(SCREEN_W // 4):
            b = data[y * (SCREEN_W // 4) + xb]
            for i in range(4):
                idx = (b >> (6 - 2 * i)) & 0x03
                px[xb * 4 + i, y] = ((0, 0, 0) if idx == 0 else
                                     (255, 255, 255) if idx == 1 else
                                     (255, 0, 0) if idx == 2 else (255, 255, 0))
    return img


def _dark_cols(img: Image.Image, y0: int, y1: int) -> list[int]:
    px = img.load()
    cols = []
    for x in range(SCREEN_W):
        for y in range(y0, y1):
            if sum(px[x, y]) < 180:
                cols.append(x)
                break
    return cols


def _block_starts(img: Image.Image, y0: int, y1: int, min_run: int = 10) -> list[int]:
    """连续黑列段的起点（段宽 >= min_run 才算块）——两个并排块给 [a, b]。"""
    cols = _dark_cols(img, y0, y1)
    starts, prev = [], -100
    for x in cols:
        if x != prev + 1:
            starts.append(x)
        prev = x
    # 段长过滤：并排单像素噪声会被段长剔除
    runs, cur = [], None
    for x in cols:
        if cur and x == cur[-1] + 1:
            cur.append(x)
        else:
            cur = [x]
            runs.append(cur)
    return [r[0] for r in runs if len(r) >= min_run]


def _dark_rows(img: Image.Image, x0: int, x1: int) -> list[int]:
    px = img.load()
    rows = []
    for y in range(SCREEN_H):
        for x in range(x0, x1):
            if sum(px[x, y]) < 180:
                rows.append(y)
                break
    return rows


def _two_cell_grid(extra: str = "") -> dict:
    """2×1 grid of two 40px black blocks; container starts at x=10,y=10."""
    return {"default": [{"type": "div", "props": {
        "tw": f"bg-white p-[10px] {extra}",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px", "gap": "10px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
        ],
    }}]}


# ── justify-items ────────────────────────────────────────────────────

def test_justify_items_center_centres_within_the_track():
    # 无 justify-items：块贴轨道左缘（x=10 与 x=60）。
    base = _block_starts(_decode(render_canvas_to_bitmap(_two_cell_grid())), 10, 30)
    assert base == [10, 60], f"基线：块应在轨道起点 {base}"

    # justify-items:center + 30px 窄块 ⇒ 每列(40px)内居中 ⇒ 偏移 +5px。
    page = {"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px] justify-items-center",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px", "gap": "10px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[30px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[30px] h-[20px]", "children": []}},
        ],
    }}]}
    cols = _block_starts(_decode(render_canvas_to_bitmap(page)), 10, 30)
    assert cols == [15, 65], \
        f"justify-items:center 应把 30px 块在 40px 轨道内居中(+5px): {cols[:2]}"


def test_justify_items_end_pins_to_track_end():
    page = {"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px] justify-items-end",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px", "gap": "10px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[30px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[30px] h-[20px]", "children": []}},
        ],
    }}]}
    cols = _block_starts(_decode(render_canvas_to_bitmap(page)), 10, 30)
    assert cols == [20, 70], \
        f"justify-items:end 应贴轨道右缘(40-30=+10px): {cols[:2]}"


# ── gap 双轴 ─────────────────────────────────────────────────────────

def test_gap_x_only_changes_the_column_gap():
    # 两列 40px 块 + gap-x-[30px] ⇒ 第二块起点 = 10 + 40 + 30 = 80。
    page = {"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px] gap-x-[30px]",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
        ],
    }}]}
    cols = _block_starts(_decode(render_canvas_to_bitmap(page)), 10, 30)
    assert cols == [10, 80], f"gap-x-[30px] 列间距: {cols}"


def test_gap_y_only_changes_the_row_gap():
    # 单列 + **固定行**：顶层节点拿到整屏盒 ⇒ 高度 definite ⇒ auto 行会按
    # align-content:stretch 拉伸填满（CSS 正确行为），把 gap 观察不出来；
    # 固定行高才隔离出纯行间距。
    page = {"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px] gap-y-[25px]",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px", "gridTemplateRows": "20px,20px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
        ],
    }}]}
    rows = _dark_rows(_decode(render_canvas_to_bitmap(page)), 10, 50)
    # 第二行块顶 = 10 + 20 + 25 = 55。
    assert rows[0] == 10 and rows[-1] == 74, f"gap-y-[25px] 行间距: {rows[0]}..{rows[-1]}"


def test_style_rowgap_columngap_equivalent_to_tokens():
    a = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px]",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px",
                  "rowGap": 25, "columnGap": 30},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
        ],
    }}]})
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[10px] gap-x-[30px] gap-y-[25px]",
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "40px,40px"},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[40px] h-[20px]", "children": []}},
        ],
    }}]})
    assert a == b, "style rowGap/columnGap 必须与 gap-x-/gap-y- 令牌字节等价"
