# NOTE4C 待确认通知（Confirm Notification）设计

日期：2026-09-02
状态：approved（章节 1-5 均经用户确认）
分类：Architectural
相关：Canvas Loop（page_sync）、配对鉴权（pairing）、Web Admin UI

## 1. 背景与目标

NOTE4C（400×300 4 色 BWRY EPD，ESP32-S3）目前通过 page_sync 模块轮换显示
服务端下发的画板位图，仅支持 BOOT/上下键翻页。现需求：设备可以展示一条
「待确认消息」，供用户做出同意/不同意选择，并把结果回传服务端（存库/供
外部 Agent 查询）。

触发方式：**BOOT 短按 → 设备发一次 HTTP GET 拉取**（不轮询、不长连接）。
展示时长：**5 分钟**（可被 BOOT 短按提前关闭）。
交互：展示期间 上键=同意、下键=不同意、BOOT 短按=关闭（不 ack）。

### 目标
- 服务端：通知 FIFO 队列 + 4 个 HTTP 端点 + FastMCP 服务（同端口 9002）
- 设备端：notify 模块（GET 拉取/展示/ack/5min 自动关闭），BOOT/上键/下键接线
- 真机验证：注入通知 → BOOT 拉取显示 → 按键 ack → 服务端落库

### 非目标（YAGNI）
- 不做通知跨设备广播（每条固定 device_id）
- 不做通知优先级/多级分层（FIFO 单队列）
- 不做通知轮询（仅 BOOT 触发拉取）
- 不做 Web UI 编辑/重发通知（仅创建 + 查历史）
- 不做设备 ack 的 WebSocket 实时推送（HTTP POST 即可）
- 设备重启后不保留 notify 内存状态（不跨重启持久化）

## 2. 架构

```
[Web UI / 外部 Agent / MCP client]
 │ POST /api/notifications {device_id, title, body, ttl_sec?}
        ▼
[服务端] notify_store（data/notifications.jsonl，append-only + Lock）
        │ FIFO 队列，按 device_id
        ▼
[服务端] GET /next 时：取 pending 未过期第一条 → mark_shown →
        render_canvas_to_bitmap(title+body) → base64
        ▼
[设备] BOOT 短按 → GET /api/notifications/next?device_id=...
        │
        ├─ 200 {bitmap_base64, notification:{id,title,body,expires_at}} → NOTIFYING
        │
        └─ 204 无 → 回 IDLE（不提示）
        │
        ▼
   展示 5min（FreeRTOS timer）
        │
        ├─ 上键 → POST /{id}/ack {decision:"agree"} → 恢复画板原页
        ├─ 下键 → POST /{id}/ack {decision:"reject"} → 恢复画板原页
        └─ BOOT / 5min到 → notify_dismiss → 恢复画板原页（不 ack）
```

FastMCP 服务与 FastAPI 同进程同端口（9002）：
- `POST /mcp`（Streamable HTTP transport，`mcp.mount(app)`）
- `/mcp` 走 operator token 鉴权
- 三 tool 直接 import notify_store，不经过 HTTP

## 3. 服务端

### 3.1 数据模型（youn_server/notify_store.py，新文件）

```python
@dataclass
class Notification:
    id: str            # uuid4 hex
    device_id: str
    title: str
    body: str
    created_at: float  # unix ts
    ttl_sec: int       # 默认 300
    status: str        # "pending" | "shown" | "acked" | "expired" | "error"
    decision: str | None  # "agree" | "reject" | None
    acked_at: float | None
```

存储：`data/notifications.jsonl`（append-only，每行一个 JSON）。
并发：`threading.Lock`。
方法：
- `enqueue(device_id, title, body, ttl_sec) -> Notification`（status=pending）
- `next_for(device_id) -> Notification | None`（FIFO：status=="pending" 且未过期，按 created_at 最早；懒过期检查；原子 mark_shown）
- `ack(id, decision) -> Notification | None`（幂等：已 acked 返回原 decision）
- `expire_pending()`（next_for 时懒触发）
- `recent(n=20) -> list[Notification]`

### 3.2 端点（app.py）

```
POST /api/notifications            operator 鉴权（_require_operator）
  body {device_id, title, body, ttl_sec?}
  device_id 必须已 trust（devices.db）
  → 201 {notification: {...}}  |  400 缺字段  |  400 device 未 trust

GET  /api/notifications/next       设备 token 鉴权（_require_device_token）
  ?device_id=<id>
  → 200 {bitmap_base64, notification:{id,title,body,expires_at}}
  |  204 无待处理
  （拉到即 mark_shown，位图实时 render）

POST /api/notifications/{id}/ack   设备 token 鉴权
  body {decision: "agree"|"reject"}
  → 200 {status:"acked", decision}
  |  404 无此通知

GET  /api/notifications/history     operator 鉴权
  ?device_id=&limit=20
  → {notifications:[...]}
```

### 3.3 位图生成

拉取时实时渲染（不复用 schedule cache）：
```python
bitmap = render_canvas_to_bitmap({
  "default":[{"type":"div","props":{
     "tw":"flex flex-col p-[16px] gap-[8px] bg-white",
     "children":[
       {"type":"div","props":{"tw":"text-[20px] font-bold","style":{"color":"#000000"},"children":title}},
       {"type":"div","props":{"tw":"text-[16px]","style":{"color":"#000000"},"children":body}},
     ]}}]
})
```
body 过长（渲染失败）→ 该条置 error，返回 500。

### 3.4 FastMCP（youn_server/mcp_server.py，新文件）

```python
from fastmcp import FastMCP
mcp = FastMCP("youn-notify", transport="streamable-http")

@mcp.tool
def push_notification(device_id: str, title: str, body: str, ttl_sec: int = 300) -> dict: ...

@mcp.tool
def list_notifications(device_id: str = "", limit: int = 20) -> dict: ...

@mcp.tool
def ack_notification(notification_id: str, decision: str) -> dict: ...
```

