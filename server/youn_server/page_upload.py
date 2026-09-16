"""图片 → 画板页的绑定。

这段逻辑原本是 ``create_app()`` 里的闭包，只有 ``POST /api/uploads`` 能用。
MCP 也要提供同样能力时，复制一份会让两条路径漂移（归一化尺寸、像素上限、
canvas_json 形状、时长/顺序的继承规则）。所以抽成模块，两边都调这里。

对外只暴露四件事：
    upload_paths()        上传件的落盘位置（归一化图 + 原图）
    normalize_upload()    把任意图片裁/缩成面板尺寸并留白边
    canvas_for_upload()   包一个画板 JSON，让渲染器 1:1 画出这张图
    bind_image_to_page()  上面三步 + 渲染 + 写页，一站到底
"""
from __future__ import annotations

import io
import re
import secrets
from pathlib import Path
from typing import Optional

from PIL import Image

from . import pages as pages_mod
from .canvas_render import RenderError, render_canvas_to_bitmap
from .config import settings

#: 面板几何（与渲染器一致；放这里，上传路径就不必再从 app 传进来）
PANEL_WIDTH = 400
PANEL_HEIGHT = 300

_UPLOAD_ID_RE = re.compile(r"[0-9a-f]{32}")
#: 解码前的像素上限：先在 open() 之后用尺寸拒绝，避免把巨图读进内存
_UPLOAD_MAX_PIXELS = 4000 * 4000
#: 字节上限，与端点此前的口径一致
_UPLOAD_MAX_BYTES = 25 * 1024 * 1024


class UnknownPage(ValueError):
    """目标页不存在。带上现有页名，调用方可以直接回给操作员/代理。"""

    def __init__(self, page: str, known: list[str]) -> None:
        super().__init__(f"unknown page: {page!r}")
        self.page = page
        self.known = known


def upload_paths(upload_id: str) -> tuple[Path, Path]:
    if not _UPLOAD_ID_RE.fullmatch(upload_id):
        raise ValueError(f"invalid upload id: {upload_id!r}")
    return (settings.uploads_dir / f"{upload_id}.png",
            settings.uploads_dir / f"{upload_id}.src.png")


def normalize_upload(data: bytes) -> bytes:
    """把图片装进面板并居中留白边。

    渲染器从不放大（``min(box/iw, 1.0)``），所以存下来的文件必须就是页要画的
    尺寸：更大 ⇒ 渲染时又被缩一次，更小 ⇒ 缩成中间一小块。
    """
    try:
        img = Image.open(io.BytesIO(data))
        # open() 之后尺寸已知：先按像素上限拒绝，再 load() 解码
        if img.width * img.height > _UPLOAD_MAX_PIXELS:
            raise ValueError(f"image too large: {img.width}x{img.height}")
        img.load()
    except ValueError:
        raise
    except Exception as e:  # noqa: BLE001
        # PIL 的 UnidentifiedImageError / OSError 都不是 ValueError，不归一化
        # 就会从调用方（HTTP 端点的 except ValueError）漏出去变成 500。
        raise ValueError(str(e)) from e
    if "A" in img.getbands() or "transparency" in img.info:
        # Alpha 合成到白底：直接 convert("RGB") 会把透明处涂黑，与白色留白矛盾
        base = Image.new("RGBA", img.size, (255, 255, 255, 255))
        img = Image.alpha_composite(base, img.convert("RGBA")).convert("RGB")
    elif img.mode not in ("RGB", "L"):
        img = img.convert("RGB")
    scale = min(PANEL_WIDTH / img.width, PANEL_HEIGHT / img.height, 1.0)
    nw, nh = max(1, int(img.width * scale)), max(1, int(img.height * scale))
    img = img.resize((nw, nh), Image.Resampling.LANCZOS)
    sheet = Image.new("RGB", (PANEL_WIDTH, PANEL_HEIGHT), (255, 255, 255))
    sheet.paste(img, ((PANEL_WIDTH - nw) // 2, (PANEL_HEIGHT - nh) // 2))
    out = io.BytesIO()
    sheet.save(out, format="PNG")
    return out.getvalue()


def canvas_for_upload(upload_id: str) -> dict:
    """一个 contain 装好的图片，按面板尺寸绘制。

    渲染器只认节点自己的 ``w-[Npx]``/``h-[Npx]`` tw token 或 ``style.width/height``
    —— ``w-full``/``h-full`` 不被解析，没尺寸的 img 量出来是 0x0，只会贴出一个
    像素。所以给 img 加上既有页同样的显式 ``style``
    （见 ``server/data/pages/NOTE4C-3400FC/logo-1024.json``）。存下来的文件已经是
    面板尺寸且留过白边，这里是 1:1 画。
    """
    return {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": [{"type": "img",
                      "props": {"src": f"uploads://{upload_id}",
                                "style": {"width": f"{PANEL_WIDTH}px",
                                          "height": f"{PANEL_HEIGHT}px"}}}]}}]}


def bind_image_to_page(
    device: str,
    page: str,
    data: bytes,
    *,
    create: bool = False,
    duration_minutes: Optional[int] = None,
    order: Optional[int] = None,
) -> dict:
    """把一张图片绑定到某设备的某一页，返回 ``{"page": …, **PageEntry}``。

    - 页已存在 ⇒ 沿用它的 duration/order（两条调用方的语义一致）。
    - 页不存在且 ``create=True`` ⇒ 新建：时长用 ``duration_minutes``（默认 10，
      仍受最小页时长约束），顺序用 ``order``，不填则**追加到末尾**。
    - 页不存在且 ``create=False`` ⇒ 抛 :class:`UnknownPage`（端点据此回 400 并
      附上现有页名；在这里回列表而不是让调用方重新扫一遍，避免同一操作里两次
      磁盘快照之间的并发删除把页抢走）。
    """
    sources = pages_mod.list_pages(device)
    source = next((s for s in sources if s.name == page), None)
    if source is None and not create:
        raise UnknownPage(page, [s.name for s in sources])

    if len(data) > _UPLOAD_MAX_BYTES:
        raise ValueError("image too large")
    normalized = normalize_upload(data)

    if source is None:
        duration = int(duration_minutes if duration_minutes else 10)
        if order is None:
            order = max((s.order for s in sources), default=-1) + 1
    else:
        duration = source.duration_minutes
        order = source.order

    upload_id = secrets.token_hex(16)
    settings.uploads_dir.mkdir(parents=True, exist_ok=True)
    norm_path, orig_path = upload_paths(upload_id)
    orig_path.write_bytes(data)        # 原图留着，便于以后重新裁
    norm_path.write_bytes(normalized)  # 画布实际绘制的那张

    canvas_json = canvas_for_upload(upload_id)
    try:
        bitmap = render_canvas_to_bitmap(canvas_json)
    except RenderError as e:
        raise ValueError(f"render failed: {e.path}: {e.message}") from e
    entry = pages_mod.upsert_page(device, page, canvas_json, duration, order, bitmap)
    return {"page": page, "upload_id": upload_id, **entry.to_dict()}
