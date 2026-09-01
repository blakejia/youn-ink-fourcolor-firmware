"""Image conversion for the 400x300 EPD.

Two output formats, both directly consumed by the device firmware:

  1bpp        15000 bytes   (400 * 300 / 8)
               Bit layout: pixel 0 = bit 7 of byte 0; 1 = white, 0 = black.
               Matches docs/inkscreen_image_converter.js.

  2bpp BWRY   30000 bytes   (400 * 300 * 2 / 8)
               Per-pixel 2 bits packed MSB-first in each byte:
                 00 = BLACK, 01 = WHITE, 10 = YELLOW, 11 = RED
               Matches firmware/main/rawdraw/rawdraw.h::Color and
               boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc.

Both use Floyd-Steinberg dithering — without it, gradients look terrible on EPD.

The input is anything Pillow can decode (JPEG, PNG, WEBP, BMP). It is fit-padded
to 400x300 on a white background, dithered to the target palette, and packed
into the raw buffer.

This is a port of docs/inkscreen_image_converter.js — that file is Node+sharp,
this is Pillow. Keeping both around gives the operator a choice of CLI tool.
"""
from __future__ import annotations

import io
from pathlib import Path
from typing import Literal

from PIL import Image

SCREEN_W = 400
SCREEN_H = 300

# 2bpp palette indices
IDX_BLACK = 0b00
IDX_WHITE = 0b01
IDX_YELLOW = 0b10
IDX_RED = 0b11

# sRGB values approximating the on-screen look
PALETTE_BWRY: list[tuple[int, int, int]] = [
    (0, 0, 0),         # BLACK
    (255, 255, 255),   # WHITE
    (255, 215, 0),     # YELLOW
    (220, 30, 30),     # RED
]


def _fit_white_bg(src: Image.Image) -> Image.Image:
    """Center-crop or letterbox onto a 400x300 white canvas."""
    if src.mode != "RGB":
        src = src.convert("RGB")
    src = src.copy()
    src.thumbnail((SCREEN_W, SCREEN_H), Image.Resampling.LANCZOS)

    canvas = Image.new("RGB", (SCREEN_W, SCREEN_H), PALETTE_BWRY[IDX_WHITE])
    x = (SCREEN_W - src.width) // 2
    y = (SCREEN_H - src.height) // 2
    canvas.paste(src, (x, y))
    return canvas


def _nearest_palette_index_for(
    palette: list[tuple[int, int, int]], rgb: tuple[int, int, int]
) -> int:
    """Euclidean nearest-neighbor over the given palette (not just BWRY)."""
    r, g, b = rgb
    best = 0
    best_d = 1 << 30
    for i, (pr, pg, pb) in enumerate(palette):
        d = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2
        if d < best_d:
            best_d = d
            best = i
    return best


