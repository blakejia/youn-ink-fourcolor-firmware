# AGENTS.md

给在此仓库工作的 AI agent 的项目规则。**先读这份，再动手。**

本仓库是 NOTE4C（Zectrix ESP32-S3 4.2″ 四色墨水屏）的个人 AI 助手系统：ESP32 固件
（C++ + Rust）、FastAPI 服务端、React 管理后台。

面向人的构建/部署说明见 `README.md`；反代与安全清单见 `server/DEPLOY.md`。

---

## 0. 铁律

1. **改代码前先读对端全逻辑。** 改映射、枚举、ABI、协议字段前，必须读完整语义再改。
   曾因 `NetworkProbeTarget`（`network_probe_target.h` 的 `HttpOta=0 / WebSocket=1
   / Mqtt=2`）与 Rust 解码反向，导致 IP 快连解析出错误的探测端点。改函数签名
   必须同步 C ABI + 所有 C++ 调用点。
2. **不推送，除非用户明确说了推。** 默认停在本地提交。
3. **不把未验证的固件放进 OTA 频道。** 见 §5。
4. **没有正向证据支撑的行为改动不做。** 见 §4「证据纪律」。
5. **不要把部分完成说成完成。** 缺前置条件就明说缺什么，不要交付 stub / 假回退。

---

## 1. 架构与所有权边界

```text
Rust：纯规则 —— 解析、状态决策、缓存编解码、Action 选择
C++ ：机制 —— 任务生命周期、硬件执行、实际状态存储、副作用
C  ：ESP-IDF —— GPIO/SPI/I2C/NVS/FreeRTOS/Wi-Fi driver
```

- `firmware/main/application.cc` 是**唯一**的应用编排者。
- Rust **禁止**直接触碰 GPIO/SPI/I2C/NVS/FreeRTOS/Wi-Fi/HTTP/JSON DOM/EPD，
  **禁止**返回回调，**禁止**从违反 `noexcept` 的栈帧跨 FFI。
- 唯一的 ESP-IDF 接触面是 `firmware/main/rust/src/shim.rs` 的声明 + 同目录
  `shim.cpp` 的实现。其他 Rust 模块不得自行 `extern "C"`。
- 跨轮状态不留在 Rust：由 C++ 以 POD 快照传入（Rust 不持有跨任务隐式全局状态）。
- C ABI 用稳定的、按用途命名的 `rf_*` 符号；C++ 不直接依赖 Rust 内部类型。

### Rust 模块清单（`firmware/main/rust/src/`）

策略：`wifi_policy` `power` `input` `lifecycle` `notify_policy` `page_compare_policy`
`battery_activity_policy` `charge_policy` `led_policy` `time_gate_policy` `settings`
`pairing` `pairing_response` `protocol_parse`

机制/协调：`page_sync` `notify` `device_signature` `json`（最小扫描器）`log` `shim`

### 单一归档约束

整个固件只产出 **一个** `librust_firmware.a`。多个 Rust 归档会各自带上
`#[panic_handler]` / `core` / `compiler_builtins`，链接时冲突。
`Cargo.toml` 同时声明 `staticlib` + `rlib`：前者给设备，后者让 `cargo test` 能在 host 链接。

`no_std` **只对 xtensa 生效**（`#![cfg_attr(target_arch = "xtensa", no_std)]`）——
host 上 no_std 无法 unwind，测试 harness 需要 std。`panic = "abort"`：panic 穿过
开启了 C++ 异常的帧会崩。

---

## 2. 构建

```bash
export PATH="$HOME/.cargo/bin:$PATH"     # 必须在 export.sh 之前
source ~/data/esp-idf-v6.0/export.sh
cd firmware
IDF_TARGET=esp32s3 idf.py build
```

- **`IDF_TARGET=esp32s3` 不可省。** 裸 `idf.py build` 默认 esp32，会在
  SSD2683/st7701 面板依赖处失败。
- **`cargo` 必须先于 `export.sh` 进 PATH**，否则 `export.sh` 重排 PATH 后 cargo 丢失，
  CMake 报 `cargo: command not found`。
- **双门禁不可互替**：`cargo test`（host）**和** `idf.py build`。host 绿 ≠ 设备绿
  （历史上 `to_ascii_lowercase` 在 xtensa 上编译失败）。
- 清理 `build/` 必须同时 `cargo clean`，否则留下陈旧 `librust_firmware.a`。
- **新增 `.rs` 必须登记进 `firmware/main/CMakeLists.txt` 的 `RUST_SOURCES`**
  （显式列表，非 glob）。漏登记 = 增量构建静默链接上一版 Rust 代码。

### 证明改动真的进了产物

不看文件体积，看符号：

```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-nm build/xiaozhi.elf | grep rf_<symbol>
```

### 格式

**没有** formatter / linter / CI / pre-commit。不要新增，也不要跑全仓格式化——
既有文件有大量漂移，全仓重排会淹没真实 diff。只收敛自己新写的代码。

---

## 3. 测试

```bash
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test   # 375
cd server && .venv/bin/python -m pytest tests/ -q                             # 302
```

### Rust 测试要求

