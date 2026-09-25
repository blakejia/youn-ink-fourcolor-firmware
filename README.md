# Youn Ink Four Color

面向 **Zectrix ESP32-S3 4.2 寸四色墨水屏**（NOTE4C / BWRY SSD2683）的个人 AI 助手系统。
三部分构成：ESP32-S3 固件（C++ + Rust）、Python 后端服务（FastAPI）、React 管理后台。

能力：语音对话 / TTS、待办同步、天气·新闻·日历·电子书·相册页面、AP 传图、
OTA 固件管理、浏览器直连串口刷写与日志、四色屏 RawDraw UI、基于电压的相对电量上报。

![Youn Ink Four Color 2BP BWRY architecture](README-2bp-architecture.png)

## 目录结构

```text
.
├── firmware/        ESP32-S3 固件（ESP-IDF v6.0；C++ 机制层 + Rust 策略层）
│   ├── main/          application.cc 编排、rawdraw UI、display、boards、protocols
│   ├── main/rust/     Rust 策略模块（单一 librust_firmware.a）
│   ├── main/components/  78__esp-wifi-connect / 78__esp-ml307 / 78__xiaozhi-fonts
│   ├── components/   上游 LCD 驱动与 json（st7701 / gc9a01 / nv3023 …）
│   └── scripts/      release.py 等打包工具（build.sh 依赖，当前 fork 不可用）
├── server/          FastAPI 后端（WebSocket 对话、TTS、Discovery、页面、OTA、MCP）
│   └── youn_server/  业务模块（app / pages / pairing / ota / canvas / serial_firmware …）
├── frontend/        React + Vite 管理后台（构建产物由服务端 static 挂载）
├── data/            空目录（服务端实际数据在 server/data/，见 §二）
├── docs/            设计文档与实现记录
├── brick_safe_backup.sh  刷机前整片备份（防砖第一步）
└── backup/          历史备份镜像
```

## 快速开始

三步，按顺序做。每步都有独立章节展开。

```bash
# 1. 后端
cd server && python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env          # 见「服务端部署」——至少填 OPENAI_API_KEY / MASTER_KEY / OPERATOR_TOKEN
./start.sh install && ./start.sh start

# 2. 管理后台
cd ../frontend && npm install && npm run build    # 产物 frontend/dist，服务端重启后自动挂载

# 3. 固件
cd ../firmware && cp .env.example .env            # 填 DEVICE_MASTER_KEY，须与 server/.env 的 MASTER_KEY 一致
export PATH="$HOME/.cargo/bin:$PATH"              # cargo 必须在 export.sh 之前进 PATH
source ~/data/esp-idf-v6.0/export.sh
IDF_TARGET=esp32s3 idf.py build
```

---

# 一、编译条件

## 1.1 固件工具链

固件同时含 **C++ 与 Rust**，两套工具链都必须就位，缺任一个都会在构建中途报错。

| 组件 | 版本 / 要求 | 说明 |
| --- | --- | --- |
| ESP-IDF | **v6.0** | `IDF_TARGET` 必须为 `esp32s3` |
| Xtensa GCC | 15.2.0 | 由 ESP-IDF 提供 |
| Rust | 1.85+（本机实测 esp 工具链 1.97 nightly / stable 1.98） | `edition = "2024"` 要求 ≥1.85；需 `espup install -t esp32s3` |
| Rust 目标 | `xtensa-esp32s3-none-elf` | 上游 rustc 无此目标，靠 esp-rs 工具链 |
| Python | 3.10+（实测 3.13） | ESP-IDF 与 `scripts/release.py` 都用 |
| Node.js | 20+ | 仅前端构建需要 |

### 为什么 `IDF_TARGET=esp32s3` 不能省

不指定时 IDF 默认 `esp32`，而本项目的屏幕驱动（SSD2683 四色 EPD）、`st7701`、
分区与 PSRAM 配置都是 S3 专属，会在配置阶段直接失败。**每次构建都要带**，
包括 `idf.py build`、`menuconfig`、`flash`。

### 为什么 `cargo` 要在 `source export.sh` 之前进 PATH

