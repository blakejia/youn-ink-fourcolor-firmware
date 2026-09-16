"""flex-shrink / flex-basis / flex-wrap / align-content / 表格标签。

这些是"补齐覆盖"那一批；主轴口径统一在 canvas/layout.py 的 AXIS 里，
本文件只断言**用户可见的几何**，不碰实现细节。
"""
from __future__ import annotations

from youn_server.canvas_render import render_canvas_to_png_debug


def D(tw, *kids):
    return {"type": "div", "props": {"tw": tw, "children": list(kids)}}


def S(tw, *kids):
    return {"type": "span", "props": {"tw": tw, "children": list(kids)}}


def render(root, wrap="flex flex-col p-[10px]"):
    _png, bounds = render_canvas_to_png_debug({"default": [D(wrap, root)]})
    out = []
    for b in bounds:
        b = dict(b)
        b["path"] = (b["path"].replace("windowData.default[0]", "")
                     .replace(".props.children", "»"))
        out.append(b)
    return out


def at(bounds, path):
    for b in bounds:
        if b["path"] == path:
            return b
    raise AssertionError(f"没有 {path}，只有 {[b['path'] for b in bounds]}")


# ── flex-shrink ────────────────────────────────────────────────────

def test_children_shrink_to_fit_a_too_narrow_row():
    b = render(D("flex flex-row w-[400px]", D("w-[300px] h-[20px]", ), D("w-[300px] h-[20px]")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["w"] + c["w"] <= 400, f"两个 300 宽的子节点没有收缩: {a['w']}+{c['w']}"
    assert abs(a["w"] - c["w"]) <= 2, f"等权收缩应大致等宽: {a['w']} vs {c['w']}"
    assert c["x"] + c["w"] <= 410, "收缩后仍溢出容器"


def test_flex_shrink_zero_opts_out():
    b = render(D("flex flex-row w-[400px]",
                 D("w-[300px] h-[20px] flex-shrink-[0]"),
                 D("w-[300px] h-[20px]")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["w"] == 300, f"声明了 flex-shrink-0 却被压缩到 {a['w']}"
    assert c["w"] < 200, f"另一个应承担全部缩量: {c['w']}"


# ── flex-basis ─────────────────────────────────────────────────────

def test_flex_basis_sets_the_main_size():
    b = render(D("flex flex-row w-[400px]", D("flex-basis-[100px] h-[20px]")))
    a = at(b, "»[0]»[0]")
    assert a["w"] == 100, f"flex-basis 应为 100，实测 {a['w']}"


def test_flex_basis_then_grow_splits_the_remainder():
    b = render(D("flex flex-row w-[400px]",
                 D("flex-basis-[100px] flex-grow-[1] h-[20px]"),
                 D("flex-basis-[100px] flex-grow-[1] h-[20px]")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert abs(a["w"] - 200) <= 2 and abs(c["w"] - 200) <= 2, f"basis100+grow ⇒ 200/200，实测 {a['w']}/{c['w']}"


# ── flex-wrap ──────────────────────────────────────────────────────

def test_flex_wrap_makes_new_lines():
    kids = [D("w-[150px] h-[20px]") for _ in range(5)]
    b = render(D("flex flex-row w-[380px] flex-wrap gap-[0px]", *kids))
    rows = sorted({at(b, f"»[0]»[{i}]")["y"] for i in range(5)})
    assert len(rows) == 3, f"5×150 放进 380 应为 3 行，实测 {len(rows)} 行"
    for i in range(5):
        assert at(b, f"»[0]»[{i}]")["x"] >= 9, "每行都应从容器左缘开始"


def test_no_wrap_by_default_keeps_one_line():
    kids = [D("w-[150px] h-[20px]") for _ in range(3)]
    b = render(D("flex flex-row w-[380px]", *kids))
    rows = {at(b, f"»[0]»[{i}]")["y"] for i in range(3)}
    assert len(rows) == 1, "没写 flex-wrap 就不该换行"


# ── align-content ──────────────────────────────────────────────────

def test_align_content_centres_the_lines():
    kids = [D("w-[150px] h-[20px]") for _ in range(4)]
    out = render(D("flex flex-row w-[380px] h-[300px] flex-wrap content-center gap-[0px]",
                   *kids))
    # ys 换算成"相对容器内容盒顶部"的偏移（容器自己高 300，外面还有 p-[10px]）
    ys = [at(out, f"»[0]»[{i}]")["y"] - 10 for i in range(4)]
    top, bottom = min(ys), max(ys) + 20
    gap_top, gap_bottom = top, 300 - bottom
    # 多行确实被移离顶部并大致居中。允许一行高度的偏差：当前实现用
    # 自由空间整除分配，行数为奇数时会差半行（记为技术债，见 layout.place）。
    assert gap_top > 40 and gap_bottom > 40, f"多行仍贴在顶部: 上 {gap_top} 下 {gap_bottom}"
    assert abs(gap_top - gap_bottom) <= 24, f"多行未居中: 上留白 {gap_top} 下留白 {gap_bottom}"


def test_align_content_flex_start_is_default():
    kids = [D("w-[150px] h-[20px]") for _ in range(4)]
    out = render(D("flex flex-row w-[380px] h-[300px] flex-wrap gap-[0px]", *kids))
    assert at(out, "»[0]»[0]")["y"] == 10, "默认应从容器顶部开始排行"


# ── 表格标签 ───────────────────────────────────────────────────────

def test_table_tags_render_as_flex_rows():
    table = {"type": "table", "props": {"tw": "flex flex-col w-full", "children": [
        {"type": "tr", "props": {"tw": "flex flex-row w-full justify-between", "children": [
            {"type": "td", "props": {"tw": "w-[120px]", "children": ["标签"]}},
            {"type": "td", "props": {"tw": "w-[120px] text-right", "children": ["1,024"]}},
        ]}}]}}
    b = render(table)
    left = at(b, "»[0]»[0]»[0]")
    right = at(b, "»[0]»[0]»[1]")
    assert left["w"] == 120 and right["w"] == 120, f"td 宽度 {left['w']}/{right['w']}"
    assert right["x"] + right["w"] - 10 > 350, "单元格没有铺到行右端"
