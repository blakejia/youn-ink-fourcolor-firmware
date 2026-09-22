"""grid justify-content / align-content: track distribution in free space.

Fixed tracks in a definite container leave free space; CSS distributes it per
justify-content (columns) / align-content (rows). Previously `_offsets` always
started at the origin, so center/end/space-* were inert for grid.

Geometry: root div p-[10px] → inner (10,10,380,280); columns 60+60 gap-x 10,
rows 40+40 gap-y 10. A gap is required: without it adjacent black blocks merge
into one dark run and starts become undetectable.
  cols: used 130, free 250 → start  flex-start 10 | center 135 | end 260
  rows: used 90,  free 190 → start  flex-start 10 | center 105 | end 200
Children carry explicit w/h matching their track so justify-items cannot mask
the track position.
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
    return [x for x in range(SCREEN_W)
            if any(sum(px[x, y]) < 180 for y in range(y0, y1))]


def _dark_rows(img: Image.Image, x0: int, x1: int) -> list[int]:
    px = img.load()
    return [y for y in range(SCREEN_H)
            if any(sum(px[x, y]) < 180 for x in range(x0, x1))]


def _run_starts(vals: list[int]) -> list[int]:
    """连续段的起点（段间必有空档 —— 测试模板保证了 gap）。"""
    runs, cur = [], None
    for v in vals:
        if cur is None or v != cur[-1] + 1:
            cur = [v]
            runs.append(cur)
        else:
            cur.append(v)
    return [r[0] for r in runs]


def _grid(tw_extra: str = "", style_extra: dict | None = None) -> dict:
    return {"default": [{"type": "div", "props": {
        "tw": f"bg-white p-[10px] gap-x-[10px] gap-y-[10px] {tw_extra}".strip(),
        "style": {"color": "#000000", "display": "grid",
                  "gridTemplateColumns": "60px,60px",
                  "gridTemplateRows": "40px,40px",
                  **(style_extra or {})},
        "children": [
            {"type": "div", "props": {"tw": "bg-black w-[60px] h-[40px]", "children": []}},
            {"type": "div", "props": {"tw": "bg-black w-[60px] h-[40px]", "children": []}},
        ],
    }}]}


def _col_starts(tw_extra: str = "", justify: str | None = None) -> list[int]:
    st = {"justifyContent": justify} if justify else {}
    img = _decode(render_canvas_to_bitmap(_grid(tw_extra, st)))
    return _run_starts(_dark_cols(img, 15, 35))[:2]


def _grid_single_col(tw_extra: str = "", style_extra: dict | None = None) -> dict:
    """单列模板：行优先放置下两块才会纵向堆叠（双列会把两块放进同一行，
    第二行为空 —— 与 test_canvas_grid_align 的 gap-y 教训同类）。"""
    page = _grid(tw_extra, style_extra)
    page["default"][0]["props"]["style"]["gridTemplateColumns"] = "60px"
    return page


def _row_starts(tw_extra: str = "", align: str | None = None) -> list[int]:
    st = {"alignContent": align} if align else {}
    img = _decode(render_canvas_to_bitmap(_grid_single_col(tw_extra, st)))
    return _run_starts(_dark_rows(img, 15, 60))[:2]


def test_justify_content_default_is_start():
    assert _col_starts() == [10, 80], "默认 flex-start：轨道贴左缘 10/80"


def test_justify_content_center():
    assert _col_starts(justify="center") == [135, 205], \
        "center: free 250 → start 10+125 = 135"


def test_justify_content_end():
    assert _col_starts(justify="end") == [260, 330], \
        "end: start 10+250 = 260"


def test_justify_content_space_between():
    assert _col_starts(justify="space-between") == [10, 330], \
        "space-between: 首轨贴左、末轨贴右(10+380-60=330)"


def test_justify_content_token_matches_style_key():
    a = _col_starts(tw_extra="justify-center")
    b = _col_starts(justify="center")
    assert a == b == [135, 205], "justify-center 令牌与 justifyContent 样式键等价"


def test_align_content_default_is_start():
    assert _row_starts() == [10, 60], "默认 flex-start：行贴上缘 10/60"


def test_align_content_center():
    assert _row_starts(align="center") == [105, 155], \
        "center: free 190 → start 10+95 = 105"


def test_align_content_end():
    assert _row_starts(align="end") == [200, 250], \
        "end: start 10+190 = 200"
