"""Canvas JSON tree -> 400x300 2bpp BWRY bitmap renderer.

Supports a subset of MindReset Canvas API (windowData + props.tw + props.style).
Unsupported features raise RenderError with a JSON-pointer-like path so the
client can localize the mistake.

Element tree shape:
    {
      "default": [
        {
          "type": "div",
          "props": {
            "tw": "flex flex-col gap-[8px] p-[12px] bg-black",
            "style": {"color": "#FFFFFF", "borderRadius": "8px", "border": "1px solid black"},
            "children": [ ... ]
          }
        }
      ]
    }

Supported:
    - types: div, span, img
    - props.tw tokens: flex, flex-row, flex-col, gap-[Npx], p-[Npx], px-[Npx],
      py-[Npx], m-[Npx], w-[Npx], h-[Npx], items-*, justify-*, bg-{white|black|red|yellow},
      border, border-{white|black|red|yellow}, rounded-[Npx], overflow-hidden,
      text-[Npx]
    - props.style: backgroundColor, color, borderRadius, padding, paddingX/Y,
      margin, marginX/Y, width, height, border, overflow, fontWeight
    - text content via props.children string
    - image src: data:image/...;base64,... or http(s) URL (must be anonymously
      fetchable)

Output: exactly 30000 bytes (400 px * 300 px * 2 bpp / 8).
"""
from __future__ import annotations

import base64
import hashlib
import io
import logging
import re
from typing import Any, Optional

import httpx
from PIL import Image, ImageDraw, ImageFont

from .image_conv import (
    PALETTE_BWRY, IDX_BLACK, IDX_WHITE, IDX_YELLOW, IDX_RED,
    SCREEN_W, SCREEN_H, pack_2bpp, _floyd_steinberg_palette,
)

log = logging.getLogger(__name__)

# 2bpp palette indices -> sRGB for Pillow drawing.
_PILLOW_BG = {
    IDX_BLACK:  (0, 0, 0),
    IDX_WHITE:  (255, 255, 255),
    IDX_YELLOW: (255, 215, 0),
    IDX_RED:    (220, 30, 30),
}
_COLOR_NAME_TO_IDX = {
    "black": IDX_BLACK, "white": IDX_WHITE,
    "yellow": IDX_YELLOW, "red": IDX_RED,
}

# Bundled font (repo has SourceHanSansSC-Normal.otf via lvgl).
_DEFAULT_FONT_CANDIDATES = (
    "/mnt/data/project/youn-ink-fourcolor-firmware/firmware/managed_components/lvgl__lvgl/scripts/built_in_font/SourceHanSansSC-Normal.otf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
)


class RenderError(Exception):
    """Raised when an element/property is unsupported or malformed.

    `path` is a dotted JSON-pointer so the client can localize the failure
    (e.g. "windowData.default[0].props.children[2].type").
    """

    def __init__(self, path: str, message: str) -> None:
        super().__init__(f"{path}: {message}")
        self.path = path
        self.message = message


# ─── token / style parsing ───────────────────────────────────────────
_TOKEN_RE = re.compile(r"\[(-?\d+)px\]")


def _parse_px(token: str) -> Optional[int]:
    if token is None:
        return None
    m = _TOKEN_RE.search(token)
    return int(m.group(1)) if m else None


def _parse_padding(style: dict, tw: str) -> tuple[int, int, int, int]:
    """Resolve (top, right, bottom, left) in px."""
    pt = pr = pb = pl = 0
    # style overrides (explicit paddingX/paddingY or per-side)
    if "paddingTop" in style:
        pt = int(style["paddingTop"])
    if "paddingRight" in style:
        pr = int(style["paddingRight"])
    if "paddingBottom" in style:
        pb = int(style["paddingBottom"])
    if "paddingLeft" in style:
        pl = int(style["paddingLeft"])
    # tw tokens
    for tok in tw.split():
        if tok == "p":
            pt = pr = pb = pl = _parse_px(tok) or 0
        elif tok.startswith("p-["):
            pt = pr = pb = pl = _parse_px(tok) or 0
        elif tok.startswith("px-["):
            pl = pr = _parse_px(tok) or 0
        elif tok.startswith("py-["):
            pt = pb = _parse_px(tok) or 0
    return pt, pr, pb, pl


def _parse_margin(style: dict, tw: str) -> tuple[int, int, int, int]:
    mt = mr = mb = ml = 0
    for tok in tw.split():
        if tok == "m":
            mt = mr = mb = ml = _parse_px(tok) or 0
        elif tok.startswith("mx-["):
            ml = mr = _parse_px(tok) or 0
        elif tok.startswith("my-["):
            mt = mb = _parse_px(tok) or 0
    return mt, mr, mb, ml


