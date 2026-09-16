"""兼容垫片：实现已拆到 youn_server.canvas 包（便于将来整体抽成库）。

调用方（app.py / mcp_server.py / page_upload.py 与全部测试）继续 from
youn_server.canvas_render import ...，不用改一行。
"""
from __future__ import annotations

from .canvas import (CAPABILITIES, CanvasRenderer, RenderError,
                     render_canvas_to_bitmap, render_canvas_to_png,
                     render_canvas_to_png_debug)
from .canvas.renderer import CanvasRenderer as _CanvasRenderer
from .images_reexport import (IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW,
                              PALETTE_BWRY, SCREEN_H, SCREEN_W, _PILLOW_BG,
                              pack_2bpp)

__all__ = ["CAPABILITIES", "RenderError", "CanvasRenderer", "_CanvasRenderer",
           "render_canvas_to_bitmap", "render_canvas_to_png",
           "render_canvas_to_png_debug", "SCREEN_W", "SCREEN_H",
           "IDX_BLACK", "IDX_WHITE", "IDX_YELLOW", "IDX_RED", "PALETTE_BWRY",
           "_PILLOW_BG", "pack_2bpp"]