ESP-IDF 的 `export.sh` 会重排 `PATH`。若先把 `~/.cargo/bin` 放进去再 source，
cargo 会从 `PATH` 中丢失，CMake 自定义命令随即报
`cargo: command not found`。固定顺序：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
```

`cargo` 是 rustup shim；工具链 `esp` 已装时 `cargo +esp build` 才能编译 Xtensa 目标。

### 首次安装

```bash
# ESP-IDF v6.0（路径按实际调整，下文示例用 ~/data/esp-idf-v6.0）
mkdir -p ~/data && cd ~/data
git clone -b v6.0 --recursive https://github.com/espressif/esp-idf.git esp-idf-v6.0
cd esp-idf-v6.0 && ./install.sh esp32s3

# Rust + esp-rs Xtensa 工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install espup
espup install -t esp32s3      # 提供 rust-src 与 Xtensa target；装完后工具链名为 esp
```

## 1.2 构建固件

```bash
cd firmware

export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh

IDF_TARGET=esp32s3 idf.py build
```

产物：

| 文件 | 用途 |
| --- | --- |
| `build/xiaozhi.bin` | **应用镜像**（写 `0x20000`）；管理后台串口刷写与 OTA 都用它 |
| `build/bootloader/bootloader.bin` | 引导（写 `0x0`） |
| `build/partition_table/partition-table.bin` | 分区表（写 `0x8000`） |
| `build/ota_data_initial.bin` | otadata 初始（写 `0xd000`） |
| `build/flasher_args.json` | 以上四项的地址映射，`idf.py flash` 依据 |

`firmware/.env` 会被 `build.sh` 读取；**直接 `idf.py build` 则不读**，
此时 `DEVICE_MASTER_KEY` 必须由 `-D` 显式传入：

```bash
IDF_TARGET=esp32s3 idf.py -DDEVICE_MASTER_KEY="$(grep ^MASTER_KEY= ../server/.env | cut -d= -f2-)" build
```

### `DEVICE_MASTER_KEY` 缺失的后果

设备签名主密钥在编译期烘进 Rust 归档（`option_env!("DEVICE_MASTER_KEY")` →
`main/rust/src/device_signature.rs`）。缺失或为空时固件回落到字符串
`REPLACE_ME_AT_BUILD_TIME_WITH_32_BYTE_RANDOM`：

- 固件能正常启动、联网、跑页面同步；
- **但 `pair-start` 的 HMAC 签名永远校验失败**，设备无法完成首次配对。

**影响范围仅限首次配对**：固件启动时读 NVS，已有 `base_url` + `token` 就直接进
`SERVER_PAIR_OK`（`main/common/server_pairing.cc`），完全不走签名路径。因此
占位符构建的固件烧到**已配对**设备上照常工作 —— 但换新设备、或清空 NVS
（`Reset device` 会同时清 Wi-Fi 凭据与 pairing token）后就再也配不上。

服务端侧 `server/.env` 的 `MASTER_KEY` 少于 32 字节同样直接拒绝所有签名。
两侧必须逐字节一致。验证某份 `xiaozhi.bin` 用的是哪把钥匙：

```bash
KEY=$(grep ^MASTER_KEY= server/.env | cut -d= -f2-)
grep -qaF "$KEY" firmware/build/xiaozhi.bin && echo "真实密钥已烘入" || echo "占位符构建"
```

### 增量构建的两个陷阱

1. **改 Rust 后必须确认它真的被重编**。`main/CMakeLists.txt` 用显式文件列表
   （非 glob）驱动 cargo；新增 `.rs` 模块若没登记进 `RUST_SOURCES`，
   增量构建会静默链接上一版归档。改 `main/rust/src/` 后核对构建输出里出现过
   `Building Rust firmware modules (xtensa-esp32s3-none-elf)`。
2. **清理 `build/` 必须同时清 Rust target**。只 `rm -rf build` 会留下陈旧的
   `librust_firmware.a`：

   ```bash
   rm -rf build && (cd main/rust && cargo clean)
   ```

工具链或 ESP-IDF 版本变更时，用 `IDF_TARGET=esp32s3 idf.py fullclean` 并配合
`cargo clean`。

### `build.sh` 的状态（当前 fork 不可用）

`firmware/build.sh` 是上游的**多渠道打包**脚本（读 `main/boards/<board>/config.json`
→ `scripts/release.py` → zip + 同步到管理后台与 OTA 目录）。本 fork 的
`main/boards/zectrix-s3-epaper-4.2/` 下**没有 `config.json`**（历史上也从未有过），
`build.sh` 会在 `[ERROR] 未找到板型配置文件` 处退出。此外模板
`firmware/.env.example` 曾含字面 `...` 行导致 `source` 失败（已修）。

本仓库的固件构建请直接用上面的 `idf.py build`。

## 1.3 屏幕配置

`firmware/main/Kconfig.projbuild` 提供两种面板：

```text
ZECTRIX_EPD_PANEL_4COLOR_SSD2683   四色 BWRY（默认，NOTE4C 用）
ZECTRIX_EPD_PANEL_1BPP             黑白 1bpp
```

`firmware/sdkconfig.defaults.esp32s3` 已默认选中四色屏。改面板后需重新构建：

```bash
IDF_TARGET=esp32s3 idf.py menuconfig     # Zectrix Board → EPD panel type
IDF_TARGET=esp32s3 idf.py build
```

RawDraw 主题层会把红/黄语义色降级为黑白可读样式，因此同一份业务代码适配两种屏。

## 1.4 编译服务端与前端

```bash
# 服务端：无编译步骤，纯 Python
cd server && python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

