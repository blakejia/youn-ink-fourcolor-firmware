"""CSS Grid 子集：轨道定尺（px/%/auto/fr）+ 行优先放置 + span。

与 flex 的分水岭：**列宽由轨道决定，不随内容变化**（flex 是内容驱动）。
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


GAP0 = "gap-[0px]"


def test_two_equal_columns_split_the_width():
    b = render(D(f"grid w-[380px] grid-cols-[1fr,1fr] {GAP0} h-[40px]",
                 D("bg-black"), D("bg-black"), D("bg-black"), D("bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["w"] == 190 and c["w"] == 190, f"1fr/1fr 应为 190/190，实测 {a['w']}/{c['w']}"
    assert c["x"] == a["x"] + 190, f"第二列起点 {c['x']}"
    assert at(b, "»[0]»[2]")["y"] > a["y"], "第 3 个应换到第二行"


def test_fixed_then_flexible_column():
    b = render(D(f"grid w-[380px] grid-cols-[80px,1fr] {GAP0} h-[40px]",
                 D("bg-black"), D("bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["w"] == 80, f"固定列 {a['w']}"
    assert c["w"] == 300, f"弹性列 {c['w']}"
    assert c["x"] == a["x"] + 80


def test_columns_do_not_follow_content():
    """grid 的列宽由轨道决定 —— 内容只有 1 个字符也不缩到内容宽。"""
    b = render(D(f"grid w-[380px] grid-cols-[1fr,1fr] {GAP0} h-[40px]",
                 S("", "短"), D("bg-black")))
    a = at(b, "»[0]»[0]")
    assert a["w"] == 190, f"内容很短，列仍应为 190，实测 {a['w']}"


def test_col_span_covers_two_tracks():
    b = render(D(f"grid w-[380px] grid-cols-[1fr,1fr] {GAP0} h-[80px] grid-rows-[20px,20px]",
                 D("col-span-[2] bg-black"), D("bg-black")))
    wide = at(b, "»[0]»[0]")
    assert wide["w"] == 380, f"span 2 应为 380，实测 {wide['w']}"
    nxt = at(b, "»[0]»[1]")
    assert nxt["y"] > wide["y"], "span 之后的条目应换到下一行"


def test_gap_between_tracks():
    b = render(D("grid w-[380px] grid-cols-[1fr,1fr] gap-[10px] h-[40px]",
                 D("bg-black"), D("bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    # 两列各 (380-10)/2 = 185，第二列从 10+185+10 开始
    assert a["w"] == 185, f"列宽 {a['w']}"
    assert c["x"] == a["x"] + 185 + 10, f"gap 未生效: {c['x']}"


def test_col_start_places_in_a_specific_track():
    b = render(D(f"grid w-[380px] grid-cols-[1fr,1fr] {GAP0} h-[40px]",
                 D("col-start-[2] bg-black")))
    a = at(b, "»[0]»[0]")
    assert a["x"] == 10 + 190, f"col-start-[2] 应落在第二列，实测 x={a['x']}"


def test_fixed_row_heights():
    b = render(D(f"grid w-[380px] grid-cols-[1fr] {GAP0} grid-rows-[20px,50px]",
                 D("bg-black"), D("bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["h"] == 20, f"第一行 {a['h']}"
    assert c["h"] == 50, f"第二行 {c['h']}"


def test_auto_rows_follow_content():
    b = render(D(f"grid w-[380px] grid-cols-[1fr] {GAP0}",
                 D("h-[24px] bg-black"), D("h-[60px] bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["h"] == 24 and c["h"] == 60, f"auto 行应取内容高: {a['h']}/{c['h']}"
    assert c["y"] == a["y"] + 24, "第二行紧接第一行"


def test_percent_track():
    b = render(D(f"grid w-[400px] grid-cols-[25%,75%] {GAP0} h-[40px]",
                 D("bg-black"), D("bg-black")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["w"] == 100 and c["w"] == 300, f"25%/75% 实测 {a['w']}/{c['w']}"
