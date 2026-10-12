# 24 小时设备日志审计 — 2026-10-08 11:16 → 10-09 11:16

问题：拉最近 24 h 的设备日志，看有没有报错和相关异常。

**数据源与切分方式**（两个源必须一起看，单看一个会误判）：

| 源 | 覆盖 | 切分方式 |
|---|---|---|
| `server/data/devicelogs/NOTE4C-3400FC.log` | 09-29 09:16 → 10-09 11:12（13587 行） | 按服务端 ISO 时间戳切批；同一时间戳 = 同一批（`devicelog.append_lines` 给整批打同一个 `received_iso`）。续行（`wifi:` 的第二行等）无 `I (ms) tag:` 前缀，不能按正则分 tag |
| `journalctl --user -u youn-ink-server` | 每行独立时间戳，覆盖到上次重启 | 唯一有请求行时间戳的来源；`server/data/server.log` 的 uvicorn 访问行**没有**时间戳（该文件第 8..28186 行之后一条访问行都没有），不能按字符串比较切日期 |

窗口内设备日志 2715 行：`I` 2331 / `W` 104 / 续行 203 / `dropped` 标记 77。

---

## 1. 结论：窗口内 0 条 `E`，104 条 `W` 里只有 1 条真异常

| 类 | 条数 | 判定 |
|---|---|---|
| `E (...)` 设备错误 | **0** | — |
| `W wifi:` → 续行 `Password length matches WPA2 standards, authmode threshold changes from OPEN to WPA2` | 37 | IDF 握手噪声，每轮唤醒一次，无行为影响 |
| `W CustomLcdDisplay: EPD busy wait: 5000/10000/15000(/20000) ms` | 66 | 刷新任务等波形完成，四色面板全刷本就 ~40 s，**设计噪声** |
| `W PageSync: log upload failed (status=400), 85 lines stay pending` | **1** | **唯一真异常**，见 §3 |

全文件 `panic|guru|abort()|assert failed|stack smashing` 命中 **0**。
全文件 16 条 `E (...)` **全部在 2026-09-30**（06:01/06:21/11:15），内容是
`esp-tls: select() timeout` → `HTTP_CLIENT: Connection failed` →
`GET https://note-device.1024.center:31443/api/... failed: ESP_ERR_HTTP_CONNECT`，
即**公网端点连不上**，与本窗口无关（本窗口请求全 200，端点本身在服务）。
**撤回**本报告初稿里的"本窗口走 LAN"：§9.3 核实设备走的就是这条公网端点，
journal 里的 `10.0.0.10` 是透明代理的回源地址，不是设备。

## 2. 丢行：77 个 `dropped` 标记，共 10223 行，全部在刷入前

按 `devicelog` 语义，`dropped` 的行**不在文件里**，所以真实产出 = 实收 + dropped：

| 阶段 | 单轮真实产出 | ring | 结局 |
|---|---|---|---|
| 10-08 11:16 – 16:53（旧固件 2048 B 环） | 1000–25000 B（最高 25358 B） | 2048 B | 丢 57–97%，**绝大多数轮次丢 80%+** |
| 10-08 23:29 – 23:50（短暂干净） | ~1300 B | 2048 B | 0 丢 |
| 10-09 07:49 之后（新固件 6144 B 环，`5a6c342`） | ~1300–1830 B | 6144 B | **08:10 起 21 批连续 0 丢** |

`07:59:56` 那批 dropped=1 是新旧环交界的一次残留；`08:10:02` 之后全部为 0。
**这是 09-28 上线设备日志以来第一段完整无缺口的上报。**

## 3. 唯一真异常：一次 `device-log` 400（已自愈，原因确定）

| 证据 | 内容 |
|---|---|
| journal（旧进程 990） | `Oct 09 07:39:41 POST /api/device-log HTTP/1.1" 400 Bad Request` |
| 设备日志（在 07:49:50 那批里） | `W (11779) PageSync: log upload failed (status=400), 85 lines stay pending` |

同一次事件。**根因是刷写与服务端重启之间的 90 秒错位**，不是固件缺陷：

1. `07:39:3x` 用户刷入带 6144 B 环的固件（`5a6c342`），设备带着**旧会话积压的 85 行**启动；
2. `07:39:41` 设备上传 → 打到**还在跑旧代码的**服务端（`app.py` 上限 4096）→
   `decoded body too large` → 400。这正是 `log_upload_policy.rs` 注释警告的耦合
   （"The server's decoded-body limit MUST stay above this, or every batch is a 400"）；