# 前端
cd frontend && npm install && npm run build    # → frontend/dist
```

**前端构建产物由服务端挂载**（`server/youn_server/app.py` 读 `frontend/dist`，
存在时注册 `/`、`/login`、`/devices`、`/pages`、`/images`、`/ota`、`/serial`
六条 SPA 路由）。`dist/` 不存在时服务照常启动，只是后台 UI 不可用并记一条
`frontend/dist not found; web admin UI disabled` 警告。**重建前端后需重启服务端**。

前端开发模式（HMR + API 代理到 9002）：

```bash
cd frontend && npm run dev     # http://localhost:5173
```

## 1.5 测试

```bash
# Rust 策略层：375 个测试
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test

# 服务端：302 个测试
cd server && .venv/bin/python -m pytest tests/ -q
```

**两套门禁不可互相替代**：`cargo test` 在 host 上跑，而部分代码在 Xtensa 上
行为不同（历史上 `to_ascii_lowercase` 即在设备端编译失败）。改动同时触及
Rust 与 C++ 时，`cargo test` 与 `idf.py build` 都必须通过。

---

# 二、服务端部署

## 2.1 安装

```bash
cd server
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env
```

`server/.env.example` 是**权威且完整**的变量清单，照抄即可。最少必须填四项：

| 变量 | 生成方式 | 不填的后果 |
| --- | --- | --- |
| `OPENAI_API_KEY` + `OPENAI_BASE_URL` | 任何 OpenAI 兼容端点 | ASR / LLM / TTS 全部不可用 |
| `MASTER_KEY` | `python -c "import secrets;print(secrets.token_urlsafe(32))"` | **拒绝所有设备配对** |
| `OPERATOR_TOKEN` | `openssl rand -hex 32` | operator API 无鉴权（公网绝不能空） |
| `DISCOVERY_SHARED_SECRET` | 32+ 字节随机 | UDP 发现应答签名弱 |

`MASTER_KEY` 必须与固件构建时的 `DEVICE_MASTER_KEY` 完全一致。

## 2.2 端口

| 端口 | 协议 | 用途 |
| --- | --- | --- |
| `9002` | HTTP + WebSocket | 主服务：对话、页面、OTA API、MCP、静态后台 |
| `8766` | UDP | 设备发现（`discover_host` / `discover_reply`） |
| `8766` | HTTP | 图片推送 + OTA 备用入口（与发现共用同一端口号，协议不同） |

注意：`config.py` 里的代码默认是 `9001`，但 `server/.env.example` 与所有实际部署
都用 `9002` —— 本机 rerank-proxy 占用 9001。**从 `.env.example` 复制就对了**；
若手工写 `.env` 而漏掉 `LISTEN_PORT`，服务会去抢 9001 并失败。

## 2.3 启动（systemd 用户单元）

单元文件**版本化在仓库内**，`start.sh` 负责安装符号链接，保持单一真相源：

```bash
cd server
./start.sh install      # 建链接 + enable + loginctl enable-linger
./start.sh start
```

`install` 会开启 linger，因此**开机自启、退出登录后继续运行**；崩溃由
`Restart=always` 自动拉回。

```bash
./start.sh status       # 退出码与 systemd 一致
./start.sh logs         # 应用日志 data/server.log（10 MB × 5 自动轮转）
./start.sh journal      # stdout/stderr：uvicorn 访问日志、traceback
./start.sh restart
./start.sh stop
./start.sh uninstall
```

### 单元文件里不能改错的地方

- `WorkingDirectory` 必须是 `server/` —— 应用按工作目录解析 `.env` 与 `data/`。
- `Type=simple`：`systemctl start` 在进程起来时就返回，**端口就绪还要 2–4 秒**
  （uvicorn 绑定 + MCP session manager 初始化）。开机后立刻刷后台可能看到一次
  连接失败，刷新即可。
- `ProtectSystem=strict` 把文件系统挂成只读，唯一可写区由 `ReadWritePaths`
  指定为 `server/data`；`PrivateTmp` 提供可写 `/tmp`。
- **日志分两路，不要合并**：应用自己用 `RotatingFileHandler` 写
  `data/server.log`，stdout/stderr 交给 journal。若把 systemd 的 stdout 也
  `append:` 到同一文件，轮转后 systemd 会继续写被重命名过的旧 inode，轮转失效。

本机无 root，因此跑在 user manager 下。有 root 的机器可复制同一单元到
`/etc/systemd/system/`，把 `WantedBy=` 改成 `multi-user.target` 并补 `User=`。

## 2.4 首次配对

1. 设备上电 → **同时按住上键+下键 3 秒**进配网页（单键长按是 1 秒，走导航）
   → 连上设备热点，浏览器打开 `http://192.168.4.1` 填 SSID/密码/服务器地址
