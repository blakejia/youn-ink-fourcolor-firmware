# 面板记录来源（panel record source）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 RTC 面板记录能表达「玻璃上是谁画的」，从而画板在 UI/设置/通知拿走玻璃后不再误判「已是目标页」而跳过重绘。

**Architecture:** 在 `rf_panel_record_t` 既有 `_pad[3]` 里取 1 字节作 `source`（所有偏移与 48 B 总长不变）。来源**只在画板交权点**（`stop_display()`，全仓 3 处）写入，复用既有 pending→commit 通道。`record_trusted()` 追加「来源必须是画板」，把「UI 占屏后 record 说谎」这一分叉一次性消除。顺手把 Rust 侧的裸字节偏移读法换成 `#[repr(C)]` 结构体，使布局契约可被真实断言（原先只能断言一个镜像结构，是空转的）。

**Tech Stack:** Rust（`no_std` on xtensa，host 可测）、C++（ESP-IDF v6.0）、ESP32-S3、cargo test + `IDF_TARGET=esp32s3 idf.py build`。

**Spec:** `docs/superpowers/specs/2026-09-29-panel-record-source-design.md`
**证据记录:** `docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md`

## Global Constraints

- **双门禁不可互替**：`cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test` **和** `cd firmware && export PATH="$HOME/.cargo/bin:$PATH" && source ~/data/esp-idf-v6.0/export.sh && IDF_TARGET=esp32s3 idf.py build`。host 绿 ≠ 设备绿。
- `IDF_TARGET=esp32s3` 不可省（裸 `idf.py build` 默认 esp32，会在 SSD2683 面板依赖处失败）。
- `cargo` 必须先于 `export.sh` 进 PATH。
- 本计划**不新增** `.rs` 文件（故无需改 `RUST_SOURCES`）。
- 证明改动进产物：`xtensa-esp32s3-elf-nm build/xiaozhi.elf | grep rf_<symbol>`，并核对时间链 `src → librust_firmware.a → xiaozhi.bin`。
- **不改 `page_compare_policy.rs`**：`record_trusted` 仍是布尔输入。
- **不改** `RefreshActivePage` / `RefreshActivePageRect` / `PumpClockRefresh` / `RefreshRect` / `UpdateWifiStatus` / `SetLifeBarVisible` 的任何行为。
- **promotion 路径保持 `rf_panel_record_invalidate()` 不变**（`application.cc:863`）。
- 不引入新锁；复用 `g_panel_mux` 与既有 pending→commit 通道。
- 不跑全仓格式化；只收敛自己新写的代码。
- 每个任务独立提交，正文写**为什么**与**证据**。

## 起始事实（执行者必读，全部已核实）

- `rf_panel_record_t` 现状（`firmware/main/rust/include/shim_power.h`）：
  `magic@0 (u32)`、`valid@4 (u8)`、`_pad[3]@5`、`displayed_md5[33]@8`、`displayed_index@44 (i32)`、`sizeof == 48`。
- `shim_power.h` **只被 C++ 消费**（`main.cc:14`、`boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc:9`、
  `rust/shim.cpp:37`、`application.cc:33`），故其中用 `_Static_assert`（C++11）安全。
- Rust 侧**没有**该结构体的类型定义；`page_sync.rs` 目前用裸字节读：
  `record_magic_ok()` @51、`record_index()` @56、`record_trusted()` @708、
  `glass_matches` 用 `rec[8..40]`（@717/@730/@784）、测试 @2224-2235 用 `rec.0[8..40]`。
  唯一的 Rust 类型是 `struct PanelRecord([u8; 48])`（`read_panel_record()` @45 返回）。
- 画板写入：`page_sync.rs:816`、`:842` 调 `shim::rf_panel_mark_pending(md5, idx)`；
  空页提示 `:857` 调 `rf_panel_mark_pending(EMPTY_PAGE.md5, -1)`。
- commit 钩子：`firmware/main/rust/shim.cpp` 的 `rf_panel_commit_hook_register()`（`AddOnRefreshIdle` 回调，`portENTER_CRITICAL(&g_panel_mux)` 内提交 `g_pending_*`）。
- `stop_display()` 三个调用点：`ui/rawdraw_ui_manager.cc:404`、`application.cc:862`、`rust/src/notify.rs:132`。
- C++ 侧声明位置：`firmware/main/rust/include/page_sync.h:80`（`void page_sync_stop_display(void);`）。
- 主机 stub：`firmware/main/rust/src/shim.rs:325-377`（`PANEL_REC` 元组 + `stage_panel_record`）。
- `stage_panel_record` 既有调用点：`page_sync.rs:1382 / 1431 / 2085 / 2150`。
- 静态库实际路径：`firmware/main/rust/target/xtensa-esp32s3-none-elf/release/librust_firmware.a`
  （另有 host 用的 `firmware/main/rust/target/debug/librust_firmware.a`）。
