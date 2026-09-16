"""画板能力矩阵：文档承诺的能力必须真的生效，且 MCP 与渲染器同源。

历史教训：文档字符串/MCP schema 里写着 m-[Npx]、margin、fontWeight 支持，
渲染器里却根本没有实现（`_parse_margin` 定义后从未被调用），调用方写下去
是**静默无效**的。这些用例把"承诺"钉在"行为"上。
"""

from __future__ import annotations

from youn_server.canvas_render import (
    CAPABILITIES, render_canvas_to_bitmap, render_canvas_to_png_debug,
)
from youn_server.image_conv import SCREEN_W, SCREEN_H

PAD = 10
AVAIL_L, AVAIL_R = PAD, 400 - PAD


def D(tw, *kids):
    return {"type": "div", "props": {"tw": tw, "children": list(kids)}}


def S(tw, *kids, style=None):
    props = {"tw": tw, "children": list(kids)}
    if style:
        props["style"] = style
    return {"type": "span", "props": props}


def render(root, wrap="flex flex-col p-[10px]"):
    _png, bounds = render_canvas_to_png_debug({"default": [D(wrap, root)]}, dither=False)
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


def ink(raw: bytes) -> int:
    """非白像素数。"""
    n = 0
    for y in range(SCREEN_H):
        for xb in range(SCREEN_W // 4):
            byte = raw[y * (SCREEN_W // 4) + xb]
            for xi in range(4):
                if ((byte >> ((3 - xi) * 2)) & 3) != 1:
                    n += 1
    return n


# ─── margin（文档承诺过，此前从未被调用）────────────────────────────

def test_margin_reserves_space_around_a_child():
    b = render(D("flex flex-row w-full gap-[10px]",
                 S("", "AA"), S("m-[10px]", "BB")))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert a["x"] == AVAIL_L
    assert c["x"] - (a["x"] + a["w"]) == 20, "gap 10 + 左边距 10 应为 20"


def test_margin_x_and_y_apply_on_their_own_axes():
    b = render(D("flex flex-col w-full",
                 S("", "第一"),
                 S("mx-[12px] my-[6px]", "第二")))
    first, second = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert second["x"] == first["x"] + 12, "mx 应作用在横轴"
    assert second["y"] == first["y"] + first["h"] + 6, "my 应作用在纵轴"


def test_margin_style_keys_are_honored():
    b = render(D("flex flex-row w-full gap-[10px]",
                 S("", "AA"),
                 S("", "BB", style={"margin": 8, "marginX": 8})))
    a, c = at(b, "»[0]»[0]"), at(b, "»[0]»[1]")
    assert c["x"] - (a["x"] + a["w"]) == 18, "gap 10 + margin 8"


# ─── 字重（文档写了 fontWeight，但没有任何实现）──────────────────────

def test_bold_ink_is_thicker_than_regular():
    plain = ink(render_canvas_to_bitmap(
        {"default": [D("flex flex-col p-[10px]", S("", "加粗对比"))]}, dither=False))
    bold = ink(render_canvas_to_bitmap(
        {"default": [D("flex flex-col p-[10px]", S("font-bold", "加粗对比"))]}, dither=False))
    assert bold > plain * 1.15, f"加粗 {bold} vs 常规 {plain}，粗细没差别"


def test_bold_style_key_is_honored():
    by_token = ink(render_canvas_to_bitmap(
        {"default": [D("flex flex-col p-[10px]", S("font-bold", "粗体"))]}, dither=False))
    by_style = ink(render_canvas_to_bitmap(
        {"default": [D("flex flex-col p-[10px]",
                       S("", "粗体", style={"fontWeight": "bold"}))]}, dither=False))
    assert by_style == by_token, "style.fontWeight 应等价于 font-bold"


# ─── 行内对齐 ───────────────────────────────────────────────────────

def _ink_columns(tw: str, style=None) -> tuple[int, int]:
    """返回 (最左墨迹列, 最右墨迹列)。

    被对齐的盒子必须**显式**给宽度（w-[300px]）：盒子若只有文字那么宽，
    居中/右对齐都无处可去 —— 那样测的是"没生效"还是"本来就没空间"分不清。
    """
    box_tw = f"w-[300px] {tw}".strip()
    canvas = {"default": [D("flex flex-col p-[0px]", S(box_tw, "文字", style=style))]}
    raw = render_canvas_to_bitmap(canvas, dither=False)
    cols = [x for y in range(SCREEN_H) for x in range(SCREEN_W)
            if ((raw[y * (SCREEN_W // 4) + (x // 4)] >> ((3 - (x % 4)) * 2)) & 3) != 1]
    return min(cols), max(cols)


def test_text_center_centres_inside_its_box():
    left, right = _ink_columns("text-center")
    centre = (left + right) / 2
    assert abs(centre - 150) <= 3, f"中心 {centre}，盒子中心应为 150"


def test_text_right_aligns_inside_its_box():
    _l, right = _ink_columns("text-right")
    assert abs(right - 300) <= 3, f"右缘 {right}，盒子右缘应为 300"


def test_text_align_style_key_is_honored():
    assert _ink_columns("", {"textAlign": "right"})[1] == _ink_columns("text-right")[1]


def test_left_is_the_default():
    left, _r = _ink_columns("")
    assert left <= 1, f"默认应左对齐（字形左侧留白 1px 内属正常），实测起始列 {left}"


# ─── 真实用途：表格数字列右对齐 ─────────────────────────────────────

def test_table_number_column_right_aligns_on_one_line():
    """两列标签+数值的表格：所有数值右缘落在同一条竖线上（用户的实际场景）。"""
    def row(label, value):
        return D("flex flex-row w-full justify-between",
                 S("", label),
                 S("w-[120px] text-right", value))

    b = render(D("flex flex-col w-full gap-[6px]",
                 row("一月份产量", "1,024"),
                 row("二月份产量", "18,337"),
                 row("三月份产量", "9")))
    rights = []
    for i in range(3):
        cell = at(b, f"»[0]»[{i}]»[1]")
        rights.append(cell["x"] + cell["w"])
    assert all(abs(r - AVAIL_R) <= 2 for r in rights), f"数值列右缘不齐: {rights}"


# ─── 装饰：边框 / 圆角 / 裁切 ───────────────────────────────────────

def test_rounded_corner_is_clipped_to_white():
    canvas = {"default": [D("flex flex-col p-[0px]",
                            D("w-[100px] h-[100px] bg-black rounded-[40px]"))]}
    raw = render_canvas_to_bitmap(canvas, dither=False)
    def px(x, y):
        return (raw[y * (SCREEN_W // 4) + (x // 4)] >> ((3 - (x % 4)) * 2)) & 3
    assert px(2, 2) == 1, "圆角处应被裁成白色"
    assert px(50, 50) == 0, "中心应仍是黑色"


def test_overflow_hidden_clips_a_child():
    canvas = {"default": [D("flex flex-col p-[0px]",
                            D("w-[50px] h-[50px] overflow-hidden",
                              D("w-[200px] h-[200px] bg-black")))]}
    raw = render_canvas_to_bitmap(canvas, dither=False)
    def px(x, y):
        return (raw[y * (SCREEN_W // 4) + (x // 4)] >> ((3 - (x % 4)) * 2)) & 3
    assert px(20, 20) == 0, "容器内应是黑色"
    assert px(120, 120) == 1, "超出 overflow-hidden 容器的部分应被裁掉"


# ─── 防漂移：MCP 能力清单必须来自渲染器 ─────────────────────────────

def test_mcp_schema_exposes_every_supported_token():
    from youn_server.mcp_server import describe_canvas_schema
    # FastMCP 的 @mcp.tool 会把函数换成 FunctionTool，工具本体在 .fn 上
    schema = describe_canvas_schema.fn()
    flat = " ".join(schema["tw_tokens"])
    missing = [tok for group in CAPABILITIES["tw_tokens"].values() for tok in group
               if tok.split("[")[0] not in flat]
    assert not missing, f"MCP 没告诉调用方这些能力: {missing}"


def test_mcp_schema_lists_the_not_supported_traps():
    from youn_server.mcp_server import describe_canvas_schema
    # FastMCP 的 @mcp.tool 会把函数换成 FunctionTool，工具本体在 .fn 上
    schema = describe_canvas_schema.fn()
    assert schema.get("not_supported"), "MCP 应显式列出不支持的写法"
    for trap in CAPABILITIES["not_supported"]:
        assert trap in schema["not_supported"], f"MCP 漏报不支持项: {trap}"
    assert any("z-index" in t for t in schema["not_supported"]), "API 层的不支持项也不该丢"