3. `07:41:13` 服务端重启（上限 8192）→ `07:49:50` 同一批重传，`201 Created`，积压 0。

偏差校验：该批里最老的设备行 uptime 617009 ms，配服务端接收时刻 07:49:50 ⇒ 启动于
07:39:34，与 journal 里 `07:39:38 w 1→1 rr=11` 的复位对齐。

## 4. 复位与在线性（`rr` 逐轮快照）

| `rr` | 次数 | 时段 | 判读 |
|---|---|---|---|
| `4` = PANIC | 31 | 10-08 11:27 → **16:53:56 止** | 见 §5 |
| `11` = USB | 76 | 10-08 14:34 → 10-09 11:12 | 烧录 / USB 端口被打开（打开即复位） |
| `8` = DEEPSLEEP | 1 | 10-08 22:30 | 正常唤醒 |

**刷入（07:39）之后 0 次 `rr=4`**，全部是 `rr=11`。
窗口内 `a=0&r=0` 贯穿全程 ⇒ 设备一直插着 USB（`power.rs:68` 的 `mains` 短路），
**没有进过深睡**，所以这个窗口**不能**用来判省电效果。

## 5. 一处未解：10-08 16:53 → 22:28 的 5 小时 34 分空档

| 证据 | 值 |
|---|---|
| 服务端收到的设备请求 | 16:43:45 之后到 22:30:19 之间 **一条都没有**（journal 逐条核过；同期 MCP 请求 204 条，服务端本身没停） |
| 空档前最后一次会话 | `16:53:56 GET ... w=1&a=0&g=0&rr=4` + `16:53:59` 上传 → 干净启动、`wrote=1 ssid=yi02 channel=1`、`schedule updated: 3 pages (0 carried)` |
| 空档后恢复时 | 22:30:22 那批首行 `... [dropped 1787 lines]`（全文件最大单批），启动于 22:28 前后 |
| 网络变化 | 空档前 `192.168.3.129`（AP `yi02`，ch1）→ 空档后 `10.0.0.171`（AP `媞媞家的无线`，ch11） |

**可确定**：设备在空档里**一直在醒来并写日志**——`dropped 1787` 意味着本地写了约
1800 行才被覆盖，单次启动产不出这个量（冷启动批 ~180 行）。
**不可确定**：为什么连不上。计数器在恢复时是 `w=1&a=0&g=0`（未 ratchet，与"整段没有一次
成功 sync"一致），而 `er=2&eb=48638` 保留 ⇒ RTC 段没被清，**但环里覆盖空档的行已被丢弃，
两端日志都没有空档内的记录**。空档前那次会话以 `rr=4`（PANIC）启动收尾，是本项目
`2026-09-24-rr4-diagnosis.md` 里那条**仍未正向归因**的 panic 线；本窗口的 31 次 `rr=4`
全在刷入前，**刷入后一次都没有复现**。

⇒ 这条需要一个**设备侧的动作**才能关掉：在电池态下拔掉电池再上电（非 USB 复位），
看崩溃前的日志是否留存。列为待办，不在本报告下结论。

## 6. 服务端侧（同窗口，journal）

| 状态码 | 次数 | 说明 |
|---|---|---|
| 200 | 848 | 正常 |
| 201 | 98 | `device-log` 与页面写 |
| 400 | 8 | **7 条 MCP 协议探测**（`POST/DELETE/GET /mcp/`，客户端握手试探）+ **1 条 `device-log`**（§3） |
| 401 | 3 | `127.0.0.1` 本地手测（`.env` 尚未注入 token 时） |
| 4xx 其他 | 2 | `HEAD /mcp/` 405、`GET /mcp/` 406（MCP 客户端探测） |
| **5xx** | **0** | — |

`[ERROR]` 行只有两类，且**全部出现在 01:42–03:41**：`OPERATOR_TOKEN is empty` × 14、
`MASTER_KEY is empty` × 10。`server/.env` 存在，那段时间在反复重启/token 未注入，
属自造噪声，不是生产故障（判据：同窗口设备 `schedule` 全部 200）。

## 7. 待办

1. §5 的 5h34m 空档 — 需一次电池态拔电重上电，才能判断是网络丢失还是卡死。
2. `rr=4` 归因 — 本窗口刷入后 0 次复现，仍阻塞（同 `2026-09-24-rr4-diagnosis.md`）。
3. 深睡占空比的省电验收 — 本窗口全程插 USB，`a=r=0`，**本窗口不能作为判据**；
   需拔线长跑（`2026-10-09-physical-access-checklist.md` 阶段 1）。

