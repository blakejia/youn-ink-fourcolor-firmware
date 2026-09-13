# Youn Ink Four Color

这是一个面向 ESP32-S3 墨水屏设备的个人 AI 助手项目。当前主线由三部分组成：ESP32 固件、Python 后端服务、以及图片/待办/设备管理页面。

项目重点不是一个通用 npm 包，而是一套可以真实运行在墨水屏设备上的系统：语音对话、TTS 播放、待办同步、天气/新闻/日历/电子书/相册页面、AP 传图、OTA 固件管理，以及适配四色屏的 RawDraw UI。

## 2BP 四色图像链路

![Youn Ink Four Color 2BP BWRY architecture](README-2bp-architecture.png)

相册图片可由 PC/NAS 管理端或设备 AP 页面进入服务端，转换为 `2BP BWRY`（黑、白、红、黄）后通过 Wi-Fi 推送到 ESP32-S3 四色墨水屏。本仓库的 2BP 四色链路与 NOTE4 的 4BP 黑白灰阶相册独立维护：面板颜色、像素格式和刷新驱动均不同。

## 当前状态

- 后端已经切换为 `server/` 下的 Python 服务，根目录旧 Node `scripts/` 已删除。
- 固件主界面使用 RawDraw 渲染，默认按四色屏设计，同时保留 1bpp 黑白屏兼容。
- 主题暂时只保留一个默认视觉方向：偏任天堂感的四色主题，强调红、黄、黑、白的语义使用。
- 图片传输支持 1bpp 黑白与 2bpp 四色 BWRY 两种格式。
- 根目录 `.gitignore` 已排除构建产物、日志、pid、数据库、本地配置和密钥文件。

## 目录结构

```text
.
├── firmware/        ESP32-IDF 固件，RawDraw UI、页面渲染、屏幕驱动、AP 传图
├── server/          Python 后端，WebSocket 对话、TTS、Discovery、图片上传、OTA API
├── frontend/        管理前端源码，使用独立的 package/pnpm 工作流
├── docs/            历史设计文档和实现记录
├── documents/       项目资料
└── package.json     仅保留仓库级辅助命令，不再作为旧 Node 服务入口
```

注意：`firmware/scripts/` 和 `frontend/scripts/` 仍然有用，分别属于固件工具和前端工具；删除的是根目录历史遗留的 `scripts/`。

## 后端服务

后端入口是 `server/llmserve.py`，推荐通过 `server/start.sh` 管理。服务默认端口：

| 端口 | 协议 | 用途 |
| --- | --- | --- |
| `9002` | WebSocket | ESP32 语音、LLM、TTS、同步消息 |
| `8766` | UDP | 设备发现 |
| `8766` | HTTP | OTA API（备用入口，见下） |

### 安装依赖

```bash
cd server
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

### 启动服务

服务由 **systemd 用户单元**托管（单元文件版本化在 `server/systemd/`）：

```bash
cd server
cp .env.example .env    # 只需一次，填好里面的密钥
./start.sh install      # 只需一次：装单元 + enable + 开启 linger
./start.sh start
```

`install` 会开启 `loginctl enable-linger`，因此**开机自启、退出登录后继续运行**，
崩溃也会自动拉回（`Restart=always`）。详见 `server/DEPLOY.md` §5。

常用命令：

```bash
cd server
./start.sh status
./start.sh logs         # 应用日志（data/server.log，自动轮转）
./start.sh journal      # stdout/stderr（访问日志、traceback）
./start.sh restart
./start.sh stop
```

### 本地模拟设备

```bash
cd server
python3 mock_client.py --server ws://127.0.0.1:9002
```

## 图片和设备管理

图片上传走绑定页面的接口：`POST /api/uploads`，multipart 表单带 `image` 文件与
`page`（已存在的页面名），配置了 `OPERATOR_TOKEN` 时需带 `X-Operator-Token` 请求头；
上传后替换该页面的图片。`server/push_image.py` 是同一后端应用挂到 `8766` 端口的
备用入口（另带 UDP 发现），OTA 相关接口（上传固件、查询、下载）仍可用：

常用接口：

```bash
curl http://127.0.0.1:9002/api/health
```

上传图片示例（替换名为 `album` 的页面）：

```bash
curl -X POST http://127.0.0.1:9002/api/uploads \
  -H "X-Operator-Token: $OPERATOR_TOKEN" \
  -F "image=@/path/to/photo.jpg" \
  -F "page=album"
