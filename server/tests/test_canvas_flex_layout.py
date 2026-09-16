"""flex 布局：可用宽度传递、stretch 与显式尺寸、justify-* 自由空间分配。

现场问题（400x300 四色面板）：行容器的 justify-* 落在错误的窄盒里，表格类
页面无法把数字列右对齐到面板边缘。根因是三条：

1. `_measure_node` 拿不到父容器可用宽度 —— `w-full` 不解析，一律退化成
   100x24 兜底值（于是子节点自带 57px 幽灵内边距，右对齐后可见文字够不到边）。
2. `align-items: stretch`（CSS 默认）**覆盖**子节点显式写下的宽/高，
   而 CSS 里 stretch 只对 cross-size 为 auto 的子节点生效。
3. `justify-between` 的自由空间从未分配，静默退化成 flex-start。
"""

from __future__ import annotations

import pytest

from youn_server.canvas_render import render_canvas_to_png_debug

PAD = 10
AVAIL_L = PAD          # p-[10px] 下的可用区左缘
AVAIL_R = 400 - PAD    # 右缘


def D(tw, *kids):
    return {"type": "div", "props": {"tw": tw, "children": list(kids)}}


def S(tw, *kids):
    return {"type": "span", "props": {"tw": tw, "children": list(kids)}}


def render(root, wrap="flex flex-col p-[10px]"):
    """wrap 默认列容器；测「主轴宽度」时必须用行容器，否则 stretch 会替 w-full 蒙对。"""
    canvas = {"default": [D(wrap, root)]}
    _png, bounds = render_canvas_to_png_debug(canvas, dither=False)
    out = []
    for b in bounds:
        b = dict(b)
        b["path"] = (b["path"].replace("windowData.default[0]", "")
                     .replace(".props.children", "»"))
        out.append(b)
    return out


def texts(bounds):
    return [b for b in bounds if b["type"] == "text"]


def node_at(bounds, path):
    for b in bounds:
        if b["path"] == path:
            return b
    raise AssertionError(f"没有 {path} 这一层，只有 {[b['path'] for b in bounds]}")


# ─── 可用宽度：w-full ────────────────────────────────────────────────

def test_w_full_row_fills_available_width():
    """w-full 必须撑满父容器可用宽度。

    放在**行**父容器里考：行方向是主轴，stretch 帮不上忙 —— 否则这条会
    靠"列容器把一切拉满"蒙对，测不出 w-full 到底解析没解析。
    """
    b = render(D("flex flex-row w-full", S("", "BBBB")), wrap="flex flex-row p-[10px]")
    row = node_at(b, "»[0]")
    assert row["x"] == AVAIL_L
    assert row["w"] == AVAIL_R - AVAIL_L, f"行宽 {row['w']}，应为 {AVAIL_R - AVAIL_L}"


def test_w_full_column_child_fills_available_width():
    b = render(D("flex flex-col w-full", S("", "BBBB")))
    col = node_at(b, "»[0]")
    assert col["w"] == AVAIL_R - AVAIL_L


def test_h_full_uses_available_height():
    """h-full 同理：300 - 上下各 10 = 280。用列容器考（高度是主轴，不会 stretch）。"""
    b = render(D("flex flex-col h-full w-full", S("", "BBBB")))
    row = node_at(b, "»[0]")
    assert row["h"] == 300 - 2 * PAD, f"高 {row['h']}"


# ─── stretch 不得覆盖显式尺寸 ────────────────────────────────────────

def test_stretch_does_not_override_explicit_width():
    """列容器里子节点显式 w-[150px] 必须赢过 align-items: stretch。"""
    b = render(D("flex flex-col w-full",
                 D("flex flex-row w-[150px] justify-end", S("", "BBBB"))))
    row = node_at(b, "»[0]»[0]")
    assert row["w"] == 150, f"显式 w-[150px] 被 stretch 覆盖成 {row['w']}"


def test_stretch_does_not_override_explicit_height():
    b = render(D("flex flex-row w-full h-full", D("h-[40px]", S("", "BBBB"))))
    child = node_at(b, "»[0]»[0]")
    assert child["h"] == 40, f"显式 h-[40px] 被 stretch 覆盖成 {child['h']}"


def test_auto_size_child_still_stretches():
    """没写尺寸的子节点仍应被 stretch 撑满（CSS 默认行为，不能修坏）。"""
    b = render(D("flex flex-col w-full", D("flex flex-row", S("", "BBBB"))))
    child = node_at(b, "»[0]»[0]")
    assert child["w"] == AVAIL_R - AVAIL_L


# ─── justify-* 自由空间分配 ──────────────────────────────────────────

def test_justify_end_puts_child_right_edge_at_available_right():
    """用户验收 ①：w-full + justify-end ⇒ 子元素右缘落在 390±2。"""
    b = render(D("flex flex-row w-full justify-end", S("", "BBBB")))
    child = node_at(b, "»[0]»[0]")
    right = child["x"] + child["w"]
    assert abs(right - AVAIL_R) <= 2, f"右缘 {right}，应为 {AVAIL_R}"


