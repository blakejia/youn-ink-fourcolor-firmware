# 上传绑定页面（upload → page binding）设计

日期：2026-09-12
状态：待复核

## 问题（已取证）

Web 端的「上传图片」今天**到不了设备**，这不是配置问题而是断路：

- `POST /api/images`（`server/youn_server/app.py:347-387`）把图片转成 1bpp/2bpp 帧缓冲，写到 `server/data/images/{随机 id}.bin`，然后试图通过**活跃 WebSocket 会话**推给设备（`_push_image_to_device`，`app.py:142-180`）。
- 而当前固件**不建立 WS 会话**，也没有 `image_push_meta`/`image_push_done` 的处理器（`firmware/main/rust/src/page_sync.rs` 无任何匹配；`protocol.py:83-84` 只是消息定义）。
- 那批字节**没有任何 HTTP GET 能取到**（`data/images/` 不对外）。

设备唯一能上屏的路径是页面轮询：operator 建页面（`POST /api/pages`，canvas_json）→ 服务端渲染成 30000 字节 2bpp 位图 → `data/pages/{md5}.bin` → 设备 `GET /api/pages/schedule` + `GET /api/pages/bitmap/{md5}.bin`（`page_sync.rs:155-208`）→ 上屏。所以"上传后不知道怎么推送到设备"的直接原因就是：**上传从来没有和页面绑定过**。

## 目标

上传一张图片时**必须指定它替换哪个已有页面**，服务端把图片自动做成该页面的画板并重新渲染位图；页面的名字、顺序、时长不变，因此轮播位置不动，设备在下一轮轮询看到新 md5 后自动重绘。

## 非目标

- 不从上传**新建**页面（只做替换；新建继续走页面管理页）。
- 不使用 S3/CDN（用户已明确本轮不做）。
- 不做"上传后立刻轮到这一页"。轮播时机由服务端 epoch 循环权威决定（`pages.py:304-326`），单页最长要等一轮。这是个独立小功能。
- 不做多图/画布编辑（上传即整页一图）。

## 设计

### 1. 数据流

```
Web: 选页面 ▾ + 选图片 → POST /api/uploads (multipart, operator token)
  → 校验页面存在（不存在 ⇒ 400 + 可选页面列表）      ← "必须绑定"的强制点
  → 原图落盘 data/uploads/{id}.{ext}
  → 生成 canvas_json：一块铺满的 img 节点，src = uploads://{id}
  → 现状复用（与 POST /api/pages::create_page 同一形状，app.py:580-604）：
       bitmap = render_canvas_to_bitmap(canvas_json)          (canvas_render.py:585)
       upsert_page(name, canvas_json, duration_minutes, order, bitmap)  (pages.py:141)
     duration_minutes 与 order 从**该页现有 {name}.json 读出**，原样传回 ⇒ 逐字节不变
  → data/pages/{新 md5}.bin + 更新 {name}.json
  → 返回 {page, md5, duration_minutes, order}

设备：下一轮 GET /api/pages/schedule 看到该页新 md5
  → GET /api/pages/bitmap/{新 md5}.bin → 全刷上屏
```

### 2. 服务端

**新端点** `POST /api/uploads`（operator 认证，同 `_require_operator`）

- multipart 字段：`image`（文件）、`page`（目标页面名，必填）。
- 校验顺序：`page` 非空且存在于 `pages.py` 的页面列表 → 否则 `400 {"detail": "...", "pages": [...]}`；`image` 可解码为 PNG/JPEG/WebP → 否则 `400`；解码后像素数上限（防解压炸弹，如 ≤ 4000×4000）。
- 落盘：`data/uploads/{id}.{ext}`，`id` = 128 位随机十六进制（capability 风格，与现有 md5 寻址的位图一致）。
- canvas 生成（唯一的新"渲染"逻辑，很薄）：

  ```json
  {"width": 400, "height": 300, "background": "#ffffff",
   "children": [{"type": "img", "src": "uploads://<id>",
                 "x": 0, "y": 0, "w": 400, "h": 300}]}
  ```

  适配方式 **contain**：等比缩放到 400×300 之内、居中、四周补白（数据不丢）。
- 复用 `upsert_page`：`name`/`duration_minutes`/`order` 原样保留（读现有 `{name}.json` 后只替换 `canvas_json`）。

**渲染器改动**：`canvas_render._load_image`（`canvas_render.py:535-549`）目前只接受 `data:` 与 `http(s)`。新增 `uploads://<id>` 分支，直接读 `data/uploads/`（`config.py:51` 声明的 `uploads_dir` 目前**从未被使用**，正好启用）。

> 为什么不是 http URL：浏览器侧不需要直接取这张图（编辑器预览走服务端 `/api/pages/preview`）。用内部 scheme 同样满足"页面 JSON 小 + 原图保留"，且少一个端点、少一个授权面。若以后需要浏览器直接访问，再加一个只读端点即可。