```

设备进入 AP 传图模式后，手机连接设备热点并访问：

```text
http://192.168.4.1
```

## 固件

固件位于 `firmware/`，基于 ESP-IDF。默认面向 ZecTrix ESP32-S3 4.2 寸墨水屏，支持四色 BWRY 屏，也保留 1bpp 黑白屏配置。

### 编译

```bash
cd firmware
source ~/Documents/esp/v6.0/esp-idf/export.sh
idf.py build
```

根目录辅助命令：

```bash
npm run firmware:build
```

### 屏幕配置

固件 Kconfig 中有屏幕类型选择：

```text
ZECTRIX_EPD_PANEL_4COLOR_SSD2683  四色 BWRY 屏
ZECTRIX_EPD_PANEL_1BPP            黑白 1bpp 屏
```

如果要刷回旧黑白屏，先在 `idf.py menuconfig` 中切到 `1bpp black/white EPD`，再重新构建烧录。RawDraw 主题层会把红/黄语义色降级成黑白可读样式。

## UI 说明

固件 UI 目前走 RawDraw 组件体系，重点页面包括：

- 对话：显示用户语音、识别状态、AI 回复。
- 待办：本地展示、服务端同步、完成/删除/编辑。
- 设置：音量、亮度、主题、网络、同步、OTA 等。
- 相册：缩略图列表、大图展示、AP 传图入口。
- 天气/天气详情、新闻、黄历、年度进度、日历、电子书、日志。
- 快速切换 Overlay：用于页面间快速跳转。

四色屏主题层通过语义样式绘制组件，不建议在业务页面里继续新增裸 `RED/YELLOW/BLACK/WHITE`。新增 UI 时优先使用 RawDraw 组件和 theme token。

## 环境变量

常用后端环境变量：

完整且带注释的清单见 **`server/.env.example`**（它就是权威列表，照抄即可）。
最常改的几个：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `OPENAI_API_KEY` / `OPENAI_BASE_URL` | — | 任何 OpenAI 兼容端点（ASR + LLM + TTS） |
| `LISTEN_PORT` | `9002` | WebSocket + HTTP API 端口（9001 被本机 rerank-proxy 占用） |
| `OPERATOR_TOKEN` | 空 | 后台/操作员 API 令牌；为空则这些接口无鉴权 |
| `MASTER_KEY` | 空 | 设备签名密钥（HMAC），与固件一致；为空则拒绝所有配对请求 |
| `DISCOVERY_SHARED_SECRET` | 占位 | UDP 发现应答的签名密钥 |

不要提交 `.env`、数据库、日志、pid、构建目录和固件产物。

## Git 提交范围

建议提交：

- `firmware/main/`、`firmware/components/`、`firmware/partitions/` 等固件源码。
- `server/*.py`、`server/static/`、`server/requirements.txt`、`server/DEPLOY.md`。
- `frontend/src/`、`frontend/package.json`、`frontend/pnpm-lock.yaml` 等前端源码。
- 根目录 README、文档、配置模板。

不要提交：

- `firmware/build/`
- `firmware/managed_components/`
- `firmware/sdkconfig`
- `firmware/releases/`
- `server/.env`
- `server/todo.db`
- `server/*.pid`
- `server/*.log`
- `frontend/.env*`
- `frontend/dist/`
- `node_modules/`