def _direction(tw: str) -> str:
    if "flex-row" in tw:
        return "row"
    if "flex-col" in tw:
        return "column"
    return "row"


def _align_items(tw: str) -> str:
    for tok in tw.split():
        if tok.startswith("items-"):
            return tok[len("items-"):]
    return "stretch"


def _justify(tw: str) -> str:
    for tok in tw.split():
        if tok.startswith("justify-"):
            return tok[len("justify-"):]
    return "flex-start"


def _bg_color(tw: str, style: dict) -> Optional[int]:
    for tok in tw.split():
        if tok.startswith("bg-"):
            name = tok[len("bg-"):]
            if name in _COLOR_NAME_TO_IDX:
                return _COLOR_NAME_TO_IDX[name]
    bc = style.get("backgroundColor")
    if bc:
        return _bg_from_hex(bc)
    return None


def _border_color(tw: str, style: dict) -> Optional[int]:
    for tok in tw.split():
        if tok.startswith("border-") and tok != "border":
            name = tok[len("border-"):]
            if name in _COLOR_NAME_TO_IDX:
                return _COLOR_NAME_TO_IDX[name]
    return None


def _bg_from_hex(s: str) -> Optional[int]:
    s = s.strip()
    if not s.startswith("#"):
        return None
    h = s[1:]
    if len(h) not in (3, 6):
        return None
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    r = int(h[0:2], 16); g = int(h[2:4], 16); b = int(h[4:6], 16)
    best = min(_PILLOW_BG.items(), key=lambda kv: (kv[1][0]-r)**2 + (kv[1][1]-g)**2 + (kv[1][2]-b)**2)
    return best[0]


def _radius(tw: str, style: dict) -> int:
    for tok in tw.split():
        if tok.startswith("rounded-"):
            v = _parse_px(tok)
            if v:
                return v
    r = style.get("borderRadius")
    if r:
        try:
            return int(str(r).rstrip("px"))
        except ValueError:
            return 0
    return 0


def _has_border(tw: str, style: dict) -> bool:
    return "border" in tw.split() or bool(style.get("border"))


def _overflow_hidden(tw: str, style: dict) -> bool:
    return "overflow-hidden" in tw or style.get("overflow") == "hidden"


def _size_from_tw(tw: str, axis: str) -> Optional[int]:
    for tok in tw.split():
        if tok.startswith(f"{axis}-["):
            return _parse_px(tok)
    return None


def _resolve_font_size(tw: str, style: dict) -> int:
    for tok in tw.split():
        if tok.startswith("text-[") and tok.endswith("px]"):
            return _parse_px(tok) or 16
        if tok.startswith("text-") and "[" in tok:
            m = re.search(r"\[(\d+)px\]", tok)
            if m:
                return int(m.group(1))
    raw_size = style.get("fontSize")
    if raw_size:
        try:
            return int(str(raw_size).rstrip("px"))
        except ValueError:
            return 16
    return 16


def _load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in _DEFAULT_FONT_CANDIDATES:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


# ─── layout primitives ───────────────────────────────────────────────
class _Box:
    def __init__(self, x: int, y: int, w: int, h: int) -> None:
        self.x = x
        self.y = y
        self.w = w
        self.h = h


def _measure_text(text: str, font: ImageFont.FreeTypeFont) -> tuple[int, int]:
    bbox = font.getbbox(text)
    return bbox[2] - bbox[0], bbox[3] - bbox[1]


# ─── node tree resolution ───────────────────────────────────────────
def _resolve_children(node: dict, path: str) -> list:
    if "props" not in node:
        raise RenderError(path, "missing props")
    children = node["props"].get("children")
    if children is None:
        return []
    if isinstance(children, str):
        return [("text", children)]
    if isinstance(children, dict):
        return [("node", children)]
    if isinstance(children, list):
        out = []
        for i, c in enumerate(children):
            if isinstance(c, str):
                out.append(("text", c))
            elif isinstance(c, dict):
                out.append(("node", c))
            else:
                raise RenderError(f"{path}.children[{i}]",
                                  f"invalid child type {type(c).__name__}")
        return out
    raise RenderError(f"{path}.children", f"invalid type {type(children).__name__}")