**删除的死路**（已核实无活引用）：`POST/GET/DELETE /api/images`（`app.py:347-405`）、`POST /api/push_image` 别名（`414-419`）、`_push_image_to_device`（`142-180`）、`session.py` 的 `pending_pushes`/`LIST_IMAGES` 分派、`protocol.py` 的 `IMAGE_PUSH_META/IMAGE_PUSH_DONE`、`image_conv.py`（只被 `/api/images` 使用）、`data/images/` 目录。

### 2b. 接口契约

`POST /api/uploads`（operator：`X-Operator-Token`，同其它管理端点）

| 情况 | 响应 |
|---|---|
| 请求 | `multipart/form-data`：`image`（文件，PNG/JPEG/WebP）、`page`（目标页面名，必填） |
| 200 | `{"page": "<name>", "md5": "<32hex>", "duration_minutes": <int>, "order": <int>}` |
| 400 | `page` 为空或不存在 ⇒ `{"detail": "...", "pages": ["<现有页面名>", ...]}` |
| 400 | 图片无法解码，或解码后超过 4000×4000 像素（防解压炸弹） |
| 401/403 | operator token 缺失/错误（沿用 `_require_operator`） |
| 500 | 渲染失败——页面保持原样（先渲染后写盘） |

`uploads://<id>` 是**渲染器内部引用，不是 HTTP 端点**：`id` 必须匹配 `^[0-9a-f]{32}$`，拼接后的路径必须落在 `data/uploads/` 内（与 `pages.py:216-226` 的 confine 纪律一致）。

被删除的接口：`POST /api/images`、`GET /api/images`、`DELETE /api/images/{id}`、`POST /api/push_image`。

### 3. Web 端

上传卡片（现有 `frontend/src/pages/Images.jsx`，导航标签改为「替换页面画面」）：

- 「目标页面 ▾」下拉，数据来自现有 `GET /api/pages`。
- 文件选择 + 上传；**移除** `format`（1bpp/bwry2bpp）与 `target_device_id`——两者都属于已删除的 WS 推送路径；canvas 渲染固定产出 2bpp。
- 成功提示：「已替换页面「X」，设备将在下一轮轮询后更新（该页 md5 已变化）」。
- **移除该页原来的图片列表**：图片不再是独立实体，页面列表已经由 `frontend/src/pages/Pages.jsx` 管理；同一个东西维护两份列表是上一版混乱的来源之一。删除/排序仍在 Pages 页做。

不存在"图片实体"之后，`api.js` 里的 `uploadImage`/`deleteImage` 改为新的上传接口与页面删除，`GET /api/images` 不再被调用。

## 错误处理与不变量

- **必须绑定**：`page` 缺失或不存在 ⇒ 400，不写任何文件（先校验后落盘）。
- **渲染失败不破坏页面**：位图是内容寻址的，新的 md5 写不进去时旧位图与旧 `{name}.json` 保持原样；上传的 `data/uploads/{id}` 成为孤儿（可接受，后续可加 GC）。
- **页面身份不变**：`name`/`order`/`duration_minutes` 不变 ⇒ schedule 位置不变，设备不需要重新拉整表（`schedule_md5` 会变，因为它含各页 md5——这是预期的，正是它触发重绘）。
- `GET /api/uploads/*` 不存在，所以没有新的公网读面。

## 测试

服务端 pytest（沿用现有 conftest 的 operator token 隔离方式）：

1. 未指定页面 / 页面不存在 ⇒ 400，且 `data/uploads/` 与 `data/pages/` 均无新文件。
2. 指定一个真实页面 + 一张图 ⇒ 200；该页 `{name}.json` 的 `canvas_json` 变成上传生成的 img 画板；`order`/`duration_minutes` 与替换前**逐字节相同**；`data/pages/{新 md5}.bin` 恰好 30000 字节。
3. 同一页面连续上传两张不同图 ⇒ md5 变化、页面条目数不变（不产生重复页面）。
4. 非图片字节 ⇒ 400 且页面未变。
5. 已删除的 `/api/images` 返回 404（回归护栏，防止有人加回来）。

Web：`npm run build` 干净 + 上传对话框冒烟（选择页面、上传、提示）。

设备侧：替换后用现有取证方式确认——服务器日志出现该页新 md5 的 `GET /api/pages/bitmap/{md5}.bin`，且设备日志出现 `show page`。

## 风险

- `_load_image` 新增内部 scheme 属渲染器边界改动，需确保路径拼接不越出 `data/uploads/`（与 `pages.py:216-226` 的 `get_bitmap` 同样的 confine 纪律）。
- contain 会让非 4:3 的图两侧留白；若观感不可接受再改 cover（一行）。
