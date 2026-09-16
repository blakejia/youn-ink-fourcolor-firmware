"""给垫片用的常量再导出（测试与旧代码从 canvas_render 直接取这些东西）。"""
from .canvas.images import PALETTE_RGB as _PILLOW_BG          # noqa: F401
from .image_conv import (IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW,  # noqa: F401
                         PALETTE_BWRY, SCREEN_H, SCREEN_W, pack_2bpp)
