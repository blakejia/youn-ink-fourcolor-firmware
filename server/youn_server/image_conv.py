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


#: 最大-最小通道差在此之内视为中性色（灰）
_NEUTRAL_TOL = 24


def _nearest_palette_index_for(
    palette: list[tuple[int, int, int]], rgb: tuple[int, int, int]
) -> int:
    """最近色。**中性色只允许落黑白**，其余走欧氏距离。

    为什么必须单独处理中性色：纯欧氏距离在感知上是错的 —— 灰 128 到红
    (220,30,30) 的平方距离是 27672，反而**小于**到白 (255,255,255) 的 48387。
    于是文字的灰度反锯齿边被吸成红色（实测通知屏 448 个红像素），照片的灰部
    也会整体偏红。中性色的正确表示只有黑白：掺进任何饱和色都是凭空加色相。
    """
    r, g, b = rgb
    if max(rgb) - min(rgb) <= _NEUTRAL_TOL:
        # 候选只能是**本身就中性**的调色板色（黑/白）。若在全部色里按亮度挑，
        # 红的亮度 87 恰好接近中灰 ⇒ 灰 64 又会被吸成红（实测踩过这个坑）。
        achromatic = [i for i, c in enumerate(palette)
                      if max(c) - min(c) <= _NEUTRAL_TOL]
        cand = achromatic or list(range(len(palette)))
        lum = 0.299 * r + 0.587 * g + 0.114 * b
        best, best_d = cand[0], 1 << 30
        for i in cand:
            c = palette[i]
            cl = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
            d = abs(cl - lum)
            if d < best_d:
                best_d, best = d, i
        return best
    best = 0
    best_d = 1 << 30
    for i, (pr, pg, pb) in enumerate(palette):
        d = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2
        if d < best_d:
            best_d = d
            best = i
    return best


# ─── 平色图形的自动分层映射 ───────────────────────────────────────────
_FLAT_TONES = 4           # 视为"平色"时允许的主色档数（映射到调色板的档数）
_FLAT_MIN_COVERAGE = 0.95  # 前 _FLAT_TONES 档主色的覆盖率下限
_TONE_MIN_SHARE = 0.02    # 仅有占比 >=2% 者才算一档主色（滤掉过渡色）
_TONE_TOL = 12            # 每通道容差内的颜色视为同一档


def _luminance(c: tuple[int, int, int]) -> float:
    return 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]


