# 面板记录来源（panel record source）设计

**日期**：2026-09-29（初稿）／2026-09-29 修订（机制更正 + 钩子收敛）
**状态**：待用户复审
**前置证据**：`docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md`

> **修订说明**：初稿把「UI 内容照样渲染进 framebuffer」当成常态，据此列了 7 处
> `Clear + RenderAll` 需要写来源。复核 `RenderAll` 后发现相反：**画板持屏时
> `RenderAll` 直接早退，UI 根本画不上去**——UI 能上屏的前提是画板已交权。据此本稿
> 改用**单一交权钩子**（3 处）而非 7 处绘制点。两处错误及更正见 §1.1、§3.6。

---

## 1. 问题

设备出现「屏幕停在陈旧帧、且系统认为无需重绘」。2026-09-29 17:51 与 17:56 两次
`rr=4`（`ESP_RST_PANIC`）复位后，屏幕停在 `title='对话'` + `wifi=0` 的一帧上，
而链路实际正常（`dns ok` / `tcp ok` / `schedule 200` / `device-log 201`）。

### 1.1 因果链（每一环均有代码依据）

1. `g_panel_rec` 是 `RTC_DATA_ATTR`（`shim.cpp:88`），**跨 panic/USB 复位不清**——
   为深睡省电有意选的（`shim_power.h` 头注释：没有它「每次唤醒都要付一次 ≥15 s 全刷」）。
2. 复位前 record 记着「某页已在玻璃上」且 `valid=1`。
3. 重启后 `record_trusted()` 只查 `magic && valid`（`page_sync.rs:708-710`），record 仍可信。
4. 本次开机目标页与 record 相同 → md5 相等 → `paint_if_changed` 得 `COMPARE_SKIP_SAME`
   （`page_sync.rs:801-805`），**跳过重绘**。
5. **玻璃上却是 UI 画的整屏外壳**——而 UI 要能上屏，必须先有一个**画板未持屏的时间窗**：
   `RenderAll` 在 `page_sync_is_displaying()` 时直接 `return`（`rawdraw_ui_manager.cc:729-730`），
   所以 UI 无法在画板持屏时覆盖玻璃。两个真实窗口：
   - **（a）promotion 启动竞态**（本次现象的最可能来源）：`Initialize()` 在
     `page_sync_start()` 之前就 `BuildRawDrawUi()`（`application.cc:521`），此刻
     `DISPLAYING=false`，`Init` 的 `RefreshActivePage` **真的画进了 framebuffer**；
     冷启动配对设备 `ui_boot_paint_deferred_=true`（`:519`），于是这次绘制**没有伴随面板刷新**。
     约 1 秒后第一次 `NoteButtonActivity()` 把 deferred 置回 false 并
     `RequestActivePageRefresh()`（`:1113-1118`），**把那个已在 framebuffer 里的外壳刷上屏**
     ——而画板此刻已持屏（`page_sync_start()` 已把 `DISPLAYING=true`，但因 md5 命中而从未重绘）。
   - **（b）交权后画板没重画**：`SwitchPage` → `stop_display()` → 退出时
     `allow_display()`，若画板下一轮 `paint_if_changed` 恰好跳过，record 仍称画板在屏。
6. 结果：**record 称「画板某页在屏上」，玻璃上却是 UI 内容**——分叉。

**对本设计的含义**：要修的不是「7 处绘制点都记账」，而是
**「谁把玻璃从画板手里拿走」这一件事必须记账**。

### 1.2 交权只有三个入口

`page_sync_stop_display()` / Rust `page_sync::stop_display()` 是**唯一的「UI 拿走玻璃」转换点**，
调用者只有 3 处：

| 位置 | 场景 |
|---|---|
| `ui/rawdraw_ui_manager.cc:404` | `SwitchPage()` —— UI 显式切页（含设置页） |
| `application.cc:862` | `ServicePromotion()` —— promotion 冷启动 |
| `rust/src/notify.rs:132` | 通知上屏（`show_bitmap`） |

`ServicePromotion()` 目前紧跟一行 `rf_panel_record_invalidate()`（`application.cc:863`），
其注释已把本设计的道理写了一半：「the glass is about to show a UI page instead
（the §4.3.5 divergence … without it the next wake's md5 compare matches and skips,
leaving the blank frame up）」。**本设计就是把这个临时补丁泛化为 record 的常态属性**，
并补上另外两个入口。

