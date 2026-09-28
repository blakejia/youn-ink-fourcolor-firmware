# NOTE4C 设备日志上报设计

日期：2026-09-28
状态：设计草案，等待用户复核
范围：固件日志的按需上行（默认关闭）、崩溃前日志保留、服务端落盘与后台开关

## 1. 目标与边界

设备当前只能靠**物理串口**观察日志。这带来两个实际问题：

1. **排查必须人在设备旁。** 最近一次 `rr=4` 崩溃排查中，用户人在设备旁才能用网页后台读串口；设备一旦不在身边，就只能靠服务端 HTTP 记录反推。
2. **崩溃现场拿不到。** 本板每次深睡都等于重启，而 `CONFIG_ESP_COREDUMP_ENABLE_TO_NONE=y`（`sdkconfig.defaults:21`）、分区表无 coredump 分区，panic 现场不留痕。已有的 `docs/superpowers/progress/2026-09-24-rr4-diagnosis.md` 结论是「服务端只有 HTTP 200，拿不到 panic 文本」。

本设计给设备加一条**按需的日志上行通道**：默认关闭；打开后设备把最近的日志行送到服务端并落盘成纯文本，供后台直接查看。**崩溃前的日志必须保留**——这是本设计的主要价值。

统一边界（沿用既有约定）：

```text
Rust：纯策略 —— 何时上、上多少、退避、分段、编码、开关状态机
C++/ESP-IDF：内存域（RTC）/日志钩子/HTTP/FreeRTOS/临界区
C ABI：固定 #[repr(C)] POD 输入输出，只传事实和动作
```

### 不在本次范围

- **不在 panic 路径写 flash。** 该方案（把缓冲落 NVS）已评估并否决：本板 `rr=4` 的候选点包含 EPD/SPI 路径，在可能已关闭 cache、栈已损坏时写 NVS 有二次崩溃与卡死风险，换取的是「跨掉电保留」——而设备本来就在电/电池间切换，掉电概率远低于软件崩溃。`.rtc_noinit` 已免费提供崩溃现场存活（见 §3）。
- **不维护 `format → tag/level` 映射表。** 经核实不需要（见 §3）。
- **不做日志压缩、实时推送、搜索/过滤 UI、下载按钮。**
- **不改动** EPD policy、`rr=4` 归因、以及任何既有协议语义。

## 2. 三条约束本设计的既有事实

三条均已核对源码，直接决定方案形状。

**① `CBuf::<160>` 溢出会让整条 query 作废。**
`fetch_schedule` 用 `CBuf::<160>` 构造 path（`firmware/main/rust/src/page_sync.rs:172`）；`push_bytes` 在放不下时置 `overflowed` 并**丢弃该段**，`write!` 返回 `Err`（`firmware/main/rust/src/shim.rs:163-171`、`:207-214`），调用方不得使用结果。当前 path 实测 **93 字节**（`?w=&a=&r=&g=&f=&er=&eb=&rr=` 加满电池三键），余 67 B。

→ **日志正文不能搭车 schedule 查询串**；但**开关是单个布尔量**，可以搭车。

**② 开关能走 schedule 响应体，且已有先例。**
`get_schedule` 返回的 `policy` 块已含 `sleep_window`/`poll_interval_minutes`/`sleep_poll_interval_minutes`/`min_page_duration_minutes`（`server/youn_server/app.py:678-687`）。响应体无长度限制，固件侧已有 `protocol_parse::policy_minutes_to_s` 这类解析（`page_sync.rs:338`）。

**③ `.rtc_noinit` 不被清零，`.rtc.bss` 会被清零。**
`cpu_start.c:466-471`：非深睡唤醒时 memset `.rtc_bss`。因此 `RTC_DATA_ATTR` 的**无初值**变量（如 `g_wakes`，`firmware/main/rust/shim.cpp:185`）在每次非深睡复位后归零——这正是本轮 `rr=4` 后观察到 `w=1` 的成因。
`.rtc_noinit` 是 `NOLOAD` 段（`~/data/esp-idf-v6.0/components/esp_system/ld/esp32s3/sections.ld.in:97-104`），全仓无任何代码 memset 它（grep `_rtc_noinit_start` 仅命中各 SoC 的链接脚本定义）。