- 既有布局测试写法参考：`firmware/main/rust/tests/page_compare_policy.rs:246-251` 用
  `use core::mem::{offset_of, size_of};` 断言**真实** `#[repr(C)]` 类型（非镜像）。

---

### Task 1: Rust 侧建立 `PanelRecord` 真类型（含 `source`），布局契约可断言

**Files:**
- Modify: `firmware/main/rust/src/page_sync.rs`（`#[repr(C)] pub struct PanelRecord` + `RF_PANEL_SRC_*` 常量；`read_panel_record` 改用真类型；`record_magic_ok`/`record_index` 改用字段）
- Modify: `firmware/main/rust/include/shim_power.h`（结构体加 `source`、`RF_PANEL_SRC_*` 宏、`_Static_assert` 钉住布局）
- Modify: `firmware/main/rust/shim.cpp`（`g_pending_source`、`rf_panel_mark_pending_src`、commit 钩子、invalidate）
- Test: `firmware/main/rust/src/page_sync.rs`（同文件 `mod tests`）

**Interfaces:**
- Consumes: 无（首个任务）。
- Produces:
  - Rust：`#[repr(C)] pub struct PanelRecord { pub magic: u32, pub valid: u8, pub source: u8, pub _pad: [u8; 2], pub displayed_md5: [u8; 33], pub displayed_index: i32 }`
  - Rust：`pub const RF_PANEL_SRC_NONE: u8 = 0;`（`CANVAS = 1`、`UI = 2`、`SETTINGS = 3`、`NOTIFICATION = 4`）
  - Rust：`fn read_panel_record() -> PanelRecord`（函数名不变，返回类型换真结构体）
  - C：`#define RF_PANEL_SRC_*`、`void rf_panel_mark_pending_src(const char* md5, int index, uint8_t source);`

- [ ] **Step 1: 写失败的布局契约测试**

在 `firmware/main/rust/src/page_sync.rs` 的 `mod tests` 内（`md5hex` 辅助附近）加入：

```rust
    /// 布局契约：断言**真实**的 PanelRecord（reader 实际用的那个类型），
    /// 不是镜像体——镜像体只能证明测试自己写对了，证明不了 reader 的读法。
    /// source 占用原 _pad[3] 的首字节，故既有偏移与总长全部不变。
    #[test]
    fn panel_record_layout_keeps_every_offset_and_size() {
        use core::mem::{offset_of, size_of};
        assert_eq!(size_of::<PanelRecord>(), 48);
        assert_eq!(offset_of!(PanelRecord, magic), 0);
        assert_eq!(offset_of!(PanelRecord, valid), 4);
        assert_eq!(offset_of!(PanelRecord, source), 5);
        assert_eq!(offset_of!(PanelRecord, displayed_md5), 8);
        assert_eq!(offset_of!(PanelRecord, displayed_index), 44);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test panel_record_layout`
Expected: **编译失败**，报 `PanelRecord` 没有 `source` 字段（当前只有 `struct PanelRecord([u8; 48])`）。编译失败即本步的红。

- [ ] **Step 3: 定义真类型与常量，改 reader**

用以下内容替换 `page_sync.rs` 里的 `struct PanelRecord([u8; 48])`：

```rust
/// 与 `rust/include/shim_power.h` 的 `rf_panel_record_t` 逐字段对应。
/// 用真结构体而非裸字节偏移：布局契约才有可断言的对象（见 tests）。
#[repr(C)]
pub struct PanelRecord {
    pub magic: u32,              /* 0  */
    pub valid: u8,               /* 4  */
    pub source: u8,              /* 5  */
    pub _pad: [u8; 2],           /* 6  */
    pub displayed_md5: [u8; 33], /* 8  */
    pub displayed_index: i32,    /* 44 */
}

/// 玻璃上的内容由谁绘制。数值与 shim_power.h 的 RF_PANEL_SRC_* 一致。
pub const RF_PANEL_SRC_NONE: u8 = 0;
pub const RF_PANEL_SRC_CANVAS: u8 = 1;
pub const RF_PANEL_SRC_UI: u8 = 2;
pub const RF_PANEL_SRC_SETTINGS: u8 = 3;
pub const RF_PANEL_SRC_NOTIFICATION: u8 = 4;

impl PanelRecord {
    const ZERO: PanelRecord = PanelRecord {
        magic: 0,
        valid: 0,
        source: RF_PANEL_SRC_NONE,
        _pad: [0; 2],
        displayed_md5: [0; 33],
        displayed_index: 0,
    };
    /// 前 32 字节是服务端下发的 md5；第 33 字节是 NUL 终止位。
    fn md5(&self) -> &[u8] {
        &self.displayed_md5[..32]
    }
}
```