---

## 2. 目标与非目标

**目标**：让 record 能表达「玻璃上是谁画的」，从而 `paint_if_changed` 不再在
UI 已经/将要覆盖玻璃时误判跳过。

**非目标**：
- 不改变任何刷新行为（不做 C-2 的「物理关闭局部刷新」）。
- 不解释 `rr=4` 的成因（独立主线，靠串口 backtrace）。
- 不改动 `RefreshRect` 的局部刷新语义与性能特征。
- **不修启动外壳的「上屏时机」**（§1.1(a) 的 framebuffer 内容问题）——不抑制那次绘制、
  不改启动顺序、不挂起画板。但**记账**必须关掉：终审发现 `BuildRawDrawUi`（`application.cc:521`）
  跑在 `page_sync_start()`（`:413`，由 WiFi 连上后的配对任务触发）之前，`DISPLAYING=false`
  的窗口是**秒级**（不是微秒），期间 UI 帧可以上玻璃而没有 `stop_display*`。
  修法：在 `Init` 的刷新回调里、越过 deferred 返回之后加一处条件清除——当画板未持屏且记录仍
  声称画板时调 `rf_panel_record_invalidate()`。必须用 invalidate（同步清已提交记录）而非
  `stop_display_src`：`paint_if_changed` 读的是已提交记录。见 §3.8。

---

## 3. 设计

### 3.1 ABI：在既有 padding 里加 `source`，偏移与总长不变

```c
/* Layout is relied on by the host stub and the Rust reader: valid at offset 4,
 * source at offset 5, md5 at offset 8, index at offset 44, sizeof == 48.
 * Keep them in sync. */
typedef struct {
    uint32_t magic;              /* 0  */
    uint8_t  valid;              /* 4  */
    uint8_t  source;             /* 5  <-- 新增，占用原 _pad[3] 的第一字节 */
    uint8_t  _pad[2];            /* 6  <-- 由 _pad[3] 缩为 _pad[2] */
    char     displayed_md5[33];  /* 8  */
    int32_t  displayed_index;    /* 44 */
} rf_panel_record_t;             /* sizeof == 48 —— 所有偏移不变 */
```

**为什么这样切**：`_pad[3]` 本就为对齐存在，取出 1 字节，**magic/valid/md5/index 的
偏移与结构总长全部不变**；主机 stub（`shim.rs`）与 Rust reader 的既有布局断言无需修改，
只新增 `source@5` 一条。

### 3.2 `source` 取值

```c
/* rf_panel_source_t：玻璃上的内容由谁绘制 */
#define RF_PANEL_SRC_NONE         0  /* 未知/未记录（invalidate 之后的默认） */
#define RF_PANEL_SRC_CANVAS       1  /* 画板页 */
#define RF_PANEL_SRC_UI           2  /* UI 整屏（对话/天气/新闻/相册等） */
#define RF_PANEL_SRC_SETTINGS     3  /* 设置页 */
#define RF_PANEL_SRC_NOTIFICATION 4  /* 通知（全屏位图弹窗） */
```

用户要求「更细」，故 UI 与 settings、notification 分开。粒度判据是
**「谁把玻璃整个拿走」**，不按 renderer 穷举（否则 20+ 个 renderer 各占一值，
且新增 renderer 就漏一个）。

**设置页单列**：它有独立的进入/退出与交还语义（`page_sync_allow_display()`）。
**通知单列**：`notify_dismiss()` → `resume_display()` 会主动取回屏幕。

### 3.3 判定规则（核心）：只在交权点写来源

| 事件 | `source` |
|---|---|
| 画板成功上屏（`blit_and_refresh` / 空页提示） | `CANVAS` |
| **`stop_display()`**（= UI/设置/通知拿走玻璃） | 由调用者指定（见 §3.5） |
| `rf_panel_record_invalidate()` | `NONE`（且 `valid=0`） |
| 其余一切绘制（`RenderAll`、`RefreshRect`、时钟 tick、局部/浮层） | **不碰** |

**为什么用交权点而不是绘制点**：
1. **正确性**：`RenderAll` 在画板持屏时早退，所以「UI 真的上屏」与「画板已交权」
   是同一件事的两个说法；挂在交权点不会漏记。