def _floyd_steinberg_palette(
    img: Image.Image, palette: list[tuple[int, int, int]]
) -> Image.Image:
    """Quantize `img` to `palette` with Floyd-Steinberg dithering.

    Operates in float space so error propagation stays accurate. Returns a
    palettized Image where pixel values are indices into `palette`.
    """
    src = img.convert("RGB")
    px = src.load()
    w, h = src.size
    # work buffer: floats per channel
    r = [[float(px[x, y][0]) for x in range(w)] for y in range(h)]
    g = [[float(px[x, y][1]) for x in range(w)] for y in range(h)]
    b = [[float(px[x, y][2]) for x in range(w)] for y in range(h)]

    out = Image.new("P", (w, h))
    out_palette: list[int] = []
    for pr, pg, pb in palette:
        out_palette.extend([pr, pg, pb])
    # Pad to 256 entries (Pillow requires)
    while len(out_palette) < 768:
        out_palette.extend([0, 0, 0])
    out.putpalette(out_palette)

    out_px = out.load()

    for y in range(h):
        for x in range(w):
            old = (r[y][x], g[y][x], b[y][x])
            idx = _nearest_palette_index_for(palette, (int(old[0]), int(old[1]), int(old[2])))
            new = palette[idx]
            out_px[x, y] = idx
            er = old[0] - new[0]
            eg = old[1] - new[1]
            eb = old[2] - new[2]
            # distribute 7/16, 3/16, 5/16, 1/16 within bounds
            if x + 1 < w:
                r[y][x + 1] += er * 7 / 16
                g[y][x + 1] += eg * 7 / 16
                b[y][x + 1] += eb * 7 / 16
            if y + 1 < h:
                if x - 1 >= 0:
                    r[y + 1][x - 1] += er * 3 / 16
                    g[y + 1][x - 1] += eg * 3 / 16
                    b[y + 1][x - 1] += eb * 3 / 16
                r[y + 1][x] += er * 5 / 16
                g[y + 1][x] += eg * 5 / 16
                b[y + 1][x] += eb * 5 / 16
                if x + 1 < w:
                    r[y + 1][x + 1] += er * 1 / 16
                    g[y + 1][x + 1] += eg * 1 / 16
                    b[y + 1][x + 1] += eb * 1 / 16

    return out


def pack_1bpp(img: Image.Image) -> bytes:
    """Pack a 400x300 1bpp palettized image into 15000 bytes."""
    if img.size != (SCREEN_W, SCREEN_H):
        raise ValueError(f"expected {SCREEN_W}x{SCREEN_H}, got {img.size}")
    px = img.load()
    out = bytearray(SCREEN_W * SCREEN_H // 8)
    for y in range(SCREEN_H):
        row_start = y * (SCREEN_W // 8)
        for x in range(SCREEN_W):
            idx = px[x, y]
            # WHITE palette index = 1 → 1 bit; BLACK = 0 → 0 bit.
            bit = 1 if idx == IDX_WHITE else 0
            byte_idx = row_start + (x // 8)
            bit_idx = 7 - (x % 8)
            if bit:
                out[byte_idx] |= 1 << bit_idx
    return bytes(out)


def pack_2bpp(img: Image.Image) -> bytes:
    """Pack a 400x300 2bpp palettized image into 30000 bytes."""
    if img.size != (SCREEN_W, SCREEN_H):
        raise ValueError(f"expected {SCREEN_W}x{SCREEN_H}, got {img.size}")
    px = img.load()
    out = bytearray(SCREEN_W * SCREEN_H * 2 // 8)
    for y in range(SCREEN_H):
        row_start = y * (SCREEN_W // 4)
        for x in range(SCREEN_W):
            idx = px[x, y] & 0x3
            byte_idx = row_start + (x // 4)
            bit_off = (3 - (x % 4)) * 2
            out[byte_idx] |= idx << bit_off
    return bytes(out)


def convert_to_format(src: Image.Image, fmt: Literal["1bpp", "bwry2bpp"]) -> bytes:
    """End-to-end: load image → fit → dither → pack raw bytes."""
    fitted = _fit_white_bg(src)
    if fmt == "1bpp":
        # dither to black/white only
        pal_bw = [PALETTE_BWRY[IDX_BLACK], PALETTE_BWRY[IDX_WHITE]]
        dithered = _floyd_steinberg_palette(fitted, pal_bw)
        return pack_1bpp(dithered)
    elif fmt == "bwry2bpp":
        dithered = _floyd_steinberg_palette(fitted, PALETTE_BWRY)
        return pack_2bpp(dithered)
    else:
        raise ValueError(f"unsupported format: {fmt}")


def convert_file(src_path: Path, fmt: Literal["1bpp", "bwry2bpp"]) -> bytes:
    with Image.open(src_path) as im:
        return convert_to_format(im, fmt)


def convert_bytes(data: bytes, fmt: Literal["1bpp", "bwry2bpp"]) -> bytes:
    with Image.open(io.BytesIO(data)) as im:
        return convert_to_format(im, fmt)
