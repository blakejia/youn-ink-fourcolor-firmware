# 设备接入鉴权设计

日期：2026-09-02
状态：已批准（§1–§4 逐节确认）
前置：`docs/superpowers/specs/2026-09-01-canvas-loop-design.md`

## 概述

设备接入引入完整鉴权链路：配网表单新增服务端 base URL 输入（取代 OTA URL
输入框），首次连接通过 6 位屏显配对码完成 proof-of-possession 配对，服务端
发放永久 token，设备存 NVS 后所有请求带 `Authorization: Bearer`。
schedule / bitmap 保持公开（内容寻址安全），其余 API 全部鉴权。

## 协议：三步配对

```
设备                                          服务端
  │ POST /api/devices/pair-start                  │
  │  {device_id, board_type}                      │
  │ ←─────────────────────────────────────────────│
  │  {code: "482913", expires_in: 300}            │
  │                    operator 界面输入 6 位码     │
  │                    POST /api/devices/pair-confirm
  │                      {device_id, code}        │
  │ ←─────────────────────────────────────────────│
  │  {status: "ready"}                            │
  │ POST /api/devices/pair-claim                  │
  │  {device_id, code}                            │
  │ ←─────────────────────────────────────────────│
  │  {token: "<64hex>"}                           │
```

安全属性：code 服务端生成、5 分钟有效、一次性、通过人眼传递
（proof-of-possession）；token = `secrets.token_hex(32)`，256 bit 熵。

## 路由矩阵

| 路由 | 鉴权 |
|---|---|
| `GET /api/health` | 公开 |
| `POST /api/devices/pair-start` | 公开（同 IP 5 次/5 分钟） |
| `POST /api/devices/pair-claim` | 公开（需有效 code；错 5 次锁 10 分钟） |
| `POST /api/devices/pair-confirm` | operator |
| `GET /api/pages/schedule` | 公开 |
| `GET /api/pages/bitmap/{md5}.bin` | 公开 |
| `WS /ws` | device token |
| `GET/POST/DELETE /api/pages`（管理） | operator |
| `POST/GET/DELETE /api/images`、`/api/push_image` | operator |
| `GET /api/ota/check`、`GET /api/ota/download/{filename}` | device token |
| `POST /api/ota` | operator |
| `GET /api/devices`、approve/revoke | operator |

旧 `?secret=` WS 配对机制删除。

## 数据契约（设备/服务端共享）

- NVS：namespace `server`，keys：`base_url`(≤128B)、`token`(64hex)、
  `device_id`(≤32B，MAC 派生如 `NOTE4C-AABBCC`)
- 服务端表：`pairing_sessions(device_id PK, code, created_at, expires_at,
  confirmed)`；`device_secrets` 加 UNIQUE(token) 索引
- 端点拼接（base 尾部斜杠保存时剥掉）：`{base}/ws`、`{base}/api/pages/schedule`、
  `{base}/api/pages/bitmap/{md5}.bin`、`{base}/api/ota/check`、
  `{base}/api/ota/download/{filename}`、`{base}/api/devices/pair-start`、
  `{base}/api/devices/pair-claim`、`{base}/api/health`
- 鉴权头：`Authorization: Bearer <token>`（WS 走 SetHeader，不用 query）

## 服务端实现

- `youn_server/pairing.py` 新增：session CRUD、code 生成/校验、限速/锁定
- `youn_server/devices.py`：加 `get_device_by_token`
- `youn_server/app.py`：3 个 pair 路由；`require_device_token` dependency；
  WS 握手改 token；删 `?secret=` 分支
- 过期会话在 pair-start 时顺带清理

## 固件实现

- `firmware/main/common/server_pairing.{h,cc}` 新增：init/配对状态机/
  `BuildEndpoint(path)`；配对码屏显（大字号，复用 APTransfer 全屏绘制模式）；
  5 分钟超时重新生成
- `HttpClientWrapper`（薄封装 esp_http_client）：统一注入 Bearer 头；
  page_sync / OTA / pairing 共用
- WS：`SetHeader("Authorization", "Bearer <token>")`
- 配网表单：主表单加 `server_url`（必填，placeholder `https://youn.example.com`，
  校验 http(s) scheme）；高级表单删 `ota_url` 及其 handler 分支
- 运行时改地址：长按 BOOT 5 秒清除 `server/*` 重启进配网；不做软键盘
- 兼容：无 `server/base_url` 的旧设备 → 屏显"请重新配网"进配网模式；
  `websocket/url` 旧键保留一次 fallback

## 错误处理

- code 过期/复用 → 401，设备重新 pair-start
- token 无效或设备被 revoke → 统一 401（不区分原因）
- WS 401：发 `{"type":"error","message":"unauthorized"}` 后 close
- base_url 不可达：屏显"无法连接服务器，检查地址"

## 测试

- 服务端 pytest：配对 happy path、code 过期/复用/锁定、token 反查、
  revoke 后 401、WS 无 token 401、schedule/bitmap 无 token 200、限速
- 设备真机：配对全流程、重启免配对、长按 BOOT 重配、错地址提示

## 实施顺序

1. 服务端（subagent A）
2. 固件 core（subagent B）：server_pairing + HttpClientWrapper + WS 头
3. 固件配网 UI（subagent C）：表单字段 + handler
4. 集成验证：服务端 pytest 全绿 + 固件 idf.py build 通过