2. **少而集中**：3 处 vs 7 处，且不与「哪个 renderer 在画」耦合。
3. **零性能风险**：时钟 tick 的 `RenderAll`（`rawdraw_ui_manager.cc:1305`，注释明示
   *"Do NOT Clear() the whole buffer"*，为让 diff 小走局部刷新）**不在交权点**，
   天然不写来源。若按初稿挂在 7 处绘制点，则需要额外规则才能排除它——那正是初稿的风险所在。

### 3.4 record 可信判据收紧

```rust
fn record_trusted(rec: &[u8; 48]) -> bool {
    record_magic_ok(rec) && rec[4] != 0 && rec[5] == RF_PANEL_SRC_CANVAS
}
```

原来只查 `magic && valid`；现追加「来源必须是画板」。UI/设置/通知拿走玻璃后，
record 读作不可信 → `paint_if_changed` 不再跳过 → **下一轮周期必然重绘**，分叉消除。

### 3.5 写入路径（复用既有 pending→commit，不新增同步原语）

`source` 与 md5/index 走同一条通道：

- `shim_power.h` / `shim.cpp`：新增
  `void rf_panel_mark_pending_src(const char* md5, int index, uint8_t source);`
  原 `rf_panel_mark_pending(md5, index)` 保留并转调 `..., RF_PANEL_SRC_CANVAS`
  （画板是既有唯一调用者，语义不变）；
- commit 钩子（`AddOnRefreshIdle` 回调）把 `source` 一并提交——保持
  「刷新真正 idle 后才记账」的既有语义，中断的刷新不会被记成完成；
- `rf_panel_record_invalidate()` 置 `source = NONE` 并清 `valid`。

**三个交权点各自的动作**（Rust `page_sync::stop_display()` 增加带参变体）：

```rust
// page_sync.rs
pub fn stop_display() { stop_display_src(RF_PANEL_SRC_UI); }
pub fn stop_display_src(source: u8) {
    SUSPENDED.store(true, Ordering::Release);
    DISPLAYING.store(false, Ordering::Release);
    // record 关于玻璃的说法此刻起不再成立：staged，刷新 idle 后提交。
    unsafe { shim::rf_panel_mark_pending_src(EMPTY_PAGE.md5.as_ptr(), -1, source) };
}
```

| 调用点 | 传入 |
|---|---|
| `rawdraw_ui_manager.cc:404`（`SwitchPage`） | 目标是 Settings → `SETTINGS`；否则 `UI` |
| `rust/src/notify.rs:132` | `NOTIFICATION` |
| `application.cc:862`（`ServicePromotion`） | 维持 `rf_panel_record_invalidate()`（`NONE`）**不变** |

**为何 promotion 仍用 invalidate**：promotion 发生时屏上通常还没有任何 UI 内容
（`BringUpPanel` 紧随其后），「谁在玻璃上」此刻**尚不可知**，`NONE` 是诚实的取值；
等 `Init` 真正画完再写会更准，但那要求盯绘制点，与 §3.3 的收敛相悖。`NONE` 与
`UI` 在 `record_trusted` 下同效（都不可信），差别只在日志可读性。

### 3.6 全屏绘制路径清单（**信息性，不需改动**）

初稿要求这 7 处都写来源；复核后确认**都不需要**，因为：
- 画板持屏时 `RenderAll` 早退（`:729-730`），这 7 处画不进玻璃；
- 画板不持屏时，玻璃的守卫转换（`stop_display`）**已经**写过来源。

| 行 | 函数 | 说明 |
|---|---|---|
| 356 | `Init` | 启动外壳。绘制本身不修；其刷新回调是 §3.8 缝隙守卫的落点（越过后才清除记录） |
| 431 | `SwitchPage` | 其上的 `:404 stop_display()` 已负责（本设计改这里） |
| 514 | `RefreshActivePage` | `Clear+RenderAll` 先经 `page_sync_is_displaying()` 守卫 |
| 533 | `RefreshActivePageRect` | 同上 |
| 706 | `HandleInput` | 同上 |
| 1437 | `UpdateWifiStatus` | 额外门 `current_page_ == Wifi` |
| 1477 | `SetLifeBarVisible` | 额外门 `current_page_ == LifeBar` |

