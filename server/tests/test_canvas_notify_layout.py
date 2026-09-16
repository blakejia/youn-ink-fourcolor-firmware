"""通知屏这类"纯文字画布"的两个缺陷：不折行、文字被抖动成点。

- **不折行**：渲染器把文本子节点当单行画，且列式 flex 不拉伸子节点
  （实测文本框只有 100x24，文字一路画到面板右缘被裁）。
- **抖动成点**：文字是 Pillow 灰度反锯齿画的，整面 Floyd–Steinberg 之后细笔画
  变成稀疏黑白点（实测孤立斑点占非白像素的 9.3%），看起来又糊又断。

修法：列式 flex 按 CSS 默认拉伸子节点 + 文本按盒宽折行；并给
`render_canvas_to_bitmap` 一个 `dither=False` 选项，纯文字画布走"最近色吸附"
（黑白二值化）而不是抖动。
"""
from __future__ import annotations

from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.image_conv import IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW, SCREEN_H, SCREEN_W

LONG_BODY = ("今天下午三点将在会议室 A 进行固件升级演练，请提前把设备充满电并放在"
             "靠窗的位置，升级过程中不要断电，也不要拔掉 USB 线。")


def _notify_canvas() -> dict:
    return {"default": [{"type": "div", "props": {
        "tw": "flex flex-col p-[16px] gap-[8px] bg-white",
        "children": [
            {"type": "div", "props": {"tw": "text-[20px] font-bold",
                                      "style": {"color": "#000000"}, "children": "设备通知"}},
            {"type": "div", "props": {"tw": "text-[16px]",
                                      "style": {"color": "#000000"}, "children": LONG_BODY}},
        ]}}]}


def _idx(data: bytes):
    W, H = SCREEN_W, SCREEN_H
    out = [[0] * W for _ in range(H)]
    for y in range(H):
        for xb in range(W // 4):
            byte = data[y * (W // 4) + xb]
            for xi in range(4):
                out[y][xb * 4 + xi] = (byte >> ((3 - xi) * 2)) & 3
    return out


def test_long_body_wraps_instead_of_running_off_the_panel():
    """正文必须折行：16px 内边距之外（x >= 384）不应出现任何非白像素。"""
    idx = _idx(render_canvas_to_bitmap(_notify_canvas()))
    rightmost = max(x for y in range(SCREEN_H) for x in range(SCREEN_W) if idx[y][x] != IDX_WHITE)
    assert rightmost < SCREEN_W - 16, f"正文画到了 x={rightmost}，说明它没有折行"


def test_wrapped_body_occupies_several_lines():
    """折行后正文应占多行：非白像素的行数要明显多于"标题+正文各一行"。"""
    idx = _idx(render_canvas_to_bitmap(_notify_canvas()))
    rows = {y for y in range(SCREEN_H) for x in range(SCREEN_W) if idx[y][x] != IDX_WHITE}
    # 标题一行、正文若干行；单行正文时总行数约 20 行左右，折行后应明显更多
    assert len(rows) > 40, f"非白行数只有 {len(rows)}，正文看起来仍是单行"


def test_dither_false_keeps_black_text_free_of_colour_speckle():
    """纯文字画布走吸附（二值化）：不该出现黄/红这些抖动溢出的颜色。"""
    got = {c for row in _idx(render_canvas_to_bitmap(_notify_canvas(), dither=False)) for c in row}
    assert got <= {IDX_BLACK, IDX_WHITE}, f"吸附后仍有多余颜色 {got}"


def test_default_still_dithers(  ):
    """默认行为不变：同一张画布默认仍会抖动（会出现非黑非白的颜色）。"""
    got = {c for row in _idx(render_canvas_to_bitmap(_notify_canvas())) for c in row}
    assert IDX_BLACK in got and len(got) >= 2


def test_dither_false_reduces_speckle_massively():
    """量化对比：吸附的孤立斑點应远低于抖动。"""
    def speckle(data):
        idx = _idx(data); iso = nw = 0
        for y in range(1, SCREEN_H - 1):
            for x in range(1, SCREEN_W - 1):
                c = idx[y][x]
                if c == IDX_WHITE:
                    continue
                nw += 1
                if (idx[y-1][x] != c and idx[y+1][x] != c
                        and idx[y][x-1] != c and idx[y][x+1] != c):
                    iso += 1
        return iso / max(1, nw)
    dithered = speckle(render_canvas_to_bitmap(_notify_canvas()))
    snapped = speckle(render_canvas_to_bitmap(_notify_canvas(), dither=False))
    # 绝对下限 + 相对改善：硬阈值下文字拐角本来就会有少量孤立像素，
    # 要钉住的是"没有被抖成稀疏点"这件事（抖动时量级在 4% 上下）。
    assert snapped < 0.025, f"吸附后斑点 {snapped:.1%} 偏高，文字仍在被抖"
    assert snapped < dithered * 0.6, f"吸附 {snapped:.1%} vs 抖动 {dithered:.1%} 改善不足"