`read_panel_record()`：

```rust
fn read_panel_record() -> PanelRecord {
    let mut rec = PanelRecord::ZERO;
    unsafe {
        shim::rf_panel_record_get(&mut rec as *mut PanelRecord as *mut u8);
    }
    rec
}
```

`record_magic_ok()` / `record_index()` 改为字段访问：

```rust
fn record_magic_ok(rec: &PanelRecord) -> bool {
    rec.magic == PANEL_MAGIC
}

fn record_index(rec: &PanelRecord) -> i32 {
    rec.displayed_index
}
```

（Rust 侧**已有** `const PANEL_MAGIC: u32 = 0x50414E31;`（`page_sync.rs:36`）——
用它，**不要**新建 `RF_PANEL_MAGIC`。测试里也用 `PANEL_MAGIC`。）

其余读点同步改成字段访问：`glass_matches` 的 `rec[8..40] == md5[..]` 改为
`rec.md5() == &md5[..]`；测试 @2224-2235 的 `rec.0[8..40]` 改为 `rec.md5()`。
**逐个调用点改，改完 `cargo build` 让编译器把漏掉的位置点出来。**
`record_trusted` 本步先保持原判据（`rec[4] != 0` → `rec.valid != 0`），Task 2 再收紧。

- [ ] **Step 4: 改 C 头与 C++ shim**

`shim_power.h`（该头只被 C++ 消费，`_Static_assert` 可用）：

```c
#include <stddef.h>   /* offsetof */

/* Layout is relied on by the host stub and the Rust reader: valid at offset 4,
 * source at offset 5, md5 at offset 8, index at offset 44, sizeof == 48.
 * Keep them in sync. */
typedef struct {
    uint32_t magic;              /* 0  */
    uint8_t  valid;              /* 4  */
    uint8_t  source;             /* 5  */
    uint8_t  _pad[2];            /* 6  */
    char     displayed_md5[33];  /* 8  */
    int32_t  displayed_index;    /* 44 */
} rf_panel_record_t;             /* sizeof == 48 */

/* 玻璃上的内容由谁绘制。粒度按「谁把玻璃整个拿走」，不按 renderer 穷举。 */
#define RF_PANEL_SRC_NONE         0
#define RF_PANEL_SRC_CANVAS       1
#define RF_PANEL_SRC_UI           2
#define RF_PANEL_SRC_SETTINGS     3
#define RF_PANEL_SRC_NOTIFICATION 4

_Static_assert(sizeof(rf_panel_record_t) == 48, "rf_panel_record_t must stay 48 bytes");
_Static_assert(offsetof(rf_panel_record_t, valid) == 4, "valid at 4");
_Static_assert(offsetof(rf_panel_record_t, source) == 5, "source at 5");
_Static_assert(offsetof(rf_panel_record_t, displayed_md5) == 8, "md5 at 8");
_Static_assert(offsetof(rf_panel_record_t, displayed_index) == 44, "index at 44");

void rf_panel_record_get(rf_panel_record_t* out);
void rf_panel_mark_pending(const char* md5, int index);
void rf_panel_mark_pending_src(const char* md5, int index, uint8_t source);
void rf_panel_record_invalidate(void);
void rf_panel_commit_hook_register(void);
```

在 `g_pending_index` 旁加 `static uint8_t g_pending_source = RF_PANEL_SRC_NONE;`
（与 `g_pending_*` 同组、同受 `g_panel_mux` 保护）。原有实现是**在临界区外
`snprintf` 进 `staged[33]`、临界区内 `memcpy` 发布**，新函数照此模式（注意临界区只进一次）：