后两处（`UpdateWifiStatus` / `SetLifeBarVisible`）**没有** `page_sync_is_displaying()`
守卫，理论上可在画板持屏时执行 `Clear+RenderAll`。但两者都要求 `current_page_`
恰为 Wifi/LifeBar，而进入这些页必经 `SwitchPage` → `stop_display()`，故**当前不可达**。
按「只修有证据支持的问题」的既有纪律，**本设计不改它们**，仅在此登记为观察项。

### 3.7 Rust 侧改动

- `page_sync.rs`：`record_trusted()` 追加 `rec[5] == RF_PANEL_SRC_CANVAS`；
  新增 `stop_display_src(u8)` 与 `page_sync_stop_display_src` 的 `extern "C"` 导出；
  `stop_display()` 转调默认 `UI`。
- `page_compare_policy.rs`：**不改**。`record_trusted` 仍是布尔输入，收紧发生在上游。
- `shim.rs`：`rf_panel_mark_pending_src` 声明；主机 stub 的 `PANEL_REC` 元组
  加 `source` 字段，`stage_panel_record` 加参（4 个既有调用点同步更新）。
- `shim_power.h`：结构体与 `RF_PANEL_SRC_*` 常量。

### 3.8 启动外壳缝隙的记账（终审后补，2026-09-30）

**缝隙**：`BuildRawDrawUi`（`application.cc:521`）在 `page_sync_start()`（`:413`）之前跑。
后者在 `ServerPairingTaskTrampoline` 里，由 WiFi-Connected 事件触发（`StartServerPairingOnce`，
`:559`，`s_pairing_started` 一次性去重），故 `page_sync_start()` 是**全仓唯一**的调用点，
而 `DISPLAYING=false` 的窗口长度 = 「构建 UI 到 WiFi 连上」，**秒级**。

**为什么它不是「Init 那一次绘制」**：配对启动时 `ui_boot_paint_deferred_=true`（`:519`），
`Init` 的 `TriggerRefresh(true)` 被回调整体早退拦下；而清掉 deferred 之后的
`RefreshActivePage` 会在 `DISPLAYING` 为真时早退。故 `Init` 自身大概率上不了玻璃。
**真正能带 UI 帧上屏的是窗口内 flusher**（`NoteButtonActivity` / promotion 的
`RequestActivePageRefresh`），它们不碰记录。

**修法**（`application.cc` 的 `BuildRawDrawUi`，`Init()` 返回之后）：

```cpp
    if (ui_boot_paint_deferred_.load(std::memory_order_acquire) &&
        __omp_shell("page_sync_is_displaying() && !rf_panel_record_pending()) {")
        rf_panel_record_t rec;
        rf_panel_record_get(&rec);
        if (rec.magic == RF_PANEL_MAGIC && rec.valid != 0 &&
            rec.source == RF_PANEL_SRC_CANVAS) {
            rf_panel_record_invalidate();
        }
    }
```

**为什么挂在「进入 framebuffer」而不是「刷新入口」**（2026-09-30 修正，前一版挂错了位置）：
`ui_boot_paint_deferred_` 抑制的是**刷新请求**，不是渲染——外壳像素已经进了 framebuffer，
之后**任何**一次 flush 都会把它带上玻璃。挂在刷新入口等于赌「那一次 flush 恰好走这个回调」，
而那正是未定问题；落在 `Init()` 返回之后则与「谁请求了那次 flush」无关。
7.5 小时串口日志支持这一点：58 次 `skipping repaint` 里只有 **1** 次后面跟了刷新事务
（启动那次，`up=10040`，其 20 s `EPD busy wait` 起于 skip 后 1.2 s，且发生在
`page_sync_start()` 仍在等 WiFi 期间——WiFi 于 `up=18750` 才连上）。

**三个合取项**：
- `ui_boot_paint_deferred_`：只在「配对启动、外壳刷新被推迟」的这一次动手。
- `!page_sync_is_displaying()`：画板持屏时记录说的就是画板，不能动。
- `!rf_panel_record_pending()`：`SwitchPage` 已把交权 staged 进 `g_pending_*`（合法、只等提交），
  而 `rf_panel_record_invalidate()` 会连 `g_pending_valid` 一起清掉——那会把一次合法交权抹成
  `NONE`。虽然 `NONE` 与 `UI/SETTINGS` 在 `record_trusted` 下同效，但记录会失真。