# ─── render driver ───────────────────────────────────────────────────
class _CanvasRenderer:
    def __init__(self) -> None:
        self.surf = Image.new("RGB", (SCREEN_W, SCREEN_H), _PILLOW_BG[IDX_WHITE])
        self._img_url_cache: dict[str, Image.Image] = {}
        self._http = httpx.Client(timeout=10.0, follow_redirects=True)

    def close(self) -> None:
        self._http.close()

    def render(self, canvas_json: dict) -> bytes:
        if not isinstance(canvas_json, dict):
            raise RenderError("windowData", "must be an object")
        default = canvas_json.get("default")
        if not isinstance(default, list):
            raise RenderError("windowData.default", "must be a list")
        if not default:
            return pack_2bpp(_floyd_steinberg_palette(_blank_image(), PALETTE_BWRY))
        for i, root in enumerate(default):
            if not isinstance(root, dict):
                raise RenderError(f"windowData.default[{i}]", "must be an object")
            self._render_node(root, _Box(0, 0, SCREEN_W, SCREEN_H),
                              path=f"windowData.default[{i}]")
        raw = self._to_2bpp()
        self._http.close()
        return raw

    def _render_node(self, node: dict, box: _Box, *, path: str) -> None:
        ntype = node.get("type")
        if ntype not in ("div", "span", "img"):
            raise RenderError(f"{path}.type",
                              f"unsupported element type {ntype!r}")

        if ntype == "img":
            self._render_img(node, box, path)
            return

        props = node.get("props") or {}
        tw = str(props.get("tw", ""))
        style = props.get("style") or {}

        bg = _bg_color(tw, style)
        if bg is not None:
            self._fill_box(box, _PILLOW_BG[bg])

        radius = _radius(tw, style)
        if radius > 0:
            self._clip_round(box, radius)

        children = _resolve_children(node, path)

        if children and not any(c[0] == "text" for c in children):
            self._layout_children(children, tw, style, box, path)
        else:
            for kind, val in children:
                if kind == "text":
                    self._draw_text(val, tw, style, box, path)

        if _has_border(tw, style):
            color = _border_color(tw, style) or IDX_BLACK
            self._stroke_box(box, _PILLOW_BG[color], radius=radius)

    def _layout_children(self, children, tw, style, box, path):
        pt, pr, pb, pl = _parse_padding(style, tw)
        inner = _Box(box.x + pl, box.y + pt,
                     max(0, box.w - pl - pr), max(0, box.h - pt - pb))
        if inner.w <= 0 or inner.h <= 0:
            return
        direction = _direction(tw)
        gap = _parse_px(tw) or 0

        measurements = []
        for kind, val in children:
            if kind == "text":
                font_size = _resolve_font_size(tw, style)
                w, h = _measure_text(val, _load_font(font_size))
            elif kind == "node":
                w, h = self._measure_node(val, tw, path)
            else:
                w, h = 0, 0
            measurements.append(((kind, val), (w, h)))

        align = _align_items(tw)
        justify = _justify(tw)

        if direction == "row":
            sizes = self._row_layout(measurements, inner, gap, align, justify)
            x = inner.x
            for ((kind, val), (w, h)), (cx, cy, cw, ch) in zip(measurements, sizes):
                if kind == "text":
                    self._draw_text(val, tw, style, _Box(cx, cy, cw, ch), path)
                else:
                    self._render_node(val, _Box(cx, cy, cw, ch), path=path)
                x += cw
        else:
            sizes = self._column_layout(measurements, inner, gap, align, justify)
            y = inner.y
            for ((kind, val), (w, h)), (cx, cy, cw, ch) in zip(measurements, sizes):
                if kind == "text":
                    self._draw_text(val, tw, style, _Box(cx, cy, cw, ch), path)
                else:
                    self._render_node(val, _Box(cx, cy, cw, ch), path=path)
                y += ch

    def _row_layout(self, items, box, gap, align, justify):
        out = []
        total_w = sum(s[1][0] for s in items) + gap * max(0, len(items) - 1)
        x_cursor = _justify_offset(justify, box.x, box.w, total_w, gap, len(items))
        for (_, (w, h)) in items:
            out.append((x_cursor, box.y + _align_offset(align, box.h, h), w, h))
            x_cursor += w + gap
        return out

    def _column_layout(self, items, box, gap, align, justify):
        out = []
        total_h = sum(s[1][1] for s in items) + gap * max(0, len(items) - 1)
        y_cursor = _justify_offset(justify, box.y, box.h, total_h, gap, len(items))
        for (_, (w, h)) in items:
            out.append((box.x + _align_offset(align, box.w, w), y_cursor, w, h))
            y_cursor += h + gap
        return out

    def _measure_node(self, node, tw, path):
        w = _size_from_tw(tw, "w")
        h = _size_from_tw(tw, "h")
        if w and h:
            return w, h
        return (w or 100, h or 24)

    def _fill_box(self, box: _Box, color) -> None:
        draw = ImageDraw.Draw(self.surf)
        draw.rectangle([box.x, box.y, box.x + box.w - 1, box.y + box.h - 1], fill=color)

    def _stroke_box(self, box: _Box, color, *, radius: int = 0) -> None:
        draw = ImageDraw.Draw(self.surf)
        if radius > 0:
            draw.rounded_rectangle(
                [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1],
                radius=radius, outline=color, width=1,
            )
        else:
            draw.rectangle(
                [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1],
                outline=color, width=1,
            )

    def _clip_round(self, box: _Box, radius: int) -> None:
        mask = Image.new("L", self.surf.size, 0)
        draw = ImageDraw.Draw(mask)
        draw.rounded_rectangle(
            [box.x, box.y, box.x + box.w - 1, box.y + box.h - 1],
            radius=radius, fill=255,
        )
        white = Image.new("RGB", self.surf.size, _PILLOW_BG[IDX_WHITE])
        self.surf = Image.composite(self.surf, white, mask)

    def _draw_text(self, text, tw, style, box, path):
        font_size = _resolve_font_size(tw, style)
        font = _load_font(font_size)
        color_hex = style.get("color")
        if color_hex:
            color_idx = _bg_from_hex(color_hex) or IDX_BLACK
            color = _PILLOW_BG[color_idx]
        else:
            color = (0, 0, 0)
        draw = ImageDraw.Draw(self.surf)
        try:
            draw.text((box.x, box.y), text, fill=color, font=font)
        except ValueError:
            draw.text((box.x, box.y), text, fill=color,
                      font=_load_font(16))

    def _render_img(self, node, box, path):
        src = node.get("props", {}).get("src")
        if not src:
            raise RenderError(f"{path}.props.src", "missing")
        try:
            img = self._load_image(src)
        except Exception as e:
            log.warning("image load failed for %s: %s", src, e)
            return
        iw, ih = img.size
        if iw == 0 or ih == 0:
            return
        scale = min(box.w / iw, box.h / ih, 1.0)
        nw, nh = max(1, int(iw * scale)), max(1, int(ih * scale))
        img = img.resize((nw, nh), Image.Resampling.LANCZOS)
        x = box.x + (box.w - nw) // 2
        y = box.y + (box.h - nh) // 2
        if img.mode != "RGB":
            img = img.convert("RGB")
        try:
            dith = _floyd_steinberg_palette(img, PALETTE_BWRY)
        except Exception as e:
            raise RenderError(f"{path}.props.src", f"image dither failed: {e}")
        px = dith.load()
        out_rgb = Image.new("RGB", (nw, nh))
        opx = out_rgb.load()
        for y2 in range(nh):
            for x2 in range(nw):
                opx[x2, y2] = _PILLOW_BG[px[x2, y2]]
        self.surf.paste(out_rgb, (x, y))

    def _load_image(self, src) -> Image.Image:
        if src in self._img_url_cache:
            return self._img_url_cache[src]
        if src.startswith("data:image/"):
            b64 = src.split(",", 1)[1]
            img = Image.open(io.BytesIO(base64.b64decode(b64)))
        elif src.startswith("http://") or src.startswith("https://"):
            r = self._http.get(src)
            r.raise_for_status()
            img = Image.open(io.BytesIO(r.content))
        else:
            raise RenderError("src", f"unsupported scheme: {src[:16]}")
        img.load()
        self._img_url_cache[src] = img
        return img

    def _to_2bpp(self) -> bytes:
        dithered = _floyd_steinberg_palette(self.surf, PALETTE_BWRY)
        raw = pack_2bpp(dithered)
        return raw


def _blank_image() -> Image.Image:
    return Image.new("RGB", (SCREEN_W, SCREEN_H), _PILLOW_BG[IDX_WHITE])


def _align_offset(align: str, container_size: int, child_size: int) -> int:
    if child_size >= container_size:
        return 0
    if align in ("center", "items-center"):
        return (container_size - child_size) // 2
    if align in ("end", "flex-end", "items-end"):
        return container_size - child_size
    return 0


def _justify_offset(justify: str, origin: int, container_size: int,
                    total_size: int, gap: int, count: int) -> int:
    if count == 0:
        return origin
    free = container_size - total_size
    if justify in ("center", "justify-center"):
        return origin + max(0, free // 2)
    if justify in ("between", "space-between", "justify-between"):
        return origin
    if justify in ("end", "flex-end", "justify-end"):
        return origin + max(0, free)
    return origin


def render_canvas_to_bitmap(canvas_json: dict) -> bytes:
    """Render canvas_json to a 30000-byte 2bpp BWRY bitmap.

    Raises RenderError on unsupported element / property / image source.
    """
    r = _CanvasRenderer()
    try:
        return r.render(canvas_json)
    finally:
        r.close()