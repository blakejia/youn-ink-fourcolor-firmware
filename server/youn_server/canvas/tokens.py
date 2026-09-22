"""tw 令牌 / style 键 → 一个与实现无关的 Spec。

覆盖范围见 capabilities.py。解析不认识的令牌**不报错**，只记一条 warning：
历史上静默忽略造成过一整类"文档写了却没生效"的问题，至少要让它在日志里可见。
"""
from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field
from typing import Optional

from .errors import RenderError
from .text import resolve_weight

log = logging.getLogger(__name__)

_PX = re.compile(r"\[(-?\d+)px\]")
_PCT = re.compile(r"\[(-?\d+(?:\.\d+)?)%\]")
_INT = re.compile(r"\[(-?\d+)\]")
_RATIO = re.compile(r"\[(\d+(?:\.\d+)?)/(\d+(?:\.\d+)?)\]")

_COLOR_IDX = {"black": 0, "white": 1, "yellow": 2, "red": 3}
_PILLOW_BG = {0: (0, 0, 0), 1: (255, 255, 255), 2: (255, 215, 0), 3: (220, 30, 30)}

ALIGN = {"start": "flex-start", "end": "flex-end", "flex-start": "flex-start",
         "flex-end": "flex-end", "center": "center", "stretch": "stretch",
         "baseline": "baseline", "between": "space-between",
         "around": "space-around", "evenly": "space-evenly",
         # CSS style 键的完整拼写（justify-content: "space-between" 等）
         "space-between": "space-between", "space-around": "space-around",
         "space-evenly": "space-evenly"}


def color_index(value) -> Optional[int]:
    """#RRGGBB / 调色板色名 → 调色板索引（就近取色，与量化口径一致）。"""
    if value is None:
        return None
    s = str(value).strip()
    if s.lower() in _COLOR_IDX:
        return _COLOR_IDX[s.lower()]
    if not s.startswith("#"):
        return None
    h = s[1:]
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    if len(h) != 6:
        return None
    rgb = (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16))
    return min(_PILLOW_BG.items(),
               key=lambda kv: sum((a - b) ** 2 for a, b in zip(kv[1], rgb)))[0]


def _tracks(inner: str) -> list[str]:
    """`1fr,1fr` / `80px_1fr` / `minmax(50px,1fr)` → 轨道串列表。"""
    out, depth, cur = [], 0, ""
    for ch in inner:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch in ",_" and depth == 0:
            out.append(cur)
            cur = ""
            continue
        cur += ch
    if cur:
        out.append(cur)
    return [t.strip() for t in out if t.strip()]


def _int_of(value, what: str) -> Optional[int]:
    if value is None or value == "":
        return None
    s = str(value).strip().lower()
    if s == "auto":
        return None
    m = re.match(r"^(-?\d+)(px)?$", s)
    if not m:
        raise RenderError(what, f"尺寸只支持整数/Npx/N%，收到 {value!r}")
    return int(m.group(1))


def _pct_of(value) -> Optional[float]:
    if value is None:
        return None
    s = str(value).strip()
    if s.endswith("%"):
        try:
            return float(s[:-1]) / 100.0
        except ValueError:
            return None
    return None


@dataclass
class Spec:
    """一个节点的全部样式（解析后的确定值）。"""

    direction: str = "row"          # row | column
    wrap: bool = False
    items: str = "stretch"
    self_align: str = "auto"
    justify_items: str = "stretch"          # grid: inline-axis child alignment
    content: str = "flex-start"
    justify: str = "flex-start"
    gap: int = 0
    gap_x: Optional[int] = None             # None = fall back to gap
    gap_y: Optional[int] = None
    padding: tuple[int, int, int, int] = (0, 0, 0, 0)     # t r b l
    margin: tuple[int, int, int, int] = (0, 0, 0, 0)
    width: Optional[int] = None
    width_pct: Optional[float] = None
    width_full: bool = False
    height: Optional[int] = None
    height_pct: Optional[float] = None
    height_full: bool = False
    min_w: Optional[int] = None
    max_w: Optional[int] = None
    min_h: Optional[int] = None
    max_h: Optional[int] = None
    aspect: Optional[float] = None
    grow: float = 0.0
    shrink: float = 1.0
    basis: Optional[int] = None
    bg: Optional[int] = None
    color: Optional[int] = None
    border: int = 0
    border_color: Optional[int] = None
    radius: int = 0
    clip: bool = False
    font_size: int = 16
    weight: str = "regular"
    align: str = "left"
    line_clamp: Optional[int] = None
    nowrap: bool = False
    line_height: Optional[float] = None
    letter_spacing: int = 0
    truncated: bool = False
    explicit_w: bool = False
    explicit_h: bool = False
    display: str = "flex"
    grid_cols: list[str] = field(default_factory=list)
    grid_rows: list[str] = field(default_factory=list)
    col_start: Optional[int] = None
    col_span: Optional[int] = None
    row_start: Optional[int] = None
    row_span: Optional[int] = None
    underline: bool = False
    strike: bool = False
    vertical: bool = False                  # style writingMode == vertical-rl
    radii: Optional[tuple[int, int, int, int]] = None   # tl tr br bl; None = use radius
    unknown: list[str] = field(default_factory=list)