**为什么是 invalidate 而不是 `stop_display_src`/`mark_pending_src`**：`paint_if_changed`
读的是**已提交**的 `g_panel_rec`，而 `mark_pending_src` 只写 `g_pending_*`，要等刷新 idle
才提交。普通唤醒时提交方是 `RunPowerCycle` 的 `page_sync_paint_if_changed()`——正是这条要用
修复判定的路径，循环依赖。`rf_panel_record_invalidate()` 同步清 `g_panel_rec`，无此问题。

**反过来不能做**：不能在这里中止 UI 刷新（那会把一次既有绘制变成白屏或推迟上屏），
那是改机制，超出本设计的边界。

**待证**：缝隙是否真的可达，由真机串口日志判定——看
`RawDraw UI Manager initialized`、`PageSync: started` 与紧随的 `skipping repaint`
之间的先后，以及这处 invalidate 是否真的触发过。若日志证明不可达，这处守卫保持为惰性防御。

---

## 4. 测试

**布局契约（Rust，每模块自带）**
- `offset_of!(rf_panel_record_t, source) == 5`、`size_of == 48`、
  `valid == 4`、`displayed_md5 == 8`、`displayed_index == 44` —— 证明加字段未移动任何既有偏移。

**行为测试（Rust 纯逻辑，主机可跑）**
- `record_trusted()`：`source=CANVAS` → true；`UI`/`SETTINGS`/`NOTIFICATION`/`NONE` → false；
  `magic` 错 → false；`valid=0` → false。
- `paint_if_changed`：record `CANVAS` 且 md5 相同 → 跳过（既有行为）；
  record `UI` 且 md5 相同 → **重绘**（本设计要修的分叉）。
- `stop_display_src(NOTIFICATION)` 后再 `sync_once()` + `paint_if_changed()` →
  必须重绘且日志不出现 `skipping repaint`。
- **哨兵**：把 `rec[5] == RF_PANEL_SRC_CANVAS` 从 `record_trusted` 删除，
  「UI→重绘」测试必须变红。

**回归**
- 既有 `glass already shows … skipping repaint` 测试仍绿（画板连续两轮同页 → 第二轮仍跳过）。
- 既有布局断言（48/8/44）全部保持。

**真机验收（用户串口已接）**
- BOOT 双击强制同步后 `show page …`，确认 record 来源在日志中可读。

---

## 5. 风险与取舍

| 风险 | 评估与缓解 |
|---|---|
| 收紧后重绘次数增加 | 设计意图。**仅「交权之后」的首轮多一次全刷**；交权是用户驱动的低频事件（进设置页、收通知、promotion），**非每分钟**。§3.3 的选择正是为守住这条。 |
| 深睡省电回归 | 深睡唤醒后画板正常重绘 → 记 `CANVAS` → 下次唤醒仍可跳过。**红线，必须守住**。 |
| 启动外壳仍可能先上屏一次 | 明确非目标（不改上屏时机）。但 record 不再说谎：§3.8 的缝隙守卫在 UI 帧上玻璃时清掉画板记录，故画板会重绘回来。 |
| 未守卫的两处 `Clear+RenderAll` | 当前不可达（§3.6），登记为观察项；若将来可达，同属「UI 越权覆盖画板」，应加守卫而非本条记账。 |
| `stage_panel_record` 签名变更 | 4 个既有调用点同步更新（`page_sync.rs:1382/1431/2085/2150`）。 |

---

## 6. 明确不做

- **不做 C-2**（物理关闭局部刷新）：`RefreshRect` 的浮层用途仍在
  （`rawdraw_ui_manager.cc:659/679` 快速切换），且四色屏上局部刷新的可靠性**缺乏真机证据**。
- 不改 `RefreshRect` 语义；不改 `PumpClockRefresh`。
- 不给 `UpdateWifiStatus` / `SetLifeBarVisible` 加守卫（无证据表明可达）。
- 不引入新锁或同步原语（复用 `g_panel_mux` 与既有 pending→commit 通道）。
- 不解释、不修 `rr=4`。
