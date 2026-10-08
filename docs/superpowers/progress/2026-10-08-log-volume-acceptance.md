# 日志量削减 + 上报闸门：真机验收记录 — 2026-10-08

设备首次在本机（USB 直连）可用，对 `e81025b`（环形排空）与 `65e675a`（`wifi` INFO 降级）
两个提交，以及设备日志上报的两侧开关做验收。所有结论来自**设备自身经 `rf_logbuf` 上报的
日志**（`server/data/devicelogs/NOTE4C-3400FC.log`）与 **UART0 串口**（GPIO43/44，115200），
不用 WebSerial 抓包（会丢 `CustomLcdDisplay`/`Application` 的 INFO 行，见
`2026-09-29-screen-stale-after-panic.md` §10/§11）。

## 1. 刷写前的设备身份（决定性判据）

`app_desc` 的 `time`/`date` 字段在本项目**陈旧**（10/8 16:36 构建的 bin 仍写
`08:57:44 Sep 29 2026`，`.o` 复用旧对象），**不能**用来判版本。唯一可信字段是
`app_elf_sha256`：

| | `app_elf_sha256` |
|---|---|
| 刷写前设备（读回 `ota_0`） | `05defdd7…3ba42` |
| 本地 HEAD `build/xiaozhi.elf` | `5ebe8fa3…50c1ea` |
| 刷写后设备（再读回） | `5ebe8fa3…50c1ea` ✅ |

设备上的分区表与 `partitions/v2/16m.csv` 一致：`ota_0@0x20000 4032K`、`ota_1@0x410000
4032K`、`otadata@0xd000 8K`、`assets@0x800000 8M`。`ota_1` 为空白（`magic=0xff`），
`otadata` 两条记录 `ota_seq`/`state` 均为擦除值（`0xFFFFFFFF`）⇒ bootloader 回落 `ota_0`。

刷写前两个提交的设备状态：
- `e81025b`（环形排空）**已经在设备上**：批次载荷在 **10/8 14:34** 从 ~1000 B（旧 1024 B 读上限）
  跃到 ~1790 B（≈2048 B 环减去每帧 6 B 头的载荷）。
- `65e675a`（`wifi` INFO→WARN）**不在设备上**：16:53 的自身日志仍带 `I (…) wifi:` 行。

## 2. 刷写

- 参数与仓库既有前端 `flashTarget.js` 一致：活动槽 `ota_0` → `write-flash 0x20000 build/xiaozhi.bin`，
  `esp32s3 / dio / 80m / 16MB`，**不写 otadata**。esptool v5.3.1。
- `Wrote 2924336 bytes (1746982 compressed) … Hash of data verified.`
- 刷前另存 `backup/otadata-20261008-preflash.bin`；旧镜像可用同样方式原地重刷恢复。
- 刷后设备以 `rr=11`（`ESP_RST_USB`）重启并重新上线（`10.0.0.171`）。

## 3. `65e675a`（wifi INFO 降级）——已验证

按 tag 归因同一个「冷启动连 AP」类型批次（字节数，含续行）：

| tag | 旧固件（16:33:07） | 新固件（22:49:32 起） |
|---|---|---|
| `wifi` | **538 B** | **0 B** |
| `Application` | 339 B | 1272 B（占比上升，绝对值未变） |
| `FAST_RC` | 414 B | — |
| 其余 | `WifiStation`119 / `esp_netif_handlers`89 / `Pairing`75 | 大致不变 |

新固件启动批次（22:49:32）里 **0** 条 `I (…) wifi:`；仅剩 1 条 `W (…) wifi:` 告警
（`Password length matches WPA2 standards…`）——正是设计要保留的级别。后续两批均为
**0 条 wifi INFO、0 条 wifi WARN**。

> 注：该 AP（`yi02` 同款 TIM-less 行为）在旧固件里每轮重印的 `<ba-add>` /
> `AP's beacon interval` 属 INFO，已随本提交消失。