2. 设备 UDP 广播 `discover_host` → 服务器回 `discover_reply`
3. 设备带 HMAC 签名连 WS 到 `PUBLIC_WS_URL` → 服务端建 device（`trust=0`）
4. **在服务器上放行**：

```bash
curl -X POST http://127.0.0.1:9002/api/devices/<deviceId>/approve \
     -H "X-Operator-Token: $OPERATOR_TOKEN"
```

5. 设备重连，进入对话页面

`ALLOWED_DEVICE_IDS` 可设白名单（逗号分隔，留空表示接受所有设备）。

## 2.5 公网部署

9002 / 8766 **不要直接暴露公网**，只放行 80 / 443 并反代到本地端口。
`server/DEPLOY.md` 给出 Caddy（推荐，自动 Let's Encrypt）与 Nginx 两套完整配置。

反代配置、安全清单、OTA 与串口固件仓库的完整 API 示例都在 **`server/DEPLOY.md`**。

## 2.6 管理后台

服务端启动时挂载 `frontend/dist`。浏览器打开 `http://<host>:9002/`，
在 `/login` 填 `OPERATOR_TOKEN`（存 localStorage，随请求发
`X-Operator-Token` 头）。页面：

| 路由 | 用途 |
| --- | --- |
| `/devices` | 设备列表、信任审批、电量视图 |
| `/pages` | 页面管理、画布编辑（`CanvasEditor`） |
| `/images` | 图片上传、转四色、推送 |
| `/ota` | 固件版本与 OTA 管理 |
| `/serial` | **浏览器直连串口**：读设备日志 + esptool-js 刷写 |

`/serial` 页需要桌面 Chrome/Edge 89+ 或 Firefox 151+（WebSerial），
且**必须在设备所插的那台机器上打开**——串口不经服务端转发。

## 2.7 MCP

`/mcp` 挂 FastMCP（streamable-http），由 `X-Operator-Token` 鉴权。
客户端可用 omp / Claude Code / 官方 SDK 接入。

## 2.8 本地模拟设备

```bash
cd server
python3 mock_client.py --server ws://127.0.0.1:9002
```

---

# 三、烧录与部署

## 3.1 防砖第一步：整片备份

`brick_safe_backup.sh` 完整备份 16 MB flash（含 bootloader、分区表、MAC、
校准数据与全部应用槽）。**首次刷写或改分区表之前必须做**。

```bash
./brick_safe_backup.sh /dev/ttyACM0
# 产出 backup/flash-backup-<timestamp>.bin + SHA256
```

## 3.2 完整刷写（首次 / 分区表变更）

```bash
cd firmware
IDF_TARGET=esp32s3 idf.py -p /dev/ttyACM0 flash monitor
```

`idf.py flash` 按 `build/flasher_args.json` 写四个区：

```text
0x0      bootloader/bootloader.bin
0x8000   partition_table/partition-table.bin
0xd000   ota_data_initial.bin
0x20000  xiaozhi.bin
```

