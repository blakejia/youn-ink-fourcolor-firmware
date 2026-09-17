"""四色墨水屏上文字必须是"干净的黑白"。

面板只有 4 色、没有灰阶 ⇒ 文字的抗锯齿灰边**不可能被表示**。旧路径把整张画布
（含文字的灰边）一起做 Floyd–Steinberg ⇒ 灰边被误差扩散成一粒粒黑点，
表现为笔画毛边、笔画内部被"打"出单像素空洞。

红样本（实测，改前）：
  '用量'   32px → 灰 465 · 洞 0
  'LLM 用量' 32px → 灰 763 · 洞 2
  '02:04'  18px → 灰 263 · 洞 1
  '剩余额度' 14px → 灰 495 · 洞 2
"""
from __future__ import annotations

import pytest

from youn_server.canvas.renderer import CanvasRenderer

PALETTE_RGB = {(0, 0, 0), (255, 255, 255), (255, 215, 0), (220, 30, 30)}

# 覆盖不同字号与字形（汉字 / 拉丁 / 数字 / 标点）——单一样本抓不到"打洞"
SAMPLES = [("用量", 32), ("LLM 用量", 32), ("LLM 用量", 24),
           ("02:04", 18), ("Kimi 5h", 20), ("剩余额度", 14)]


def _render(text: str, size: int):
    canvas = {"default": [{"type": "div", "props": {
        "tw": "w-[390px] h-[80px] flex items-center justify-center",
        "children": [{"type": "span", "props": {
            "tw": f"text-[{size}px]", "children": [text]}}]}}]}
    r = CanvasRenderer(collect_bounds=True)
    bmp = r.render(canvas, True)          # 真实发布路径（含末尾整体抖动）
    return r.painter.surf.convert("RGB"), bmp, r.bounds[-1]


def _idx(bmp: bytes, x: int, y: int) -> int:
    return (bmp[y * 100 + x // 4] >> ((3 - (x % 4)) * 2)) & 3


@pytest.mark.parametrize("text,size", SAMPLES)
def test_text_pixels_are_pure_black_or_white_before_quantisation(text, size):
    """量化前，文字盒内不得出现任何中间灰 —— 灰边正是毛边的来源。"""
    surf, _bmp, box = _render(text, size)
    bad = [(px, py) for py in range(box["y"], box["y"] + box["h"])
           for px in range(box["x"], box["x"] + box["w"])
           if surf.getpixel((px, py)) not in PALETTE_RGB]
    assert not bad, f"{text!r}@{size}px 有 {len(bad)} 个抗锯齿灰像素，例：{bad[:4]}"


@pytest.mark.parametrize("text,size", SAMPLES)
def test_text_strokes_have_no_single_pixel_holes(text, size):
    """笔画内不得有被黑像素四面包围的白洞（误差扩散在笔画里打洞的直接证据）。"""
    _surf, bmp, box = _render(text, size)
    holes = []
    for py in range(box["y"] + 1, box["y"] + box["h"] - 1):
        for px in range(box["x"] + 1, box["x"] + box["w"] - 1):
            if _idx(bmp, px, py) == 1 and all(
                    _idx(bmp, px + dx, py + dy) == 0
                    for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1))):
                holes.append((px, py))
    assert not holes, f"{text!r}@{size}px 笔画内有 {len(holes)} 个洞，例：{holes[:6]}"


def test_text_still_has_the_right_amount_of_ink():
    """字形不能因为改渲染方式而崩掉 —— 防"把文字弄没了也过前两条"。"""
    _surf, bmp, box = _render("LLM 用量", 32)
    ink = sum(1 for py in range(box["y"], box["y"] + box["h"])
              for px in range(box["x"], box["x"] + box["w"])
              if _idx(bmp, px, py) == 0)
    assert 400 <= ink <= 2600, f"'LLM 用量'@32px 墨迹 {ink} 像素，超出合理区间 400–2600"