挂载：`mcp.mount(app)` → `/mcp`。
鉴权：`/mcp` 挂依赖校验 `X-Operator-Token`（与 operator 一致）。
依赖：`requirements.txt` 加 `fastmcp>=2.0`。

## 4. 设备端（固件）

### 4.1 BSP 接线

config.h（TODO_* → 实际名）：
```c
#define UP_BUTTON_GPIO     GPIO_NUM_39   // 右侧上键，低有效
#define DOWN_BUTTON_GPIO   GPIO_NUM_18   // 下键/电源，低有效
#define CONFIRM_BUTTON_GPIO GPIO_NUM_0   // BOOT/确认
#define VBAT_PWR_GPIO      GPIO_NUM_18
```
zectrix-s3-epaper-4.2.cc：`kBoardUpButtonGpio = UP_BUTTON_GPIO` 等（原 TODO_* 引用改实际宏）。up/down/confirm button 实例化已存在，无需新增。

### 4.2 notify 模块（firmware/main/common/notify.{h,cc}，新文件）

- 状态：`IDLE | FETCHING | NOTIFYING`
- `notify_init(http_client)` / `notify_deinit()`
- `notify_request_next()`：异步 GET `/api/notifications/next?device_id=...`（复用 HttpClientWrapper）
- `notify_show_bitmap(data, meta)`：memcpy 进 framebuffer + `RequestUrgentFullRefresh`（复用 page_sync.cc 安全路径）
- `notify_post_ack(id, decision)`：POST `/api/notifications/{id}/ack`
- `notify_dismiss()`：恢复 `page_sync_show_page(current_index)`
- `notify_is_active()`：查询
- 5min 过期：FreeRTOS 定时器 → notify_dismiss

### 4.3 application.cc 接线

```c
void Application::OnBootClick() {
    if (notify_is_active()) { notify_dismiss(); return; }
    // BOOT 短按 = 拉取待确认通知。仅在画板显示（page_sync 接管屏幕）时
    // 生效，避免打断 RawDraw（相册/设置/TTS）的既有 BOOT 确认语义。
    if (!page_sync_is_displaying()) return;
    notify_request_next();
}
void Application::OnUpClick() {
    if (notify_is_active()) { notify_post_ack("agree"); notify_dismiss(); return; }
    if (page_sync_is_displaying()) { page_sync_prev(); return; }
    // 现有路由
}
void Application::OnDownClick() {
    if (notify_is_active()) { notify_post_ack("reject"); notify_dismiss(); return; }
    if (page_sync_is_displaying()) { page_sync_next(); return; }
    // 现有路由
}
```

### 4.4 CMakeLists.txt

`main` target SRCS 加 `common/notify.cc`。

### 4.5 状态机

```
IDLE ──BOOT──▶ FETCHING ──204/超时/≥400/校验失败──▶ IDLE
                    │ 200 有效 bitmap
                    ▼
                 NOTIFYING ──上键(agree)/下键(reject)/BOOT/5min──▶ IDLE
```

### 4.6 错误处理

| 场景 | 行为 |
|---|---|
| GET /next 204 | 回 IDLE，不提示 |
| GET 超时（>3s） | 回 IDLE，不重试 |
| GET ≥400 | 回 IDLE，日志，不阻塞翻页 |
| bitmap 长度≠30000 | 丢弃回 IDLE |
| 展示中 BOOT | dismiss，不发 ack，服务端该条保持 shown |
| ack 网络失败 | 保持 NOTIFYING，允许重按 |
| ack 404 | 视作已处理 dismiss |
| 5min 到 | 自动 dismiss |

## 5. 测试矩阵

### 服务端（pytest，新增 test_notify.py + test_mcp.py）
- enqueue→next FIFO 顺序
- next 后 mark_shown，再 next=None
- ttl 过期→next=None, status=expired
- ack 置 decision；重复 ack 幂等
- HTTP POST /notifications→201；缺字段→400；device 未 trust→400
- GET /next 带 device token→200+bitmap；204 空
- POST /{id}/ack→200；404
- GET /history→列表
- /mcp 探测；tools/call 三 tool；无 token→401

### 固件（idf.py build + 真机，无 pytest）
- 编译通过，无 undefined ref
- Web UI 注入通知 → BOOT → EPD 显示位图
- 上键→ack=agree；下键→ack=reject
- 5min 自动 dismiss
- 无通知时 BOOT→翻页照常
- 设备重启→notify 状态清零

## 6. 改动文件总览

```
服务端
  server/youn_server/notify_store.py   (新)
  server/youn_server/mcp_server.py     (新)
  server/youn_server/app.py            (改, +4 端点 +mcp.mount)
  server/youn_server/config.py         (改, +notify 存储目录/ttl)
  server/requirements.txt              (改, +fastmcp)
  server/tests/test_notify.py          (新)
  server/tests/test_mcp.py             (新)

固件
  firmware/main/common/notify.h        (新)
  firmware/main/common/notify.cc       (新)
  firmware/main/CMakeLists.txt         (改)
  firmware/main/application.cc         (改, +3 回调分支)
  firmware/main/boards/zectrix-s3-epaper-4.2/config.h      (改, TODO_*→实际)
  firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc (改, 宏引用)

文档
  docs/superpowers/specs/2026-09-02-notify-confirm-design.md (本文件)
```

## 7. 提交计划

1. spec commit（本文件）
2. 服务端 commit（notify_store + 端点 + mcp + 测试）→ `38+13=51 tests` 全绿
3. 固件 commit（notify + 接线 + 改名）→ `idf.py build` 通过 + 真机验证