---

## 8. 受控重启与持续监测（2026-10-09 11:39–）

用户无法插拔/断电，授权用刷写方式重启。执行记录：

### 8.1 刷写前的先决检查（结果否掉了"需要刷写"这个前提）

| 检查 | 结果 |
|---|---|
| 端口持有者 | 无（`/proc/*/fd` 全扫） |
| 10 s 内 USB 枚举次数 | 0（链路稳定，非复位循环） |
| 构建产物 vs 固件 HEAD | `shim_log.cpp` mtime 07:36:33 < bin 07:37:58；07:37 之后无新固件提交 ⇒ **当前** |
| 镜像里的环魔数 | `0x4c4f5f31`（= `0x4c4f4731 ^ 6144`）在 bin 偏移 `0x1611bc` ⇒ `bc5abb4` 的魔数版本化**已在构建里** |
| `ota_1` @0x410000 | **整片 0xFF（未写）** |
| `otadata` 条目 0 | `seq=1 state=2`，但**存储 CRC 不匹配**（`0x4743989a` vs 计算 `0x75944181`）⇒ 引导器不采用它，回落到 `ota_0` |
| `ota_0` @0x20000 的 `app_elf_sha256` | `3c181041…9030` = **与已构建镜像逐字节相同** |

⇒ **设备已经跑着 HEAD**。内容相同的重刷只会平白增加一次半写风险，因此改为**复位重启**
（`esptool --after hard-reset chip-id`），不用写 flash。这也解释了审计 §4 里为何全程 `rr=11`。

### 8.2 备份（已做，刷写路径仍可用）

`/tmp/pre-rr-reboot-20261009-1139.bin`（0x0..0x80000，512 KB）
SHA-256 `561300b3780d1319f4c4c2ec8bf89404866280102edb6652c8f483ff9840893a`
（`firmware/backup/` 已有 09-11/09-15/10-08 的旧备份可持续沿用）

### 8.3 重启结果（11:41:38 复位）

| 项 | 值 |
|---|---|
| 复位类型 | `rr=11`（USB，来自 RTS 硬复位） |
| 上电→上线 | 5.3 s 拿到 IP `10.0.0.178`，6.5 s 内 `cache_writeback wrote=1` |
| 首轮上报 | `PageSync: log upload ok: 86 lines, 5614 B` → 服务端 `201` |
| 残留丢行 | `dropped 3`（重启前环里的尾巴，非新问题） |
| 服务端自复位起的错误 | **0** |

**5614 B 单批被完整接收**，这是 6144 B 环 + 服务端 8192 B 上限这条耦合链的端到端正证。

### 8.4 持续监测（已挂成 systemd 用户服务）

`note4c-monitor.service` → `~/.local/share/note4c-monitor/monitor.py`，日志
`~/.local/share/note4c-monitor/monitor.log`。`Linger=yes` + `enabled`，重启主机后自动起。

**刻意不碰 `/dev/ttyACM0`**——打开该端口即复位芯片，会把要观测的占空比变成冷启动。
数据全取自设备日志文件与 `journalctl`。监视项：

- 每次 `schedule` 轮询（含 `w/a/r/g/er/eb/c/v/rr` 增量与非 200 状态）
- 每次 `device-log` 上传（非 201 即告警）
- 设备侧 `E (...)`、panic/backtrace/assert
- `[dropped N lines]`（环溢出）
- **静默告警**：>20 min 无轮询 —— 正是 §5 那个 5h34m 空档的指纹
- 服务端 `Started/Stopped youn-ink-server`

（`server/data/devicelogs/NOTE4C-3400FC.log` 里设备自打的错误行形如
`<iso> E (1234) tag: msg`，**不带** `[E]`；判据已按此写。）

### 8.5 监测运行证据（11:41–11:52）

| 时刻 | 事件 |
|---|---|
| 11:41:16 | `w=1&a=0&r=0&g=0&er=0&eb=0&rr=11` — 复位后第一次轮询，计数器全零 |
| 11:41:19 | `POST /api/device-log` **201** |
| 11:41:42 | `w=1`（与 11:41:16 同为冷启动，`w` 未增）|
| 11:41:45 | 上传 `86 lines, 5614 B` **201** |
| **11:51:51** | `w=2&g=2&er=2&eb=39578&rr=11` **200** |
| 11:51:54 | 上传 `84 lines, 5524 B` **201**，`dropped 1` |

