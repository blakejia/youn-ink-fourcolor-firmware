# 省电与性能：真机状态盘点与压榨空间 — 2026-10-09

设备在本机（USB 直连）可用后的第一次省电侧盘点。所有数字来自设备自身经 `rf_logbuf`
上报的日志（`server/data/devicelogs/NOTE4C-3400FC.log`）、服务端 `schedule` 查询串里的
RTC 计数器（`w/a/r/g/f/er/eb/rr/c`），以及 UART0 抓的三次冷启动
（`/tmp/warm-capture.log`、`/tmp/clean-capture.log`、`/tmp/boot-capture.log`，均在 `/tmp`）。

> **观测前提（决定下面大半结论）**：设备**插着 USB**。`power.rs::decide()` 在 `mains` 上短路
> 返回 `StayAwake`（`firmware/main/rust/src/power.rs:54`），所以插电期间设备从不进深睡，
> 走不到深睡 teardown —— 而那正是 `awake_ms` / `radio_ms` 的记账点。**省电验收必须拔掉
> USB**，否则 `a=0&r=0` 不是回归。

---

## 1. 一条被误判的"回归"（先撤回）

服务端查询串在 10/8 16:53 之后一直是 `a=0&r=0`，看着像时长账本不再累积。**不成立**：

- 逐日统计 `a>0` 的批次数：09-25 = 123、09-29 = 113、09-30 = 141、10-02 = 143、10-08 = 62，
  **每一天都有非零批**（10/8 的 62 个非零批集中在 09:00–16:53，即插电之前）。
- 充电态与 `a>0` 的对应（10/8 抽样）：`a>0` 只出现在 `c=4`（电池），
  `c=2/c=3`（插电充电）时为 0。

**判据**：`a/r` 只在**电池供电且真的睡下去**的周期里累积。插电 + 反复 USB 复位刷写期间恒为 0
是设计使然。

## 2. 省电那批 SDD 的真机项：**一项都没做**（这是最大的空账）

`.superpowers/sdd/2026-09-17-firmware-power-optimization/final-report.md` 的收尾清单，
7 个任务的代码全部在 HEAD（`d22be9c..3bd491a` 已核实 merge-base），但**真机一项都没跑**：

| # | 待验项 | 判据 | 当前能否主机完成 |
|---|---|---|---|
| 1 | T2 成对启停 + 三合一守卫 | 静默唤醒刷屏后 Wi-Fi 重连日志 + 下周期同步正常；交互冷开机全程不断射频 | **可**（拔 USB 后等一轮唤醒） |
| 2 | T3 收益/回吐 | ≥1h 轮询，`radio_ms` 下降且 `awake_ms` 不恶化 | **可**（同上，计数器现成） |
| 3 | T5 长跑 | ≥2h：音频无爆音、EPD 无花屏、页面照常、`awake_ms` 不恶化 | **可**（2h 观察窗） |
| 4 | T6 LED 五态 | 充电中/充满/放电空闲/活动脉冲/override 闪烁，以**代码推导表**为准 | 需肉眼（LED） |
| 5 | T7 音频 | 通知提示音/语音正常，无爆音、无 codec 掉电 | 需肉眼（听） |
| 6 | F1/F2 通知在飞 | 慢 `/next` + 页面无变化的一轮 quiet wake ⇒ 不睡、射频全程在 | **可**（服务端造慢响应） |

红线条目（final-report §现状）：①USB-JTAG 控制台在 DFS 下可用；②MAX_MODEM 收益/回吐；
③通知 ack 在新时序下仍可达；④画质。前三项可主机验证，④需肉眼。

**其中 T3（收益/回吐）是唯一有客观数值判据的一条，也是"省电到底有没有省到"的核心。**

## 3. 本次新查到的一个量化事实：快连缓存在深睡下工作正常

顺手核了 `wifi_station.cc` 的快连缓存（Stage 3，commit `e260846`），得到一个决定性对照：

| 唤醒类型 | `stage=wifi event=sta_connected t_ms` | 快连 |
|---|---|---|
| **真深睡唤醒**（设备日志，220 批） | **509 ms** | `event=fast_success` |
| **USB 复位唤醒**（UART0 抓包 ×3） | **4883 / 4911 / 4902 ms** | `reason=fail_threshold fail_count=0` |

⇒ 深睡唤醒走快连（0.5 s），USB 复位走全扫描（4.9 s），**每次唤醒差约 4.4 秒射频**。
这 4.4 s 不是回归，是复位类型的差异（`g_rtc_wifi_cache` 是 `RTC_DATA_ATTR`，USB/CPU
复位不清 RTC slow memory，但 RAM 侧的 `g_fast_cache` 已被清且 seed 早于首次 STA start）。

**顺带纠正一处日志误导**：`fast_direct_connect_skip reason=fail_threshold fail_count=0`
—— 实际条件是 `wifi_direct_allowed`/`wifi_scan_forced` 为真才落到 `else if (kFastRcEnable)`
分支，该标签把"策略要求扫描"说成了"失败次数超阈值"。日志文本误导，但不影响行为。

