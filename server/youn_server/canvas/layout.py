"""flex 布局：测量 + 摆放。

口径（每条都是踩过坑定下来的，别再改回去）：
  · 所有 div/span 都是 flex 容器，默认 flex-direction:row。
  · stretch 只在子节点**没写** cross-size 时生效（显式尺寸优先）。
  · 子节点尺寸不继承父节点的 w-/h-（父的尺寸描述的是父）。
  · 容器没写尺寸时按内容算；绝不兜底成 100x24。
  · 顶层节点由 renderer 给整屏盒子（400x300），h-full/w-full 才有确定可用区。
  · 文字在**父节点的内容盒**里绘制（对齐/折行都相对父盒）。
  · 主轴尺寸按方向取：row 取宽、column 取高（硬编宽会把 h-full 算成宽）。
  · measure() 返回内容盒尺寸（不含 margin）；margin 由 place() 计。
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from .errors import RenderError
from .text import fit_box, load_font
from .tokens import Spec, parse

_SPECS: dict[int, Spec] = {}


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


def measure(node, avail_w: int, avail_h: int, path: str) -> tuple[int, int]:
    """内容盒尺寸（不含 margin）。"""
    if not isinstance(node, dict):
        raise RenderError(path, "节点必须是对象")
    if node.get("type") not in ("div", "span", "img"):
        raise RenderError(path, f"unsupported element type {node.get('type')!r}")
    spec = spec_of(node, path)
    w, w_exp = _axis(spec, "w", avail_w)
    h, h_exp = _axis(spec, "h", avail_h)
    if node.get("type") == "img":
        return (w or 0), (h or 0)

    kids = children_of(node, path)
    pt, pr, pb, pl = spec.padding
    inner_w = w if w_exp else max(0, avail_w - pl - pr)
    inner_h = h if h_exp else max(0, avail_h - pt - pb)

    sizes: list[tuple[int, int, int, int]] = []          # (w, h, ml+mr, mt+mb)
    for i, kid in enumerate(kids):
        cpath = f"{path}.props.children[{i}]"
        if isinstance(kid, str):
            tw_, th_ = _text_size(kid, spec, inner_w)
            sizes.append((tw_, th_, 0, 0))
        else:
            ksp = spec_of(kid, cpath)
            kw, kh = measure(kid, inner_w, inner_h, cpath)
            mt, mr, mb, ml = ksp.margin
            sizes.append((kw, kh, ml + mr, mt + mb))

    n = len(sizes)
    if spec.direction == "row":
        cw = sum(s[0] + s[2] for s in sizes) + spec.gap * max(0, n - 1)
        ch = max((s[1] + s[3] for s in sizes), default=0)
    else:
        cw = max((s[0] + s[2] for s in sizes), default=0)
        ch = sum(s[1] + s[3] for s in sizes) + spec.gap * max(0, n - 1)

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


def place(node, box: Box, path: str) -> list[tuple[str, object, Box, str]]:
    """把子节点摆进 box → [(kind, node|文字, Box, 子路径)]。"""
    spec = spec_of(node, path)
    kids = children_of(node, path)
    if not kids:
        return []
    pt, pr, pb, pl = spec.padding
    inner = Box(box.x + pl, box.y + pt, max(0, box.w - pl - pr), max(0, box.h - pt - pb))

    row = spec.direction == "row"
    entries = []                       # (kid, text, ksp, w, h, ml, mr, mt, mb, cpath)
    for i, kid in enumerate(kids):
        cpath = f"{path}.props.children[{i}]"
        if isinstance(kid, str):
            tw_, th_ = _text_size(kid, spec, inner.w)
            entries.append((None, kid, None, tw_, th_, 0, 0, 0, 0, cpath))
        else:
            ksp = spec_of(kid, cpath)
            kw, kh = measure(kid, inner.w, inner.h, cpath)
            if kid.get("type") == "img" and (kw == 0 or kh == 0):
                kw = kw or inner.w
                kh = kh or inner.h
            mt, mr, mb, ml = ksp.margin
            entries.append((kid, None, ksp, kw, kh, ml, mr, mt, mb, cpath))

    main_idx, cross_idx = (3, 4) if row else (4, 3)
    m_a, m_b = (5, 6) if row else (7, 8)            # 主轴的 margin 两侧
    c_a, c_b = (7, 8) if row else (5, 6)            # cross 轴的 margin 两侧
    avail_main = inner.w if row else inner.h
    cross_size = inner.h if row else inner.w
    origin_main = inner.x if row else inner.y
    origin_cross = inner.y if row else inner.x

    # 主轴：basis → grow
    bases = [(e[main_idx] if (e[2] is None or e[2].basis is None) else e[2].basis)
             for e in entries]
    used = sum(bases) + sum(e[m_a] + e[m_b] for e in entries)         + spec.gap * max(0, len(entries) - 1)
    free = avail_main - used
    grows = [e[2].grow if e[2] is not None else 0.0 for e in entries]
    if free > 0 and sum(grows) > 0:
        gsum = sum(grows)
        for i, g in enumerate(grows):
            bases[i] += int(free * g / gsum)
    total = sum(bases) + sum(e[m_a] + e[m_b] for e in entries)         + spec.gap * max(0, len(entries) - 1)

    out: list[tuple[str, object, Box, str]] = []
    pos, step = _justify(spec.justify, origin_main, avail_main, total, len(entries))
    for i, e in enumerate(entries):
        kid, text, ksp, _w, _h, ml, mr, mt, mb, cpath = e
        if kid is None:
            out.append(("text", text, Box(int(inner.x), int(inner.y),
                                          int(inner.w), int(inner.h)), cpath))
            continue
        align = spec.items
        if ksp is not None and ksp.self_align != "auto":
            align = ksp.self_align
        main = bases[i]
        pos += e[m_a]
        avail_cross = max(0, cross_size - e[c_a] - e[c_b])
        explicit_cross = bool(ksp and (ksp.explicit_h if row else ksp.explicit_w))
        cross_natural = _h if row else _w
        cross = (avail_cross if (align == "stretch" and not explicit_cross)
                 else min(cross_natural, avail_cross or cross_natural))
        off = 0
        if align == "center":
            off = max(0, (avail_cross - cross) // 2)
        elif align == "flex-end":
            off = max(0, avail_cross - cross)
        if row:
            child = Box(int(pos), int(origin_cross + e[c_a] + off), int(main), int(cross))
        else:
            child = Box(int(origin_cross + e[c_a] + off), int(pos), int(cross), int(main))
        out.append(("node", kid, child, cpath))
        pos += main + e[m_b] + spec.gap + (step if i < len(entries) - 1 else 0)
    return out