→ **崩溃日志放 `.rtc_noinit`，能穿过 panic 重启存活。** 这是本设计的支点，也是「日常与崩溃共用一条通路」成立的原因：崩溃后第一次上报会自然带走上次的尾巴，无需 panic 专属代码。

## 3. 固件 C++：RTC 环形缓冲与日志钩子

### 新增文件

- `firmware/main/rust/shim_log.cpp`（或并入 `shim.cpp`，见 §7 待定项）
- `firmware/main/rust/include/shim_log.h`

照 `g_panel_rec` 的形状（`firmware/main/rust/shim.cpp:87` 的 `RTC_DATA_ATTR static rf_panel_record_t`）：RTC 结构 + `portMUX` 自旋锁 + 一组 `rf_*` 访问器。

### 缓冲

```c
RTC_NOINIT_ATTR static rf_logbuf_t g_logbuf;
```

`NOINIT` 的代价是上电时内容随机、不能依赖 C 初始化，因此需要 `magic` 校验（与 `g_panel_rec` 的 `RF_PANEL_MAGIC` 同一手法，注解见 `shim.cpp:85-90`）。

```c
typedef struct {
    uint32_t magic;    /* 不匹配 => 缓冲作废，重置游标 */
    uint32_t seq;      /* 单调行号，Rust 用它做「自上次 ack 以来」的差值 */
    uint32_t head;     /* 写游标（字节偏移） */
    uint32_t tail;     /* 已 ack 游标（字节偏移） */
    uint32_t dropped;  /* 环形覆盖丢弃的行数 */
    uint8_t  data[2048];
} rf_logbuf_t;
```

行格式：`[seq:u32][len:u16][text:len]`。

- `text` 是 `vsnprintf` 出来的**完整一行**（含级别、tag、uptime），见下。
- **无独立 `uptime_ms` 字段**：`esp_log_timestamp()` 已在线内（`(27930)`），重复存是第二处真相。
- **无 tag/level 字段**：见下，它们已在 `text` 内。

容量：2048 B / 平均行约 70 B ≈ 29 行环形。

### 钩子

```c
esp_log_set_vprintf(&log_capture_hook);   /* 装一次；返回值是原钩子，须链式转发给 UART */
```

**为什么能得到完整一行。** 本板用 **LOG V1**（`firmware/sdkconfig:3335` `CONFIG_LOG_VERSION_1=y`）。V1 把格式与来源在编译期拼进格式串：

```c
/* ~/data/esp-idf-v6.0/components/log/include/esp_log_format.h:15 */
#define LOG_FORMAT(letter, format) \
    LOG_COLOR_##letter #letter " (%" PRIu32 ") %s: " format LOG_RESET_COLOR "\n"

/* ~/data/esp-idf-v6.0/components/log/include/esp_log.h（V1 分支） */
esp_log(config, tag, LOG_FORMAT(I, format), esp_log_timestamp(), tag, ##__VA_ARGS__);
```

所以钩子收到的 `format` 形如 `"I (%lu) %s: EPD busy wait: %u ms\n"`、`va_list` 为 `(uptime, "CustomLcdDisplay", 15000)`。**一次 `vsnprintf` 即得与串口一致的整行**，tag 与级别都不丢。

钩子体内（`log_capture_hook`）必须：

- 用**固定栈缓冲** `vsnprintf`（禁止分配）；
- 在 `portENTER_CRITICAL` 内做纯字节拷贝（照 `shim.cpp:343-365` 的自旋锁用法；该锁是叶子锁，不嵌套）；
- **禁止任何日志调用**——钩子是所有 `ESP_LOGx` 的出口，体内打日志会无限递归；
- 把结果同时**转发给原钩子**（否则串口输出消失）；
- 关闭开关时**仍然写入缓冲**（成本仅一次拷贝），只是不上报。这换来「打开开关后立刻能看到之前的历史」，是有意的取舍。

**可重入要求**：IDF 明确要求该回调可重入、可能被多任务并行调用（`~/data/esp-idf-v6.0/components/log/include/esp_log_write.h:26-27`）。自旋锁 + 定长拷贝满足该要求。

### C ABI

声明进 `firmware/main/rust/include/shim_log.h`，命名沿用 `rf_panel_*` 惯例：