def _dominant_tones(img: Image.Image, tol: int = _TONE_TOL,
                    min_share: float = 0.0
                    ) -> list[tuple[tuple[int, int, int], float]]:
    """图片自己的若干"档"颜色及其占比，按占比降序（相近色合并）。

    Returns the FULL list (not truncated): the caller needs both "how many tones"
    and "how much of the image they cover" to tell flat art from a photo.
    """
    px = img.load()
    total = img.width * img.height
    buckets: dict[tuple[int, int, int], list[int]] = {}
    for y in range(img.height):
        for x in range(img.width):
            r, g, b = px[x, y]
            k = (r >> 3, g >> 3, b >> 3)
            e = buckets.get(k)
            if e is None:
                buckets[k] = [1, r, g, b]
            else:
                e[0] += 1
                e[1] += r
                e[2] += g
                e[3] += b
    cand = sorted(((v[0], (v[1] // v[0], v[2] // v[0], v[3] // v[0]))
                   for v in buckets.values()), key=lambda t: -t[0])
    merged: list[list] = []          # [[rgb, count], ...]
    for cnt, rgb in cand:
        for m in merged:
            if all(abs(a - b) <= tol for a, b in zip(m[0], rgb)):
                m[1] += cnt
                break
        else:
            merged.append([rgb, cnt])
    merged.sort(key=lambda m: -m[1])
    out = [(rgb, cnt / total) for rgb, cnt in merged]
    # min_share 滤掉抗锯齿的过渡色：它们各占万分之一不到，却能把主色表从 3 档
    # 挤到 4 档，进而让亮度单调分配整条偏一格（实测：鲸鱼身体被映射成了黑色）。
    if min_share > 0:
        out = [t for t in out if t[1] >= min_share]
    return out


def _monotone_level_map(
    tones: list[tuple[int, int, int]], palette: list[tuple[int, int, int]]
) -> dict[tuple[int, int, int], int]:
    """把若干档颜色按亮度**单调**分配给调色板里递增的若干色，总亮度差最小。

    单调是重点：它保证暗的仍暗、亮的仍亮（不会把近似的两档并成一个），
    并让最亮的一档自然落到白、最暗的一档落到黑 —— 黑线稿因此用得上黑色。
    """
    from itertools import combinations

    order = sorted(range(len(palette)), key=lambda i: _luminance(palette[i]))
    cs = sorted(tones, key=_luminance)
    n, m = len(cs), len(order)
    best: tuple[int, ...] | None = None
    best_cost: float | None = None
    for combo in combinations(range(m), n):          # m<=4, n<=4 ⇒ 代价可忽略
        cost = sum(abs(_luminance(cs[i]) - _luminance(palette[order[j]]))
                   for i, j in enumerate(combo))
        if best_cost is None or cost < best_cost:
            best, best_cost = combo, cost
    assert best is not None
    return {cs[i]: order[j] for i, j in enumerate(best)}


def _apply_lut(img: Image.Image, lut: dict[tuple[int, int, int], int],
               palette: list[tuple[int, int, int]]) -> Image.Image:
    """按 LUT 量化成索引图；未命中的像素用最近色兜底。"""
    src = img.convert("RGB")
    px = src.load()
    w, h = src.size
    out = Image.new("P", (w, h))
    pal: list[int] = []
    for pr, pg, pb in palette:
        pal.extend([pr, pg, pb])
    while len(pal) < 768:
        pal.extend([0, 0, 0])
    out.putpalette(pal)
    opx = out.load()
    memo: dict[tuple[int, int, int], int] = {}
    for y in range(h):
        for x in range(w):
            rgb = px[x, y]
            idx = lut.get(rgb)
            if idx is None:
                idx = memo.get(rgb)
                if idx is None:
                    idx = _nearest_palette_index_for(palette, rgb)
                    memo[rgb] = idx
            opx[x, y] = idx
    return out


def classify_flat(img: Image.Image, palette: list[tuple[int, int, int]]
                  ) -> dict[tuple[int, int, int], int] | None:
    """看起来是"平色图形"就返回 色→索引 的 LUT，否则 None（按照片处理）。

    ⚠️ 必须在**原始分辨率**上调用：一旦先做 LANCZOS 缩放，抗锯齿会造出大量
    中间色，平色图看起来就像照片（实测：800x800 的橙 logo 先缩到 240x240 再判断
    ⇒ 被判成照片 ⇒ 仍然满屏斑点）。
    """
    tones = _dominant_tones(img, min_share=_TONE_MIN_SHARE)
    coverage = sum(share for rgb, share in tones[:_FLAT_TONES])
    # 只看覆盖率，**不**看"总共有多少档"：抗锯齿的边缘会分出成百上千个占比
    # 万分之一的碎档（实测鲸鱼 logo：3 档主色覆盖 99.5%，却另有几百个碎档）。
    # 照片的 top-4 覆盖率会远低于阈值，照样会被送去做抖动。
    if coverage >= _FLAT_MIN_COVERAGE:
        return _monotone_level_map([rgb for rgb, _ in tones[:_FLAT_TONES]], palette)
    return None


def auto_quantize(img: Image.Image, palette: list[tuple[int, int, int]]
                  ) -> tuple[Image.Image, str]:
    """自动选择量化方式：平色图形 → 亮度单调分层映射；其余 → Floyd–Steinberg。

    为什么不是"吸附二选一"：对已贴调的平色图，抖动与吸附逐字节相同（零误差）；
    对离盘远的平色图，吸附会把**相近色调并成一个**（鲸鱼 logo 的嘴就是这样消失的）。
    分层映射既保住色调区分、又不产生斑点，且把黑色留给暗部（线稿）。
    """
    tones = _dominant_tones(img, min_share=_TONE_MIN_SHARE)
    coverage = sum(share for rgb, share in tones[:_FLAT_TONES])
    lut = _monotone_level_map([rgb for rgb, _ in tones[:_FLAT_TONES]], palette) \
        if coverage >= _FLAT_MIN_COVERAGE else None
    if lut is not None:
        used = ",".join(sorted({str(palette[i]) for i in lut.values()}))
        return _apply_lut(img, lut, palette), \
            f"levels{len(lut)}(cover={coverage:.3f})->[{used}]"
    return _floyd_steinberg_palette(img, palette), \
        f"dither(auto, cover={coverage:.3f}, tones={len(tones)})"


def _snap_to_palette(
    img: Image.Image, palette: list[tuple[int, int, int]]
) -> Image.Image:
    """Quantize to `palette` by nearest color, with **no** error diffusion.

    Same shape as ``_floyd_steinberg_palette``: a P image whose pixel values are
    indices into `palette`. 给"本来就是平色/线稿"的图形用 —— 抖动会在整片色块
    内部留下交错的斑点（对 logo 就是噪点），吸附没有误差传播，色块内部是纯色。
    """
    src = img.convert("RGB")
    px = src.load()
    w, h = src.size
    out = Image.new("P", (w, h))
    out_palette: list[int] = []
    for pr, pg, pb in palette:
        out_palette.extend([pr, pg, pb])
    while len(out_palette) < 768:
        out_palette.extend([0, 0, 0])
    out.putpalette(out_palette)
    opx = out.load()
    memo: dict[tuple[int, int, int], int] = {}
    for y in range(h):
        for x in range(w):
            rgb = px[x, y]
            idx = memo.get(rgb)
            if idx is None:
                idx = _nearest_palette_index_for(palette, rgb)
                memo[rgb] = idx
            opx[x, y] = idx
    return out


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