def parse(tw: str, style: dict, path: str) -> Spec:
    sp = Spec()
    style = style or {}
    for tok in (tw or "").split():
        if tok == "grid":
            sp.display = "grid"
        elif tok.startswith("grid-cols-["):
            sp.grid_cols = _tracks(tok[11:-1])
        elif tok.startswith("grid-rows-["):
            sp.grid_rows = _tracks(tok[11:-1])
        elif tok.startswith("col-span-["):
            m = _INT.search(tok)
            sp.col_span = int(m.group(1)) if m else 1
        elif tok.startswith("col-start-["):
            m = _INT.search(tok)
            sp.col_start = int(m.group(1)) if m else None
        elif tok.startswith("row-span-["):
            m = _INT.search(tok)
            sp.row_span = int(m.group(1)) if m else 1
        elif tok.startswith("row-start-["):
            m = _INT.search(tok)
            sp.row_start = int(m.group(1)) if m else None
        elif tok in ("flex", "flex-row"):
            sp.direction = "row"
        elif tok == "flex-col":
            sp.direction = "column"
        elif tok == "flex-wrap":
            sp.wrap = True
        elif tok.startswith("w-["):
            if _PCT.search(tok):
                sp.width_pct = _pct_of(_PCT.search(tok).group(1) + "%")
            else:
                sp.width = _PX.search(tok) and int(_PX.search(tok).group(1))
            sp.explicit_w = True
        elif tok == "w-full":
            sp.width_full = True
            sp.explicit_w = True
        elif tok.startswith("h-["):
            if _PCT.search(tok):
                sp.height_pct = _pct_of(_PCT.search(tok).group(1) + "%")
            else:
                sp.height = _PX.search(tok) and int(_PX.search(tok).group(1))
            sp.explicit_h = True
        elif tok == "h-full":
            sp.height_full = True
            sp.explicit_h = True
        elif tok.startswith("min-w-["):
            sp.min_w = _PX.search(tok) and int(_PX.search(tok).group(1))
        elif tok.startswith("max-w-["):
            sp.max_w = _PX.search(tok) and int(_PX.search(tok).group(1))
        elif tok.startswith("min-h-["):
            sp.min_h = _PX.search(tok) and int(_PX.search(tok).group(1))
        elif tok.startswith("max-h-["):
            sp.max_h = _PX.search(tok) and int(_PX.search(tok).group(1))
        elif tok.startswith("aspect-["):
            m = _RATIO.search(tok)
            if m:
                sp.aspect = float(m.group(1)) / float(m.group(2))
        elif tok == "flex-1":
            sp.grow, sp.shrink, sp.basis = 1.0, 1.0, 0
        elif tok.startswith("flex-grow-["):
            m = _INT.search(tok)
            sp.grow = float(m.group(1)) if m else 0.0
        elif tok.startswith("flex-shrink-["):
            m = _INT.search(tok)
            sp.shrink = float(m.group(1)) if m else 1.0
        elif tok.startswith("flex-basis-["):
            m = _PX.search(tok)
            sp.basis = int(m.group(1)) if m else None
        elif tok.startswith("gap-x-["):
            sp.gap_x = int(_PX.search(tok).group(1)) if _PX.search(tok) else None
        elif tok.startswith("gap-y-["):
            sp.gap_y = int(_PX.search(tok).group(1)) if _PX.search(tok) else None
        elif tok.startswith("gap-["):
            sp.gap = int(_PX.search(tok).group(1)) if _PX.search(tok) else 0
        elif tok.startswith("p-["):
            v = int(_PX.search(tok).group(1))
            sp.padding = (v, v, v, v)
        elif tok.startswith("px-["):
            v = int(_PX.search(tok).group(1))
            t, _r, bo, _l = sp.padding
            sp.padding = (t, v, bo, v)
        elif tok.startswith("py-["):
            v = int(_PX.search(tok).group(1))
            _t, r, _b, l = sp.padding
            sp.padding = (v, r, v, l)
        elif tok.startswith("pt-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.padding
            sp.padding = (v, r, b, l)
        elif tok.startswith("pr-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.padding
            sp.padding = (t, v, b, l)
        elif tok.startswith("pb-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.padding
            sp.padding = (t, r, v, l)
        elif tok.startswith("pl-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.padding
            sp.padding = (t, r, b, v)
        elif tok.startswith("m-["):
            v = int(_PX.search(tok).group(1))
            sp.margin = (v, v, v, v)
        elif tok.startswith("mx-["):
            v = int(_PX.search(tok).group(1))
            t, _r, bo, _l = sp.margin
            sp.margin = (t, v, bo, v)
        elif tok.startswith("my-["):
            v = int(_PX.search(tok).group(1))
            _t, r, _b, l = sp.margin
            sp.margin = (v, r, v, l)
        elif tok.startswith("mt-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.margin
            sp.margin = (v, r, b, l)
        elif tok.startswith("mr-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.margin
            sp.margin = (t, v, b, l)
        elif tok.startswith("mb-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.margin
            sp.margin = (t, r, v, l)
        elif tok.startswith("ml-["):
            v = int(_PX.search(tok).group(1))
            t, r, b, l = sp.margin
            sp.margin = (t, r, b, v)
        elif tok.startswith("items-"):
            sp.items = ALIGN.get(tok[6:], "stretch")
        elif tok.startswith("self-"):
            sp.self_align = ALIGN.get(tok[5:], "auto")
        elif tok.startswith("content-"):
            sp.content = ALIGN.get(tok[8:], "flex-start")
        elif tok.startswith("justify-items-"):
            sp.justify_items = ALIGN.get(tok[14:], "stretch")
        elif tok.startswith("justify-"):
            sp.justify = ALIGN.get(tok[8:], "flex-start")
        elif tok.startswith("text-[") and tok.endswith("px]"):
            sp.font_size = int(_PX.search(tok).group(1))
        elif tok == "text-center":
            sp.align = "center"
        elif tok == "underline":
            sp.underline = True
        elif tok == "line-through":
            sp.strike = True
        elif tok == "text-right":
            sp.align = "right"
        elif tok.startswith("font-"):
            # 全字重词（font-medium/black/thin…）；font-bold 走同一解析。
            # 未知的 font-* 按不认识的令牌告警（font-sans 等字体族不在子集内）。
            fw = resolve_weight(tok[5:])
            if fw is None:
                sp.unknown.append(tok)
            else:
                sp.weight = fw
        elif tok.startswith("line-clamp-["):
            m = _INT.search(tok)
            sp.line_clamp = int(m.group(1)) if m else None
        elif tok == "truncate":
            # Tailwind's truncate = overflow:hidden + text-overflow:ellipsis +
            # whitespace-nowrap. Without the line cap the ellipsis branch in
            # text.clamp() was unreachable (`max_lines=None` returns early), so
            # the token only stopped wrapping and let the text run past the box.
            # An explicit line-clamp-[N] alongside it wins (checked either order).
            sp.truncated = True
            sp.nowrap = True
            if sp.line_clamp is None:
                sp.line_clamp = 1
        elif tok == "whitespace-nowrap":
            sp.nowrap = True
        elif tok.startswith("leading-["):
            m = _PX.search(tok)
            sp.line_height = float(m.group(1)) if m else None
        elif tok.startswith("tracking-["):
            m = _PX.search(tok)
            sp.letter_spacing = int(m.group(1)) if m else 0
        elif tok.startswith("bg-"):
            sp.bg = color_index(tok[3:])
            if sp.bg is None:
                sp.unknown.append(tok)
        elif tok == "border":
            sp.border, sp.border_color = 1, 0
        elif tok.startswith("border-["):
            m = _PX.search(tok)
            sp.border = int(m.group(1)) if m else 1
            sp.border_color = sp.border_color if sp.border_color is not None else 0
        elif tok.startswith("border-"):
            sp.border, sp.border_color = 1, color_index(tok[7:])
            if sp.border_color is None:
                sp.unknown.append(tok)
        elif tok.startswith("rounded-["):
            m = _PX.search(tok)
            sp.radius = int(m.group(1)) if m else 0
        elif tok == "overflow-hidden":
            sp.clip = True
        else:
            sp.unknown.append(tok)

    # ── style 键（文档承诺的必须生效）──
    st = style
    if "color" in st:
        sp.color = color_index(st["color"])
    if "backgroundColor" in st:
        sp.bg = color_index(st["backgroundColor"])
    if "borderRadius" in st:
        raw = st["borderRadius"]
        if isinstance(raw, (list, tuple)) or (isinstance(raw, str) and "," in raw):
            # Per-corner form: "tl,tr,br,bl" (also accepts a 4-item list).
            # Kept separate from the scalar path below, whose _int_of would
            # reject the comma form rather than silently collapsing it.
            parts = (list(raw) if isinstance(raw, (list, tuple))
                     else [x.strip() for x in raw.split(",")])
            if len(parts) == 4:
                try:
                    sp.radii = tuple(
                        int(float(str(x).rstrip("px"))) for x in parts
                    )  # type: ignore[assignment]
                except (TypeError, ValueError):
                    raise RenderError(path, "style.borderRadius 的四角值必须是数字")
            else:
                raise RenderError(path, "style.borderRadius 四角形式需要 4 个值")
        else:
            sp.radius = _int_of(raw, path + ".style.borderRadius") or 0
    if "paddingX" in st:
        v = _int_of(st["paddingX"], path + ".style.paddingX") or 0
        t, _r, bo, _l = sp.padding
        sp.padding = (t, v, bo, v)
    if "paddingY" in st:
        v = _int_of(st["paddingY"], path + ".style.paddingY") or 0
        _t, r, _b, l = sp.padding
        sp.padding = (v, r, v, l)
    for key, idx in (("paddingTop", 0), ("paddingRight", 1),
                     ("paddingBottom", 2), ("paddingLeft", 3)):
        if key in st:
            v = _int_of(st[key], f"{path}.style.{key}") or 0
            t, r, b, l = sp.padding
            sp.padding = (v, r, b, l) if idx == 0 else (
                t, v, b, l) if idx == 1 else (t, r, v, l) if idx == 2 else (t, r, b, v)
    for key, setter in (("margin", "all"), ("marginX", "x"), ("marginY", "y")):
        if key in st:
            v = _int_of(st[key], f"{path}.style.{key}") or 0
            if setter == "all":
                sp.margin = (v, v, v, v)
            elif setter == "x":
                t, _r, bo, _l = sp.margin
                sp.margin = (t, v, bo, v)
            else:
                _t, r, _b, l = sp.margin
                sp.margin = (v, r, v, l)
    for key, idx in (("marginTop", 0), ("marginRight", 1),
                     ("marginBottom", 2), ("marginLeft", 3)):
        if key in st:
            v = _int_of(st[key], f"{path}.style.{key}") or 0
            t, r, b, l = sp.margin
            sp.margin = (v, r, b, l) if idx == 0 else (
                t, v, b, l) if idx == 1 else (t, r, v, l) if idx == 2 else (t, r, b, v)
    if "gap" in st:
        sp.gap = _int_of(st["gap"], path + ".style.gap") or 0
    if "rowGap" in st:
        sp.gap_y = _int_of(st["rowGap"], path + ".style.rowGap")
    if "columnGap" in st:
        sp.gap_x = _int_of(st["columnGap"], path + ".style.columnGap")
    if "justifyItems" in st:
        sp.justify_items = ALIGN.get(str(st["justifyItems"]).lower(), "stretch")
    if "justifyContent" in st:
        sp.justify = ALIGN.get(str(st["justifyContent"]).lower(), "flex-start")
    if "alignContent" in st:
        sp.content = ALIGN.get(str(st["alignContent"]).lower(), "flex-start")
    if str(st.get("writingMode", "")).lower() == "vertical-rl":
        sp.vertical = True
    for key, attr in (("width", "w"), ("height", "h")):
        if key in st:
            pct = _pct_of(st[key])
            px = _int_of(st[key], f"{path}.style.{key}")
            if pct is not None:
                setattr(sp, f"{attr}idth_pct" if attr == "w" else "height_pct", pct)
            else:
                setattr(sp, "width" if attr == "w" else "height", px)
            setattr(sp, "explicit_w" if attr == "w" else "explicit_h", True)
    for key, attr in (("minWidth", "min_w"), ("maxWidth", "max_w"),
                      ("minHeight", "min_h"), ("maxHeight", "max_h")):
        if key in st:
            setattr(sp, attr, _int_of(st[key], f"{path}.style.{key}"))
    if "aspectRatio" in st:
        try:
            sp.aspect = float(str(st["aspectRatio"]).split("/")[0]) / (
                float(str(st["aspectRatio"]).split("/")[1]) if "/" in str(st["aspectRatio"]) else 1.0)
        except (ValueError, ZeroDivisionError, IndexError):
            sp.aspect = None
    if "flexGrow" in st:
        sp.grow = float(st["flexGrow"])
    if "flexShrink" in st:
        sp.shrink = float(st["flexShrink"])
    if "flexBasis" in st:
        sp.basis = _int_of(st["flexBasis"], path + ".style.flexBasis")
    if "alignSelf" in st:
        sp.self_align = ALIGN.get(str(st["alignSelf"]), str(st["alignSelf"]))
    if "borderWidth" in st:
        sp.border = _int_of(st["borderWidth"], path + ".style.borderWidth") or 0
        sp.border_color = 0 if sp.border_color is None else sp.border_color
    if st.get("border"):
        sp.border = sp.border or 1
        sp.border_color = 0 if sp.border_color is None else sp.border_color
    if st.get("overflow") == "hidden":
        sp.clip = True
    if "fontSize" in st:
        sp.font_size = _int_of(st["fontSize"], path + ".style.fontSize") or 16
    if "fontWeight" in st:
        # 无效值按 CSS 忽略（保留 font-* 令牌已设的字重）。
        fw = resolve_weight(st["fontWeight"])
        if fw is not None:
            sp.weight = fw
    if "textAlign" in st:
        ta = str(st["textAlign"]).lower()
        sp.align = ta if ta in ("center", "right", "left") else sp.align
    if "lineClamp" in st:
        sp.line_clamp = _int_of(st["lineClamp"], path + ".style.lineClamp")
    if str(st.get("textOverflow", "")).lower() == "ellipsis":
        # CSS text-overflow:ellipsis. Only meaningful with a line cap and no
        # wrapping: with an explicit lineClamp it applies there; bare, it means
        # the single-line case (clamp 1 + nowrap), i.e. byte-identical to the
        # `truncate` token (see test_canvas_text_overflow).
        sp.truncated = True
        sp.nowrap = True
        if sp.line_clamp is None:
            sp.line_clamp = 1
    td = str(st.get("textDecoration", "")).lower()
    if "underline" in td:
        sp.underline = True
    if "line-through" in td:
        sp.strike = True
    if "none" in td:
        sp.underline = sp.strike = False
    if str(st.get("whiteSpace", "")).lower() == "nowrap":
        sp.nowrap = True
    if str(st.get("display", "")).lower() == "grid":
        sp.display = "grid"
    for key, attr in (("gridTemplateColumns", "grid_cols"),
                      ("gridTemplateRows", "grid_rows")):
        if key in st:
            v = st[key]
            setattr(sp, attr, _tracks(v) if isinstance(v, str)
                    else [str(x) for x in v])
    for key, attr in (("gridColumnStart", "col_start"),
                      ("gridColumnSpan", "col_span"),
                      ("gridRowStart", "row_start"),
                      ("gridRowSpan", "row_span")):
        if key in st:
            try:
                setattr(sp, attr, int(st[key]))
            except (TypeError, ValueError):
                pass
    if "lineHeight" in st:
        try:
            sp.line_height = float(str(st["lineHeight"]).rstrip("px"))
        except ValueError:
            sp.line_height = None
    if "letterSpacing" in st:
        try:
            sp.letter_spacing = int(float(str(st["letterSpacing"]).rstrip("px")))
        except ValueError:
            sp.letter_spacing = 0

    if sp.unknown:
        log.warning("%s: 忽略不认识的令牌 %s（见 CAPABILITIES）", path, sp.unknown)
    return sp