| 符号 | 方向 | 语义 |
| --- | --- | --- |
| `void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines)` | Rust ← C++ | 取最早未 ack 的连续段；**不移动 tail** |
| `void rf_logbuf_ack(uint32_t seq_hi)` | Rust → C++ | 上报成功后才推进 tail |
| `void rf_logbuf_stats(uint32_t* dropped, uint32_t* used)` | Rust ← C++ | 覆盖丢弃计数与已用字节 |

**read 不改状态、ack 才改**——与 `g_panel_rec` 的「pending 只在刷新真正 idle 后才 commit」是同一防丢思路（`shim.cpp:88-90`）。上报失败若不推进 tail，日志在下次重试时仍在。

## 4. 固件 Rust：`log_upload_policy.rs`

纯策略，`Inputs → decide() → Action`，无 IO、无 RTC 访问、无 HTTP。形状对齐 `battery_activity_policy` / `notify_policy`。

```rust
#[repr(C)]
pub struct Inputs {
    /* 两个开关都是三态：0 = 无意见, 1 = 明确关, 2 = 明确开（见 §5 合成语义）。 */
    pub local_set: u8,       /* 设备侧设置菜单（NVS） */
    pub server_set: u8,      /* schedule 响应 policy.log_upload */
    pub has_pending: u8,
    pub wifi_ready: u8,
    pub pending_bytes: u32,  /* @4  */
    pub pending_lines: u32,  /* @8  */
    pub fail_streak: u32,    /* @12 */
    _pad: [u8; 4],           /* @16, 补齐到 8 以对齐 i64 */
    pub last_fail_s: i64,    /* @24 */
    pub now_s: i64,          /* @32 */
}                            /* size == 40，8 字节对齐 */

pub enum Action {
    Skip { reason: &'static str },
    Upload { max_bytes: u32 },
}
```

合成（独立函数，可单独测）：

```rust
pub const OPINION_NONE: u8 = 0;
pub const OPINION_OFF: u8 = 1;
pub const OPINION_ON: u8 = 2;

/// 服务端明确 → 服务端；否则本地明确 → 本地；都无意见 → 关。
pub fn resolve_enabled(local_set: u8, server_set: u8) -> bool {
    match server_set {
        OPINION_ON => true,
        OPINION_OFF => false,
        _ => local_set == OPINION_ON,
    }
}
```

决策表（顺序即优先级，与 `power.rs` 同风格）：

| 条件 | Action | reason |
| --- | --- | --- |
| `resolve_enabled(...) == false` | Skip | `"disabled"` |
| `has_pending == 0` | Skip | `"empty"` |
| `wifi_ready == 0` | Skip | `"no_net"` |
| 退避未到期 | Skip | `"backoff"` |
| 否则 | Upload | — |

- **关掉时 `resolve_enabled` 必须先判**，两态合成失败（都无意见）也走 `"disabled"`，与「默认关闭」一致。

- **退避复用既有模式**（60 s → ×2 → 上限 900 s；失败递增、成功清零），与 `notify_policy` 一致，不发明第二套约定。
- **分段上限 1024 B/次**：单次 HTTP 在深睡周期内有超时预算（`notify.rs` 用 10 s），且请求体要能放进定长缓冲。
- **请求体**：`{"seq_hi":N,"dropped":D,"lines":"<base64>"}`。base64 避免正文非 ASCII 字节破坏 JSON；`dropped` 让服务端知道丢了 N 行（不静默）。1024 B 原始 → 约 1.4 KB base64，body 缓冲按 **2048 B**。
- **不解析日志内容**：Rust 只搬运。

### 接线

- 在 `RunPowerCycle` 的既有 schedule 周期内追加一次上报尝试（`firmware/main/application.cc:1219` 附近，`page_sync_sync_once()` 之后）。
- 开关从 schedule 响应解析，存 RTC（C++ 侧持有），随 `Inputs` 传入。
- `lib.rs` 加 `pub mod log_upload_policy;`；`CMakeLists.txt` 的 `RUST_SOURCES` 加对应文件（**显式列表，非 glob**，见 `firmware/main/CMakeLists.txt:175-200`；漏登记会静默链接旧代码）。

## 5. 服务端

共有**两个**开关：设备侧（本地，面板设置菜单）与服务端（后台 UI）。两者都是信任点；**冲突时以服务端为准**。

### 合成语义（三态，可判定）

「冲突」要求两侧都表达过意见，所以服务端开关是**可空三态**，不是布尔：

