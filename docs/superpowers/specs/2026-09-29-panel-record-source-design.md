# 面板记录来源（panel record source）设计

**日期**：2026-09-29
**状态**：待用户复审
**前置证据**：`docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md`（本次排查记录）

---

## 1. 问题

设备出现「屏幕停在陈旧帧、且系统认为无需重绘」的情形。2026-09-29 17:51 与 17:56 两次 `rr=4`（`ESP_RST_PANIC`）复位后，屏幕停在 `title='对话'` + `wifi=0` 的一帧上，而链路实际正常（`dns ok` / `tcp ok` / `schedule 200` / `device-log 201`）。

### 1.1 因果链（每一环均有代码依据）

1. `g_panel_rec` 是 `RTC_DATA_ATTR`（`shim.cpp:88`），**跨 panic/USB 复位不清** —— 这是为深睡省电有意选的（`shim_power.h` 头注释：没有它「每次唤醒都要付一次 ≥15 s 全刷」）。
2. 复位前 record 记着「某页已在玻璃上」且 `valid=1`。
3. panic 重启后，`record_trusted()` 只查 `magic` 与 `valid`（`page_sync.rs:708`），record 仍读作可信。
4. 本次开机目标页恰好与 record 记录的相同 → md5 相等 → `paint_if_changed` 判定 `COMPARE_SKIP_SAME`（`page_sync.rs:800-803`），**跳过重绘**。
5. 而玻璃上实际是 **UI 画的整屏外壳**：`BuildRawDrawUi` 的 `ui_boot_paint_deferred_` 只跳过**面板刷新**，UI 内容照样渲染进 framebuffer（`application.cc:663-666`）；且 `rawdraw_ui_manager.cc` 有 7 处 `Clear + RenderAll` 全屏重画，**全都不更新 record**。
6. 结果：**record 说「画板某页在屏上」，玻璃上却是 UI 内容** —— 分叉。

### 1.2 这不是冷启动独有的

`invalidate` 只在三处调用（`application.cc:863` promotion、`:1508` 策略睡眠、`:1602` 手动睡眠），**没有**覆盖冷启动/panic。但更普遍的问题是：**任何 UI 全屏重画之后，record 都不再描述玻璃实际内容**。复位只是最容易观察到的一次。

---

## 2. 目标与非目标

**目标**：让 record 能正确表达「玻璃上是谁画的」，从而 `paint_if_changed` 不再在 UI 占屏后误判跳过。

**非目标**：
- 不改变任何刷新行为（不做 C-2 的「物理关闭局部刷新」）。
- 不解释 `rr=4` 的成因（那是独立主线，靠串口 backtrace）。
- 不改动 `RefreshRect` 的局部刷新语义与性能特征。

---

## 3. 设计

### 3.1 ABI：在既有 padding 里加 `source`，偏移与总长不变

```c
/* Layout is relied on by the host stub and the Rust reader: source at offset 5,
 * valid at 4, md5 at 8, index at 44, sizeof == 48. Keep them in sync. */
typedef struct {
    uint32_t magic;              /* 0  */
    uint8_t  valid;              /* 4  */
    uint8_t  source;             /* 5  <-- 新增，占用原 _pad[3] 的第一字节 */
    uint8_t  _pad[2];            /* 6  <-- 由 _pad[3] 缩为 _pad[2] */
    char     displayed_md5[33];  /* 8  */
    int32_t  displayed_index;    /* 44 */
} rf_panel_record_t;             /* sizeof == 48 —— 所有偏移不变 */
```

**为什么这样切**：`_pad[3]` 本来就为对齐而存在，取出 1 字节做 `source`，**magic/valid/md5/index 的偏移与结构总长全部不变**，主机端 stub 与 Rust reader 的既有布局断言无需修改，只新增一条 `source@5` 的断言。

### 3.2 `source` 取值