**轮询间隔 11:51:51 − 11:41:42 = 609 s**，落在 `poll_s` 600 s + 请求耗时的预期内
（`PageSync: policy: poll=600s sleep_poll=3600s screen_active=true (boot)`）。
插电态下 `a=0&r=0` 是构造使然（§4），不是回归。

监视器自身已按事件格式落盘（`POLL#` / `UPLOAD#` / `DEV-DROPPED` / `DEV-PAINT` /
`DEV-ERR` / `DEV-WARN` / `!! STALL`），并额外常抓 `show page` / `skipping repaint` ——
这两行正是 2048 B 环时代**永远被挤掉**、而判定冗余下载所必需的证据。

---

## 9. 复核中修正/加固的三点

### 9.1 `dropped` 是"自上次成功 ack 以来的累计"，不是单轮值 — §2 的求和方法成立

`shim_log.cpp`：每覆盖一行 `g_ring.dropped++`（:139），**只有** `rf_logbuf_ack`
（:241）在收到成功上报后才按上报值扣减（:264）。所以一批里报的 `dropped`
= 距上次成功上传的净覆盖行数。⇒ 把各批 `dropped` 相加就是总丢失量（§2 的 10223 行算法正确）；
而去重/累计时不能再做别的处理。

**副作用**：一次**失败**的上传不扣减。07:49:50 那批 `status=400` 后报 `85 lines stay
pending`（= 未 ack），因此 `dropped` 未被扣减，其残留出现在下一批。

### 9.2 环是 `RTC_NOINIT_ATTR`（跨复位存活），且有魔数版本化

`RTC_NOINIT_ATTR static Ring g_ring`（:74），魔数 `0x4c4f4731 ^ kDataBytes`（:50）。
跨复位存活 + 容量进魔数 ⇒ 4096↔6144 的容量变更会让旧环被识别为脏并重置，
这正是 `bc5abb4` 修掉的坑（否则新固件继承旧环游标，`lines==0` 永不上报）。

### 9.3 §5 空档的量化判据（仍非定论，但更强）

设备**不走 LAN**：其 `server_url` 是公网端点
`https://note-device.1024.center:31443`，本机解析到伪造 IP `198.18.16.106`（透明代理网段），
同一端点本机实测 `HTTP 401`（在服务）⇒ 服务端 journal 里那些请求的
`10.0.0.10` **是代理的回源地址，不是设备**。所以"设备 10.0.0.x"的说法是错的。

在空档 5.57 h 里：`dropped` 从 0（16:53 那次 201 已扣减）累到 **1787**。
一个正常 600 s 占空比周期约写 180（冷启动）+ 90（画板决策）≈ 270 行，每小时 ~1620 行；
实测只 **~328 行/小时**（1787/5.57）。**差 5 倍**，且期间零上传（无 ack）。

⇒ 空档里设备**没有在跑正常的 600 s 轮询周期**（若在跑，行数会高一个量级），
更像卡在低频重试/等待状态。**但 `w` 无法在运行期归零**（`g_wakes` 只在
`rf_power_count_wake` 自增，`shim.cpp:453`，无清零路径），而空档前最后一次轮询已是
`w=1` —— 这条与"整段只发生一次从零开始的启动"最自洽。两端的记录都缺空档内部，
仍须一次电池态拔电重上电才能定论。

---

## 10. 重启后 50 分钟窗口的终报（11:41:45 → 12:32:21）

| 指标 | 值 |
|---|---|
| 轮询 | 6 次有效（`w` 2→6），间隔 **606 / 609 / 606 / 606 / 606 s**，贴 `poll_s` 600 s |
| 上传 | 7 次，**全部 201**（含复位后那一次） |
| 服务端 非 2xx | **0** |
| 服务端 `-p warning` | **0** |
| `dropped` | 仅 11:52 那次 **1 行**（复位残留），此后 0 |
| 单批字节 | 5614 / 5524 / 1987 / 1374 / 1587 / 1319 —— 全部 < 6144 环容量 |

### 10.1 画板决策（**新环带来的、以前拿不到的判据**）

| 时刻 | `w` | 决策 | `er`/`eb` |
|---|---|---|---|
| 12:01:57 | 3 | `glass already shows "e34cb95f", skipping repaint` | 2 / 39578（不动） |
| 12:12:06 | 4 | `show page 3/3 md5="df8ba648"` | 3 / 59367（+1 / **+19789**） |
| 12:22:12 | 5 | `glass already shows "df8ba648", skipping repaint` | 3 / 59367（不动） |
| 12:32:18 | 6 | `glass already shows "df8ba648", skipping repaint` | 3 / 59367（不动） |