## 4. `e81025b`（环形排空）——已在设备上（14:34 起的产物）

`dropped` 与载荷的联合证据：

| 阶段 | 冷启动批次 `dropped`（中位） | 静默唤醒载荷 |
|---|---|---|
| 14:34 之前（读上限 1024 B） | 旧构建 | ~1000 B，`dropped` 6–10 |
| 14:34 → 22:31（排空修复） | 见下 | ~1790 B |
| 22:49 起（HEAD，两个修复） | 55（23:19:51 批） | 1295 B，**无 `dropped`**（23:29:57 批） |

- 载荷从 ~1000 B 跃到 ~1790 B 是 `e81025b` 的直接证据（读上限 1024 → 2048）。
- **23:29:57 是整份日志里首个零 `dropped` 的批次**（24 行 / 1295 B，uptime 跨度 603 s =
  一个静默唤醒周期）。

## 5. 剩余日志量的归因（`dropped` 尚未清零的原因）

`65e675a` 的提交信息已自述"不是完整的修法"。真实数字：冷启动批次（23:19:51）载荷 ~1810 B
对 2048 B 环，`dropped=55`。按 tag：

| tag | 字节 |
|---|---|
| `Application` | 1272 |
| `esp-x509-crt-bundle` | 165 |
| `CustomLcdDisplay` | 161 |
| `PageSync` | 122 |
| `RawDrawUiManager` | 67 |

即冷启动已接近装得下，缺口约 100 B；要清零 `dropped` 需再削 `Application` 的启动期 INFO
和/或扩 RTC ring（另有 `assets` 分区与 RTC 慢存余量可查）。**本记录不提出该改动的方案**
（无证据支撑前不动行为）。

## 6. 日志上报闸门：服务端覆盖（已验证）

`resolve_enabled`：服务端 ON→true，服务端 OFF→false，否则跟本地。11.5 小时日志显示设备
每轮唤醒先 GET `schedule` 再决定 POST（若设备在上报，两者成对出现且相隔 ~3 s）。

**A/B（只翻服务端意见，设备本地恒 `lo=2` 开）**：

| 时刻（服务端意见） | `GET /api/pages/schedule` | `POST /api/device-log` |
|---|---|---|
| 22:49:32（跟随） | 200 | **201** |
| 22:59:38（**22:51:39 设为强制关**） | 200（`lo=2`） | **无** |
| 23:09:42（强制关） | 200（`lo=2`） | **无** |
| 23:19:48（**23:17:15 恢复跟随**） | 200（`lo=2`） | **201** |
| 23:29:54（跟随） | 200（`lo=2`） | **201** |

⇒ 「服务端关闭 + 本地开 → 不上报（服务端赢）」**成立**；反向恢复后立即可靠恢复上报。

## 7. 电池与计数器（顺带核对）

- `schedule` GET 带 `?…&v=<mV>&p=<%>&c=<charge>`（设备日志同值，如
  `v=4173 p=100 dir=2`），符合 `2026-09-22-battery` 真机清单第 1 项。
- 计数器确认：`g=5→6`、`w=4→5`、`er=3`、`eb=59367` 逐轮增长。

## 8. 启动窗口复核（交叉引用）

UART0 抓到完整冷启动（187 行，从 `ESP-ROM` 到 `Display refresh idle`），用于独立复核
`2026-09-29-screen-stale-after-panic.md` §10/§11：`[REFRESH]` 等 0 命中而 `show page` 出现，
坐实 `custom_lcd_display.cc` 的 `#undef ESP_LOGI`；窗口 519→6149 ms ≈ 5.6 s。详见 §11。

## 9. 未验（需物理按键 / 更长观察）

- **设备本地开关三态**（设置页 BOOT 单击循环 0/1/2 + 设置页/状态栏显示）——已用一次性探针
  在真机上验过机制，见 §9b；肉眼观感（设置页/状态栏实际显示）仍需人工。
