# 复位后屏幕陈旧帧：证据记录 — 2026-09-29

## 现象（用户报告）

设备复位后屏幕停在「对话」页（状态栏 Wi-Fi 图标显示断开），随后又刷回「重点任务」。
17:51 一次，17:56 一次。用户观察到「现在看到的仍未恢复」，且手机可正常连上同一 AP。

## 观察窗口与证据来源

- `journalctl --user -u youn-ink-server`（覆盖起点 2026-09-13T21:56，见 `youn-ink-server-log-forensics`）
- `server/data/devicelogs/NOTE4C-3400FC.log`（设备上报通道）
- 用户提供的串口日志（17:56:21–17:59:34）
- 源码静态核对（本文件所有"依据"均为实读，非推断）

## 1. 复位确实发生（非深睡唤醒、非刷写）

schedule 查询串计数器（`w`=wakes, `a`=awake_ms, `r`=radio_ms, `g`=http_gets, `rr`=cached reset reason）：

```text
17:40:42  w=7&a=304169&r=304169&g=11&rr=4&lo=2     ← 正常轮询
17:51:30  w=1&a=0&r=0&g=0&rr=4&v=4096&p=96&c=4     ← 计数器全归零 = 真复位
17:56:22  w=2&a=185483&r=185483&g=2&rr=4&lo=2      ← 复位后第 2 轮
```

判据：`w` 回落到 1 且 `a`/`r`/`g` 同时归零 = RAM 已被清空 = 真复位（深睡唤醒不会这样，`a`/`r` 会在睡眠 teardown 时记账后保留）。

`rr=4` = `ESP_RST_PANIC`（异常/panic 导致的复位）。枚举权威定义见 ESP-IDF v6.0 `components/esp_system/include/esp_system.h`。

**注意 `rr` 的陈旧语义**：`shim.cpp:455` 的 `g_last_reset_reason` 是 `RTC_DATA_ATTR` 且仅在为 0 时写入 —— 同一上电周期内后续复位的 `rr` 都报第一次那个值。因此 **`rr` 不能用来计数崩溃**，判次数必须配合"计数器归零"。

## 2. 网络正常（三条独立证据）

1. 用户手机可连上同一 AP（排除 AP 侧故障）；
2. 设备自己探测成功：`17:56:34.318 dns ok` → `17:56:34.334 tcp ok`（目标 `note-device.1024.center:31443`）；
3. 服务端侧：`17:56:22` schedule `200 OK`、`17:56:25` `POST /api/device-log 201 Created`。

所以屏上「断开」图标**不是真实链路状态**。

## 3. 屏幕停在陈旧帧的机制

串口日志关键两行：

```text
17:56:25.448  PageSync: schedule updated: 3 pages (0 carried from cache)
17:56:25.448  PageSync: glass already shows "55290bc0", skipping repaint
```

`55290bc0` = 「重点任务」。即画板判定"玻璃已是目标页" → 跳过重绘。

而玻璃上实际是 **UI 整屏外壳**（`title='对话'` + `wifi=0`，`17:56:34.011` 那一帧；`0.16 s` 后 `17:56:34.172` 已修正为 `wifi=1`，但那帧未上屏）。

因果链，每环均有代码依据：

| # | 环 | 依据 |
|---|---|---|
| 1 | `g_panel_rec` 为 `RTC_DATA_ATTR`，跨 panic/USB 复位**不清** | `shim.cpp:88`；`shim_power.h` 头注释说明这是为深睡省电有意选的 |
| 2 | 复位前 record 记着「某页在玻璃上」且 `valid=1` | `shim.cpp` commit 钩子 |
| 3 | 重启后 `record_trusted()` 只查 `magic && valid`，record 仍可信 | `page_sync.rs:708-710` |
| 4 | 目标页与 record 相同 → md5 相等 → `COMPARE_SKIP_SAME` | `page_sync.rs:799-803` |
| 5 | 玻璃上是 UI 内容：`ui_boot_paint_deferred_` 只跳过**面板刷新**，UI 内容照样渲染进 framebuffer。**但读代码无法判定这一帧如何在画板持屏时上玻璃**——`RenderAll` 在 `page_sync_is_displaying()` 时早退（`rawdraw_ui_manager.cc:734`），且 `Init` 自身那次刷新被 deferred 拦掉。 | `application.cc:663-666` |
| 6 | `rawdraw_ui_manager.cc` 有 **7 处** `Clear + RenderAll` 全屏重画，**全都不更新 record**。不过其中 5 处受 `page_sync_is_displaying()` 早退保护，画板持屏时画不进玻璃 | `:356/431/514/533/706/1437/1477` |