```c
/* rf_panel_source_t：玻璃上的内容由谁绘制 */
#define RF_PANEL_SRC_NONE         0  /* 未知/未记录（invalid 之后的默认） */
#define RF_PANEL_SRC_CANVAS       1  /* 画板页（page_sync blit_and_refresh） */
#define RF_PANEL_SRC_UI           2  /* UI 整屏（对话/天气/新闻/相册等） */
#define RF_PANEL_SRC_SETTINGS     3  /* 设置页（独立的全屏页，有自己的所有权语义） */
#define RF_PANEL_SRC_NOTIFICATION 4  /* 通知弹窗（独立全屏来源） */
```

用户要求「更细」，故 UI 与 settings、notification 分开。**关键判据是"谁能整屏覆盖玻璃"**，而不是"哪个 renderer"——所以粒度按**全屏绘制者**划分，不按 renderer 穷举（否则 20+ 个 renderer 各占一个值，且新增 renderer 就漏一个）。

**设置页为何单列**：它有自己的进入/退出与屏幕交还语义（`page_sync_allow_display()`，见 `note4c-screen-ownership`），与普通 UI 页的所有权不同。**通知单列**同理（`notify_dismiss` → `resume_display` 会主动取回屏幕）。

### 3.3 判定规则（本设计的核心）

`source` 只在**整屏覆盖玻璃**时更新：

| 路径 | 是否改 `source` | 依据 |
|---|---|---|
| 画板 `blit_and_refresh` 成功 | 设为 `CANVAS` | 它是全屏 2bpp 位图 |
| `RefreshActivePage` / `RefreshActivePageRect` / `SwitchPage` / `Init` / `HandleInput` / `UpdateWifiStatus` / `SetLifeBarVisible` 的 **`Clear + RenderAll`** | 设为 `UI`（设置页设 `SETTINGS`，通知设 `NOTIFICATION`） | 先 `Clear` 再整屏重画 = 玻璃被整体替换 |
| `RefreshRect`（浮层、局部上屏） | **不碰** | 只在既有画面上叠加一小块，来源不变 |
| `PumpClockRefresh` 的 `RenderAll`（**无 `Clear`**） | **不碰** | `rawdraw_ui_manager.cc:1305` 注释明示：故意不清屏，让 diff 小以走局部刷新；来源仍是原内容 |

**第三、四条排除了性能陷阱**：若把"无 Clear 的时钟重画"也算作来源变更，则**每分钟的状态栏 tick 都会让画板判定需要重绘 → 10–25 s 全刷**。

### 3.4 record 可信判据收紧

```rust
fn record_trusted(rec: &[u8; 48]) -> bool {
    record_magic_ok(rec) && rec[4] != 0 && rec[5] == RF_PANEL_SRC_CANVAS
}
```

原来只查 `magic && valid`；现在追加「来源必须是画板」。UI/设置/通知占过屏后，record 读作不可信 → `paint_if_changed` 不再跳过 → **下一个周期必然重绘**，分叉消除。

### 3.5 写入点

`source` 与 md5/index 走**同一条 pending→commit 通道**，不新增同步原语：

- `rf_panel_mark_pending(const char* md5, int index)` 追加声明 `void rf_panel_mark_pending_src(const char* md5, int index, uint8_t source);`，原函数保留并转调 `..., RF_PANEL_SRC_CANVAS`（画板是既有唯一调用者，语义不变）；
- UI 侧新增调用点，传 `RF_PANEL_SRC_UI` / `SETTINGS` / `NOTIFICATION`；
- commit 钩子（`AddOnRefreshIdle` 回调）把 `source` 一并提交 —— 保持「刷新真正 idle 后才记账」的既有语义，中断的刷新不会被记成完成；
- `rf_panel_record_invalidate()` 置 `source = NONE`（与 `valid = 0` 一致）。

### 3.6 全屏路径清单（请重点复核）

`rawdraw_ui_manager.cc` 现有 **7 处** `Clear + RenderAll`，**每处都需按上表设置来源**：

