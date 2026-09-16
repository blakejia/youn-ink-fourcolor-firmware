"""canvas：Canvas JSON → 400x300 四色 2bpp 位图的自写渲染引擎。

拆成独立模块是为了将来整体抽成库：
  errors      对外错误类型
  capabilities 支持的能力清单（唯一真相，MCP 直接引用）
  tokens      tw 令牌 / style 键 → Spec
  text        字体（带缓存）、测量、折行、截断
  layout      flex 测量与摆放
  paint       Pillow 绘制
  images      图片取材与节点级 dither
  renderer    编排 + 对外三个函数
"""
from .capabilities import CAPABILITIES
from .errors import RenderError
from .renderer import (CanvasRenderer, render_canvas_to_bitmap,
                       render_canvas_to_png, render_canvas_to_png_debug)

__all__ = ["CAPABILITIES", "RenderError", "CanvasRenderer",
           "render_canvas_to_bitmap", "render_canvas_to_png",
           "render_canvas_to_png_debug"]