- 每个 FFI 模块自带 `#[repr(C)]` POD + **布局契约测试**：断言
  `size_of::<T>()` 与 `offset_of!(T, field)` 逐字段对齐 `rust/include/*.h`。
  头文件里用显式 `_pad` 钉住布局。**不抽共享 `abi_contract.rs`**（曾被否决：
  断言太弱、8 字节对齐判断错误、耦合过重）——每模块自带。
- 覆盖：正常 / 边界 / 陈旧 / 过期 / 缺失 / 计数回滚 / 幂等 / C-ABI 映射。
- 断言 C++ 真正消费的 `Action` 字段与数值，不做源码文本断言。
- 先红后绿：**哨兵必须只打掉对应分支**。run 之后若哨兵没红，说明测试是假的。
- 集成测试放 `firmware/main/rust/tests/`（外部视角，只能看到 pub API）。

### 服务端测试要求

- fixture 必须隔离状态：`conftest.py` 已把 `devices_db`、`pages/`、日志文件指向
  临时目录。**新增会被跨测试泄漏的全局状态时，必须同步清理**——曾因
  `power_counters` 快照跨测试泄漏而掩盖真实缺陷。

---

## 4. 证据纪律（最重要的一条）

**归因以证据为准，错了明确撤回。**

- 没有正向因果归因，**不要改行为**。宁可保持现状并记录「阻塞」，也不要凭相关
  性上一个机制。
- 相关性不是因果：日志里两个现象同时出现 ≠ 一个是另一个的原因。必须找到变量
  变化与现象变化的对应关系，或用对照（改回去是否复现）证明。
- 追不下去就先取证（读源码 / 加日志 / 查真机），**不要猜**。
- 归因错误要**明确撤回**并写明新判据，不是默默改掉。
- 已撤销的例子：曾把 Wi-Fi 断线归因到 modem sleep 档位，并实现了"连续 3 次
  BEACON_TIMEOUT 后抑制 modem sleep"。真机日志否证了它——同一 AP、同一档位，
  一次 130 秒里 14 次 `bcn_timeout` 后断线，另一次 20 分钟零超时。机制还自身
  两处失效（阈值 60s 短于实测 ~140s 存活期导致永不触发；抑制目标 BALANCED
  等于当前档位，是空操作）。已整体 revert。**这个模式要避免。**

### 真机验证

- `/dev/ttyACM0` 打开会**复位芯片**（`rr=11` = `ESP_RST_USB`）。刷写或深睡验收前
  先关串口监视器，且不要两个读者同时读。
- 固件里**没有** OTA 客户端（零 `esp_https_ota`/`esp_ota_begin` 调用点；
  `esp_https_ota` 只是 `main/CMakeLists.txt` 的链接依赖）。升级只有串口刷写一条路。
- 验证前先证明设备跑的是新代码：`nm` 看符号、比对产物 mtime 与 commit 时间。
  空闲设备不打日志 —— **不能**用「没日志」当证据。

---

## 5. 固件频道（三条独立路径，别混）

| 路径 | 谁消费 | 生效时机 |
| --- | --- | --- |
| `firmware/build/xiaozhi.bin` | 后台 `/serial` 串口刷写 | 用户点「刷写」 |
| `server/data/firmware/` | OTA 接口 | **当前设备不会拉取**（无客户端） |
| `server/data/serial-firmware/` | 手动上传的调试件 | 手动刷写 |

后台串口刷写直接读 `firmware/build/xiaozhi.bin`（`serial_firmware.py` 的
`BUILD_ARTIFACT`）——**构建完即出现在后台列表，不需要拷贝**。

不要为了「同步到后台」而 `cp` 到 `server/data/firmware/`：那是 OTA 频道，
补上 OTA 客户端后即等于让设备自动升级到未验证固件。

---

## 6. 服务端与前端

```bash
cd server && ./start.sh status|logs|journal|restart|stop
```

- 服务由 **systemd 用户单元**托管（无 root，已开 linger）。单元文件版本化在
  `server/systemd/`，改它要 `./start.sh restart`（会先 `daemon-reload`）。
- `WorkingDirectory` 必须是 `server/`——应用按工作目录解析 `.env` 与 `data/`。
- 日志分两路：应用自己 `RotatingFileHandler` 写 `data/server.log`（10 MB × 5）；
  stdout/stderr 走 journal。**不要合并**，否则轮转失效。
- **重建前端后必须重启服务端**：`frontend/dist` 在 app 启动时挂载。

### 硬编码值

- 密钥/凭据/端点走 `.env`；`.env` 与 `.env.example` 的键必须同步。
- 新增仅服务端需要的密钥进 `server/.env.example`；固件侧进 `firmware/.env.example`。

（仓库没有 Python linter / formatter，语法错误只能靠导入或测试暴露；
改完 `.py` 至少跑一次相关测试。）

### Python 风格

- 类型标注两种风格**并存**：`typing.Optional[...]`（多数，含 `devices.py`/`pages.py`）
  与 PEP 604 的 `X | None`（较新的 `notify_store.py`/`image_conv.py`）。
  **改哪个文件就跟哪个文件的既有风格**，不要为统一而全仓替换。