| 服务端 | 本地 | 生效 | 依据 |
| --- | --- | --- | --- |
| 明确开 / 明确关 | 任意 | **服务端** | 冲突以服务端为准 |
| 无意见（`NULL`） | 开 / 关 | **本地** | 无冲突 |
| 无意见 | 无意见 | 关 | 两侧都未表达 |

服务端列因此是 `log_upload INTEGER DEFAULT NULL`（`NULL` = 无意见），**不是** `DEFAULT 0`——后者等于「永远有意见」，会让本地开关彻底失效，与「两侧都能开」矛盾。后台 UI 相应是三态控件（开 / 关 / 跟随设备）。

设备侧本地开关存 NVS（`Settings` 类，`firmware/main/settings.h`），新增设置项照 `Kind::Toggle` 机制（`settings_menu.h:69-87` 加 `RF_SETTINGS_ITEM_LOG_UPLOAD = 12`；Rust 菜单模型在 `settings.rs`，模板是 `ITEM_WIFI_TOGGLE`）。

**本地开关必须落 NVS，不能照抄 Wi-Fi 开关。** 现有的 `g_wifi_switch_intent`（`application.cc:145`）是 RAM 静态量、不持久化；设备每 10 分钟深睡一次会掉 RAM，本地设置会静默失效。

### 下发与回传

- 下发：`get_schedule` 的 `policy` 块加 `"log_upload": null | 1 | 0`（`app.py:678-687`）。
- 回传：schedule **上行**加 `&lo=<0|1|2>`（本地意见：0 无意见、1 关、2 开）。实测 path 现在 93 B、上限 160；最坏情况（各计数器取 u32 最大值）加 `&lo=2` 后 125 B，余量充足。
- 后台据此显示状态：服务端明确关且本地开 → 「设备端已开（服务端已覆盖）」。**没有这个回传，运维看到「关」会以为自己没点上。**

### 上行端点

`POST /api/device-log`，照 `ack_notification` 范式（`app.py:514-528`）：

- 鉴权复用 `_require_device_token`（`app.py:120-140`）；它内置 `registry.touch()`，因此上报也会刷新「最近在线」。
- 校验：`seq_hi` 为 int；`dropped` 为 int ≥ 0；`lines` 可 base64 解码且解码后 ≤ 4096 B。**不解析内容**。

### 存储

- 目录：新配置 `device_log_dir`（默认 `./data/devicelogs`），并**加入 `config.py:86` 的 `resolve_paths` 列表与 `:98` 的 mkdir 循环**——该循环显式创建全部目录，漏加会在首次写入时 FileNotFoundError。
- 文件：`<device_log_dir>/<device_id>.log`，**纯文本追加**，一行一条：
  ```text
  2026-09-28T12:30:11+08:00 I (27930) CustomLcdDisplay: EPD busy wait: 15000 ms
  ```
  行首是**服务端接收时刻**；正文里的 `(27930)` 是**设备 uptime 毫秒**。两者不同，UI 须标清（见 §6）。
- `dropped > 0` 时写 `... [dropped N lines]` 标记行。
- **轮转自己写**（超阈值 rename + 重开，5 MB × 3），不用 `RotatingFileHandler`：追加语义下其 `doRollover` 更绕，而这里只需 20 行，且能与 `dropped` 标记共用一条写路径。

### 读取端点

`GET /api/devices/{id}/logs?tail=N`，走 `_require_operator`（运维鉴权，与 `power-history` 同类，`app.py:267`），`tail` 上限 1000。

### `.gitignore`

加 `server/data/devicelogs/`（照 `server/data/uploads/` 写法）。

## 6. 前端

- `frontend/src/pages/Devices.jsx` 表格加一列「日志上报」。**三态控件**（开 / 关 / 跟随设备），对应服务端的 `1 / 0 / NULL`；照既有 `BusyButton` 范式（`:321-328`）。
- 该列同时显示**生效结果**与**本地意见**（后者来自 schedule 上行的 `lo=`）：例如服务端明确关、本地明确开 → 「服务端已覆盖：设备端已开」。只看服务端值会让运维以为没点上。
- 日志查看复用 `BatteryDetail` 的弹窗范式（`:437` 一族）：等宽 `pre`、显示服务端时间戳列、顶部显示「共 N 行 / 已截断」、一个手动刷新按钮（**不做自动轮询**）。
- `frontend/src/api.js` 加 `setLogUpload(id, value)`（`value` 为 `true`/`false`/`null`）与 `deviceLogs(id, tail)`。既有 `request()` 在非 JSON 时返回 `Response`（`api.js:21-23`），`deviceLogs` 自行 `.json()`，**无需改 `request()`**。
- **UI 必须标明**：正文里的 `(27930)` 是设备 uptime，不是墙钟；行首才是服务端接收时刻。
- 重建前端后**必须重启服务端**（`frontend/dist` 在 app 启动时挂载）。

