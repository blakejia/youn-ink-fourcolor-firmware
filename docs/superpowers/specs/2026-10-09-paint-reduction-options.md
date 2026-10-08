# 少刷屏：现状取证与压榨方案 — 2026-10-09

配套 `docs/superpowers/progress/2026-10-09-power-performance-status.md`。那份测出"EPD 是主成本
（刷屏周期清醒 40.8 s vs 不刷屏 ~8 s，5:1）"，这份回答"怎么少刷"。

**所有数字来自** 服务端 journal（`GET /api/pages/schedule` 与 `/api/pages/bitmap/*.bin` 的
时间戳与计数）、设备自身 `rf_logbuf` 上报的日志（`PageSync:` 行）、UART0 抓包。
**未改任何代码**，本文件只是取证 + 方案。

---

## 1. 实测台账（2026-10-08 13:43–16:21，电池供电，无交互）

| 事实 | 数值 | 来源 |
|---|---|---|
| 唤醒次数 | **15** | journal 中 `/api/pages/schedule` GET |
| 其中伴随 `bitmap/*.bin` GET（=必然刷屏） | **9**（60%） | 同上 |
| 页配置 | 3 页 × `duration_minutes=30` | `server/data/pages/NOTE4C-3400FC/*.json` |
| 理论应有刷屏次数 | **1.5**（3 小时 ÷ 30 分钟 × 3 页轮换） | `pages.py::schedule_position` |
| **实际放大倍数** | **约 6×** | 9 ÷ 1.5 |
| 唤醒间隔 | 145–788 s，中位 **≈640 s** | 相邻唯一时间戳差 |
| 每次全刷清醒时长 | **40.8 s** | UART0：`show page` 15589ms → `Display refresh idle` 56469ms |
| 每次不刷屏清醒时长 | **≈8 s** | 历史日志 |
| 单次 EPD busy 记账 | **19 789 ms** | `eb` 步长（48638→68467） |
| 位图拉取耗时 | 6–11 s（含 TLS 握手） | `schedule` 200 → `bitmap` 200 间隔 |
| 位图大小 | 30 000 B（400×300 @ 2bpp） | `page_sync.rs::PAGE_BITMAP_SIZE` |

**结论：60% 的唤醒在重画一张和玻璃上一样的图。**

## 2. 根因（两条，互相独立）

### 2.1 位图缓存只活在 RAM，深睡即失效 ⇒ 每轮必下载

`page_sync.rs:605-613` 的注释写得很直白：

> Carry over bitmaps already in RAM for the same md5 (a re-sync **within one wake**) …
> Premise: the awake window (8-16 s) is far shorter than a page's dwell (>= 10 min)

**这是为"交互常亮会话"设计的**（一 wake 内多次 sync 不重复下载）。但占空比模式下每次唤醒
都是**新的一次开机**：RAM 被深睡清零，`free_pages()` 全释放。设备日志里 `schedule updated:
3 pages (**0 carried from cache**)` —— **实测每一轮都是 0**，跨唤醒缓存从未生效过。

> 注：仓库设计文档里提过"5×30KB SPIFFS 缓存"，但代码里**没有** SPIFFS 位图缓存实现
> （全仓 `SPIFFS` 无位图缓存命中）。这是设计意图与实现的差距，不是回归。

**代价**：即使最终判定"跳过重绘"，**30 KB 的位图已经下载并 TLS 解密完了**（6–11 s 射频 +
CPU）。`skipping repaint` 只省了 EPD 那 19.8 s，省不下下载。

### 2.2 唤醒节奏被 `seconds_until_next_page` 之外的因素拉短

`power.rs:79-85`：

```rust
let wake_s = if !i.sync_ok { …backoff… } else {
    match i.seconds_until_next_page {
        Some(s) => s.clamp(MIN_SLEEP_S, cap),   // cap = poll_s = 600 s
        None => cap,
    }
};
```

`cap = poll_cap(i) = min(poll_s, max_sleep_s)`，`poll_s = policy.poll_interval_minutes*60
= **600 s**`。于是**即使下一页在 30 分钟后，唤醒也被压到 ≤600 s**。30 分钟的页 ⇒ 每 10 分钟
必醒一次。这就是实测中位 640 s 的来源。

