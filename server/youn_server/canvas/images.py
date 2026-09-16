"""图片取材与量化。

两种"跳过"是**契约**，不是将就：页面不该因为一张图没了/被删了就整体渲染失败。
  · `uploads://` 的 id 形状非法（路径穿越）⇒ 拒绝，且**绝不触盘**。
  · 文件不存在 / 拉取失败 ⇒ 跳过该图，整页照常出图。
"""
from __future__ import annotations

import base64
import io
import logging
import re
from pathlib import Path
from typing import Optional

import httpx
from PIL import Image

from ..config import settings
from ..image_conv import (IDX_BLACK, IDX_RED, IDX_WHITE, IDX_YELLOW, PALETTE_BWRY,
                          _floyd_steinberg_palette, _snap_to_palette, auto_quantize)
from .errors import RenderError

log = logging.getLogger(__name__)

PALETTE_RGB = {IDX_BLACK: (0, 0, 0), IDX_WHITE: (255, 255, 255),
               IDX_YELLOW: (255, 215, 0), IDX_RED: (220, 30, 30)}

_CACHE: dict[str, Image.Image] = {}
_ID_RE = re.compile(r"[0-9a-f]{32}")


def load(src: str) -> Optional[Image.Image]:
    """取一张图（RGB）。取不到返回 None。"""
    if src in _CACHE:
        return _CACHE[src]
    try:
        if src.startswith("data:"):
            payload = base64.b64decode(src.split(",", 1)[1])
            with Image.open(io.BytesIO(payload)) as im:
                img = im.convert("RGB")
        elif src.startswith("uploads://"):
            uid = src[len("uploads://"):]
            if not _ID_RE.fullmatch(uid):
                log.warning("uploads id 形状非法，拒绝加载: %r", uid[:40])
                return None
            path = settings.uploads_dir / f"{uid}.png"
            if not path.exists():
                log.warning("uploads 里没有 %s.png，跳过", uid)
                return None
            with Image.open(path) as im:
                img = im.convert("RGB")
        elif src.startswith(("http://", "https://")):
            r = httpx.get(src, timeout=15, follow_redirects=True)
            r.raise_for_status()
            with Image.open(io.BytesIO(r.content)) as im:
                img = im.convert("RGB")
        else:
            log.warning("不支持的图片地址，跳过: %r", str(src)[:60])
            return None
    except Exception as e:  # noqa: BLE001
        log.warning("图片加载失败，跳过 %r: %s", str(src)[:60], e)
        return None
    if len(_CACHE) < 32:
        _CACHE[src] = img
    return img


def quantize(img: Image.Image, mode: Optional[bool]) -> Image.Image:
    """节点级 props.dither 三态：None=自动，True=强制抖动，False=强制吸附。"""
    if mode is None:
        return auto_quantize(img, PALETTE_BWRY)[0]
    if mode:
        return _floyd_steinberg_palette(img, PALETTE_BWRY)
    return _snap_to_palette(img, PALETTE_BWRY)


def for_node(src: str, mode: Optional[bool]) -> Optional[Image.Image]:
    img = load(src)
    if img is None:
        return None
    pal = quantize(img, mode)
    out = Image.new("RGB", pal.size)
    out.putdata([PALETTE_RGB[int(v)] for v in pal.getdata()])
    return out


def dither_flag(props: dict, path: str) -> Optional[bool]:
    if "dither" not in props:
        return None
    v = props["dither"]
    if not isinstance(v, bool):
        raise RenderError(f"{path}.props.dither", "必须是布尔值")
    return v