行为正确：页未变 ⇒ 跳过重绘且 `eb`/`er` 不增；页变了 ⇒ 刷一次，步长 19789 ms
与既有 19.8 s 一致。这两行正是审计里判定"2048 B 环时代永远看不到"的那一行。

### 10.2 唯一一条设备 `E`：两个 ISR 安装者抢同一个服务（P3，非回归）

`E (269) gpio: gpio_install_isr_service(533): GPIO isr service already installed`

机制（已核到源码，非印象）：

- `button_gpio.c:58` 先装，用 `ESP_INTR_FLAG_IRAM`；
- 板级 `zectrix-s3-epaper-4.2.cc:363` 后装，用 `0`；
- IDF `gpio.c:534` 的 `GPIO_CHECK(gpio_context.gpio_isr_func == NULL, …)` **在比较 flags 之前**
  就返回 `ESP_ERR_INVALID_STATE`，于是 IDF 自己打这条 `E`。

**无害的两条证据**：① 板级路径显式容忍该错误
（`if (isr != ESP_OK && isr != ESP_ERR_INVALID_STATE)`，:364），服务不会重复分配；
② 严格的那条（`button_gpio` 的 `ESP_RETURN_ON_FALSE(ret == ESP_OK, …)`）**从未触发** ——
全日志 0 次 `Install gpio interrupt service failed`，说明始终是 button 先装成功。

**为什么以前没见过**：它发生在 `t≈259–269 ms`，是启动 burst 里**最早**的两行；
2048 B 环下这些行必被覆盖（`shim_log.cpp:24` 的 "28 kept + 36 dropped"），
换 6144 B 后才留下来。全日志 4 次全部在 11:40–11:41（即本机 4 次 esptool 打开端口
触发的复位），与固件版本无关。登记 P3：两个安装者应统一 flags（或在 board 侧改成先查后装），
但不影响功能。

### 10.3 监测服务状态

`note4c-monitor.service` 全程 `active`、`NRestarts=0`，已按事件落 `POLL/UPLOAD/DEV-PAINT/
DEV-DROPPED/DEV-ERR/DEV-WARN/!! STALL`。若要长期留观，无需再动设备。

---

## 11. 重启后 11.7 小时全量复查（11:41 → 23:37，68 批）

数据源：`server/data/devicelogs/NOTE4C-3400FC.log`（11:41:45 之后 1955 行）
与 `journalctl` 72 次轮询。设备全程插 USB（`rr=11`、`a=r=0`）。

### 11.1 结构面：全绿

| 项 | 结果 |
|---|---|
| 批次 | 68 批连续，上行时间**严格递增无缺口**（唯一重叠是首两批的 1 行，见 11.3） |
| 上传 | 71 次 `log upload ok`，服务端 **0 次非 201** |
| 单批字节 | 1319–5614 B，全部 < 6144 环容量 |
| `dropped` | 仅复位残留共 4 行（2 批），此后 **0** |
| `err/panic/backtrace` | **0** |
| 轮询间隔 | 606 s（±1 s），贴 `poll_s` 600 s |

### 11.2 **BUG（P1）：`er`/`eb` 记录了 33 次刷新，而 `show page` 只有 26 次

现象：12 小时内 `er` 从 0 走到 33（`eb` 从 0 走到 653087），而设备日志里
`show page N/3` 只有 **26** 条；且有 **9 次** `er` 一次 **+2**、`eb` 一次 **+39578**。

**账本语义是硬的**：`rf_power_add_panel_refresh()`（`shim.cpp:456`）在
`EPD_TurnOnDisplay()` 的**唯一出口**（`custom_lcd_display.cc:980`）被调用一次，
`g_panel_refreshes++`、`g_panel_busy_ms += 实测耗时`。所以
**`er`/`eb` 精确等于 `EPD_TurnOnDisplay()` 的真实调用次数**，不掺水。
⇒ 那 9 次双计数，对应的是**同一次换页决策里跑了两次完整波形**。

**为什么 26 与 33 对不上而不是 9**：这 9 次多出来的波形**没有自己的 `show page` 行**，
因为 `custom_lcd_display.cc` 顶部有 `#undef ESP_LOGI`（该文件内所有 INFO 被编译掉，
这也是当初 `[REFRESH]` 0 命中的原因）——所以"第二次刷新"在日志里**必然静默**。
不能靠"只有 1 条 show page"证明只刷了一次。