- 锁用 `threading.Lock` 或 `threading.RLock`，与所在类的既有用法一致
  （`devices.py`/`notify_store.py`/`pairing.py` 用的是 `RLock`）。
- 模块头写 docstring，说明用途与**磁盘布局/协议契约**（见 `pages.py`、`ota.py` 的范例）。

---

## 7. 关键约束与已知陷阱

### 电池

- **无电流传感器、无燃料计。** 不许把电压换算成精确 SOC 或 mAh，也不许在命名上
  暗示它（`relative_activity` 是唯一"程度"型输出，绝不叫 percentage/capacity）。
- 采样窗口 2500–5000 mV；越界丢弃并**下一轮重试**（与"未到时间"区分）。
- 服务端电池快照**缺 `v/p/c` 时继承上一次读数**（否则徽标在可点/横杠之间闪）；
  部分缺失或越界仍清空（不冻结坏读数）。

### EPD

- **绝不并发调用 `DisplayRaw4ColorImage` 与 refresh task** ——会 busy 卡死。
  必须走 framebuffer blit + `RequestUrgentFullRefresh`。
- **EPD diff / 刷新资格的策略迁移仍处于阻塞**：`rr=4`（`ESP_RST_PANIC`）尚未
  正向归因到具体 panic 行。没有明确豁免，不要动 EPD policy。

### Wi-Fi

- 诊断断线前先核对 `wifi:pm start, type:` 的枚举含义：
  `NONE=0 / MIN_MODEM=1 / MAX_MODEM=2`。`type: 1` 是 IDF 默认的 MIN_MODEM，
  **不是** MAX_MODEM。误读曾导致半小时的错误归因。
- `yi02` 这类不发 TIM IE 的 AP 属 AP 侧问题，设备端只能快速重连。

### 协议兼容（已冻结，不要收紧）

旧 JSON fallback；MQTT 无 scheme 的 `host[:port]` 宽松解析；shim 的注册/注销与
EPD 锁语义；Wi-Fi 的缓存/快连/重连拆分与 `endpoint_missing` 处理。

---

## 8. 提交

- 一个逻辑变更一个提交，可单独回滚。
- 常规提交用 `type(scope): 摘要`（实际用过：`fix` `feat` `refactor` `docs` `chore`
  `revert`）；SDD 分批执行时该批提交用 `Task N: <摘要>`（见 §9）。
- 正文写**为什么**与**证据**（测试数、`nm` 符号、真机日志），不放过程流水。
  范例见 `70aedac`（问题 → 根因 → 改法 → 边界）与 `10278f7`（否证性证据 → revert）。
- 改动的调用点、测试、文档必须一起更新；过时路径/别名/重导出要删干净。
- 不提交（`.gitignore` 已覆盖，勿用 `-f` 强加）：`firmware/build/`、
  `firmware/main/rust/target/`、`firmware/managed_components/`、`firmware/sdkconfig`、
  `frontend/dist/`、`node_modules/`、`.env`、`*.bin`、`*.elf`、`*.map`、`*.zip`、
  `server/*.log`、`server/*.db`、`server/data/{uploads,images}/`、`.worktrees/`。
- 注意 `server/data/pages/` 是**混合目录**：`*.bin`（渲染产物）与 `*.bmp.json`
  不入库，但 `server/data/pages/<device>/*.json`（页面源文件）**属于版本控制**。
- 需要隔离工作区时用 git worktree（`.worktrees/` 已忽略），普通 checkout 先问用户。

---

## 9. 设计与计划文档

较大改动走 `docs/superpowers/specs/`（设计）→ `docs/superpowers/plans/`（计划），
文件名 `YYYY-MM-DD-<主题>.md`。执行记录放 `.superpowers/sdd/<批次>/`。

设计原则（来自既有 spec，继续沿用）：

- 逐格保持旧行为；只有已获真机/日志证据支持的问题才允许顺带修复。
- 每批独立提交、可回滚。
- 阶段之间独立结案；被证据阻塞的阶段保持阻塞，不靠"看起来相关"开工。

---

## 10. 环境速查

| 项 | 值 |
| --- | --- |
| 仓库 | `/mnt/data/project/youn-ink-fourcolor-firmware` |
| 分支 | `2bp`（默认工作分支） |
| 远端 | `origin` = blakejia fork，`upstream` = LazyYoun |
| ESP-IDF | v6.0，`~/data/esp-idf-v6.0` |
| Rust 工具链 | `esp`（esp-rs，`cargo +esp`） |
| 固件版本 | `firmware/CMakeLists.txt` 的 `PROJECT_VER`（当前 `6.5.9`） |
| 服务端口 | 9002（HTTP+WS）、8766（UDP 发现 + HTTP 图片/OTA） |
| 设备 | NOTE4C-3400FC |
| 按键 | 上 = GPIO39，下 = GPIO18，BOOT = GPIO0，LED = GPIO3；RESET = EN 针孔 |
| 配网 | 同时按住上下键 **3 秒**（单键长按 1 秒是导航）；AP `http://192.168.4.1` |
| 下载模式 | 按住 BOOT → 点 RESET → 松 BOOT |