```cpp
extern "C" void rf_panel_mark_pending_src(const char* md5, int index, uint8_t source) {
    if (md5 == nullptr) return;
    char staged[33];
    snprintf(staged, sizeof(staged), "%s", md5);
    portENTER_CRITICAL(&g_panel_mux);
    memcpy(g_pending_md5, staged, sizeof(g_pending_md5));
    g_pending_index = index;
    g_pending_source = source;
    g_pending_valid = true;
    portEXIT_CRITICAL(&g_panel_mux);
}

extern "C" void rf_panel_mark_pending(const char* md5, int index) {
    // 画板是既有的唯一调用者：默认记为画板来源，语义不变。
    rf_panel_mark_pending_src(md5, index, RF_PANEL_SRC_CANVAS);
}
```

commit 钩子内（`if (g_pending_valid)` 块）追加 `g_panel_rec.source = g_pending_source;`；
`rf_panel_record_invalidate()` 追加 `g_panel_rec.source = RF_PANEL_SRC_NONE;`。

- [ ] **Step 5: 跑布局测试 + 两个门禁**

Run:
```bash
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test panel_record_layout
cd /mnt/data/project/youn-ink-fourcolor-firmware/firmware && export PATH="$HOME/.cargo/bin:$PATH" && source ~/data/esp-idf-v6.0/export.sh && IDF_TARGET=esp32s3 idf.py build
```
Expected: 布局测试 PASS（`source@5`、`sizeof 48`、`md5@8`、`index@44`）；构建成功
——C 侧四条 `_Static_assert` 同时被编译器验证，任一条不成立就编译失败。

- [ ] **Step 6: 哨兵验证（证明布局断言不空转）**

临时把 `PanelRecord` 的 `_pad: [u8; 2]` 改成 `[u8; 3]`，**同时**把 `PanelRecord::ZERO`
里的 `_pad: [0; 2]` 改成 `[0; 3]`（否则先死在 `ZERO` 的 E0308 类型不匹配上，到不了布局断言
——那不是你要的证据），重跑：
Run: `cargo test panel_record_layout`
Expected: **变红**，且失败在 `assert_eq!(offset_of!(PanelRecord, displayed_md5), 8)`，
报告 `left: 9, right: 8`。**然后两处都改回 `[u8; 2]`/`[0; 2]`**，重跑确认绿。
（若你看到的红是编译错误而非该断言失败，说明哨兵没到位，不算证据。）

- [ ] **Step 7: 证明新符号进产物**

Run: `xtensa-esp32s3-elf-nm build/xiaozhi.elf | grep -E "rf_panel_mark_pending(_src)?$"`
Expected: `rf_panel_mark_pending` 与 `rf_panel_mark_pending_src` 均为 `T`。

- [ ] **Step 8: 提交**

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/rust/include/shim_power.h firmware/main/rust/shim.cpp
git commit -m "feat(panel): give the RTC panel record a provenance byte

The record could only say 'a canvas page is on the glass', which stops being
true the moment the UI takes the panel over. Put the provenance byte in the
record's existing _pad[3] so every offset and sizeof stay 48 -- magic 0,
valid 4, source 5, md5 8, index 44 -- and route it through the same
pending/commit channel, so an interrupted refresh is still never booked as
done. rf_panel_mark_pending keeps its signature and now defaults to CANVAS,
which is exactly what its only caller, the canvas, means.

The reader also stops poking at raw byte offsets: PanelRecord is now a
#[repr(C)] type mirroring rf_panel_record_t, so the layout contract can be
asserted against the type the reader actually uses (a mirror struct would
only have proven the test agreed with itself). shim_power.h pins the same
five facts with _Static_assert, which idf.py build enforces.

