"""Pillow 绘制：底色、描边、圆角/裁剪遮罩、文字。"""
from __future__ import annotations

from PIL import Image, ImageDraw

from .images import PALETTE_RGB
from .layout import Box
from .text import fit_box, load_font, width_of


class Painter:
    def __init__(self, size: tuple[int, int]) -> None:
        self.surf = Image.new("RGB", size, PALETTE_RGB[1])

    def fill(self, box: Box, color_idx: int) -> None:
        if box.w <= 0 or box.h <= 0:
            return
        ImageDraw.Draw(self.surf).rectangle(
            [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1],
            fill=PALETTE_RGB[color_idx])

    def stroke(self, box: Box, color_idx: int, width: int = 1) -> None:
        if box.w <= 0 or box.h <= 0:
            return
        d = ImageDraw.Draw(self.surf)
        for i in range(max(1, width)):
            d.rectangle([box.x + i, box.y + i, box.x + box.w - 1 - i,
                         box.y + box.h - 1 - i],
                        outline=PALETTE_RGB[color_idx])

    def clip_round(self, box: Box, radius: int) -> None:
        mask = Image.new("L", self.surf.size, 0)
        ImageDraw.Draw(mask).rounded_rectangle(
            [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1],
            radius=radius, fill=255)
        white = Image.new("RGB", self.surf.size, PALETTE_RGB[1])
        self.surf = Image.composite(self.surf, white, mask)

    def clip_box(self, box: Box, before: Image.Image) -> None:
        """overflow:hidden —— 用快照做遮罩，父容器背景不会被抹掉。"""
        mask = Image.new("L", self.surf.size, 0)
        ImageDraw.Draw(mask).rectangle(
            [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1], fill=255)
        self.surf = Image.composite(self.surf, before, mask)

    def paste(self, img: Image.Image, box: Box) -> None:
        if box.w <= 0 or box.h <= 0:
            return
        if img.size != (box.w, box.h):
            img = img.resize((box.w, box.h), Image.NEAREST)
        self.surf.paste(img, (box.x, box.y))

    def text(self, text: str, size: int, bold: bool, align: str, box: Box,
             color_idx: int, nowrap: bool = False, max_lines=None,
             ellipsis: bool = False, line_height=None, letter_spacing: int = 0) -> None:
        font = load_font(size, bold)
        _w, _h, lines = fit_box(text, font, box.w, size, nowrap=nowrap,
                                max_lines=max_lines, ellipsis=ellipsis,
                                line_height=line_height,
                                letter_spacing=letter_spacing)
        line_h = max(1, _h // max(1, len(lines)))
        draw = ImageDraw.Draw(self.surf)
        for i, ln in enumerate(lines):
            lw = width_of(ln, font, letter_spacing)
            dx = 0
            if align != "left":
                dx = max(0, (box.w - lw) // 2 if align == "center" else box.w - lw)
            y = box.y + i * line_h
            if not letter_spacing:
                draw.text((box.x + dx, y), ln, fill=PALETTE_RGB[color_idx], font=font)
                continue
            cx = box.x + dx                     # Pillow 没有字距，只能逐字画
            for ch in ln:
                draw.text((cx, y), ch, fill=PALETTE_RGB[color_idx], font=font)
                cx += width_of(ch, font) + letter_spacing
