"""img 不写 ``dither`` 时的自动量化：平色图形 → 按亮度**单调分层映射**；照片 → 抖动。

为什么不是"吸附二选一"：对已贴调的平色图，抖动与吸附逐字节相同（零误差）⇒ 无所谓；
对离盘远的平色图，吸附会把**相近色调并成一个**（鲸鱼 logo 的嘴就是这样消失的）。
所以平色图形走"把图片自己的若干主色按亮度排序、映射到调色板里单调递增的若干色"
（最小总代价）—— 鲸鱼的 {橙162, 浅橙197, 白255} 最优解正是 红/黄/白，
黑白线稿则得到 黑/白。

这些测试都**能在未实现时失败**：断言的是"自动结果 ≠ 抖动结果"这类真正的判别式。
"""
from __future__ import annotations

import base64
import io

import pytest
from PIL import Image

from youn_server.canvas_render import RenderError, render_canvas_to_bitmap
from youn_server.image_conv import (
    IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW, SCREEN_H, SCREEN_W,
)

WHITE = (255, 255, 255)
ORANGE = (255, 137, 49)          # 亮度 162：离盘远，但应当落到"红"
LIGHT_ORANGE = (255, 194, 57)    # 亮度 197：应当落到"黄"
BLACK = (0, 0, 0)


def _uri(img: Image.Image) -> str:
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()


def _canvas(uri: str, extra: dict | None = None) -> dict:
    props = {"src": uri, "style": {"width": f"{SCREEN_W}px", "height": f"{SCREEN_H}px"}}
    props.update(extra or {})
    return {"default": [{"type": "img", "props": props}]}