Evidence: cargo test panel_record_layout (48 / 0 / 4 / 5 / 8 / 44) with the
_pad[2]->[3] sentinel turning it red; nm shows rf_panel_mark_pending and
rf_panel_mark_pending_src as T."
```

---

### Task 2: 收紧 `record_trusted()` 并要求主机 stub 与设备记录同构

**Files:**
- Modify: `firmware/main/rust/src/shim.rs`（`rf_panel_mark_pending_src` 声明；`PANEL_REC` 元组加 `source`；`rf_panel_record_get` 写 offset 5；`stage_panel_record` 加参）
- Modify: `firmware/main/rust/src/page_sync.rs`（`record_trusted` @708；5 处调用点；4 个 `stage_panel_record` 调用点；新增行为测试）
- Test: `firmware/main/rust/src/page_sync.rs`（同文件测试模块）

**Interfaces:**
- Consumes: Task 1 的 `PanelRecord`、`RF_PANEL_SRC_*`、`rf_panel_mark_pending_src`。
- Produces:
  - `fn record_trusted(rec: &PanelRecord) -> bool`，要求 `source == RF_PANEL_SRC_CANVAS`
  - `shim::host::stage_panel_record(magic: u32, valid: u8, source: u8, md5: &[u8], index: i32)`

- [ ] **Step 1: 写失败的行为测试**

```rust
    #[test]
    fn a_record_written_by_the_ui_makes_the_canvas_repaint_the_same_md5() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(
            &format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)),
            &bitmap_body(0xa1),
        );
        // 玻璃上确实是 0xa1 那个 md5，但它是 UI 画的（来源不是画板）。
        shim::host::stage_panel_record(
            PANEL_MAGIC, 1, RF_PANEL_SRC_UI, md5hex(0xa1).as_bytes(), 0,
        );
        assert!(sync_once());
        assert!(
            paint_if_changed(),
            "same md5 but the UI painted it -> the canvas must repaint"
        );
        assert_eq!(shim::host::fb()[0], 0xa1);
        assert_eq!(shim::host::refreshes(), 1);
    }

    #[test]
    fn a_record_written_by_the_canvas_still_skips_the_repaint() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::stage_panel_record(
            PANEL_MAGIC, 1, RF_PANEL_SRC_CANVAS, md5hex(0xa1).as_bytes(), 0,
        );
        assert!(sync_once());
        assert!(!paint_if_changed(), "the canvas already shows page 0");
        assert_eq!(shim::host::refreshes(), 0);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test a_record_written_by_`
Expected: **编译失败**（`stage_panel_record` 仍是 4 参）。编译失败即本步的红。

- [ ] **Step 3: 改 stub**

`shim.rs`：
1. 在 `rf_panel_mark_pending` 旁声明 `pub fn rf_panel_mark_pending_src(md5: *const c_char, index: c_int, source: u8);`
2. `PANEL_REC` 由 `Mutex<Option<(u32, u8, Vec<u8>, i32)>>` 改为 `Mutex<Option<(u32, u8, u8, Vec<u8>, i32)>>`；
3. `rf_panel_record_get` 内 `*out.add(4) = *valid;` 之后加 `*out.add(5) = *source;`；
4. `rf_panel_mark_pending` 写入 `RF_PANEL_SRC_CANVAS`；新增 `rf_panel_mark_pending_src` 写入传入值；
5. `stage_panel_record(magic, valid, source, md5, index)` 签名加 `source`；
6. 该块文档注释同步更新为「magic@0, valid@4, source@5, md5@8, index@44」。

- [ ] **Step 4: 改 `record_trusted` 与调用点**

```rust
/// 玻璃上的说法只有在「确实是画板画上去的」时才可信。UI/设置/通知占过屏后，
/// record 记的那页已经不在玻璃上了，md5 命中也不再意味着可以跳过重绘。
fn record_trusted(rec: &PanelRecord) -> bool {
    record_magic_ok(rec) && rec.valid != 0 && rec.source == RF_PANEL_SRC_CANVAS
}
```

5 处调用点跟随签名变化（`&rec` 而非 `&rec.0`）：`prepare_decision` @713-717、
`paint_decision` @725-730、`paint_if_changed` 的 `tag` 分支 @797-801 与空页分支
@780-784、以及 `record_trusted` 自身。**用 `cargo build` 让编译器指路。**

4 处 `stage_panel_record` 调用补来源：
- `:1382` → `stage_panel_record(0xDEAD_BEEF, 1, RF_PANEL_SRC_CANVAS, md5hex(0xa1).as_bytes(), 0)`
- `:1431 / :2085 / :2150` → `stage_panel_record(PANEL_MAGIC, 1, RF_PANEL_SRC_CANVAS, md5hex(0xa1).as_bytes(), 0)`

- [ ] **Step 5: 跑测试确认通过 + 全量回归**

Run: `cargo test`
Expected: 全绿（基线 419 + 本计划新增）。特别确认既有
`a_glass_that_already_shows_the_target_page_fetches_nothing`、
`failed_sync_with_a_page_on_the_glass_paints_nothing`、
`prepare_paint_is_true_when_nothing_needs_downloading` 仍绿。

- [ ] **Step 6: 哨兵验证（证明测试不空转）**

把 `rec.source == RF_PANEL_SRC_CANVAS` 从 `record_trusted` 删掉，重跑：
Run: `cargo test a_record_written_by_`
Expected: `a_record_written_by_the_ui_makes_the_canvas_repaint_the_same_md5` **变红**。
若仍绿，说明测试没走到那条断言——修正测试后重来。**然后恢复该合取项**，重跑确认绿。

- [ ] **Step 7: 提交**

```bash
git add firmware/main/rust/src/shim.rs firmware/main/rust/src/page_sync.rs
git commit -m "feat(panel): only trust the record when the canvas painted it