**波形侧交叉验证**：`EPD busy wait` 是 `read_busy()`（:854）阻塞时每 5 s 一条。
四色路径一次刷新会调用它 **4 次**（`EPD_Display` 写 `0x10` 后 1 次
（:1100）+ `EPD_TurnOnDisplay` 内 `0x04`/`0x12`/`0x02` 各 1 次），故
**一次刷新 ≈ 3–4 条、两次 ≈ 6–8 条**。实测只有 **3 条（15 批）** 与 **6 条（9 批）**
两种取值，两簇清晰分离、互不重叠；6 条批的 `er` 恰好 +2、3 条批的 `er` 恰好 +1。
⇒ 无论按哪种口径分组，**结论一致：双刷确实发生了 9 次**。

**影响**：① `er`/`eb` 是省电验收的账本（`power-performance-status.md` §2/§4），
现值**偏高 9 次**（33 vs 26）；② 每次多刷 = 面板多吃一次 ~19.8 s 的电，是真实功耗浪费；
③ 频率与内容换页同步（每 30 分钟一轮中的某一次），不是偶发。

**尚未定论的部分**：第二次 `EPD_TurnOnDisplay()` 由谁发起——候选路径在代码里存在
（`application.cc:692` 的 `RequestUrgentFullRefresh("ui")`、
`shim.cpp:308` 的 canvas 路径、`EPD_DisplayPart()`），但**日志无法区分**
（相关 INFO 被 `#undef` 掉）。按证据纪律，此处**不猜机制、不改代码**；
定位需要在该文件恢复一行带 `reason` 的 `ESP_LOGW`，或真机 backtrace。

### 11.3 一个小的自愈瑕疵（非 bug，但会误导）

首两批重叠了 **1 行**（`W (21249) EPD busy wait: 5000 ms`，同时出现在 11:41:45 与 11:51:54）。
按 `shim_log.cpp:264`，`dropped` 只有**成功 ack** 才扣减；复位前那批上传成功与否决定了
残留是否被清。**只出现 1 次、之后 68 批无重叠**，判为复位切换期的边界抖动，无需处理。

### 11.4 页内容每 30 分钟真变 ⇒ 25 次刷新是合理的（**不是 bug**）

服务端 `page upserted` 自 11:41 起 72 次（2/小时 × 3 页，每 30 分钟整点重推全部三页），
每页 **24 个不同 md5**。核对 `task-radar.json` 内含**实时时钟**文本
（`NOTE4C 任务雷达  10-09 23:30`）与"逾 N 天"计数器 ⇒ **内容确实每轮都变**，
刷新不是冗余。

⇒ **顺带撤回** `docs/superpowers/progress/2026-10-09-paint-reduction-options.md` 里
"15 次唤醒 9 次刷屏 = 冗余"的推断口径：在每 30 分钟内容真变的排期下，
12 小时里 24 次换页对应 24 次刷新，**当前观测到的 33 次里有 9 次是 11.2 的重复刷新**，
净冗余为 0。

### 11.5 `f`（refresh_submit_ms）0 → 1：符合构造，非回归

`f` 在 22:19 首次从 0 变 1，之后保持 1。`shim.cpp` 只统计
`RequestUrgentFullRefresh()` 的**入队**耗时（每次 <1 ms），故 22:19 之前恒 0、
之后每次入队记 1 ms。**归因正确，不是卡顿。**

### 11.6 已归因 P3（复述）：两个 GPIO ISR 安装者

`button_gpio.c:58`（`ESP_INTR_FLAG_IRAM`）与 `zectrix-s3-epaper-4.2.cc:363`（flags `0`）
先后安装；IDF `gpio.c:534` 在比 flags 前就返回 `ESP_ERR_INVALID_STATE`，自打
`E (269) gpio: … already installed`。板级容忍该错、button 侧严格分支从未触发 ⇒ 无功能影响。

### 11.7 汇总

| 级别 | 项 |
|---|---|
| **P1** | `er`/`eb` 双倍高估；同一次换页重复 `EPD_TurnOnDisplay()`（9/12h，约每 30 min 一次） |
| P3 | 两个 GPIO ISR 安装者 flags 不一致（§10.2 / §11.6） |
| 待观察 | 首两批 1 行重叠（§11.3），68 批内未复现 |

**下一步（需要真机动作）**：11.2 的重复刷新定位必须靠 backtrace 或在
`EPD_TurnOnDisplay()` 入口加一行带 `reason` 的日志；本机无法插拔时无法取 UART0。
可先做的低风险动作：把 `read_busy()` 的告警阈值从 5 s 提高或改为"每次调用只在
首次阻塞时打一条"，使 busy-wait 条数不再与刷新次数混淆——**但这是观测改动，
不是修复**，需你确认是否值得。

