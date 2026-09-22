"""渲染器支持的能力清单 —— **唯一真相**。

MCP 的 describe_canvas_schema 直接引用本文件，不再各写一份（历史上两份漂移，
新增的 w-full/justify-between 只改了一边，结果 MCP 报了假账）。
"""

CAPABILITIES: dict = {
    "node_types": ["div", "span", "img"],
    "tw_tokens": {
        "layout": ["flex", "flex-row", "flex-col"],
        "size": ["w-[Npx]", "w-[N%]", "w-full", "h-[Npx]", "h-[N%]", "h-full",
                 "min-w-[Npx]", "max-w-[Npx]", "min-h-[Npx]", "max-h-[Npx]",
                 "aspect-[N/MM]"],
        "flex": ["flex-grow-[N]", "flex-1"],
        "spacing": ["gap-[Npx]", "p-[Npx]", "px-[Npx]", "py-[Npx]", "pt-[Npx]",
                    "pr-[Npx]", "pb-[Npx]", "pl-[Npx]", "m-[Npx]", "mx-[Npx]",
                    "my-[Npx]", "mt-[Npx]", "mr-[Npx]", "mb-[Npx]", "ml-[Npx]"],
        "align": ["items-start", "items-center", "items-end", "items-stretch",
                  "items-baseline", "self-start", "self-center", "self-end",
                  "self-stretch"],
        "justify": ["justify-start", "justify-center", "justify-end",
                    "justify-between", "justify-around", "justify-evenly"],
        "text": ["text-[Npx]", "text-center", "text-right", "font-bold",
                 "line-clamp-[N]", "truncate", "whitespace-nowrap",
                 "underline", "line-through",
                 "leading-[Npx]", "tracking-[Npx]"],
        "grid": ["grid", "grid-cols-[<px|%|auto|fr|minmax(a,b)>,…]",
                 "grid-rows-[…]", "col-start-[N]", "col-span-[N]",
                 "row-start-[N]", "row-span-[N]"],
        "decoration": ["bg-{white|black|red|yellow}",
                       "border-{white|black|red|yellow}", "border", "border-[Npx]",
                       "rounded-[Npx]", "overflow-hidden"],
    },
    "style_keys": ["backgroundColor", "color", "borderRadius", "padding",
                   "paddingX", "paddingY", "paddingTop", "paddingRight", "paddingBottom",
                   "paddingLeft", "margin", "marginX", "marginY", "marginTop",
                   "marginRight", "marginBottom", "marginLeft",
                   "width", "height", "minWidth", "maxWidth", "minHeight",
                   "maxHeight", "aspectRatio", "flexGrow", "flexShrink",
                   "flexBasis", "alignSelf", "gap", "border", "borderWidth",
                   "overflow", "fontSize", "fontWeight", "textAlign",
                   "lineClamp", "textOverflow", "textDecoration", "whiteSpace", "lineHeight",
                   "letterSpacing",
                   "display", "gridTemplateColumns", "gridTemplateRows",
                   "gridColumnStart", "gridColumnSpan", "gridRowStart",
                   "gridRowSpan"],
    "text_align": ["left (default)", "center", "right"],
    "not_supported": [
        "绝对定位 / position:absolute / zIndex",
        "CSS Grid 的 repeat() / 命名线 / dense 打包 / subgrid / "
        "minmax 的内容型最大值",
        "order / 百分比 gap",
        "opacity / transform / box-shadow / 渐变 / background-image",
        "多字体族（fontFamily 被忽略）",
        "表格标签 <table>/<tr>/<td>（用 div 组合）",
    ],
}