分区表（`partitions.csv`）：

```text
nvs       0x9000    16K
otadata   0xd000    8K
phy_init  0xf000    4K
ota_0     0x20000   4032K
ota_1     0x410000  4032K
assets    0x800000  8M
```

## 3.3 后台串口刷写（推荐日常用）

管理后台 `/serial` 页用 WebSerial + esptool-js 在浏览器里直连串口，服务端只做
**固件仓库**（给字节流，不开串口、不跑 esptool）。

- 刷写目标由 `frontend/src/flashTarget.js` 解析 **otadata** 决定活动 OTA 槽
  （`ota_0 @0x20000` / `ota_1 @0x410000`），而非固定写 `0x20000`；
  OTA 交替后写错槽会“刷写成功但设备照旧跑老固件”。
- 刷前**自动备份**当前 otadata 与应用槽，失败可回滚。
- 「完整镜像（写 0x0）」是高级路径，会覆盖 bootloader 与分区表，需二次确认。

固件来源是 `firmware/build/xiaozhi.bin`（`server/youn_server/serial_firmware.py`
的 `BUILD_ARTIFACT`）——**构建完即出现在后台列表里，无需拷贝**：

```bash
curl http://127.0.0.1:9002/api/firmware -H "X-Operator-Token: $OPERATOR_TOKEN"
curl -o xiaozhi.bin \
  http://127.0.0.1:9002/api/firmware/build:xiaozhi.bin/download \
  -H "X-Operator-Token: $OPERATOR_TOKEN"
```

手动上传的固件存放在 `server/data/serial-firmware/`，与 OTA 频道隔离。

## 3.4 OTA 频道

**三个固件来源互相独立，不要混用：**

| 路径 | 谁消费 | 生效时机 |
| --- | --- | --- |
| `firmware/build/xiaozhi.bin` | 后台 `/serial` 串口刷写 | 你点“刷写”时 |
| `server/data/firmware/` | OTA 接口（`/api/ota/check`） | 见下方现状 |
| `server/data/serial-firmware/` | 手动上传的调试件 | 手动刷写时 |

### 现状：设备端没有 OTA 客户端

**当前固件不含任何 OTA 下载/写入实现**，因此**不会**自动拉取
`server/data/firmware/`：

- 源码零调用（`esp_https_ota` 只在 `main/CMakeLists.txt` 的链接依赖里出现，
  没有任何 `esp_https_ota` / `esp_ota_begin` 调用点）；
- 服务端历史上从未收到过 `/api/ota/download` 请求；
- 设备实际只请求 `pages/schedule`、`notifications/next`、`pages/bitmap`。

服务端的 OTA 接口、签名与制品管理都已实现且可用，但**要真正启用 OTA
必须先在固件里补客户端**。在那之前，升级固件只有串口刷写一条路。

把固件放进 `server/data/firmware/` 目前不会让设备升级。补上客户端后，
该目录即等于“设备下次轮询就自动升级”——那时才需要严格区分验证过的版本。

### 上传到 OTA 频道

```bash
curl -X POST https://your.domain/api/ota \
     -H "X-Operator-Token: $OPERATOR_TOKEN" \
     -F "firmware=@xiaozhi.bin" \
     -F "version=6.5.9-note4c-51812e4" \
     -F "channel=stable" \
     -F "notes=first build"
```

字段名是 **`firmware`**（不是 `file`，那是串口仓库的字段名）。

服务的处理：写入 `<version>.bin` + `.sha256`，用
`DISCOVERY_SHARED_SECRET` 对 sha256 做 HMAC 生成 `.sig`，最后写 `latest.json`
并把上一版快照进 `archive/`。下载时**重新验签**。

注意：`/api/ota` 上传**只校验大小 ≤32 MB**，不做 ESP 镜像魔数校验——
那是 `/api/firmware`（串口仓库）独有的约束（首字节 `0xE9`，≤ `0x3F0000`）。
因此 OTA 上传填错文件不会有提示。

写入 OTA 槽的必须是**应用镜像** `build/xiaozhi.bin`（对应分区 `ota_0`/`ota_1`），
不是写 `0x0` 的合并镜像。

固件版本号是 `firmware/CMakeLists.txt` 的 `PROJECT_VER`（当前 `6.5.9`），
本项目不使用 semver tag 或 changelog。

## 3.5 旧版 `build.sh` 打包流程