---

## 12. 两整天长跑终检（2026-10-10 00:03 → 10-12 00:02，~48h）

设备状态：重启后连续运行 ~47.8 h（uptime 172106 s），`w=283`（= 48h×6/h），
**零复位**（全程 `rr=11`，无 `rr=4`）。服务端 543 次设备请求**全部 2xx**，
`NRestarts=0`。监测服务 `note4c-monitor` `active` 全程。

### 12.1 错误面

| 类 | 48h 数 | 判读 |
|---|---|---|
| 设备侧 `E` | **176** | **全部网络层**（TLS 握手失败 / TCP 连接失败 / HTTP 数据不完整），**零固件崩溃** |
| 服务端 5xx/非 2xx | **0** | 错误全部发生在公网代理链路上，未到达服务端 |
| `panic/Guru/abort/Backtrace` | **0** | — |
| `dropped` | 8 | 均在网络故障轮（上传失败残留），非环容量问题 |

**网络错误的形态**：集中在 10-10 午后到 10-11 凌晨（23 个分钟），每次爆发是同一页 bitmap
的 `GET` 反复失败（TLS `error=80` + 16132/30000 截断），同刻服务端却显示该 bitmap
`200 OK`——即**服务端正常、公网代理链路抖**。**全部自愈**：失败页在 5–40 min 后成功
下载并上屏，未留永久缺口。10-11 06:31 之后 21 h **零错误**。

### 12.2 P1（双刷）已归因确认

`[REFRESH]` 决策日志（恢复 `ESP_LOGI` 后）坐实：**每 30 min 的双刷 = 两个独立决策**——

| 波形 | urgent/force_full | reason | diff |
|---|---|---|---|
| 1 | 0/0 | `none` | 1.17% |
| 2 | 1/1 | `canvas` | 9.62% |

波形 1 是周期采样循环对**分钟级时钟像素**触发（`kMinDiffBitRatio=0.1%` 太敏感），
波形 2 是 `show page`。修复方向：提高 `kMinDiffBitRatio` 或对时钟区域豁免。
**尚未改代码**（48h 窗口里只恢复了日志，见 §10.3 / §11.2）。

近 24 h 该模式仍在：17 个非紧急 FULL + 17 个 6-wait 双刷批次。

### 12.3 夜间窗口仍失效（独立 P2）

00:00–06:00 `screen_active=false` 但轮询仍 600 s（非 `sleep_poll_s=3600`）。
`power.rs:52` 的分支未生效。需另查 `poll_cap`/`page_sync_poll_s()` 的来源。

### 12.4 结论

设备**健康**。176 个网络 `E` 是公网代理链路抖动 + 设备断连重试的**正常自愈**，
非固件缺陷、非服务端故障、未造成数据丢失。唯一的行为问题是 P1（双刷）和 P2（夜间窗口），
都是**已定位、待改代码**的优化项，不是稳定性 bug。

---

## 13. P1 / P2 修复（2026-10-12）

### 13.1 撤回：P1 不是"时钟阈值"，是**statusbar 与 canvas 的竞态**

§12.2 归因为"分钟级时钟像素触发周期采样"——**已撤回**。恢复 `[REFRESH]` 日志后的真实时序：

```
12989  RawDrawUiManager: statusbar update ... battery=100     ← UI 改 statusbar，标 dirty
15849  [REFRESH] Diff 1.17% → FULL reason=none urgent=0      ← 刷新任务醒来吃 dirty，全刷
17899  [REFRESH] request (urgent+full) reason=canvas         ← 2.35 s 后 canvas 请求才到
17899  PageSync: show page 3/3 md5="5eecbdc2"
35909  [REFRESH] Diff 9.62%  → FULL reason=canvas urgent=1  ← 第二次全刷
19789 ms（EB 账本 delta=2）
39578 ms
```

判据：31 个双刷批次**恒为 `urgent=0` 在先、`urgent=1` 在后**（0 例相反）；
`reason=none` 出现在 canvas 请求**之前**，说明第一次刷新时 `urgent_refresh`/`force_full_refresh_`
尚未置位。全文件 `statusbar update` 121 次、`Performing FULL` 149 次。