**这与"少刷屏"是同一件事的两面**：醒得勤 → 每轮都要重新下载位图 → 60% 的轮次在重画同一张图。

### 2.3 `screen_active` / 睡眠窗口是**死设计**

`config.py:81-85` 有 `canvas_sleep_start/end`（默认 00:00–06:00）与
`canvas_sleep_poll_interval_minutes`（默认 60 min）。设备侧 `policy.screen_active` 只喂给
`poll_cap` 的一个三元选择（`poll_s` vs `sleep_poll_s`）—— 但 `sleep_poll_s`=3600 s **仍被
`cap.min(max_sleep_s)` 压到 600 s**（`max_sleep_s` 默认 `max_sleep_min`=60 min… 实测 NVS 未设，
取默认 3600 s）。

**即"夜间 60 分钟一醒"这个设计，在 `max_sleep_s` 与 `poll_cap` 的双重 clamp 下从未生效**。
设备日志里 `policy: poll=…s sleep_poll=…s screen_active=…` 这行可以直接验证（本次未抓到，
但 `screen_active_now()` 在 06:00 前恒 false，而唤醒间隔在 00:00–06:00 期间（10-02 那批）
实测同样是 1670–1720 ms uptime_min ≈ 每 10 分钟一轮，**没有变慢**）。

## 3. 方案（按 收益/风险 排序）

### 方案 A：把"没到翻页时间"睡到点（**最大收益，最小改动**）

**做什么**：`cap` 不再被 `poll_s`（10 min）无条件压住；当 `seconds_until_next_page` 明确
大于 `poll_s` 时，允许睡到该值（受 `max_sleep_s` 约束）。

**为什么安全**：服务端已经用 `now % cycle_s` 算出"现在该显示哪页 + 还有多久换页"
（`pages.py::schedule_position`），设备只是照做。设备醒来早了也只是 `skipping repaint`。

**预期收益**：30 分钟页 ⇒ 唤醒从 **10 min → 30 min**，次数降到 1/3。
按台账：15 次唤醒 → 约 5 次；刷屏 9 次 → 3 次；EPD busy 从 9×19.8 s 降到 3×19.8 s。

**风险**：
- 服务端 schedule GET 从 10 min 一次变成 30 min 一次 ⇒ **通知/push 响应变慢**（上限 30 min）。
  缓解：把 cap 设成 `min(seconds_until_next_page, 15 min)` 而不是完全信任。
- `max_sleep_s` 上限默认 3600 s，改动需同时核对 NVS `max_sleep_min`。

**判据**：`a/r` 每轮增量应从 ≈185 s 降到 ≈（醒来+同步+刷屏）实际值；`er` 步长应按比例下降。

### 方案 B：跨唤醒位图缓存（省下载，不省刷屏）

**做什么**：把 `PAGE_BITMAP_SIZE`(30 000 B) 的位图落到 `RTC_DATA_ATTR` 或 SPIFFS，`sync_once`
时优先命中。

**收益**：命中时省 6–11 s 射频 + TLS CPU。但**注意**：RTC slow mem 只有 8 KB
（`rtc_slow_seg` 0x2000），30 KB 放不下 ⇒ **只能走 SPIFFS**（`assets` 分区 8 MB 空着）。
SPIFFS 写 30 KB 的时间与擦除寿命需实测。

**风险**：写 Flash 的时间与功耗可能**高于**省下的 6–11 s 射频；且深睡唤醒后读 SPIFFS 需要
mount，代价待测。**收益不确定，必须实测再决定** —— 这正是 AGENTS.md §4 要求的证据纪律。

**判据**：先测 SPIFFS 30 KB 写+读的耗时/功耗，与 6–11 s 下载对比；**不划算就放弃**。

### 方案 C：让"跳过重绘"发生在下载之前（**已实现，本方案撤回**）

> **更正（2026-10-09 复核）**：本节初稿把 C 列为"待做"。**复核发现它已在 HEAD 里**，
> 原描述作废，保留判据并记录新发现。