上游的 `firmware/build.sh` 会读 `.env` → `scripts/release.py <board>` 打包成
zip 并同步到管理后台与 OTA 目录。**当前 fork 因缺板型 `config.json` 不可用**
（见 1.2）。若需恢复该流程，先补
`firmware/main/boards/zectrix-s3-epaper-4.2/config.json`（含 `target` 与 `builds`）。

---

# 四、环境变量

完整带注释的清单以 **`server/.env.example`** 为准；固件侧是
**`firmware/.env.example`**（`build.sh` 读它，直接 `idf.py build` 不读）。

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `LISTEN_HOST` / `LISTEN_PORT` | `0.0.0.0` / `9002` | 主服务 |
| `PUSH_IMAGE_HOST` / `PUSH_IMAGE_PORT` | — / `8766` | 图片推送 + OTA 备用 + UDP 发现 |
| `PUBLIC_WS_URL` / `PUBLIC_HTTP_BASE` | — | 设备看到的公网地址 |
| `OPENAI_API_KEY` / `OPENAI_BASE_URL` | — | 任何 OpenAI 兼容端点 |
| `MASTER_KEY` | 空 | 设备签名密钥，须与固件一致；空则拒绝所有配对 |
| `OPERATOR_TOKEN` | 空 | 后台/operator API 令牌；空则无鉴权 |
| `DISCOVERY_SHARED_SECRET` | 占位 | UDP 发现应答签名 |
| `ALLOWED_DEVICE_IDS` | 空 | 设备白名单，空 = 全接受 |
| `DEVICE_MASTER_KEY` | 空（**固件侧**） | 烘进固件的签名密钥，须等于 `MASTER_KEY` |

---

# 五、版本控制范围

**提交**：`firmware/main/`、`firmware/main/rust/`、`firmware/components/`、
`firmware/partitions*`、`firmware/sdkconfig.defaults*`、`server/*.py`、
`server/youn_server/`、`server/tests/`、`server/systemd/`、`server/requirements.txt`、
`server/.env.example`、`frontend/src/`、`frontend/package.json`、
`frontend/package-lock.json`、`firmware/.env.example`、根目录文档。

**不提交**（`.gitignore` 已覆盖）：

- 构建产物：`firmware/build/`、`frontend/dist/`、`*.bin`、`*.elf`、`*.map`、`*.zip`
- 本地配置与密钥：`.env`、`firmware/sdkconfig`、`*.pem`、`*.key`
- 依赖目录：`node_modules/`、`firmware/managed_components/`、`firmware/main/rust/target/`
- 运行时数据：`server/*.log`、`server/*.db`、`server/*.pid`、
  `server/data/uploads/`、`server/data/images/`、`server/data/pages/*.bin`
- 隔离工作树：`.worktrees/`

`server/data/pages/<device>/<name>.json` 是页面**源文件**，属于版本控制内容；
同目录下的 `*.bin` 位图是渲染产物，不入库。

---

# 六、帮助

- **部署细节**：`server/DEPLOY.md`（反代、安全清单、OTA、串口仓库 API）
- **设计文档**：`docs/`
- **协作说明**：`CONTRIBUTING.md`
- **安全约定**：`SECURITY.md`

## 常见问题

| 现象 | 原因 |
| --- | --- |
| `idf.py build` 报 SSD2683 / st7701 相关错误 | 忘了 `IDF_TARGET=esp32s3`，IDF 按 esp32 配置 |
| `cargo: command not found` | `source export.sh` 之后才把 `~/.cargo/bin` 加进 PATH |
| 改 Rust 无效，行为像旧代码 | 新 `.rs` 未登记进 `main/CMakeLists.txt` 的 `RUST_SOURCES` |
| 后台打不开 | `frontend/dist` 不存在 → `npm run build` 后重启服务端 |
| 设备连上但配对失败（401） | `MASTER_KEY` 与固件 `DEVICE_MASTER_KEY` 不一致，或固件用了占位符 |
| 后台 `/api/*` 全部可访问 | `OPERATOR_TOKEN` 为空 |
| 打开 `/dev/ttyACM0` 后设备复位 | 正常行为：打开 USB-Serial-JTAG 会复位芯片；刷写前先关监视器 |
| 刷写成功但设备仍跑旧固件 | 写错 OTA 槽；应写 otadata 指向的活动槽（后台页面已自动处理） |