def test_justify_between_pins_both_ends():
    """用户验收 ②：w-full + justify-between ⇒ 两端分别贴 10 与 390±2。"""
    b = render(D("flex flex-row w-full justify-between",
                 D("flex flex-row", S("", "AAAA")),
                 D("flex flex-row", S("", "BBBB"))))
    first = node_at(b, "»[0]»[0]")
    last = node_at(b, "»[0]»[1]")
    assert first["x"] == AVAIL_L, f"左端 {first['x']}"
    right = last["x"] + last["w"]
    assert abs(right - AVAIL_R) <= 2, f"右端 {right}，应为 {AVAIL_R}"
    assert first["x"] + first["w"] < last["x"], "两个子元素重叠了"


def test_justify_center_centres_the_group():
    b = render(D("flex flex-row w-full justify-center", S("", "BBBB")))
    child = node_at(b, "»[0]»[0]")
    centre = child["x"] + child["w"] / 2
    assert abs(centre - 200) <= 2, f"中心 {centre}，应为 200"


def test_justify_center_of_two_children_centres_the_pair():
    b = render(D("flex flex-row w-full justify-center gap-[10px]",
                 S("", "AAAA"), S("", "BBBB")))
    a = node_at(b, "»[0]»[0]")
    c = node_at(b, "»[0]»[1]")
    left, right = a["x"], c["x"] + c["w"]
    assert abs((left + right) / 2 - 200) <= 2, f"组合中心 {(left + right) / 2}"


def test_justify_end_in_explicit_width_box():
    """显式宽度盒子里的 justify-end 也必须贴右缘（用户 T1）。"""
    b = render(D("flex flex-col w-full",
                 D("flex flex-row w-[150px] justify-end", S("", "BBBB"))))
    row = node_at(b, "»[0]»[0]")
    child = node_at(b, "»[0]»[0]»[0]")
    right = child["x"] + child["w"]
    # 加这条才能测出"对齐"：子节点若被撑成整行宽，右缘恒等于盒右缘（假通过）
    assert child["w"] < row["w"] - 10, f"子节点宽 {child['w']} 几乎等于行宽 {row['w']}，是在考对齐还是在考拉伸？"
    assert abs(right - (row["x"] + row["w"])) <= 2, f"右缘 {right} vs 盒右缘 {row['x'] + row['w']}"


# ─── 固有尺寸：容器不再退化成 100x24 ─────────────────────────────────

def test_text_node_width_is_ink_width_not_fallback():
    """子节点宽度取文字真实宽度 —— 兜底 100 会造出幽灵内边距，右对齐够不到边。"""
    b = render(D("flex flex-row w-full justify-end", S("", "BBBB")))
    span = node_at(b, "»[0]»[0]")
    assert span["w"] < 80, f"span 宽 {span['w']}，像是退化成了兜底值"
    assert span["w"] > 20, f"span 宽 {span['w']}，不像是文字宽度"


def test_column_container_height_is_content_height():
    """列容器高度应为内容高之和，而不是 24 兜底值。"""
    b = render(D("flex flex-col w-full gap-[8px]",
                 S("", "第一行"), S("", "第二行")))
    col = node_at(b, "»[0]")
    assert col["h"] > 30, f"列高 {col['h']}，像是 24 兜底值"


def test_text_ink_is_where_the_box_says():
    """盒子权利与墨迹一致：右对齐后，墨迹最后一列应贴近盒右缘。"""
    from youn_server.canvas_render import render_canvas_to_bitmap
    from youn_server.image_conv import SCREEN_W, SCREEN_H

    canvas = {"default": [D("flex flex-col p-[10px]",
                            D("flex flex-row w-full justify-end", S("", "BBBB")))]}
    raw = render_canvas_to_bitmap(canvas, dither=False)
    rightmost = 0
    for y in range(SCREEN_H):
        for xb in range(SCREEN_W // 4):
            byte = raw[y * (SCREEN_W // 4) + xb]
            for xi in range(4):
                if ((byte >> ((3 - xi) * 2)) & 3) != 1:  # 1 = 白
                    rightmost = max(rightmost, xb * 4 + xi)
    assert rightmost >= AVAIL_R - 12, f"墨迹最右 {rightmost}，离 {AVAIL_R} 太远"


# ─── 不回归 ─────────────────────────────────────────────────────────

def test_explicit_width_then_sibling_starts_after_gap():
    """用户 T7 对照：显式宽度生效，下一个子元素紧随其后。"""
    b = render(D("flex flex-row w-full",
                 D("w-[150px]", S("", "AAAA")),
                 D("flex flex-row", S("", "BBBB"))))
    first = node_at(b, "»[0]»[0]")
    second = node_at(b, "»[0]»[1]")
    assert first["w"] == 150
    assert second["x"] == first["x"] + 150


def test_unsupported_justify_value_falls_back_to_start():
    b = render(D("flex flex-row w-full justify-stretch", S("", "BBBB")))
    child = node_at(b, "»[0]»[0]")
    assert child["x"] == AVAIL_L


def test_gap_still_applies():
    b = render(D("flex flex-row w-full gap-[20px]", S("", "AA"), S("", "BB")))
    a = node_at(b, "»[0]»[0]")
    c = node_at(b, "»[0]»[1]")
    assert c["x"] - (a["x"] + a["w"]) == 20, "gap 失效"


@pytest.mark.parametrize("justify,expect_left", [("start", AVAIL_L)])
def test_justify_start_is_default(justify, expect_left):
    b = render(D(f"flex flex-row w-full justify-{justify}", S("", "BBBB")))
    assert node_at(b, "»[0]»[0]")["x"] == expect_left
