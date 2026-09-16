"""编排：遍历 canvas JSON → 画到 400x300 画布 → 量化成四色 2bpp。"""
from __future__ import annotations

import io
import logging
from typing import Any, Optional

from PIL import Image

from ..image_conv import (IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW, PALETTE_BWRY,
                          SCREEN_H, SCREEN_W, _floyd_steinberg_palette,
                          _snap_to_palette, pack_2bpp)
from .capabilities import CAPABILITIES
from .errors import RenderError
from .images import PALETTE_RGB, dither_flag, for_node
from .layout import Box, children_of, clear_specs, measure, place, spec_of
from .paint import Painter

log = logging.getLogger(__name__)


class CanvasRenderer:
    """canvas_json（windowData）→ 位图。对外三个函数都在文件末尾。"""

    def __init__(self, *, collect_bounds: bool = False) -> None:
        self.painter = Painter((SCREEN_W, SCREEN_H))
        self.collect_bounds = collect_bounds
        self.bounds: list[dict] = []

    # ── 内部 ────────────────────────────────────────────────────
    def _bound(self, path: str, kind: str, box: Box, text: str = "") -> None:
        if self.collect_bounds:
            self.bounds.append({"path": path, "type": kind, "x": box.x, "y": box.y,
                                "w": box.w, "h": box.h, "text": text})

    def render_node(self, node, box: Box, path: str) -> None:
        if not isinstance(node, dict):
            raise RenderError(path, "节点必须是对象")
        ntype = node.get("type")
        if ntype not in ("div", "span", "img"):
            raise RenderError(path, f"unsupported element type {ntype!r}")
        props = node.get("props") or {}
        spec = spec_of(node, path)
        self._bound(path, ntype, box)

        if ntype == "img":
            src = props.get("src")
            if not src:
                raise RenderError(f"{path}.props.src", "missing")
            img = for_node(str(src), dither_flag(props, path))
            if img is not None:
                self.painter.paste(img, box)
            return

        if spec.bg is not None:
            self.painter.fill(box, spec.bg)
        if spec.radius > 0:
            self.painter.clip_round(box, spec.radius)
        before = self.painter.surf.copy() if spec.clip else None

        kids = children_of(node, path)
        if kids:
            for kind, value, child_box, cpath in place(node, box, path):
                if kind == "text":
                    self._bound(cpath, "text", child_box, str(value))
                    self.painter.text(str(value), spec.font_size, spec.bold,
                                      spec.align, child_box,
                                      spec.color if spec.color is not None else IDX_BLACK,
                                      nowrap=spec.nowrap, max_lines=spec.line_clamp,
                                      ellipsis=spec.truncated,
                                      line_height=spec.line_height,
                                      letter_spacing=spec.letter_spacing)
                else:
                    self.render_node(value, child_box, cpath)

        if spec.clip and before is not None:
            self.painter.clip_box(box, before)
        if spec.border:
            self.painter.stroke(box, spec.border_color if spec.border_color is not None
                                else IDX_BLACK, spec.border)

    def render(self, canvas_json: dict, dither: bool = True) -> bytes:
        clear_specs()
        if not isinstance(canvas_json, dict):
            raise RenderError("windowData", "must be an object")
        default = canvas_json.get("default")
        if not isinstance(default, list):
            raise RenderError("windowData.default", "must be a list")
        for i, root in enumerate(default):
            self.render_node(root, Box(0, 0, SCREEN_W, SCREEN_H),
                             f"windowData.default[{i}]")
        return self.to_2bpp(dither)

    def to_2bpp(self, dither: bool) -> bytes:
        sur = self.painter.surf
        pal = (_floyd_steinberg_palette(sur, PALETTE_BWRY) if dither
               else _snap_to_palette(sur, PALETTE_BWRY))
        return pack_2bpp(pal)

    def png(self, dither: bool) -> bytes:
        sur = self.painter.surf
        pal = (_floyd_steinberg_palette(sur, PALETTE_BWRY) if dither
               else _snap_to_palette(sur, PALETTE_BWRY))
        out = Image.new("RGB", pal.size)
        out.putdata([PALETTE_RGB[int(v)] for v in pal.getdata()])
        buf = io.BytesIO()
        out.save(buf, format="PNG")
        return buf.getvalue()


# ── 对外 API（与历史上完全一致，调用方无需改动）────────────────
def render_canvas_to_bitmap(canvas_json: dict, *, dither: bool = True) -> bytes:
    return CanvasRenderer().render(canvas_json, dither)


def render_canvas_to_png(canvas_json: dict, *, dither: bool = True) -> bytes:
    r = CanvasRenderer()
    r.render(canvas_json, dither)
    return r.png(dither)


def render_canvas_to_png_debug(canvas_json: dict, *, dither: bool = True
                               ) -> tuple[bytes, list[dict]]:
    r = CanvasRenderer(collect_bounds=True)
    r.render(canvas_json, dither)      # 位图只是为了驱动同一遍布局与绘制
    return r.png(dither), r.bounds     # 对外给 PNG（编辑器要显示预览）