record_trusted() gated on magic+valid alone, so a record written before the
UI took the panel still claimed 'this page is on the glass' and the md5
compare skipped the repaint -- the 17:51/17:56 stale-frame divergence.
Require source == CANVAS, and mirror the new field in the host stub so the
tests exercise the same 48-byte layout the device uses.

Evidence: cargo test green; the sentinel (dropping the source check) turns
a_record_written_by_the_ui_makes_the_canvas_repaint_the_same_md5 red."
```

---

### Task 3: 三个交权点写入来源

**Files:**
- Modify: `firmware/main/rust/src/page_sync.rs`（`stop_display` → `stop_display_src` + `extern "C" page_sync_stop_display_src`）
- Modify: `firmware/main/rust/include/page_sync.h`（在 `:80` 的 `page_sync_stop_display` 旁加声明）
- Modify: `firmware/main/ui/rawdraw_ui_manager.cc:404`（`SwitchPage` 传 SETTINGS/UI）
- Modify: `firmware/main/rust/src/notify.rs:132`（传 NOTIFICATION）
- Test: `firmware/main/rust/src/page_sync.rs`

**Interfaces:**
- Consumes: Task 1 的 `rf_panel_mark_pending_src`；Task 2 的 `record_trusted`。
- Produces:
  - `pub fn stop_display_src(source: u8)`；`stop_display()` 转调 `RF_PANEL_SRC_UI`
  - `#[unsafe(no_mangle)] pub extern "C" fn page_sync_stop_display_src(source: u8)`
  - C++ 可见：`void page_sync_stop_display_src(uint8_t source);`

- [ ] **Step 1: 写失败测试**

```rust
    /// Task 3 的值不在策略层（source 的判别已由 Task 2 覆盖），而在**机制**：
    /// 交权点真的把「谁拿走玻璃」写进了记录。断言字面值，不做两路一致性断言。
    #[test]
    fn taking_the_panel_records_who_took_it() {
        let _g = shim::host::lock();
        reset_for_test();
        stop_display_src(RF_PANEL_SRC_NOTIFICATION);
        let rec = read_panel_record();
        assert_eq!(rec.source, RF_PANEL_SRC_NOTIFICATION, "the taker is recorded");
        assert_eq!(rec.valid, 1, "the takeover is a claim about the glass");
        assert_eq!(
            rec.displayed_index, -1,
            "no canvas page is on the glass while the notification owns it"
        );
        assert!(
            !record_trusted(&rec),
            "a record the canvas did not paint must not be trusted"
        );
        // 默认路径（UI 切页/设置页）同样只经 source 区分。
        stop_display_src(RF_PANEL_SRC_SETTINGS);
        assert_eq!(read_panel_record().source, RF_PANEL_SRC_SETTINGS);
        assert!(!record_trusted(&read_panel_record()));
    }

    /// 交权不等于撤销挂起：stop_display_src 必须仍然置 SUSPENDED。
    #[test]
    fn taking_the_panel_still_suspends_the_canvas() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        stop_display_src(RF_PANEL_SRC_UI);
        assert!(!is_displaying(), "the UI holds the glass now");
        assert!(!paint_if_changed(), "the canvas must not draw while suspended");
        start();
        assert!(is_displaying(), "the next wake hands it back");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test taking_the_panel`
Expected: 编译失败（`stop_display_src` 未定义）。编译失败即本步的红。

- [ ] **Step 3: 实现 `stop_display_src`**

```rust
/// 把屏幕从画板手里拿走。`source` 说明接手的是谁。
pub fn stop_display_src(source: u8) {
    SUSPENDED.store(true, Ordering::Release);
    DISPLAYING.store(false, Ordering::Release);
    // record 关于玻璃的说法此刻起不再成立。走 pending→commit：只有刷新真正
    // idle 后才提交，中断的刷新不会被记成完成。
    unsafe {
        shim::rf_panel_mark_pending_src(
            EMPTY_PAGE.md5.as_ptr() as *const core::ffi::c_char,
            -1,
            source,
        )
    };
}

pub fn stop_display() {
    stop_display_src(RF_PANEL_SRC_UI);
}
```

`extern "C"` 区加：

