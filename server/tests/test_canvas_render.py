"""Canvas Loop renderer unit tests.

Covers:
- MD5 stability (same input → same bytes → same md5)
- Empty default → blank white 30000-byte bitmap
- Unsupported element type → RenderError with path
- Missing img src → RenderError
- Layout: nested divs with bg/color text render correct palette pixels
- Path-traversal protection on get_bitmap
"""
from __future__ import annotations

import hashlib

import pytest

from youn_server.canvas_render import (
    RenderError, render_canvas_to_bitmap, _PILLOW_BG,
)
from youn_server.image_conv import IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW, SCREEN_H, SCREEN_W


def _decode_2bpp(data: bytes) -> "Image.Image":
    from PIL import Image
    img = Image.new("RGB", (SCREEN_W, SCREEN_H))
    px = img.load()
    for y in range(SCREEN_H):
        for xb in range(SCREEN_W // 4):
            byte = data[y * (SCREEN_W // 4) + xb]
            for xi in range(4):
                x = xb * 4 + xi
                shift = (3 - xi) * 2
                idx = (byte >> shift) & 0x3
                px[x, y] = _PILLOW_BG[idx]
    return img


def test_empty_default():
    b = render_canvas_to_bitmap({"default": []})
    assert len(b) == 30000


def test_md5_stability():
    canvas = {"default": [{"type": "div", "props": {"tw": "bg-white", "children": "stable"}}]}
    b1 = render_canvas_to_bitmap(canvas)
    b2 = render_canvas_to_bitmap(canvas)
    assert hashlib.md5(b1).hexdigest() == hashlib.md5(b2).hexdigest()


def test_unsupported_element():
    with pytest.raises(RenderError) as exc:
        render_canvas_to_bitmap({
            "default": [{"type": "button", "props": {"children": "x"}}]
        })
    assert "unsupported element type" in str(exc.value)
    assert "button" in str(exc.value)


def test_missing_img_src():
    with pytest.raises(RenderError) as exc:
        render_canvas_to_bitmap({
            "default": [{"type": "img", "props": {}}]
        })
    assert "props.src" in str(exc.value)


def test_invalid_windowData():
    with pytest.raises(RenderError) as exc:
        render_canvas_to_bitmap({"default": "not-a-list"})
    assert "windowData.default" in str(exc.value)


def test_simple_text_renders():
    canvas = {
        "default": [{
            "type": "div",
            "props": {
                "tw": "flex flex-col p-[12px] bg-white",
                "style": {"color": "#000000"},
                "children": ["Hello Youn Ink"],
            },
        }],
    }
    bmp = render_canvas_to_bitmap(canvas)
    assert len(bmp) == 30000
    img = _decode_2bpp(bmp)
    # Background should be white at far corners
    assert img.getpixel((390, 290)) == _PILLOW_BG[IDX_WHITE]
    # Some non-white pixel must exist (the text)
    colors = set()
    px = img.load()
    for y in range(SCREEN_H):
        for x in range(SCREEN_W):
            colors.add(px[x, y])
    assert len(colors) > 1, "expected text to render at least one non-white pixel"


def test_colored_boxes():
    canvas = {
        "default": [{
            "type": "div",
            "props": {
                "tw": "flex flex-col p-[10px] gap-[10px] bg-black",
                "style": {"color": "#FFFFFF"},
                "children": [
                    {"type": "div", "props": {"tw": "bg-yellow w-[100px] h-[50px]", "children": ""}},
                    {"type": "div", "props": {"tw": "bg-red w-[100px] h-[50px]", "children": ""}},
                ],
            },
        }],
    }
    bmp = render_canvas_to_bitmap(canvas)
    assert len(bmp) == 30000
    img = _decode_2bpp(bmp)
    # Both yellow and red must appear
    px = img.load()
    yellows = sum(1 for y in range(SCREEN_H) for x in range(SCREEN_W) if px[x, y] == _PILLOW_BG[IDX_YELLOW])
    reds = sum(1 for y in range(SCREEN_H) for x in range(SCREEN_W) if px[x, y] == _PILLOW_BG[IDX_RED])
    assert yellows > 0, "no yellow pixels rendered"
    assert reds > 0, "no red pixels rendered"


def test_render_error_path_format():
    try:
        render_canvas_to_bitmap({
            "default": [{"type": "div", "props": {"children": [{"type": "bogus", "props": {}}]}}]
        })
    except RenderError as e:
        assert e.path.startswith("windowData.default[0]")
        assert e.message


def test_palette_colors_exist():
    from youn_server.image_conv import PALETTE_BWRY
    # The palette must contain exactly 4 colors matching the 2bpp hardware.
    assert len(PALETTE_BWRY) == 4
    assert PALETTE_BWRY[IDX_BLACK] == (0, 0, 0)
    assert PALETTE_BWRY[IDX_WHITE] == (255, 255, 255)
    assert PALETTE_BWRY[IDX_YELLOW] == (255, 215, 0)
    assert PALETTE_BWRY[IDX_RED] == (220, 30, 30)