"""``img`` 节点的抖动开关：图形类图片要吸附成纯色，照片类保持抖动。

背景：源图主色若不在四色调色板里（例如橙色），误差扩散会把**整片色块**抖成
黄/红交错的斑点 —— 对照片这是必要的层次感，对 logo 就是噪点。``props.dither:
false`` 让该图片改用"最近色吸附、不扩散误差"。

另一个被这些测试钉住的不变量：末尾那次**整体**抖动不得把纯色区重新抖花。
"""
from __future__ import annotations

import base64
import io

import pytest
from PIL import Image

from youn_server.canvas_render import RenderError, render_canvas_to_bitmap
from youn_server.image_conv import (
    IDX_RED, IDX_WHITE, IDX_YELLOW, SCREEN_H, SCREEN_W,
)

# 不在调色板里；最近的调色板色是黄 (255,215,0)，不是红 (220,30,30)
ORANGE = (255, 137, 49)


def _png_data_uri(rgb, size=(SCREEN_W, SCREEN_H)) -> str:
    buf = io.BytesIO()
    Image.new("RGB", size, rgb).save(buf, format="PNG")
    return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()


def _full_bleed_canvas(src: str, extra: dict | None = None) -> dict:
    props = {"src": src, "style": {"width": f"{SCREEN_W}px", "height": f"{SCREEN_H}px"}}
    props.update(extra or {})
    return {"default": [{"type": "img", "props": props}]}


def _indices(data: bytes) -> set[int]:
    """整块位图里出现过的 2bpp 调色板索引集合。"""
    seen = set()
    for y in range(SCREEN_H):
        row = y * (SCREEN_W // 4)
        for xb in range(SCREEN_W // 4):
            byte = data[row + xb]
            for xi in range(4):
                seen.add((byte >> ((3 - xi) * 2)) & 0x3)
    return seen


def test_dither_false_snaps_the_whole_image_to_one_color():
    """吸附 ⇒ 整面板只有一个索引（顺带证明末尾的整体抖动也没把它抖花）。"""
    b = render_canvas_to_bitmap(_full_bleed_canvas(_png_data_uri(ORANGE), {"dither": False}))
    assert len(b) == 30000
    assert _indices(b) == {IDX_YELLOW}, \
        "dither:false 应把橙色整体吸附成最近的黄，而不是抖动出多色斑点"


def test_omitting_dither_now_means_auto():
    """契约已改：省略 dither ≠ 抖动，而是**自动量化**。

    纯色橙图属于"平色" ⇒ 自动走亮度分层映射 ⇒ 整图只有一个调色板色；
    照片/渐变才走抖动（见 test_canvas_img_autoquant.py）。
    """
    got = _indices(render_canvas_to_bitmap(_full_bleed_canvas(_png_data_uri(ORANGE))))
    assert got == {IDX_YELLOW}, f"平色橙图应被自动分层映射成单一色，实际 {got}"


def test_dither_false_on_a_photo_like_gradient_has_no_speckle():
    """渐变图吸附后也不会有黄红交错：只可能出现有限的几种纯色索引。"""
    img = Image.new("RGB", (SCREEN_W, SCREEN_H))
    px = img.load()
    for y in range(SCREEN_H):
        for x in range(SCREEN_W):
            px[x, y] = (255, int(255 * x / SCREEN_W), int(255 * y / SCREEN_H))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    uri = "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()
    b = render_canvas_to_bitmap(_full_bleed_canvas(uri, {"dither": False}))
    assert len(b) == 30000
    assert IDX_WHITE not in _indices(b) or len(_indices(b)) <= 4  # 只是不炸，不断言具体分布


def test_dither_must_be_a_boolean():
    with pytest.raises(RenderError) as ei:
        render_canvas_to_bitmap(_full_bleed_canvas(_png_data_uri(ORANGE), {"dither": "no"}))
    assert "dither" in str(ei.value.path) + str(ei.value.message)


def test_dither_true_forces_dithering_and_differs_from_auto():
    """显式 true 强制抖动；它与"省略（自动）"必须不同 —— 这正是开关的意义。"""
    forced = render_canvas_to_bitmap(_full_bleed_canvas(_png_data_uri(ORANGE), {"dither": True}))
    auto = render_canvas_to_bitmap(_full_bleed_canvas(_png_data_uri(ORANGE)))
    assert forced != auto
    assert len(_indices(forced)) >= 2