```rust
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_stop_display_src(source: u8) {
    stop_display_src(source);
}
```

- [ ] **Step 4: 接线三个调用点**

`rust/include/page_sync.h` 在 `:80` 旁加：

```c
void page_sync_stop_display_src(uint8_t source);
```

`ui/rawdraw_ui_manager.cc:404`（`SwitchPage`）：

```cpp
    // UI 显式切页 = UI 接管屏幕。来源随目标页：设置页单列，便于日志分辨
    // 屏上是哪一类 UI（对 record_trusted 而言两者同效，都让画板的说法失效）。
    page_sync_stop_display_src(
        (page == RawDrawPageId::Settings) ? RF_PANEL_SRC_SETTINGS : RF_PANEL_SRC_UI);
```

`rust/src/notify.rs:132`：

```rust
    page_sync::stop_display_src(RF_PANEL_SRC_NOTIFICATION);
```

`application.cc:862` **不改**：promotion 此刻玻璃上还没有任何 UI 内容，
「谁在玻璃上」尚不可知，`rf_panel_record_invalidate()`（`NONE`）是诚实取值，
且对 `record_trusted` 与 `UI` 同效。在提交信息里记下这个决定。

`RF_PANEL_SRC_SETTINGS` / `RF_PANEL_SRC_UI` 由 `rawdraw_ui_manager.cc` 经
`shim_power.h` 取用（该文件已 include）；`notify.rs` 用 Task 1 定义的 Rust 常量。

- [ ] **Step 5: 跑测试 + 两个门禁**

Run:
```bash
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test
cd /mnt/data/project/youn-ink-fourcolor-firmware/firmware && export PATH="$HOME/.cargo/bin:$PATH" && source ~/data/esp-idf-v6.0/export.sh && IDF_TARGET=esp32s3 idf.py build
```
Expected: 全绿；构建成功。

- [ ] **Step 6: 符号 + 时间链**

Run:
```bash
xtensa-esp32s3-elf-nm build/xiaozhi.elf | grep page_sync_stop_display
ls -l --time-style=full-iso main/rust/src/page_sync.rs main/rust/src/notify.rs main/rust/target/xtensa-esp32s3-none-elf/release/librust_firmware.a build/xiaozhi.bin
```
Expected: `page_sync_stop_display` 与 `page_sync_stop_display_src` 均为 `T`；
时间链递增 `page_sync.rs → librust_firmware.a → xiaozhi.bin`。

- [ ] **Step 7: 提交**

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/rust/include/page_sync.h firmware/main/rust/src/notify.rs firmware/main/ui/rawdraw_ui_manager.cc
git commit -m "feat(panel): mark the source at the three places that take the panel

stop_display() is the single transition where the UI takes the glass away
from the canvas -- 3 callers, versus 7 Clear+RenderAll sites whose RenderAll
early-returns while the canvas is displaying (rawdraw_ui_manager.cc:729). So
that is where the record must learn it no longer describes the glass:
SwitchPage passes SETTINGS or UI, notify passes NOTIFICATION.

ServicePromotion keeps rf_panel_record_invalidate(): at that moment no UI
content is on the glass yet, so 'who painted it' is genuinely unknown and
NONE is the honest value (equivalent to UI for record_trusted).

