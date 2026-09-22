"""CSS Grid 子集：轨道定尺 + 行优先放置。

支持（够用的那部分）：`grid-template-columns/rows` 里的 px / % / auto / fr /
minmax(a,b)；行优先自动放置；`col-start` / `col-span` / `row-span`；`gap`。
不支持（如实标注）：`repeat()`、命名线、dense 打包、subgrid、`minmax` 的内容型最大值。

定尺是一趟做完：固定轨道先占位 → auto 吃内容 max → 余下按 fr 比例分。
放置是游标扫描 + 占位表，跳过已占的格子。
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Optional

_PX = re.compile(r"^(-?\d+)px$")
_PCT = re.compile(r"^(-?\d+(?:\.\d+)?)%$")
_FR = re.compile(r"^(-?\d+(?:\.\d+)?)fr$")
_MINMAX = re.compile(r"^minmax\((.+),(.+)\)$")


@dataclass
class Track:
    kind: str            # px | pct | fr | auto | minmax
    value: float = 0.0
    lo: Optional["Track"] = None
    hi: Optional["Track"] = None


def parse_tracks(specs) -> list[Track]:
    out = []
    for raw in specs or []:
        s = str(raw).strip().lower()
        m = _MINMAX.match(s)
        if m:
            out.append(Track("minmax", lo=parse_one(m.group(1)), hi=parse_one(m.group(2))))
            continue
        out.append(parse_one(s) or Track("auto"))
    return out


def parse_one(s: str) -> Optional[Track]:
    s = s.strip()
    if s == "auto":
        return Track("auto")
    m = _PX.match(s)
    if m:
        return Track("px", float(m.group(1)))
    m = _PCT.match(s)
    if m:
        return Track("pct", float(m.group(1)))
    m = _FR.match(s)
    if m:
        return Track("fr", float(m.group(1)))
    return None


def _flat(t: Track) -> tuple[str, float]:
    if t.kind == "minmax":
        return (t.hi.kind if t.hi else "auto"), (t.hi.value if t.hi else 0.0)
    return t.kind, t.value


def resolve(tracks: list[Track], total: Optional[int], content: list[int],
            gap: int) -> list[int]:
    """一趟定尺。total=None 时按内容算（用于测量固有尺寸）。"""
    n = len(tracks)
    sizes = [0] * n
    flexible: list[tuple[int, float]] = []
    fixed_used = 0
    for i, t in enumerate(tracks):
        kind, value = _flat(t)
        if t.kind == "minmax" and t.lo is not None:
            lk, lv = (t.lo.kind, t.lo.value)
            floor = int(lv) if lk == "px" else (int(total * lv / 100) if lk == "pct" and total else 0)
            floor = max(floor, content[i] if lk == "auto" else 0)
        else:
            floor = 0
        if kind == "px":
            sizes[i] = max(int(value), floor)
            fixed_used += sizes[i]
        elif kind == "pct" and total is not None:
            sizes[i] = max(int(total * value / 100), floor)
            fixed_used += sizes[i]
        elif kind == "fr":
            flexible.append((i, max(value, 0.0)))
            sizes[i] = floor
            fixed_used += floor
        else:                                   # auto（或 total 未知时的 pct）
            sizes[i] = max(content[i], floor)
            fixed_used += sizes[i]
    if total is not None:
        free = total - fixed_used - gap * max(0, n - 1)
        if free > 0 and flexible:
            fsum = sum(f for _i, f in flexible) or 1.0
            given = 0
            for i, f in flexible:
                add = int(free * f / fsum)
                sizes[i] += add
                given += add
            sizes[flexible[-1][0]] += free - given      # 余数给最后一个，避免累积误差
        elif free > 0:
            # 没有 fr 轨道时：auto 轨道按 CSS 默认的 stretch 行为填满剩余空间
            autos = [i for i, t in enumerate(tracks) if _flat(t)[0] == "auto"]
            if autos:
                each = free // len(autos)
                for i in autos:
                    sizes[i] += each
                sizes[autos[-1]] += free - each * len(autos)
    return sizes


def _axis_offsets(sizes: list[int], gap: int, origin: int,
                   total: Optional[int], mode: str) -> list[int]:
    """轨道在自由空间里的分布（CSS justify-content / align-content 语义）。

    total=None（轴不确定）或 free<=0（fr/auto 已吃满）都退化为贴原点，
    与 flex 的 _justify 同一套模式词：center / flex-end / space-*
    （start 与 space-* 单轨等价于 start）。
    """
    used = sum(sizes) + gap * max(0, len(sizes) - 1)
    free = max(0, (total - used) if total is not None else 0)
    start, extra = origin, 0.0
    if mode == "center":
        start = origin + free // 2
    elif mode in ("flex-end", "end"):
        start = origin + free
    elif mode == "space-between" and len(sizes) > 1:
        extra = free / (len(sizes) - 1)
    elif mode == "space-around" and sizes:
        step = free / len(sizes)
        start, extra = origin + int(step / 2), step
    elif mode == "space-evenly" and sizes:
        step = free / (len(sizes) + 1)
        start, extra = origin + int(step), step
    xs: list[int] = []
    cur = float(start)
    for i, s in enumerate(sizes):
        xs.append(int(cur))
        cur += s + gap + (extra if i < len(sizes) - 1 else 0)
    return xs


def layout(node, spec, inner, entries, measure_child,
           definite=(True, True)) -> tuple[list, int, int]:
    """返回 (每个 entry 的 Box, 内容宽, 内容高)。

    entries: (kid, text, ksp, w, h, ml, mr, mt, mb, cpath)
    measure_child(idx, avail_w, avail_h, cpath) → (w, h)
    """
    gap_x = spec.gap_x if spec.gap_x is not None else spec.gap
    gap_y = spec.gap_y if spec.gap_y is not None else spec.gap
    cols = parse_tracks(spec.grid_cols) or [Track("fr", 1.0)]
    ncols = len(cols)
    col_span_of = [((e[2].col_span if e[2] else None) or 1) for e in entries]
    col_start_of = [(e[2].col_start if e[2] else None) for e in entries]

    # ── 放置（行优先 + 占位表）──
    occupied: set[tuple[int, int]] = set()
    cells: list[tuple[int, int]] = []
    r = c = 0

    def fits(r0: int, c0: int, sr: int, sc: int) -> bool:
        if c0 + sc > ncols:
            return False
        return all((r0 + dr, c0 + dc) not in occupied
                   for dr in range(sr) for dc in range(sc))

    for i, e in enumerate(entries):
        sr = ((e[2].row_span if e[2] else None) or 1)
        sc = col_span_of[i] or 1
        if col_start_of[i]:
            c = int(col_start_of[i]) - 1
            while not fits(r, c, sr, sc):
                r += 1
        else:
            guard = 0
            while not fits(r, c, sr, sc):
                c += 1
                if c + sc > ncols:
                    c, r = 0, r + 1
                guard += 1
                if guard > 4096:
                    break
        for dr in range(sr):
            for dc in range(sc):
                occupied.add((r + dr, c + dc))
        cells.append((r, c))
        c += sc
        if c >= ncols:
            c, r = 0, r + 1

    nrows = max((cells[i][0] + ((entries[i][2].row_span if entries[i][2] else 1) or 1)
                 for i in range(len(entries))), default=1)
    rows = parse_tracks(spec.grid_rows)
    while len(rows) < nrows:
        rows.append(Track("auto"))

    # ── 轨道的内容贡献 ──
    child_wh = [measure_child(i, e, inner.w, inner.h) for i, e in enumerate(entries)]
    col_content = [0] * ncols
    for i, (cr, cc) in enumerate(cells):
        w = child_wh[i][0] + (entries[i][5] + entries[i][6])
        share = -(-w // max(1, col_span_of[i]))          # 跨列时均摊
        for dc in range(col_span_of[i]):
            if cc + dc < ncols:
                col_content[cc + dc] = max(col_content[cc + dc], share)
    row_content = [0] * len(rows)
    for i, (cr, _cc) in enumerate(cells):
        h = child_wh[i][1] + (entries[i][7] + entries[i][8])
        row_content[cr] = max(row_content[cr], h)

    # 只有**自身显式**定尺寸的轴才把 auto 轨道拉伸填满（CSS 的 stretch 默认行为）。
    # 用父容器的可用尺寸当"确定尺寸"会把 auto 高的 grid 撑高 —— 实测会把
    # 内容高 24/60 的行拉成 122/158。
    definite_w = inner.w if (definite[0] and inner.w) else None
    definite_h = inner.h if (definite[1] and inner.h) else None
    col_sizes = resolve(cols, definite_w, col_content, gap_x)
    row_sizes = resolve(rows, definite_h, row_content, gap_y)
    xs = _axis_offsets(col_sizes, gap_x, inner.x, definite_w, spec.justify)
    ys = _axis_offsets(row_sizes, gap_y, inner.y, definite_h, spec.content)

    out = []
    for i, (cr, cc) in enumerate(cells):
        e = entries[i]
        ksp = e[2]
        sr = ((ksp.row_span if ksp else None) or 1)
        sc = col_span_of[i]
        cell_x, cell_y = xs[cc], ys[cr]
        cell_w = sum(col_sizes[cc:cc + sc]) + gap_x * (sc - 1)
        cell_h = sum(row_sizes[cr:cr + sr]) + gap_y * (sr - 1)

        # 条目在轨道内的尺寸与对齐（CSS：显式尺寸优先；否则行内轴 stretch、
        # 块轴按 align-self/items 拉伸或居中）—— 含 margin 内缩。
        mt, mr, mb, ml = e[7], e[6], e[8], e[5]
        avail_w = max(0, cell_w - ml - mr)
        avail_h = max(0, cell_h - mt - mb)
        natural_w, natural_h = child_wh[i][0], child_wh[i][1]
        align = (ksp.self_align if (ksp and ksp.self_align != "auto") else spec.items)
        w = (min(natural_w, avail_w) if ksp is not None and ksp.explicit_w else avail_w)
        if ksp is not None and ksp.explicit_h:
            h = min(natural_h, avail_h) if avail_h else natural_h
        elif align == "stretch":
            h = avail_h
        else:
            h = min(natural_h, avail_h) if avail_h else natural_h
        off_y = 0
        if align == "center":
            off_y = max(0, (avail_h - h) // 2)
        elif align == "flex-end":
            off_y = max(0, avail_h - h)
        # 行内轴：justify-items（默认 stretch 已体现在 w=avail_w 上）。
        ji = spec.justify_items
        off_x = 0
        if ji == "center":
            off_x = max(0, (avail_w - w) // 2)
        elif ji == "flex-end":
            off_x = max(0, avail_w - w)
        out.append((e, int(cell_x + ml + off_x), int(cell_y + mt + off_y),
                    int(w), int(h)))

    content_w = sum(col_sizes) + gap_x * max(0, ncols - 1)
    content_h = sum(row_sizes) + gap_y * max(0, nrows - 1)
    return out, content_w, content_h