⇒ 一次唤醒内：UI 先刷 statusbar（走 `DrawTexts`/dirty rect），canvas 随后请求全刷，
两者**各自提交一次波形**。这是**竞态**，不是阈值问题——
所以"提高 `kMinDiffBitRatio`"与"时钟区域豁免"两个方案都**被证据否决**：
前者压不掉 1.17%（ urgent 链路的 diff 可达 9–14%，两者分布重叠），
后者要求时钟区域信息（本机时钟是**服务端渲染进位图的内容**，固件拿不到该区域）。

### 13.2 P2 修复：`screen_active` 在周期门控处从未被读

`application.cc:1234` 的 `poll_s = page_sync_poll_s()` 是**周期门控**的唯一来源，
它只用日间 `poll_s`；`sleep_poll_s`（3600 s）仅被 `power.rs:52` 的 `poll_cap` 消费，
而 `poll_cap` 只进入 `!sync_ok` / `None` 分支 —— 有已知轮换时走 `wait_cap`，两者都用不上。
⇒ `screen_active=false` 对**唤醒节奏**完全无效（实测整夜仍 600 s）。

修法（`application.cc:1233`）：按 `screen_active` 选 `poll_s` / `sleep_poll_s`。

### 13.3 P1 待定：需要跨层去重，**不在本轮改**

竞态的两侧在**不同模块**（`rawdraw_ui_manager` 的 statusbar vs `page_sync` 的 canvas），
去重要么让 statusbar 在 canvas 持有屏幕时跳过（需确认 canvas 期间 statusbar 是否必要），
要么在 `page_sync::blit_and_refresh` 前判断"statusbar 的 dirty 已被同一次刷新覆盖"。
两者都需要**产品判断**（canvas 期间要不要显示 statusbar？），按证据纪律不猜、等你定。

### 13.4 P1 定案：登记为"已知、不修复"（2026-10-12）

调查代价已超过收益，**不再追归因**。已确证与已否证的归因：

| 归因 | 状态 | 证据 |
|---|---|---|
| 分钟级时钟像素触发周期采样 | **否证** | 时钟是服务端渲染进位图的内容，固件拿不到区域；diff 1.17% ≈ statusbar 面积 |
| UI statusbar 与 canvas 竞态 | **否证为主** | 32 个非紧急 FULL 里仅 2 个前面有 statusbar update |
| `HandleInput` 的 `TriggerRefresh(false)` | **否证** | 48h 输入事件 **0** 次（唯一 4 条在 9-29）|
| canvas 双调 `rf_request_full_refresh` | **否证** | `page_sync.rs` 单 callsite（`paint_index`→`blit_and_refresh`）|

剩余机制：第 2 次 `urgent=1` 需有人再调 `RequestUrgentFullRefresh`；唯一候选 `application.cc:692`
有 `!page_sync_is_displaying()` 守卫，F1 时刻守卫必为 false 但**从 device log 无法断定触发条件**。
归因需 UART0（本机不可达）或再加一轮 `RequestUrgentFullRefresh` 入口 reason 日志（需再刷写）。

**量化收益**：26 次/48h × ~19.8s ≈ **8.6 min/48h**，占清醒时间 <0.5%。不划算。

**判据（将来若要重开）**：`[REFRESH] Performing FULL` 出现 `urgent=0` 且**其后 ~20s 内无新的
`request (urgent+full)`** —— 那才是"采样通道自己触发"。当前 26/108 全是 `REQ→F0→F1` 紧跟，
说明 urgent 标志被消费后又被重设。

**已交付**：`[REFRESH]` 决策日志保留（`EPD_LOG_DECISION`），可长期观察；若该模式消失，
说明归因在某次无关改动里顺带消失。

### 13.5 P2 夜间窗口：真机验证通过（2026-10-13 01:30）

刷写含 `application.cc:1233` 修复的构建后，跨午夜对照：

| 窗口 | 实测轮询 | 判定 |
|---|---|---|
| 白天 08:46–23:56 | 600 s，`w` 每轮 +1 | 与修复前一致（正确，白天不该变）|
| 夜间 00:06 → 01:30 | **84 min 零轮询**（上一 poll 00:06:53 w=92）| **3600 s 生效** |

设备确认在跑该 build（app sha `f5614211…` 与构建产物逐字节一致）。
根因：`application.cc:1233` 周期门控原只看 `page_sync_poll_s()`（600 s），`sleep_poll_s`
（3600 s）从未被周期路径消费；改为 `screen_active ? poll_s : sleep_poll_s` 后，
夜间窗口（00:00–06:00）每 3600 s 才轮询，省掉 5/6 的夜间无线电。