Evidence: cargo test green; nm shows page_sync_stop_display and
page_sync_stop_display_src as T; mtime chain src -> librust_firmware.a ->
xiaozhi.bin."
```

---

### Task 4: 端到端真机验收与文档同步

**Files:**
- Modify: `docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md`（补验收结果）

**Interfaces:**
- Consumes: Task 1–3 的构建产物 `firmware/build/xiaozhi.bin`。
- Produces: 真机验收记录（哪些现象消失、哪些保留）。

- [ ] **Step 1: 冷构建并核对产物**

Run:
```bash
cd /mnt/data/project/youn-ink-fourcolor-firmware/firmware && rm -rf build main/rust/target && export PATH="$HOME/.cargo/bin:$PATH" && source ~/data/esp-idf-v6.0/export.sh && IDF_TARGET=esp32s3 idf.py build
xtensa-esp32s3-elf-nm build/xiaozhi.elf | grep -E "rf_panel_mark_pending_src|page_sync_stop_display_src"
ls -l --time-style=full-iso build/xiaozhi.bin
```
Expected: 冷构建成功；两符号 `T`；`xiaozhi.bin` mtime 晚于所有源码。
（`firmware/build.sh` 不可用：`config.json` 缺失——用 `idf.py build`。）

- [ ] **Step 2: 关串口后刷写**

```bash
fuser -v /dev/ttyACM0 || true   # 确认没有监视器占用
```
刷写走后台 `/serial` 页读 `firmware/build/xiaozhi.bin`，**只刷应用分区 `0x20000`**。
不要 `cp` 到 `server/data/firmware/`（那是 OTA 频道，实测固件无 OTA 客户端）。

- [ ] **Step 3: 真机验收（三项）**

1. **分叉消除（核心）**：进设置页 → 退出 → 触发一次周期同步。
   期望：日志出现 `show page N/N md5=...`（**不是** `skipping repaint`）。
2. **省电未回归（红线）**：拔掉 USB 让其深睡，观察若干轮唤醒。
   期望：页面未变化时**仍**出现 `glass already shows ..., skipping repaint`。
3. **通知**：等一条通知上屏后关闭。期望：画板取回屏幕并重绘。

把三项的实际串口输出抄进证据记录。

- [ ] **Step 4: 更新证据记录**

在 `docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md` 末尾追加
「§8 修复后验收」：设备跑的是哪个 commit 的产物（`nm` + mtime 证明）、三项各自的实际输出、
以及**未消失的**现象（promotion 首次外壳上屏仍会闪一次，是明确非目标）。

- [ ] **Step 5: 提交**

```bash
git add docs/superpowers/progress/2026-09-29-screen-stale-after-panic.md
git commit -m "docs: panel-source acceptance on hardware

Records which divergence is gone (settings/notification hand-back repaints
instead of skipping), that the deep-sleep skip is intact (the red line), and
that the promotion shell still paints once -- an explicit non-goal."
```

---

## Self-Review

**1. Spec coverage**

| Spec 章节 | 任务 |
|---|---|
| §3.1 ABI 加 `source`，偏移不变 | Task 1 Step 3-4（C `_Static_assert`）+ Step 1 布局测试 + Step 6 哨兵 |
| §3.2 五个 `RF_PANEL_SRC_*` 取值 | Task 1 Step 3（Rust）+ Step 4（C） |
| §3.3 判定规则：只在交权点写 | Task 3 Step 3-4 |
| §3.4 `record_trusted` 收紧 | Task 2 Step 4 |
| §3.5 写入路径 + promotion 保持 invalidate | Task 1 Step 4、Task 3 Step 4 |
| §3.6 七处绘制点**不需改** | 无任务（明确非目标）；Task 3 Step 4 记入提交信息 |
| §3.7 Rust 侧改动 | Task 1 Step 3、Task 2 Step 3-4、Task 3 Step 3-4 |
| §4 测试（布局/行为/哨兵/回归/真机） | Task 1 Step 1/5/6、Task 2 Step 1/5/6、Task 4 Step 3 |
| §5 风险（深睡红线） | Task 4 Step 3 第 2 项 |
| §6 明确不做 | Task 3 Step 4（promotion）、Task 4 Step 4（记录未消失现象） |

无缺口。

**2. Placeholder scan**：无 TBD/TODO；每个代码步骤都给了实际代码。
Task 1 Step 4 的「先读原有 `rf_panel_mark_pending` 实现并照抄其拷贝写法」是**有意的
延续指令**（我未读到该函数体），并已指明具体函数名与要照抄的点，不是省略手法。
Task 1 Step 3 与 Task 2 Step 4 的「用 `cargo build` 让编译器指路」是遍历调用点的
手法，调用点已逐条列出。

**3. Type consistency**

- `PanelRecord`：Task 1 定义（含 `source: u8`、`_pad: [u8; 2]`、`md5()`），
  Task 2 的 `record_trusted(&PanelRecord)` 与 Task 3 的调用一致。
- `stage_panel_record(u32, u8, u8, &[u8], i32)`：Task 2 定义；2 个新测试 + 4 个既有
  调用点实参顺序一致（`magic, valid, source, md5, index`）。
- `rf_panel_mark_pending_src(const char*, int, uint8_t)`：Task 1 的 C 定义与
  `shim.rs` 声明（`*const c_char, c_int, u8`）与 Task 3 的调用一致。
- `stop_display_src(u8)` / `page_sync_stop_display_src(u8)`：Task 3 定义，C++ 经
  `page_sync.h` 调用，一致。
- `RF_PANEL_SRC_*`：C 宏（Task 1 Step 4）与 Rust `pub const`（Task 1 Step 3）
  **各定义一份**，数值 0-4 两处一致；不得再多处写裸数字。