**结论**：record 说「画板某页在屏上」，玻璃上却是 UI 内容 → 分叉。

## 4. `invalidate` 的覆盖缺口

`rf_panel_record_invalidate()` 仅三处调用，**无一处覆盖冷启动/panic 路径**：

- `application.cc:863` — promotion（quiet boot 转交互）
- `application.cc:1508` — 策略睡眠（`d.invalidate_panel`）
- `application.cc:1602` — 手动睡眠

## 5. 17:56 之后的恢复过程（反证 `rr` 与重绘判据）

用户按 BOOT 双击（强制同步）后：

```text
18:03:06.487  Application: BOOT double click / force sync requested
18:03:09.842  PageSync: schedule updated: 3 pages
18:03:15.919  PageSync: show page 3/3 md5="bd52ed10"      ← 走了重绘，未跳过
18:03:47.928  RawDrawUiManager: Display refresh idle; input unlocked
```

`bd52ed10` ≠ 玻璃现状（`55290bc0`）→ 正常重绘 → 屏幕恢复。**这证明"跳过"只在目标页与玻璃现状相同时发生**，也证明强制同步链路工作正常。

## 6. 与既有文档的关系

这印证了 `note4c-screen-ownership` 记的陷阱第 4 条：

> `DISPLAYING == false` 的四种真实含义：本 boot 还没画 / 被 `stop_display()` 挂起 / 同步失败什么都没画 / **玻璃已是该页而跳过**

本次是第 4 种，且叠加了「record 不知道玻璃上是 UI 画的」这一层。

## 7. 本记录**未**证明的事

- **未**解释 `rr=4` 的成因。`rr` 只说明"发生过 panic"，永远给不出"在哪崩"；需串口 backtrace（`Guru Meditation Error` 后的调用栈）。
- **未**证明此分叉会导致除"显示陈旧"以外的后果（例如是否会影响后续唤醒的省电判定）。设计文档 §3.6 的路径清单是为此核对用。

## 8. 更正（2026-09-29 复审时）与仍未定的一项

**更正一**：本文件第 1 节把玻璃上出现 UI 内容写成「因为 render 落进了 framebuffer」，
暗示它随后被刷上屏。核实后这个机制**不成立**：`RenderAll` 在
`page_sync_is_displaying()` 时直接 return（`rawdraw_ui_manager.cc:734-737`），画板持屏时
UI 根本画不进玻璃；且配对启动时 `Init` 的那次 `TriggerRefresh` 被
`ui_boot_paint_deferred_`（`application.cc:519/663-666`）整体早退拦下。

**更正二**：`invalidate` 的缺口不止「冷启动没有」。真正的窗口是
`BuildRawDrawUi`（`application.cc:521`）到 `page_sync_start()`（`:413`）之间——
后者由 WiFi 连上后的配对任务触发（`:559` → `:413`，`s_pairing_started` 一次性去重，
`page_sync_start` 全仓仅此一个调用点），所以窗口是**秒级**而非微秒级。
窗口内 `DISPLAYING=false`，UI 帧可通过 flusher
（`NoteButtonActivity` / promotion 的 `RequestActivePageRefresh`）上玻璃，而它们不碰记录。

**仍未定 / 待真机判定**：上面那个「UI 帧在画板持屏时上玻璃」的实际机制**尚未被正向证明**。
可判定它的证据全在下一次串口日志里：

```text
RawDraw UI Manager initialized      <- UI 已构建（此时 DISPLAYING 仍为 false）
PageSync: started                   <- page_sync_start() 真正跑的时刻
glass already shows ..., skipping repaint   <- 出现在哪一个之后？
```

