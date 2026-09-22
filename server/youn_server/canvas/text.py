"""字体加载、测量、折行、截断。字体**必须缓存**：Noto CJK 有 20MB，
每次绘制重新解析会让一页平白多花 280ms（实测 453ms → 171ms）。
"""
from __future__ import annotations

import functools
import re
from typing import Optional

from PIL import ImageFont

_DEFAULT = (
    "/mnt/data/project/youn-ink-fourcolor-firmware/firmware/managed_components/"
    "lvgl__lvgl/scripts/built_in_font/SourceHanSansSC-Normal.otf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
)
_BOLD = ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",) + _DEFAULT

# 非常规/粗体的字面：Noto CJK 全字重都装在本机（Thin..Black）。常规与粗体
# 保留上表的历史链（常规首选 SourceHan）—— 旧画布的字节输出逐字不变。
_FACE_FILES = {
    "thin": "/usr/share/fonts/opentype/noto/NotoSansCJK-Thin.ttc",
    "light": "/usr/share/fonts/opentype/noto/NotoSansCJK-Light.ttc",
    "demilight": "/usr/share/fonts/opentype/noto/NotoSansCJK-DemiLight.ttc",
    "medium": "/usr/share/fonts/opentype/noto/NotoSansCJK-Medium.ttc",
    "black": "/usr/share/fonts/opentype/noto/NotoSansCJK-Black.ttc",
}

# CSS 字重到可用字面的映射（数字按 CSS font-matching：目标缺失时先向上取）。
# 本机没有 SemiBold/ExtraBold 的 Noto Sans CJK ⇒ 600→Bold(700)、800→Black(900)。
_WEIGHT_WORDS = {
    "100": "thin", "thin": "thin",
    "200": "light", "extralight": "light", "extra-light": "light",
    "300": "light", "light": "light",
    "350": "demilight", "demilight": "demilight", "demi-light": "demilight",
    "400": "regular", "normal": "regular", "regular": "regular",
    "500": "medium", "medium": "medium",
    "600": "bold", "semibold": "bold", "semi-bold": "bold",
    "700": "bold", "bold": "bold", "bolder": "bold",
    "800": "black", "extrabold": "black", "extra-bold": "black",
    "900": "black", "black": "black",
}


def resolve_weight(value) -> Optional[str]:
    """CSS fontWeight（数值/名/字重词）→ 规范字面名；无法识别返回 None。

    tokens 用 None 把未知词当不认识的令牌告警；style 键用 ``or "regular"``
    走 CSS 的“无效声明忽略”语义。
    """
    if value is None:
        return None
    if isinstance(value, bool):               # 兼容旧 load_font(size, bold)
        return "bold" if value else "regular"
    return _WEIGHT_WORDS.get(str(value).strip().lower())


_CJK = re.compile(r"[\u2e80-\u9fff\uf900-\ufaff\uff00-\uffef]")


@functools.lru_cache(maxsize=128)
def load_font(size: int, weight: "str | bool" = "regular") -> ImageFont.FreeTypeFont:
    w = resolve_weight(weight) or "regular"
    if w == "regular":
        chain = _DEFAULT
    elif w == "bold":
        chain = _BOLD
    else:
        chain = (_FACE_FILES[w],) + _DEFAULT
    for path in chain:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def metrics(font, size: int) -> int:
    try:
        ascent, descent = font.getmetrics()
        return max(1, ascent + descent)
    except Exception:  # noqa: BLE001
        return max(1, int(size * 1.25))


def width_of(text: str, font, letter_spacing: int = 0) -> int:
    if not text:
        return 0
    try:
        base = int(font.getlength(text))
    except Exception:  # noqa: BLE001
        base = int(font.getsize(text)[0])
    return base + letter_spacing * max(0, len(text) - 1)


def wrap(text: str, font, max_w: int, nowrap: bool = False,
         letter_spacing: int = 0) -> list[str]:
    """按盒宽折行：CJK 逐字断，西文优先在空格断，绝不零进展。"""
    out: list[str] = []
    for para in str(text).split("\n"):
        if nowrap or max_w <= 0 or width_of(para, font, letter_spacing) <= max_w:
            out.append(para)
            continue
        line = ""
        for ch in para:
            if width_of(line + ch, font, letter_spacing) <= max_w or not line:
                line += ch
                continue
            if not _CJK.match(ch) and " " in line:
                head, _, tail = line.rpartition(" ")
                if head:
                    out.append(head)
                    line = tail + ch
                    continue
            out.append(line)
            line = ch
        if line:
            out.append(line)
    return out or [""]


def clamp(lines: list[str], font, max_w: int, max_lines: Optional[int],
          ellipsis: bool = False, letter_spacing: int = 0) -> list[str]:
    """行数上限；超出时最后一行加省略号（能放得下才加）。

    Also covers the single-line CSS `text-overflow:ellipsis` case: a line that
    is *wider* than the box (nowrap, or one long unbreakable word) must be cut
    even when the line count is within `max_lines` — previously `clamp` only
    looked at the line count, so `truncate` never ellipsized a too-wide line.
    """
    if not ellipsis or not max_w:
        return lines
    kept = lines[:max_lines] if max_lines else lines
    if kept and width_of(kept[-1], font, letter_spacing) > max_w:
        last = kept[-1]
        while last and width_of(last + "…", font, letter_spacing) > max_w:
            last = last[:-1]
        kept[-1] = last + "…"
    return kept


def fit_box(text: str, font, max_w: int, size: int, *, nowrap: bool = False,
            max_lines: Optional[int] = None, ellipsis: bool = False,
            line_height: Optional[float] = None, letter_spacing: int = 0
            ) -> tuple[int, int, list[str]]:
    """折行后的 (宽, 高, 行列表) —— 测量与绘制共用，保证两者一致。"""
    lines = clamp(wrap(text, font, max_w, nowrap, letter_spacing), font, max_w,
                  max_lines, ellipsis, letter_spacing)
    w = max((width_of(ln, font, letter_spacing) for ln in lines), default=0)
    lh = int(size * line_height) if line_height else metrics(font, size)
    return w, max(1, lh) * len(lines), lines
