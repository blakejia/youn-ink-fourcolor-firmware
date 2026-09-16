"""flex 布局：测量 + 摆放。

**主轴口径只在这里定义一次**（AXIS），measure / 切行 / grow / shrink / 对齐
全部从它取下标 —— 上一轮就是因为在几处各自硬编了下标，`h-full` 被算成宽度、
`justify-*` 一起红。

其它口径（都是踩过坑定下来的）：
  · 所有 div/span 都是 flex 容器，默认 flex-direction:row。
  · stretch 只在子节点**没写** cross-size 时生效（显式尺寸优先）。
  · 子节点尺寸不继承父节点的 w-/h-。
  · 容器没写尺寸时按内容算；绝不兜底成 100x24。
  · 顶层节点由 renderer 给整屏盒子（400x300），h-full/w-full 才有确定可用区。
  · 文字在**父节点的内容盒**里绘制（对齐/折行都相对父盒）。
  · measure() 返回内容盒尺寸（不含 margin）；margin 由 place() 计。
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from . import grid as _grid
from .errors import RenderError
from .text import fit_box, load_font
from .tokens import Spec, parse

_SPECS: dict[int, Spec] = {}

#: entries 的下标：0 kid, 1 text, 2 ksp, 3 w, 4 h, 5 ml, 6 mr, 7 mt, 8 mb, 9 cpath
AXIS = {
    "row": {"main": 3, "cross": 4, "m_start": 5, "m_end": 6, "c_start": 7, "c_end": 8},
    "column": {"main": 4, "cross": 3, "m_start": 7, "m_end": 8, "c_start": 5, "c_end": 6},
}


def clear_specs() -> None:
    _SPECS.clear()


def spec_of(node: dict, path: str) -> Spec:
    key = id(node)
    sp = _SPECS.get(key)
    if sp is None:
        props = node.get("props") or {}
        sp = parse(str(props.get("tw", "")), props.get("style") or {}, path)
        _SPECS[key] = sp
    return sp


@dataclass
class Box:
    x: int
    y: int
    w: int
    h: int


def children_of(node: dict, path: str) -> list:
    kids = (node.get("props") or {}).get("children")
    if kids is None:
        return []
    if isinstance(kids, (str, int, float)):
        return [str(kids)]
    if isinstance(kids, list):
        return kids
    raise RenderError(path + ".props.children", "必须是字符串或数组")


def _clamp(v: Optional[int], lo: Optional[int], hi: Optional[int]) -> Optional[int]:
    if v is None:
        return None
    if lo is not None:
        v = max(v, lo)
    if hi is not None:
        v = min(v, hi)
    return v


def _axis(spec: Spec, axis: str, avail: int) -> tuple[Optional[int], bool]:
    if axis == "w":
        if spec.width is not None:
            return _clamp(spec.width, spec.min_w, spec.max_w), True
        if spec.width_pct is not None:
            return _clamp(int(avail * spec.width_pct), spec.min_w, spec.max_w), True
        if spec.width_full:
            return _clamp(avail, spec.min_w, spec.max_w), True
    else:
        if spec.height is not None:
            return _clamp(spec.height, spec.min_h, spec.max_h), True
        if spec.height_pct is not None:
            return _clamp(int(avail * spec.height_pct), spec.min_h, spec.max_h), True
        if spec.height_full:
            return _clamp(avail, spec.min_h, spec.max_h), True
    return None, False


def _text_size(text: str, spec: Spec, max_w: int) -> tuple[int, int]:
    font = load_font(spec.font_size, spec.bold)
    w, h, _ = fit_box(text, font, max_w, spec.font_size, nowrap=spec.nowrap,
                      max_lines=spec.line_clamp, ellipsis=spec.truncated,
                      line_height=spec.line_height,
                      letter_spacing=spec.letter_spacing)
    return w, h


def _entries_of(node, spec: Spec, path: str, inner: Box, avail_h: int) -> list:
    kids = children_of(node, path)
    out = []
    for i, kid in enumerate(kids):
        cpath = f"{path}.props.children[{i}]"
        if isinstance(kid, str):
            tw_, th_ = _text_size(kid, spec, inner.w)
            out.append((None, kid, None, tw_, th_, 0, 0, 0, 0, cpath))
        else:
            ksp = spec_of(kid, cpath)
            kw, kh = measure(kid, inner.w, avail_h, cpath)
            if kid.get("type") == "img" and (kw == 0 or kh == 0):
                kw = kw or inner.w
                kh = kh or inner.h
            mt, mr, mb, ml = ksp.margin
            out.append((kid, None, ksp, kw, kh, ml, mr, mt, mb, cpath))
    return out


def _split_lines(entries: list, spec: Spec, avail_main: int, ax: dict) -> list[list]:
    """按**假想主轴尺寸**切行（flex-wrap）。不换行时永远单行。"""
    if not spec.wrap or spec.direction != "row":
        return [entries]
    lines, cur, used = [], [], 0
    for e in entries:
        need = e[ax["main"]] + e[ax["m_start"]] + e[ax["m_end"]]
        extra = spec.gap if cur else 0
        if cur and used + extra + need > avail_main:
            lines.append(cur)
            cur, used = [], 0
            extra = 0
        cur.append(e)
        used += extra + need
    if cur:
        lines.append(cur)
    return lines


def _resolve_main(entries: list, spec: Spec, avail_main: int, ax: dict) -> list[int]:
    """主轴尺寸：basis → grow / shrink。"""
    bases = [(e[ax["main"]] if (e[2] is None or e[2].basis is None) else e[2].basis)
             for e in entries]

    def used():
        return (sum(bases) + sum(e[ax["m_start"]] + e[ax["m_end"]] for e in entries)
                + spec.gap * max(0, len(entries) - 1))

    free = avail_main - used()
    if free > 0:
        grows = [e[2].grow if e[2] is not None else 0.0 for e in entries]
        gsum = sum(grows)
        if gsum > 0:
            for i, g in enumerate(grows):
                bases[i] += int(free * g / gsum)
    elif free < 0:
        # flex-shrink：按 shrink × 基准尺寸 加权分摊缩量，地板 0
        shrink = [(e[2].shrink if e[2] is not None else 1.0) * max(1, bases[i])
                  for i, e in enumerate(entries)]
        ssum = sum(shrink)
        if ssum > 0:
            deficit = -free
            for i, wgt in enumerate(shrink):
                bases[i] -= min(bases[i], int(deficit * wgt / ssum + 0.5))
    return bases


def measure(node, avail_w: int, avail_h: int, path: str) -> tuple[int, int]:
    """内容盒尺寸（不含 margin）。"""
    if not isinstance(node, dict):
        raise RenderError(path, "节点必须是对象")
    if node.get("type") not in ("div", "span", "img", "table", "tr", "td"):
        raise RenderError(path, f"unsupported element type {node.get('type')!r}")
    spec = spec_of(node, path)
    w, w_exp = _axis(spec, "w", avail_w)
    h, h_exp = _axis(spec, "h", avail_h)
    if node.get("type") == "img":
        return (w or 0), (h or 0)

    pt, pr, pb, pl = spec.padding
    inner_w = w if w_exp else max(0, avail_w - pl - pr)
    inner_h = h if h_exp else max(0, avail_h - pt - pb)
    inner = Box(0, 0, inner_w, inner_h)
    entries = _entries_of(node, spec, path, inner, inner_h)
    if spec.display == "grid":
        _boxes, cw, ch = _grid.layout(
            node, spec, inner, entries,
            lambda i, e, aw, ah: (e[3], e[4]), definite=(w_exp, h_exp))
        out_w = w if w_exp else cw + pl + pr
        out_h = h if h_exp else ch + pt + pb
        return (_clamp(out_w, spec.min_w, spec.max_w) or 0,
                _clamp(out_h, spec.min_h, spec.max_h) or 0)
    ax = AXIS[spec.direction]
    lines = _split_lines(entries, spec, inner_w if spec.direction == "row" else inner_h, ax)

    if spec.direction == "row":
        cw = max((sum(e[3] + e[5] + e[6] for e in ln) + spec.gap * max(0, len(ln) - 1)
                  for ln in lines), default=0)
        ch = sum(max((e[4] + e[7] + e[8] for e in ln), default=0) for ln in lines) \
            + spec.gap * max(0, len(lines) - 1)
    else:
        cw = max((e[3] + e[5] + e[6] for ln in lines for e in ln), default=0)
        ch = sum(e[4] + e[7] + e[8] for ln in lines for e in ln) \
            + spec.gap * max(0, len(entries) - 1)

    out_w = w if w_exp else cw + pl + pr
    out_h = h if h_exp else ch + pt + pb
    if spec.aspect:
        if w_exp and not h_exp:
            out_h = int(out_w / spec.aspect)
        elif h_exp and not w_exp:
            out_w = int(out_h * spec.aspect)
    out_w = _clamp(out_w, spec.min_w, spec.max_w) or 0
    out_h = _clamp(out_h, spec.min_h, spec.max_h) or 0
    return out_w, out_h


def _justify(mode: str, origin: int, size: int, used: int, n: int) -> tuple[int, float]:
    free = max(0, size - used)
    if mode == "center":
        return origin + free // 2, 0.0
    if mode == "flex-end":
        return origin + free, 0.0
    if mode == "space-between" and n > 1:
        return origin, free / (n - 1)
    if mode == "space-around" and n:
        step = free / n
        return origin + int(step / 2), step
    if mode == "space-evenly" and n:
        step = free / (n + 1)
        return origin + int(step), step
    return origin, 0.0


def _place_line(entries: list, spec: Spec, ax: dict, avail_main: int, cross_size: int,
                origin_main: int, origin_cross: int, out: list) -> None:
    bases = _resolve_main(entries, spec, avail_main, ax)
    used = (sum(bases) + sum(e[ax["m_start"]] + e[ax["m_end"]] for e in entries)
            + spec.gap * max(0, len(entries) - 1))
    pos, step = _justify(spec.justify, origin_main, avail_main, used, len(entries))
    for i, e in enumerate(entries):
        kid, text, ksp = e[0], e[1], e[2]
        if kid is None:
            out.append(("text", text, None, e[9]))       # 盒由调用方给父内容盒
            continue
        align = ksp.self_align if (ksp is not None and ksp.self_align != "auto") else spec.items
        main = bases[i]
        pos += e[ax["m_start"]]
        avail_cross = max(0, cross_size - e[ax["c_start"]] - e[ax["c_end"]])
        if spec.direction == "row":
            natural, explicit = e[4], bool(ksp and ksp.explicit_h)
        else:
            natural, explicit = e[3], bool(ksp and ksp.explicit_w)
        # 显式尺寸不被容器钳制 —— CSS 里那是**溢出**，不是压缩
        cross = avail_cross if (align == "stretch" and not explicit) else natural
        off = 0
        if align == "center":
            off = max(0, (avail_cross - cross) // 2)
        elif align == "flex-end":
            off = max(0, avail_cross - cross)
        if spec.direction == "row":
            box = Box(int(pos), int(origin_cross + e[ax["c_start"]] + off),
                      int(main), int(cross))
        else:
            box = Box(int(origin_cross + e[ax["c_start"]] + off), int(pos),
                      int(cross), int(main))
        out.append(("node", kid, box, e[9]))
        pos += main + e[ax["m_end"]] + spec.gap + (step if i < len(entries) - 1 else 0)


def place(node, box: Box, path: str) -> list[tuple[str, object, object, str]]:
    """把子节点摆进 box → [(kind, node|文字, Box|None, 子路径)]。"""
    spec = spec_of(node, path)
    pt, pr, pb, pl = spec.padding
    inner = Box(box.x + pl, box.y + pt, max(0, box.w - pl - pr), max(0, box.h - pt - pb))
    if not children_of(node, path):
        return []
    ax = AXIS[spec.direction]
    entries = _entries_of(node, spec, path, inner, inner.h)
    if spec.display == "grid":
        boxes, _cw, _ch = _grid.layout(
            node, spec, inner, entries,
            lambda i, e, aw, ah: (e[3], e[4]))
        gl = []
        for e, x, y, w, h in boxes:
            gl.append(("text", e[1], inner, e[9]) if e[0] is None
                      else ("node", e[0], Box(int(x), int(y), int(w), int(h)), e[9]))
        return gl
    row = spec.direction == "row"
    avail_main = inner.w if row else inner.h
    cross_size = inner.h if row else inner.w
    lines = _split_lines(entries, spec, avail_main, ax)

    # 每行的 cross 尺寸
    line_cross = []
    for ln in lines:
        line_cross.append(max((e[4] + e[7] + e[8] for e in ln), default=0) if row
                          else max((e[3] + e[5] + e[6] for e in ln), default=0))
    total_cross = sum(line_cross) + spec.gap * max(0, len(lines) - 1)

    # align-content：多行时在 cross 轴分配；单行时交给 _place_line 的 align-items
    origin_cross = inner.y if row else inner.x
    if len(lines) > 1:
        cursor, cstep = _justify(spec.content, origin_cross, cross_size, total_cross,
                                 len(lines))
    else:
        cursor, cstep = origin_cross, 0.0

    out: list[tuple[str, object, object, str]] = []
    for idx, ln in enumerate(lines):
        if row:
            _place_line(ln, spec, ax, avail_main, int(line_cross[idx]),
                        inner.x, int(cursor), out)
        else:
            _place_line(ln, spec, ax, avail_main, cross_size, inner.y, int(cursor), out)
        cursor += line_cross[idx] + spec.gap + cstep

    # 文字统一在父内容盒里绘制（对齐/折行相对父盒）
    return [("text", v, inner, p) if k == "text" else (k, v, b, p) for (k, v, b, p) in out]
