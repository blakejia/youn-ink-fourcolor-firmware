"""grid 与 flex 混排 —— canvas 能力全量覆盖里的交叉部分。

flex 路径与 grid 路径是两套代码（layout.py / grid.py），单测各自绿不代表
**混在一起**也对：这里专门钉住"谁在谁里面"的尺寸与对齐传递。
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


# ── grid 在 flex 里 ─────────────────────────────────────────────────

def test_grid_inside_a_flex_row_takes_its_own_track_width():
    b = render(D("flex flex-row w-[380px] h-[40px]",
                 D("w-[80px] bg-black"),
                 D("grid grid-cols-[1fr,1fr] gap-[0px] flex-grow-[1]")))
    grid = at(b, "»[0]»[1]")
    assert grid["w"] == 300, f"grid 子节点应拿到剩余 300，实测 {grid['w']}"
    assert grid["x"] == at(b, "»[0]»[0]")["x"] + 80


def test_grid_and_flex_siblings_share_one_row():
    b = render(D("flex flex-row w-[380px] h-[40px]",
                 D("grid grid-cols-[1fr] w-[100px]"), D("w-[60px] bg-black")))
    assert at(b, "»[0]»[0]")["w"] == 100, "grid 兄弟节点宽度不对"
    assert at(b, "»[0]»[1]")["x"] == at(b, "»[0]»[0]")["x"] + 100, "flex 兄弟未紧随其后"


# ── flex 在 grid 里 ─────────────────────────────────────────────────

def test_flex_row_inside_a_grid_cell_right_aligns_its_number():
    """真实用例：grid 定列宽 + 每格里一条 flex 行做左右对齐。"""
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[0px] h-[24px]",
                 D("flex flex-row justify-between", S("", "标签"), S("", "1,024")),
                 D("flex flex-row justify-between", S("", "另一个"), S("", "9"))))
    cell = at(b, "»[0]»[0]")
    assert cell["w"] == 190, f"单元格宽 {cell['w']}"
    text = [x for x in b if x["type"] == "text" and x["path"].startswith("»[0]»[0]")]
    assert text, "格子里没有文字盒"
    assert text[0]["x"] >= cell["x"], "文字应在单元格内"


def test_flex_wrap_inside_a_grid_cell():
    kids = [D("w-[250px] h-[10px]") for _ in range(4)]
    b = render(D("grid w-[380px] grid-cols-[1fr] gap-[0px] h-[80px]",
                 D("flex flex-row flex-wrap content-start", *kids)))
    ys = {at(b, f"»[0]»[0]»[{i}]")["y"] for i in range(4)}
    assert len(ys) > 1, f"格子里没有换行（y 全相同: {ys}）"


# ── 两层嵌套 ────────────────────────────────────────────────────────

def test_grid_inside_a_grid_cell():
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[0px] h-[40px]",
                 D("grid grid-cols-[1fr,1fr] gap-[0px]",
                   D("bg-black"), D("bg-black")),
                 D("bg-black")))
    inner = at(b, "»[0]»[0]»[1]")
    assert inner["w"] == 95, f"内层 grid 的第二列应为 190/2=95，实测 {inner['w']}"


def test_flex_grid_flex_three_levels():
    b = render(D("flex flex-row w-[380px] h-[40px]",
                 D("grid grid-cols-[1fr] w-[200px]",
                   D("flex flex-row justify-between", S("", "左"), S("", "右"))),
                 D("w-[180px] bg-black")))
    assert at(b, "»[0]»[0]")["w"] == 200
    assert at(b, "»[0]»[1]")["w"] == 180


# ── 单元内对齐与显式尺寸 ────────────────────────────────────────────

def test_grid_item_with_explicit_width_is_not_stretched():
    """CSS：格子里写了宽度的条目按 justify-self 放，不铺满整轨。"""
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[0px] h-[20px]",
                 D("w-[50px] bg-black"), D("bg-black")))
    item = at(b, "»[0]»[0]")
    assert item["w"] == 50, f"显式 50px 的条目被拉伸成 {item['w']}"


def test_grid_item_with_margin_shrinks_inside_the_track():
    b = render(D("grid w-[380px] grid-cols-[1fr] gap-[0px] h-[30px]",
                 D("m-[10px] bg-black")))
    item = at(b, "»[0]»[0]")
    assert item["x"] == 20, f"margin 10 + 容器 padding 10 ⇒ x=20，实测 {item['x']}"
    assert item["w"] <= 360, f"带 margin 的条目不该溢出轨道: {item['w']}"


def test_align_items_center_inside_a_grid_track():
    b = render(D("grid w-[380px] grid-cols-[1fr] gap-[0px] h-[60px] items-center",
                 D("h-[20px] bg-black")))
    item = at(b, "»[0]»[0]")
    assert item["h"] == 20, f"高度 {item['h']}"
    centre = item["y"] + item["h"] / 2
    assert abs(centre - (10 + 30)) <= 2, f"未在轨道内居中: {centre}"


# ── 其它内容 ────────────────────────────────────────────────────────

def test_text_directly_inside_a_grid_cell():
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[0px] h-[24px]",
                 S("", "文字"), D("bg-black")))
    assert at(b, "»[0]»[0]")["w"] == 190, "文字单元格宽度不对"


def test_row_span_takes_two_rows():
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[0px] "
                 "grid-rows-[20px,20px]",
                 D("row-span-[2] bg-black"), D("bg-black"), D("bg-black")))
    tall = at(b, "»[0]»[0]")
    assert tall["h"] == 40, f"row-span-2 应为两行高 40，实测 {tall['h']}"


def test_gap_applies_between_rows_and_columns():
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] grid-rows-[20px,20px] gap-[8px]",
                 D("bg-black"), D("bg-black"), D("bg-black"), D("bg-black")))
    a, second_in_row, third = at(b, "»[0]»[0]"), at(b, "»[0]»[1]"), at(b, "»[0]»[2]")
    assert a["w"] == 186, f"列宽 (380-8)/2 = 186，实测 {a['w']}"
    assert second_in_row["x"] == a["x"] + 186 + 8, f"列间距未生效: {second_in_row['x']}"
    assert third["y"] == a["y"] + 20 + 8, f"行间距未生效: {third['y']}"
