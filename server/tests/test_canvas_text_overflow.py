"""`text-overflow: ellipsis` (and the `truncate` token) must actually truncate.

Two separate defects, one user-visible symptom ("ellipsis does nothing"):
1. `style={{textOverflow:'ellipsis'}}` was never parsed — no key in tokens.py.
2. The `truncate` token set `nowrap` + `truncated` but never set `line_clamp`,
   so `clamp()` was called with `max_lines=None` and returned early: the
   ellipsis branch was unreachable from any Tailwind spelling.

Assertions are on the rendered ink (the observable contract), not on Spec
fields: a page whose single line is too wide must show an ellipsis where the
same page without the property shows the clipped/overflowing glyphs.
"""
from __future__ import annotations

import pytest

from youn_server.canvas import text as canvas_text
from youn_server.canvas_render import render_canvas_to_bitmap

# Long CJK string: definitely wider than the 200 px box below at 20 px.
LONG = "这是一段非常长的状态描述文字需要在窄框里被省略号截断才对"


def _page(props: dict) -> dict:
    return {"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px]",
        "style": {"color": "#000000", **props.get("style", {})},
        "children": [{"type": "div", "props": {
            "tw": "w-[200px] text-[20px]",
            "style": props.get("style", {}),
            "children": LONG,
        }}],
    }}]}


def _ink_on_row(data: bytes, row: int) -> int:
    """Black pixels in one scanline of the 2bpp bitmap (400 px wide, 1 px/2 bits)."""
    from youn_server.image_conv import IDX_BLACK, SCREEN_W
    per_row = SCREEN_W // 4
    base = row * per_row
    n = 0
    for xb in range(per_row):
        b = data[base + xb]
        for i in range(4):
            if ((b >> (6 - 2 * i)) & 0x03) == IDX_BLACK:
                n += 1
    return n


def test_fit_box_clamps_and_ellipsizes_a_single_line():
    """The unit contract: with max_lines and ellipsis the last line ends in `…`."""
    font = canvas_text.load_font(20)
    _, _, lines = canvas_text.fit_box(
        LONG, font, 200, 20, nowrap=True, max_lines=1, ellipsis=True,
    )
    assert len(lines) == 1
    assert lines[0].endswith("…"), f"省略号缺失: {lines[0]!r}"
    assert canvas_text.width_of(lines[0], font) <= 200, "截断后仍超宽"


def test_fit_box_without_ellipsis_keeps_plain_text():
    """Negative control: the same clamp without ellipsis must NOT add `…`."""
    font = canvas_text.load_font(20)
    _, _, lines = canvas_text.fit_box(
        LONG, font, 200, 20, nowrap=True, max_lines=1, ellipsis=False,
    )
    assert not lines[0].endswith("…")


@pytest.mark.parametrize("spelling", ["tw", "style"])
def test_text_overflow_ellipsis_renders_something_different(spelling):
    """Both spellings must change the render — currently the style key is inert."""
    plain = render_canvas_to_bitmap(_page({}))
    styled = render_canvas_to_bitmap(
        _page({"style": {"textOverflow": "ellipsis"}} if spelling == "style" else {})
    )
    if spelling == "tw":
        # `truncate` token spelling, applied via tw on the inner node.
        page = {"default": [{"type": "div", "props": {
            "tw": "bg-white p-[4px]",
            "style": {"color": "#000000"},
            "children": [{"type": "div", "props": {
                "tw": "w-[200px] text-[20px] truncate",
                "style": {"color": "#000000"},
                "children": LONG,
            }}],
        }}]}
        styled = render_canvas_to_bitmap(page)
    assert styled != plain, f"{spelling} 拼法没有改变渲染结果 —— 属性仍是无效的"


def test_style_text_overflow_ellipsis_matches_truncate_token():
    """The two spellings are the same feature: byte-identical output."""
    a = render_canvas_to_bitmap(_page({"style": {"textOverflow": "ellipsis"}}))
    b = render_canvas_to_bitmap({"default": [{"type": "div", "props": {
        "tw": "bg-white p-[4px]",
        "style": {"color": "#000000"},
        "children": [{"type": "div", "props": {
            "tw": "w-[200px] text-[20px] truncate",
            "style": {"color": "#000000"},
            "children": LONG,
        }}],
    }}]})
    assert a == b, "text-overflow:ellipsis 与 truncate 必须等价"
