"""Font-weight ladder and `writingMode: vertical-rl`.

Weight contracts:
- each declared face renders differently (thin/light/medium/bold/black are real
  Noto CJK files, not aliases), with monotonically increasing ink
- `fontWeight: 700` byte-identical to the `font-bold` token (same resolve path)
- `font-normal` byte-identical to the default (no style) — an alias, not a face

Vertical contracts (root div p-[10px] → inner box (10,10,380,280)):
- horizontal control: glyphs at the LEFT, single line box
- vertical-rl: glyphs stack top→bottom in ONE column anchored at the RIGHT
  edge of the content box; nothing drawn to the left of that column
- a newline opens a SECOND column to the LEFT of the first (rl flow)
"""
from __future__ import annotations

from PIL import Image, ImageDraw

from youn_server.canvas import text as ct
from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.image_conv import SCREEN_H, SCREEN_W

SIZE = 40          # vertical test font size
INNER_X, INNER_W = 10, 380     # p-[10px] on the full-screen root box


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


def _ink(img: Image.Image, box: tuple[int, int, int, int]) -> int:
    x0, y0, x1, y1 = box
    px = img.load()
    return sum(1 for y in range(y0, min(y1, SCREEN_H))
               for x in range(x0, min(x1, SCREEN_W)) if sum(px[x, y]) < 180)


def _render_text(tw: str, children: str, style: dict | None = None) -> bytes:
    return render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": f"bg-white p-[10px] {tw}".strip(),
        "style": {"color": "#000000", **(style or {})},
        "children": children,
    }}]})


# ── weights ─────────────────────────────────────────────────────────

def test_each_weight_face_renders_differently_with_monotonic_ink():
    reg = _decode(_render_text("text-[32px]", "字重测试"))
    med = _decode(_render_text("text-[32px] font-medium", "字重测试"))
    bold = _decode(_render_text("text-[32px] font-bold", "字重测试"))
    black = _decode(_render_text("text-[32px] font-black", "字重测试"))
    box = (10, 10, 300, 60)
    inks = {k: _ink(im, box) for k, im in
            (("reg", reg), ("med", med), ("bold", bold), ("black", black))}
    # 每档是真实不同的字面（不是别名）：墨量严格单调递增
    assert inks["reg"] < inks["med"] < inks["bold"] < inks["black"], \
        f"字重墨量应严格递增: {inks}"


def test_fontweight_700_is_byte_identical_to_bold_token():
    a = _render_text("text-[32px]", "字重测试", {"fontWeight": 700})
    b = _render_text("text-[32px] font-bold", "字重测试")
    assert a == b, "style.fontWeight:700 必须与 font-bold 令牌走同一张字面"


def test_font_normal_is_byte_identical_to_default():
    a = _render_text("text-[32px] font-normal", "字重测试")
    b = _render_text("text-[32px]", "字重测试")
    assert a == b, "font-normal 是默认字面的别名，不得引入差异"


# ── writing-mode: vertical-rl ───────────────────────────────────────

def _col_w() -> int:
    return ct.metrics(ct.load_font(SIZE, "regular"), SIZE)


def test_horizontal_control_sits_left_in_one_line_box():
    img = _decode(_render_text(f"text-[{SIZE}px]", "甲乙",
                               {"writingMode": "horizontal-tb"}))
    assert _ink(img, (10, 10, 200, 60)) > 0, "水平基线：字应在左上"
    assert _ink(img, (10, 64, 400, 120)) == 0, "水平基线：y>64 不应有墨（单行实测止于 y=60）"


def test_vertical_rl_stacks_in_one_column_at_the_right_edge():
    cw = _col_w()
    col0_x = INNER_X + INNER_W - cw            # 最右列的左缘
    img = _decode(_render_text(f"text-[{SIZE}px]", "甲乙",
                               {"writingMode": "vertical-rl"}))
    right_band = (col0_x - 2, 10, INNER_X + INNER_W, 280)
    left_of_col = (INNER_X, 10, col0_x - 2, 280)
    assert _ink(img, right_band) > 0, f"竖排：最右列(col0_x={col0_x})应有墨"
    assert _ink(img, left_of_col) == 0, "竖排：该列左侧不应有任何墨（单列）"
    # 两字上下堆叠 ⇒ 墨迹纵向跨度明显超过单行水平框（~SIZE 高）
    stacked = _ink(img, (col0_x - 2, 10 + SIZE + 4, INNER_X + INNER_W, 280))
    assert stacked > 0, "竖排：第二个字应在第一个字下方（纵向堆叠）"


def test_vertical_newline_opens_a_column_to_the_left():
    cw = _col_w()
    col0_x = INNER_X + INNER_W - cw
    col1_x = col0_x - cw
    img = _decode(_render_text(f"text-[{SIZE}px]", "甲\n乙",
                               {"writingMode": "vertical-rl"}))
    assert _ink(img, (col0_x - 2, 10, INNER_X + INNER_W, 280)) > 0, "第一列(最右)有墨"
    assert _ink(img, (col1_x - 2, 10, col0_x - 2, 280)) > 0, "第二列在第一列左侧有墨（rl）"
    assert _ink(img, (INNER_X, 10, col1_x - 2, 280)) == 0, "第二列左侧应为空"