若 `skipping repaint` 出现在 `PageSync: started` 之后且期间玻璃上是 UI 帧，则缝隙可达，
`application.cc` 刷新回调里那处条件 invalidate 会被触发（修复生效）；若不出现，该守卫
保持为惰性防御，无副作用。**在此之前不要声称本缺陷已完全关闭。**

## 9. 7.5 小时串口日志的核查（2026-09-30，`serial-1790728810247.log`，2393 行）

**覆盖范围与结论前提**：17:56:21 → 次日 08:39:15（14.7 h）；uptime 从 750 ms 单调升到
52 980 160 ms，**零倒退 = 期间一次复位都没有**；**零** `Guru Meditation` / backtrace /
abort / assert。故这份日志**不能**用于 `rr=4` 归因（整段没有崩溃），但它完整覆盖了
17:56 那次启动——而那次正是产生陈旧帧的那次。

**级别分布**：I 2221 / W 146 / **E 24**。24 条 E 全是同两个时刻的网络失败
（`05:51:09` 与 `06:11:31`，`esp-tls select() timeout` → `HttpWrap: HTTP GET … failed:
ESP_ERR_HTTP_CONNECT`），分别打掉一次 `schedule` 和一次 `notifications/next.bin`。
失败请求串里 `w=73→75`、`er=31→32`、`rr=4`，两个时刻相距 20 min 且 `a`/`r` 未变——
符合重试回溯阶梯；设备随后自行恢复，**无持久故障**。

**关键量化（启动那次刷新）**：

| 事件 | 次数 | 其后 90 s 内出现刷新事务 |
|---|---|---|
| `skipping repaint` | 58 | **仅 1 次**（`up=10040`，即启动那次） |
| `show page` | 30 | 30（100%） |

这同时说明两件事：①画板日常的 skip 都是真的跳过（省电正常，**深睡红线未被违反**）；
②唯一一次「skip 之后还有刷新」就是启动那次，其 `EPD busy wait` 20 s 序列从 skip 后
1.2 s 开始，而 WiFi 要到 `up=18750` 才连上——**刷新发生在 `page_sync_start()` 之前**。

**这份日志**不能**回答的那一问**：17:56 那次刷新是否经由 UI 刷新回调（`RequestUrgent*`）
请求。判据本该是 `CustomLcdDisplay` 的 `[REFRESH] request` 行——它是 `ESP_LOGI`，而全局级别为
INFO（`sdkconfig: CONFIG_LOG_DEFAULT_LEVEL_INFO=y`），本该必留记录。

**但这条判据在复核中被否证，必须撤回**：本文件**逐串复算**过 `[REFRESH]`、
`request (urgent`、`Performing FULL`、`Performing PART`、`display_urgent`、`canvas`
**六个串，全部 0 命中**。若 `[REFRESH]` 真的被级别压掉，那 30 次 `show page` 也不可能被记录
（它们必然经由 `blit_and_refresh` → `rf_request_full_refresh` → `RequestUrgentFullRefresh("canvas")`
→ `[REFRESH] request`）。两者不可能同时成立。因此**零命中不能用「级别」解释**，
真实原因未定（候选：该档固件的 `custom_lcd_display.cc` 与当前树不同，或抓包工具做了过滤）。
`RING_LINE_CAP` 亦不可能是原因（导出 2393 行 < 上限）。

**结论**：关于「那次刷新走哪条路径」，本记录**不主张任何结论**。相应地，缺陷是否真的经由
启动外壳发生，仍是**待证**的——判据应改为不依赖 `[REFRESH]` 行的直接观测（例如
刷写新固件后，看 `ui_boot_paint_deferred_` 为真期间是否真的出现一次面板刷新）。

**由此撤回的改动**：曾据此把守卫改挂到「外壳像素进入 framebuffer」那一刻（`Init()` 返回之后），
该位置经复审否决——三个合取项在每次配对交互启动都平凡成立，会破坏画板赢得竞速时的
`skipping repaint` 优化（本文件表中的 `up=10040` 就是那次优化）。守卫已改回刷新回调，
见设计 §3.8。