| 行 | 函数 | 来源 |
|---|---|---|
| 356 | `Init` | UI（启动外壳） |
| 431 | `SwitchPage` | UI / SETTINGS（按目标页判定） |
| 514 | `RefreshActivePage` | 按当前页判定 |
| 533 | `RefreshActivePageRect` | 按当前页判定（虽只推 rect，但已整屏重画） |
| 706 | `HandleInput` | 按当前页判定 |
| 1437 | `UpdateWifiStatus` | 按当前页判定 |
| 1477 | `SetLifeBarVisible` | 按当前页判定 |

**「按当前页判定」的判据**：`GetCurrentPage() == RawDrawPageId::Settings` → `SETTINGS`；`notify_is_active()` → `NOTIFICATION`；否则 `UI`。集中到一个 C++ 辅助函数（如 `CurrentPanelSource()`），**不散落到 7 处**。

### 3.7 Rust 侧改动

- `page_sync.rs`：`record_trusted()` 追加 `source == CANVAS`；新增 `fn record_source(rec: &[u8;48]) -> u8`（读 `rec[5]`，与 `record_index` 同手法）。
- `page_compare_policy.rs`：`PageInputs` 的 `record_trusted` 语义不变（仍是布尔），**不需要**新增字段 —— 收紧发生在上游的 `record_trusted()`，策略层不感知来源。
- 主机 stub（`shim.rs` 的 `rf_panel_record_*`）：补齐 `source` 字段的读写与测试辅助。

---

## 4. 测试

**布局契约（Rust，每模块自带）**
- `offset_of!(rf_panel_record_t, source) == 5`、`size_of == 48`、`displayed_md5` 仍在 8、`displayed_index` 仍在 44 —— 证明加字段未移动任何既有偏移。

**行为测试（Rust 纯逻辑，主机可跑）**
- `record_trusted()`：`source=CANVAS` → true；`source=UI/SETTINGS/NOTIFICATION/NONE` → false；`magic` 错 → false；`valid=0` → false。
- `paint_if_changed`：record 说 `CANVAS` 且 md5 相同 → 跳过（保持既有行为）；record 说 `UI` 且 md5 相同 → **重绘**（本设计要修的分叉）。
- 哨兵：把 `source == CANVAS` 这一项从 `record_trusted` 去掉，上述「UI→重绘」测试必须变红。

**回归**
- 既有 `glass already shows ... skipping repaint` 相关测试必须仍绿（画板连续两周期画同一页 → 第二次仍跳过）。
- 既有布局断言全部保持（48/8/44）。

---

## 5. 风险与取舍

| 风险 | 评估与缓解 |
|---|---|
| **收紧后可重绘次数增加** | 这是**设计意图**（消除分叉），但需量化：只有「UI 全屏占屏后」的首次周期会多一次全刷。**若发现高频路径**（例如每次状态栏变化都走 `Clear+RenderAll`），须改走 `PumpClockRefresh` 的无 Clear 路径。§3.6 清单即为此核对用。 |
| `PumpClockRefresh` 若被误判成来源变更 | 已在 §3.3 明确排除（无 `Clear`）。这是最需要守住的一条。 |
| 7 处调用点漏设来源 | 集中到 `CurrentPanelSource()` 一处；测试覆盖「设置页占屏后 record 读作不可信」。 |
| 深睡省电回归 | 深睡唤醒后画板正常重绘 → 记 `CANVAS` → 下一次唤醒仍可跳过。**省电路径不受影响**（这是本设计必须守住的红线）。 |

---

## 6. 明确不做

- **不做 C-2**（物理关闭局部刷新）：`RefreshRect` 的浮层用途仍在（`rawdraw_ui_manager.cc:659/679` 快速切换），且四色屏上局部刷新的可靠性**缺乏真机证据**。若要推进，需先有证据再单独设计。
- 不改 `RefreshRect` 的语义。
- 不引入新的锁或同步原语（复用 `g_panel_mux` 与既有 pending→commit 通道）。
