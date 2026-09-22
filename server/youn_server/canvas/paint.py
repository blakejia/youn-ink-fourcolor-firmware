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


    def clip_corners(self, box: Box, radii: tuple[int, int, int, int]) -> None:
        """每角独立圆角（tl, tr, br, bl）——Pillow 的 rounded_rectangle 只吃
        单一半径，所以四角分开按扇形遮罩一遍。半径为 0 的角保持直角。
        """
        tl, tr, br, bl = (max(0, int(r)) for r in radii)
        if not any((tl, tr, br, bl)):
            return
        mask = Image.new("L", self.surf.size, 0)
        md = ImageDraw.Draw(mask)
        x0, y0 = box.x, box.y
        x1, y1 = box.x + box.w - 1, box.y + box.h - 1
        if box.w <= 0 or box.h <= 0:
            return
        for (cx, cy, r, start, end) in (
            (x0 + tl, y0 + tl, tl, 180, 270),
            (x1 - tr, y0 + tr, tr, 270, 360),
            (x1 - br, y1 - br, br, 0, 90),
            (x0 + bl, y1 - bl, bl, 90, 180),
        ):
            if r > 0:
                md.pieslice([cx - r, cy - r, cx + r, cy + r],
                            start=start, end=end, fill=255)
        # 十字实心区：把四角扇形之外的主体补满
        md.rectangle([x0 + tl, y0, x1 - tr, y1], fill=255)
        md.rectangle([x0, y0 + tl, x1, y1 - bl], fill=255)
        white = Image.new("RGB", self.surf.size, PALETTE_RGB[1])
        self.surf = Image.composite(self.surf, white, mask)

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
             ellipsis: bool = False, line_height=None, letter_spacing: int = 0,
             underline: bool = False, strike: bool = False) -> None:
        font = load_font(size, bold)
        _w, _h, lines = fit_box(text, font, box.w, size, nowrap=nowrap,
                                max_lines=max_lines, ellipsis=ellipsis,
                                line_height=line_height,
                                letter_spacing=letter_spacing)
        line_h = max(1, _h // max(1, len(lines)))
        draw = ImageDraw.Draw(self.surf)
        # 面板只有 4 色、没有灰阶 ⇒ 抗锯齿灰边不可能被表示：它只会在末尾的
        # 整体 Floyd–Steinberg 里被扩散成一粒粒，表现为笔画毛边与笔画内空洞
        # （实测 'LLM 用量'@32px：763 个灰像素 / 2 个洞）。改走 FreeType 的
        # 单色（1-bit + hinting）渲染 ⇒ 量化前就是纯黑白，抖无可抖。
        draw.fontmode = "1"
        for i, ln in enumerate(lines):
            lw = width_of(ln, font, letter_spacing)
            dx = 0
            if align != "left":
                dx = max(0, (box.w - lw) // 2 if align == "center" else box.w - lw)
            y = box.y + i * line_h
            if not letter_spacing:
                draw.text((box.x + dx, y), ln, fill=PALETTE_RGB[color_idx], font=font)
            else:
                cx = box.x + dx                 # Pillow 没有字距，只能逐字画
                for ch in ln:
                    draw.text((cx, y), ch, fill=PALETTE_RGB[color_idx], font=font)
                    cx += width_of(ch, font) + letter_spacing
            if underline or strike:
                # Pillow 没有 text-decoration，自己画线；用同一套单色画家，
                # 线在量化前就是纯黑（半调的细线会被抖动打散）。
                # 纵向锚点取自行盒而非 size 比例：Noto CJK@16px 行盒高 24px，
                # 字形底缘之下还有下延部，按 size 比例画会穿进字形（实测
                # y+16 落在笔画中间）。行盒底部 y+line_h 才是第一个安全行。
                x0, x1 = box.x + dx, box.x + dx + lw - 1
                if x1 > x0:
                    if strike:
                        sy = y + int(size * 0.55)
                        draw.line([(x0, sy), (x1, sy)],
                                  fill=PALETTE_RGB[color_idx], width=1)
                    if underline:
                        uy = min(box.y + box.h - 1, y + line_h - 1)
                        draw.line([(x0, uy), (x1, uy)],
                                  fill=PALETTE_RGB[color_idx], width=1)