## 7. 实施与验证顺序

每步独立可验证、独立提交、可单独回滚。

1. **服务端**：可空列 + 两端点 + `policy.log_upload` 三态下发 + 目录 + 轮转 + pytest。可先脱离固件用 curl 验证。
2. **固件 C++**：缓冲 + 钩子 + ABI + 布局契约测试。
3. **固件 Rust**：`log_upload_policy.rs`（含 `resolve_enabled` 合成）+ 单测 + 接线；schedule 上行加 `&lo=`。
4. **固件本地开关**：`settings_menu.h` 新项 + `settings.rs` 菜单 + NVS 读写 + 设置页 Toggle 接线。
5. **前端**：三态控件 + 弹窗 + `api.js`。
6. **端到端真机验收**：两侧开关的四种组合各验一遍；制造一次复位 → 崩溃前日志仍在。

### 测试要求（沿用既有纪律）

Rust：

- 决策表每分支：正常 / 边界 / 陈旧 / 过期 / 缺失 / 计数回滚 / 幂等 / C-ABI 映射。
- **布局契约测试**：`size_of::<Inputs>()` 与逐字段 `offset_of!` 对齐 `rust/include/log_upload_policy.h`；头文件用显式 `_pad` 钉住。
- **哨兵必须只打掉对应分支**；run 后若哨兵没红即测试是假的。
- 断言 C++ 真正消费的 `Action` 字段数值，不做源码文本断言。

服务端：

- fixture 必须隔离新目录——`conftest.py` 已隔离 `devices_db`/`pages/`/日志，新目录同步；这仓库有 `power_counters` 跨测试泄漏掩盖真缺陷的前科。
- 用例：开关默认关 / 开→下发 / POST 鉴权（无 token、错 token、未信任）/ base64 解码 / 超大 body 拒绝 / `dropped` 标记 / 轮转 / tail 端点。

门禁：`cargo test` 与 `IDF_TARGET=esp32s3 idf.py build` **不可互替**；服务端 `pytest`；前端 `npm run build`。

### 已核实的两处接线细节

- **`shim_log.cpp` 用独立文件**，并登记进 `firmware/main/CMakeLists.txt` 的 `SOURCES` 列表（该列表显式逐文件登记，`rust/shim.cpp` 即在 `:64`）。独立文件与 `shim.cpp` 的关注点不同，分开更清楚。
- **钩子链式转发**：`esp_log_set_vprintf` 用 `__atomic_exchange_n` 返回**旧钩子**，默认值是 `&vprintf`（`~/data/esp-idf-v6.0/components/log/src/os/log_write.c:17-26`）。保存返回值，钩子内先拷贝进缓冲、再调用它，串口输出即完好。
  所有 `ESP_LOGx` 都经由这一个函数（`log_print.c:17`、`log.c:43`），**UART0 与次要 USB-Serial-JTAG 控制台（`sdkconfig:2728,2740`）不构成两条路径**——分发发生在 `vprintf` 内部，与钩子无关。

## 8. 风险

- **钩子在任意上下文被调**，包括 ISR 与 cache 关闭窗口。设计用自旋锁 + 定长拷贝，且禁止体内日志调用；但若 `vsnprintf` 在 cache 关闭时不可用，钩子需在该窗口直接跳过（实现时验证）。
- **关闭开关仍有写入成本**（一次 memcpy + 定长格式化）。刻意如此，换「打开即有历史」。
- **`.rtc_noinit` 上电内容随机**，靠 magic 兜底；magic 校验失败即视作空缓冲。
- **崩溃日志可能被崩溃后的新日志挤掉**：环形 29 行，崩溃重启后设备会继续写。首次上报（周期 10 分钟）通常早于覆盖，但**不保证**；若真机发现被覆盖，可缩短崩溃后首次上报的延迟。