**`wrote=0` 的真义**（设备日志 222 次）：`SaveFastConnectCache` 返回 `changed`，yi02 的
`(ssid,bssid,channel)` 在 RAM 缓存里没变 ⇒ false；但 `SaveWifiCacheToRtc()` 在 `return`
之前已经执行 ⇒ RTC 镜像**照样写了**。`wrote` 是"RAM 是否变"，不是"是否持久化"。

## 4. 已确认生效的省电机制（HEAD，带证据）

| 机制 | 证据 |
|---|---|
| 深睡占空比 | 静默唤醒间隔 598–610 s（= 服务端页时长/cap 600），`a≈183–191 s`/`r≈同值`（上界，`awake≥radio` 构造使然） |
| 页面未变则跳过重绘 | `PageSync: glass already shows "<md5>", skipping repaint`（本次观测 97 次） |
| 面板记录跨深睡 | `er`/`eb` 逐轮增长（3/39578 → 4/79156），刷新记账不随 RAM 清零 |
| DFS | `esp_pm_configure(min=80, max=240, light_sleep=false)`；`pm: Light sleep: DISABLED` 日志确认降级生效 |
| MAX_MODEM 省电档 | 设备日志 31 行全部 `Setting WiFi power save level: LOW_POWER (MAX_MODEM)` |

**每次唤醒的清醒成本**：EPD 全刷一次 `eb≈19789 ms`（48638→59367→79156 的步长 19789 ms），
串口实测 `show page` 到 `Display refresh idle` 约 **40.8 s**（15589→56469 ms，其中两段
5/10/15 s 的 `EPD busy wait`）。不刷屏的周期清醒约 **8 s**。

⇒ **EPD 刷新是绝对主成本**（40.8 s vs 8 s，5:1）。任何省电压榨的第一优先级都是"少刷屏"，
而不是调 Wi-Fi 档位或 tickless。

## 5. 压榨空间（按证据排序，均未实施）

1. **`dropped` 未清零 → 每轮约 100 B 的 `Application` INFO 挤掉 EPD/电池等关键行。**
   已在 `2026-10-08-log-volume-acceptance.md` §5 量化（冷启动批 1810 B vs 2048 B 环）。
   这不是省电，是**可观测性**：省电验收要靠日志判据，日志被冲掉就等于盲测。
2. **快连缓存收益已被深睡路径吃掉**（0.5 s），USB 复位路径的 4.4 s 差距只在刷写场景，
   不值得为它改代码。
3. **EPD 刷新节流**：当前页时长（服务端 `duration`）驱动换页。若把静态页的时长调长，
   直接按比例削掉 `eb` 与 `er`。**但这是内容决策，不是固件优化**，且改动在服务端。
4. **`CONFIG_FREERTOS_USE_TICKLESS_IDLE` 未开**（`sdkconfig:3230`）⇒ light sleep 不可用
   （`main.cc:96` 已注明踩过 `ESP_ERR_NOT_SUPPORTED` 变 0.3 s 启动死循环的坑）。
   DFS-only 下 CPU 空闲仍跑 80 MHz。开 tickless 需要先解决 USB-Serial-JTAG 控制台
   被 light sleep 拆 pad 的问题（`main.cc:88-96` 的红线）——**风险高，收益仅限清醒窗口
   的 CPU 功耗**，而清醒窗口里 EPD 波形占 40 s/40.8 s，CPU 占空极低。**不推荐。**
5. **音频轨在刷屏前断电**（`rf_rails_audio(0)`，T2 引入）——SDD 自己登记为 deferred：
   "可能在继续活着的会话里关掉音频，须真机确认音频路径能否自恢复"。**这是已知未验风险。**

## 6. 建议的验收顺序（全部拔 USB，主机可完成）

1. **T3 计数器对照**（≥1h，最有信息量）：记录 `a/r/er/eb` 增量，确认 `r` 下降且 `a` 不恶化。
2. **T1/T2**（同窗口内顺带）：静默唤醒刷屏后 Wi-Fi 重连 + 下周期同步正常；交互冷开机不断射频。
3. **T5 长跑**（≥2h，可与 1 并行）：音频/EPD/页面/`awake_ms`。
4. **F1/F2**：服务端造慢 `/next`，验证"通知在飞不睡、射频不断"。
5. **T6/T7** 需肉眼，可让你在设备旁一次做完（LED 五态 + 音频）。

## 7. 未验（本记录登记）

- 上述 6 项全部。
- `dropped` 清零方案（需先定 `Application` INFO 的削减范围）。
- `rr=4` panic 归因：`2026-09-24-rr4-diagnosis.md` 已判定"无正向归因、Task 6 阻塞"，
  本轮未推进（且 `rr` 是 RTC 缓存的一次性快照，不能计数，见
  `docs/superpowers/progress/2026-09-24-rr4-diagnosis.md` 与本文件 §1 同源纪律）。