def _indices(data: bytes) -> set[int]:
    seen = set()
    for y in range(SCREEN_H):
        row = y * (SCREEN_W // 4)
        for xb in range(SCREEN_W // 4):
            byte = data[row + xb]
            for xi in range(4):
                seen.add((byte >> ((3 - xi) * 2)) & 0x3)
    return seen


def _speckle_ratio(data: bytes) -> float:
    """非白像素里"四邻都不同色"的孤立斑點占比 —— 抖动的指纹。"""
    W, H = SCREEN_W, SCREEN_H
    idx = [[0] * W for _ in range(H)]
    for y in range(H):
        for xb in range(W // 4):
            byte = data[y * (W // 4) + xb]
            for xi in range(4):
                idx[y][xb * 4 + xi] = (byte >> ((3 - xi) * 2)) & 0x3
    iso = nw = 0
    for y in range(1, H - 1):
        for x in range(1, W - 1):
            c = idx[y][x]
            if c == IDX_WHITE:
                continue
            nw += 1
            if (idx[y-1][x] != c and idx[y+1][x] != c
                    and idx[y][x-1] != c and idx[y][x+1] != c):
                iso += 1
    return iso / max(1, nw)


def _whale_like() -> Image.Image:
    """三档平色：白底 + 橙身 + 浅橙（嘴/下巴）—— 应当映射成 红/黄 两种可区分色。"""
    img = Image.new("RGB", (SCREEN_W, SCREEN_H), WHITE)
    for y in range(60, 240):
        for x in range(60, 340):
            img.putpixel((x, y), ORANGE)
    for y in range(190, 240):          # 下方的"嘴/下巴"
        for x in range(80, 200):
            img.putpixel((x, y), LIGHT_ORANGE)
    return img


def _line_art() -> Image.Image:
    img = Image.new("RGB", (SCREEN_W, SCREEN_H), WHITE)
    for x in range(40, 360):
        for y in range(140, 160):
            img.putpixel((x, y), BLACK)
    return img


def _gradient() -> Image.Image:
    img = Image.new("RGB", (SCREEN_W, SCREEN_H))
    px = img.load()
    for y in range(SCREEN_H):
        for x in range(SCREEN_W):
            px[x, y] = (128, int(255 * x / SCREEN_W), int(255 * y / SCREEN_H))
    return img


# ── 平色图形：自动分层映射 ────────────────────────────────────────────
def test_flat_art_keeps_two_tones_apart():
    """核心回归：橙色与浅橙必须落到**两种不同**的调色板色（红与黄），嘴才不会消失。"""
    b = render_canvas_to_bitmap(_canvas(_uri(_whale_like())))
    got = _indices(b)
    assert {IDX_RED, IDX_YELLOW} <= got, f"身体与嘴应当分别落到红与黄，实际 {got}"
    # 关键：分层映射给的是**实心块**；抖动版虽然也含红黄，却是 20%+ 的孤立斑点
    assert _speckle_ratio(b) < 0.05, \
        f"自动量化应给出实心色块，斑点率 {_speckle_ratio(b):.1%} 说明它其实在抖动"


def test_auto_is_not_the_same_as_dithering_for_flat_art():
    """判别式：自动结果必须**不同于**显式抖动（否则这个特性是空操作）。"""
    uri = _uri(_whale_like())
    assert render_canvas_to_bitmap(_canvas(uri)) != \
        render_canvas_to_bitmap(_canvas(uri, {"dither": True}))


def test_line_art_uses_black():
    """黑线稿应当用上黑色（单调映射在两档时给出 黑/白）。"""
    got = _indices(render_canvas_to_bitmap(_canvas(_uri(_line_art()))))
    assert IDX_BLACK in got and IDX_WHITE in got


def test_a_large_flat_source_is_not_mistaken_for_a_photo():
    """回归：判断"是否平色"必须在**缩放之前**。

    LANCZOS 把 800x800 缩到 240x240 时会造出大量中间色；若先缩再判，这张平色图
    会被当成照片 ⇒ 又变成满屏斑点（这正是上一轮实测到的现象）。
    """
    img = Image.new("RGB", (800, 800), WHITE)
    for y in range(200, 800):
        for x in range(200, 800):
            img.putpixel((x, y), ORANGE)
    for y in range(600, 800):
        for x in range(250, 500):
            img.putpixel((x, y), LIGHT_ORANGE)
    canvas = {"default": [{"type": "img", "props": {
        "src": _uri(img), "style": {"width": "240px", "height": "240px"}}}]}
    b = render_canvas_to_bitmap(canvas)
    assert {IDX_RED, IDX_YELLOW} <= _indices(b), f"应保持红/黄两档，实际 {_indices(b)}"
    assert _speckle_ratio(b) < 0.05, f"斑点率 {_speckle_ratio(b):.1%} 说明它还是走了抖动"


# ── 照片：仍然抖动 ────────────────────────────────────────────────────
def test_a_gradient_still_dithers():
    uri = _uri(_gradient())
    assert render_canvas_to_bitmap(_canvas(uri)) == \
        render_canvas_to_bitmap(_canvas(uri, {"dither": True}))


# ── 显式 flag 仍然优先 ────────────────────────────────────────────────
def test_explicit_false_forces_the_plain_snap():
    """显式 dither:false 仍是"最近色吸附"（不加分层映射），与自动不同。"""
    uri = _uri(_whale_like())
    auto = render_canvas_to_bitmap(_canvas(uri))
    forced = render_canvas_to_bitmap(_canvas(uri, {"dither": False}))
    assert auto != forced, "自动分层映射与纯吸附应当不同（后者会并色）"
    # 背景本来就是白 ⇒ 断言"非白只剩一种"（纯吸附会并掉两种橙）
    assert _indices(forced) - {IDX_WHITE} == {IDX_YELLOW}, \
        f"纯吸附应把两种橙并成同一种颜色，实际 {_indices(forced)}"


def test_dither_must_still_be_a_boolean_when_present():
    with pytest.raises(RenderError):
        render_canvas_to_bitmap(_canvas(_uri(_whale_like()), {"dither": "auto"}))


def test_upload_style_canvas_carries_no_dither_key():
    """上传路径生成的 canvas 不带 dither ⇒ 它吃到自动量化（这正是 B 的收益）。"""
    from youn_server.page_upload import canvas_for_upload
    node = canvas_for_upload("0" * 32)["default"][0]["props"]["children"][0]
    assert "dither" not in node["props"]