`application.cc:1357` 的 `page_sync_prepare_paint()` 在 `:1392` 的
`page_sync_paint_if_changed()` **之前**执行；`prepare_paint()`（`page_sync.rs:779-797`）
先读 RTC 面板记录，`COMPARE_SKIP_SAME` / `COMPARE_USE_CACHE` 时**根本不调
`ensure_bitmap`**。下载前的比对已经在正确的位置，无需再做。

**新发现（这才是那 60% 的真因）**：`prepare_decision()` 的 `glass_matches` 要求
**index 也匹配**——`record_index(rec) == idx as i32 && rec.md5() == &md5[..]`
（`page_sync.rs:750-761`）。而 `prepare_paint` 是 **index-scoped**，
`paint_if_changed` 才是 md5-scoped。服务端每轮下发 `current_index`，轮换推进时
index 与记录不一致 ⇒ 即使玻璃上是同一张图，也会走 `COMPARE_FETCH` 重新下载。

**真正可做的是 C'**：index 不匹配但 md5 匹配且记录可信时，是否也该跳过下载。
**这需要证据而非推理**——index 与 md5 在什么情况下会不一致而玻璃上仍是同一页？
设备日志里 `PageSync: show page N/M md5=...` 已带 index，可直接统计。见 §6。

**判据（不变）**：改完后 `schedule` 200 之后不应再出现 `bitmap/*.bin` GET（当记录命中时）。

### 方案 D：服务端页时长调长（**零固件改动**）

**做什么**：把 `duration_minutes` 从 30 调到 60/120。

**收益**：直接按比例削 `er`/`eb` 与唤醒次数（配合方案 A）。

**风险**：**这是内容决策不是技术优化** —— 页面内容多久该换一次是产品问题。
且当前 3 页 30 min 已经不长。

### 方案 E：夜间窗口真正生效（**修 bug 性质**）

**做什么**：让 `screen_active=false` 时的 `sleep_poll_s`（3600 s）不被 `max_sleep_s`/
`poll_cap` 压回 600 s。

**收益**：00:00–06:00 六小时按 60 min 一醒 = 6 次 vs 现在的 36 次。

**风险**：夜间不刷新 ⇒ 次日清晨屏上是昨夜的图。需产品确认。

## 4. 建议顺序

| 序 | 方案 | 性质 | 前置 |
|---|---|---|---|
| 1 | **A**（睡到点） | 行为改动，收益最大 | **已实现并刷入**（`109a01e`），待电池态验收 |
| 2 | **C'**（放宽 index 约束） | **已被数据否证** | — |
| 3 | **E**（夜间窗口） | 修死设计 | 产品确认夜间不刷新可接受 |
| 4 | **B**（SPIFFS 缓存） | **先测后做** | Flash 写读耗时/功耗实测 |
| 5 | D（页时长） | 内容决策 | 你拍板 |

**A 的量级估算**（按台账，3 小时窗口）：现状 15 唤醒 / 9 刷屏 / 9×19.8 = 178 s EPD busy；
仅 A 约 5 唤醒 / 3 刷屏 / 59 s（**-67%**）。

**C' 已被否证（2026-10-09）**：我原以为那 60% 的重复下载源于 `prepare_decision` 要求
index 也匹配。统计设备日志里全部 **53 条** `show page N/M md5=...`：**43 个不同 md5，
每一个都只对应唯一一个 index，零例外**。所以"index 不匹配但 md5 匹配"在当前配置下
**不可能发生**，C' 无从下手。这条假设作废——真因待电池态数据判定。

**明确不推荐**：`CONFIG_FREERTOS_USE_TICKLESS_IDLE`（light sleep 会拆
USB-Serial-JTAG 控制台 pad，`main.cc:88-96` 已记踩坑导致 0.3 s 启动死循环），
以及继续调 Wi-Fi 省电档（已是 MAX_MODEM）。

## 5. 未验 / 待你决策

- 方案 A 的"通知延迟上限"需要你给一个可接受的值（15 min？30 min？）。
- 方案 B 的 Flash 代价必须实测，我没有 Flash 写入的功耗数据。
- 方案 D 是内容决策，不该由我定。
- 本文件的 `screen_active` 死设计结论，是从**代码路径 + 10-02 那批日志的唤醒间隔**推出的；
  若要作为改动依据，应再抓一次 00:00–06:00 窗口的 `policy:` 日志行做正面确认。