- **崩溃前日志存活**：需制造一次非 USB 复位（USB 复位本身会复位 USB 外设）。
- **`dropped` 清零**：属 §5 的后续改动，未做。
- **服务端"跟随设备" + 本地各三态的完整六行矩阵**：本次只验了服务端覆盖关/跟随两行；
  本地三态需物理按键。

## 9b. 按键探针：设备本地三态 + 两段确认（2026-10-09 00:00 前后）

本地开关三态与两段确认需要物理按键，本机没有注入手段，所以用一次性探针把事件喂给
**物理按键最终到达的同一函数** `SettingsRenderer::HandleInput`（`rawdraw_ui_manager.cc:616`
在真实路径上也是先把它交给活动 renderer）。探针在 `Run()` 里跑一次，守卫是
`esp_reset_reason() == ESP_RST_USB`（只在串口刷写后的那次启动生效）。

**两个刻意的设计取舍**（都为了避免制造假象）：

- **不走 UI 管理器的重绘/刷新路径**：只调 renderer 的 `HandleInput` + `Render`，且 `Render`
  画进一块 400×300 的 scratch 字节数组，**不碰真实 framebuffer**。管理器的路径会在
  `rawdraw::Clear(fb)` 之后 `TriggerRefresh`，而设置页让画板挂起后 `DISPLAYING=false`
  （不早退）⇒ 会把**清空后的白 framebuffer** 刷上玻璃（AGENTS.md §7 明列的路径）。
- **延后到启动爆发之后**（15 s）再跑：启动爆发 9–20 KB 对 2048 B 环，早跑会被开机日志冲掉。

**实测（UART0，`/tmp/probe-capture.log`）**：

```
W (15689) PROBE: begin local_lo=2 section=0
W (15839) PROBE: BOOT enter options (=日志上报)
I (15999) Application: 日志上报: local opinion 2 -> 1
W (16149) PROBE: local_lo 2 -> 1
I (16159) Application: 日志上报: local opinion 1 -> 0
W (16309) PROBE: local_lo 1 -> 0
I (16319) Application: 日志上报: local opinion 0 -> 2
W (16469) PROBE: local_lo 0 -> 2
W (17219) PROBE: BOOT arm 重置设备 (must NOT confirm)
W (17219) Application: Settings: 11 armed; press again within 5 s to confirm
```

- **本地三态 ✅**：`2→1→0→2`，与 `settings.rs::log_upload_toggle_target` 的单测一致
  （ON→OFF→NONE→ON）；每步 app 的 handler 都落了一行，且 NVS 读回与写入一致。收尾停在
  ON(2)，上传不中断。
- **两段确认 ✅（负向）**：单次按下 `11`（重置设备）只 `armed`，**没有** `confirmed`、没有
  重启、没有擦除 —— 写路径根本不执行。第二次按下的**正向前提**由 Rust 单测钉死
  （`fresh = armed == Some(pressed) && now - armed_at <= 5000`），本探针刻意不制造。

**撤探针与复核**：`git checkout` 还原 `application.cc`/`application.h`，重建（`0x2c9f30`，
与 `65e675a` 提交信息同尺寸）刷回。干净启动抓包：**0** 条 `PROBE`、**0** 条 Settings
`armed`/`confirmed`、**0** 条三态切换；设备 `app_elf_sha256` = `5ebe8fa3…50c1ea`（与本地
HEAD 产物一致）；`git status firmware/` 为空。

**仍未验**：本地三态与服务端三态的完整六行矩阵（§6 已验两行 + 本节验了本地三态的机制）。
崩溃前日志存活（需非 USB 复位）亦未验。

## 10. 门禁

- `cargo test`（Rust host）：**426 passed / 0 failed**。
- `server pytest tests/`：**337 passed**。
- 两者均对应已刷入设备的同一棵树（§1 的 `elf_sha256`）。
