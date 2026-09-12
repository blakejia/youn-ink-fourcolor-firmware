# 设备只取它要画的那一页 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 一次唤醒至多下载一张位图（要画的那一页）；屏上已经是它时一张都不下；不改哈希、不新增持久化。

**Architecture:** `paint_if_changed()` 本来就是对的做法——先比对 RTC 记录（`glass already shows … skipping repaint`）再用 `ensure_bitmap()` 按需取一页。多余的是 `sync_schedule()` 里那段**预下载全部页**的循环：它在本轮用不到，而且 PSRAM 表不跨睡眠，醒来必然全部重下。改动 = 该循环只保留"同 md5 复用"，不再下载；提交条件从"所有位图都在 RAM"改为"收到并记录了 schedule"（重试保护移交给已经具备该能力的绘制路径）。

**Tech Stack:** Rust（`no_std`，`firmware/main/rust/`）、cargo 主机测试（fake shim）、ESP-IDF v6.0 真机验证。

**Spec:** `docs/superpowers/specs/2026-09-12-device-fetch-only-what-it-paints-design.md`

## Global Constraints

- **不改哈希算法、不在设备上校验内容哈希**（用户明确"相信服务端的 md5"）。
- **不新增持久化**：RTC 记录（`displayed_md5[33]` + index，48 字节）已是唯一的跨睡眠记忆，够用。
- **不改服务端、不改轮换语义**（`current_index` 仍由服务端权威决定）。
- **前提**（写进代码注释）：设备醒着的窗口（8–16 s）远短于页驻留（≥10 min ⇒ 醒来期间不会轮到下一页），所以只取当前页是充分的。
- 提交契约的新定义：**收到并记录了 schedule 即提交**；"真正需要的那张没到手就不重绘"由绘制路径保证（下载失败 ⇒ 不 `mark_pending` ⇒ RTC 记录保持陈旧 ⇒ 下轮重试）。
- Rust 工具链：`export PATH="$HOME/.cargo/bin:$PATH"`；测试 `cd firmware/main/rust && cargo test`（当前基线全绿）。
- 固件构建与刷写只在主会话做（子代理不构建）：`source ~/data/esp-idf-v6.0/export.sh && idf.py build`（改 Rust 后需 `rm -rf build`）。
- 真机验证的串口纪律：一次只开一个读者；打开即复位设备；短窗。凭据/日志不得含 token。

---

### Task 1: `sync_schedule` 不再预下载

**Files:**
- Modify: `firmware/main/rust/src/page_sync.rs`（`sync_schedule`，约 371-467 行；以及约 1203 与 1252 两条用例）
- Test: 同文件 `mod tests`（沿用 `shim::host` 夹具）

**Interfaces:**
- Consumes: `with_table`、`Table{count, pages, server_index, next_wake_s, override_index, schedule_md5, have_schedule_md5, policy}`、`Page{md5, bitmap}`、`ensure_bitmap(idx, md5)`（绘制路径已有的按需下载）、`shim::host::{script_ok, script_get, calls_matching, stage_panel_record, lock}`、`reset_for_test`
- Produces: `sync_schedule` 内**零次**位图请求；`have_schedule_md5` 在收到 schedule 后即为真

- [ ] **Step 1: 改测试（先红）**

把 `sync_downloads_pages_and_commits_the_schedule`（约 1203 行）替换为：

```rust
    fn sync_records_the_pages_but_downloads_nothing() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        // No bitmap responses are scripted on purpose: a sync that fetches any
        // would fail here instead of silently passing.
        sync_once();

        let (count, server_index, committed) =
            with_table(|t| (t.count, t.server_index, t.have_schedule_md5));
        assert_eq!(count, 2);
        assert_eq!(server_index, 0, "no position in the response -> page 0");
        assert!(committed, "the schedule was received, so its md5 is committed");
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: the page that will be painted is fetched by the paint path"
        );
    }

    fn the_paint_path_fetches_exactly_the_target_page() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));

        sync_once();
        assert!(paint_if_changed(), "first paint draws the target page");

        let gets = shim::host::calls_matching("http_get");
        assert_eq!(gets.len(), 2, "schedule + the one page being painted");
        assert!(
            gets.iter().any(|c| c.contains(&md5hex(0xa1))),
            "the fetched bitmap is page 0's, not every page's"
        );
        assert_eq!(page0_byte(), 0xa1);
    }

    fn a_glass_that_already_shows_the_target_page_fetches_nothing() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::stage_panel_record(0x50414E31, 1, md5hex(0xa1).as_bytes(), 0);

        sync_once();
        assert!(!paint_if_changed(), "the glass already shows page 0");
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: nothing to paint, so nothing to download"
        );
    }
```

再把 `unchanged_schedule_is_not_re_downloaded`（约 1228 行）换成有意义的版本——改后 sync 不再取图，原用例里的 `before` 与被比的值**都会是 null**，断言退化为空转：

```rust
    fn an_unchanged_schedule_does_not_re_fetch_the_painted_page() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed(), "the paint path fetched the page");

        // Only now is there something cached to preserve, so this pair of
        // assertions means what it says.
        let before = with_table(|t| t.pages[0].bitmap);
        assert!(!before.is_null(), "the painted page's bitmap is in RAM");
        let gets_before = shim::host::calls_matching("http_get").len();
        sync_once();

        assert_eq!(
            with_table(|t| t.pages[0].bitmap),
            before,
            "the cached bitmap is kept, not re-fetched"
        );
        assert_eq!(
            shim::host::calls_matching("http_get").len() - gets_before,
            1,
            "the second poll hit only the schedule endpoint"
        );
    }
```

并把 `a_missing_bitmap_keeps_the_schedule_uncommitted_for_a_retry`（约 1252 行）替换为：

```rust
    fn a_failed_page_fetch_records_nothing_so_the_next_wake_retries() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_get(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), 500, b"");

        sync_once();
        assert!(
            with_table(|t| t.have_schedule_md5),
            "the sync commits on receipt; the retry is the paint path's job now"
        );
        assert!(!paint_if_changed(), "no bitmap -> nothing painted");

        // The retry mechanism is the RTC record: it is written only once a
        // bitmap has landed. This assertion is what replaces the old "do not
        // commit the schedule while a bitmap is missing" gate, so it is the
        // one this test must pin.
        let rec = read_panel_record();
        assert_ne!(
            &rec.0[8..40],
            &md5hex(0xa1)[..],
            "a failed fetch must not be recorded as displayed"
        );

        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed(), "the retry paints once the bitmap arrives");
        let rec = read_panel_record();
        assert_eq!(&rec.0[8..40], &md5hex(0xa1)[..], "and only then is it recorded");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test page_sync 2>&1 | tail -20`
Expected: FAIL — `sync_records_the_pages_but_downloads_nothing` 断言 `1` 却得到 `3`（仍在预下载）；`the_paint_path_...` 得到 `3` 而不是 `2`。

- [ ] **Step 3: 改 `sync_schedule`**

把下载循环（`// Reuse cached bitmaps with the same md5, download the rest.` 起到 `let all_ready = …` 前）替换为只做复用的版本：

```rust
    // Carry over bitmaps already in RAM for the same md5 (a re-sync within one
    // wake), and download NOTHING. The page that is going to be painted is
    // fetched on demand by `ensure_bitmap`, which is also where a failure is
    // handled: it leaves the RTC record stale so the next wake retries.
    //
    // Premise: the awake window (8-16 s) is far shorter than a page's dwell
    // (>= 10 min), so the rotation cannot move on while we are awake — the
    // other pages' bitmaps would never be used before the sleep clears them.
    let mut carried = 0;
    for i in 0..new_count {
        let carried_slot = with_table(|t| {
            for j in 0..t.count {
                if t.pages[j].is_ram() && t.pages[j].md5 == new_pages[i].md5 {
                    let bmp = t.pages[j].bitmap;
                    t.pages[j].bitmap = core::ptr::null_mut(); // ownership moves
                    return Some(bmp);
                }
            }
            None
        });
        if let Some(bmp) = carried_slot {
            new_pages[i].bitmap = bmp;
            carried += 1;
        }
    }
```

并把提交条件与日志改为：

```rust
    with_table(|t| {
        t.free_pages();
        t.pages = new_pages;
        t.count = new_count;
        t.server_index = new_index.min(new_count.saturating_sub(1));
        t.next_wake_s = new_wake_s;
        t.override_index = None;
        // Commit unconditionally: the schedule was parsed and its md5s are
        // recorded. "Do not commit while the page you need is missing" is now
        // enforced where the page is actually fetched (paint_if_changed ->
        // ensure_bitmap): a failed fetch never reaches rf_panel_mark_pending,
        // so the RTC record stays stale and the next wake retries.
        t.schedule_md5 = new_md5;
        t.have_schedule_md5 = true;
    });

    log_i!("PageSync", "schedule updated: {} pages ({} carried from cache)",
        new_count,
        carried
    );
    true
```

同时删掉不再使用的 `downloaded` 变量与 `all_ready` 绑定（grep 已确认它们只出现在本函数与日志里：`:411/:435/:442/:451/:460-463`），并把快路径上方那句会变陈旧的注释（`:393` 附近，"…even when nothing is re-downloaded"）改写成新契约下的真实含义：热路径仍更新 `server_index`/`next_wake_s`/`override_index`，位图改由绘制路径按需取。

另外两个 setup 夹具（`setup_one_page` 约 1109 行、`setup_two_pages` 约 1114 行）现在会真的取图，改后由 `paint_if_changed()` 取。**全量跑完后**，本文件里凡是失败**或变成空转**的用例逐个适配，并把改了什么写进报告（已知要动的就是上面三条；setup 夹具若需调整也算在内）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test 2>&1 | tail -5`
Expected: PASS（全量，含既有用例）

- [ ] **Step 5: 验证判别力（哨兵）**

临时把预下载加回去（把 Step 3 的循环体改成 `if let None = carried_slot { 下载 }`），确认 `sync_records_the_pages_but_downloads_nothing` 与 `a_glass_that_already_shows_the_target_page_fetches_nothing` 双双失败；还原后重新通过。把两次运行写进报告。

- [ ] **Step 6: Commit**

```bash
git add firmware/main/rust/src/page_sync.rs
git commit -m "perf(page_sync): fetch only the page that will be painted"
```

---

### Task 2: 真机验收（服务端可观察）

**Files:** 无源码改动（验证任务；构建与刷写由主会话完成，或按主会话指示）

**Interfaces:**
- Consumes: Task 1 的固件；运行中的服务端（`http://10.0.0.90:9002`，`cd server && ./start.sh status|restart`）；设备 `/dev/ttyACM0`（115200，`/home/pi/.espressif/python_env/idf6.0_py3.14_env/bin/python`，**一次只开一个读者，打开即复位设备**）

- [ ] **Step 1: 记录基线（改前的每轮请求数，现成的）**

基线不必等刷之前——现有日志里就有：设备现在跑的就是改前固件，`data/server.log` 每个轮询周期都有 `GET /api/pages/bitmap/…`，设备日志有 `schedule updated: N pages, N downloaded`。数一个周期的次数写进报告作为对照（2 页 ⇒ 2 次）。

- [ ] **Step 2: 刷入改后固件并让设备跑两轮唤醒**

用主会话确认过的命令刷 `firmware/build/xiaozhi.bin`（只写 app 分区 0x20000）。设备插电常醒 ⇒ 观察 2–3 个轮询周期即可。

- [ ] **Step 3: 主验收——每轮的位图请求数**

```bash
cd /mnt/data/project/youn-ink-fourcolor-firmware/server
grep -a "GET /api/pages/bitmap" data/server.log | tail -20
```

Expected: 每个轮询周期内 `bitmap` 请求 **≤1**；屏上未变的周期 **0**；换页的周期恰好 **1**。

- [ ] **Step 4: 交叉核对设备侧**

短窗读串口一次（60–90 s）：出现 `show page i/N md5="…"` 的周期应与 Step 3 里"下载 1 张"的周期对应；其余周期只有 `GET /api/pages/schedule`。（重绘节奏 10–20 分钟，短窗可能正好错过 ⇒ 如实写"未观察到"并说明什么能证明，不得推断。）

- [ ] **Step 5: 不回归**

确认这些路径仍正常：服务端 0 页（空页提示）、服务端不可达（屏保持原样、按退避重试）、手动翻页（`override_index`）、通知弹层后交还屏幕。

- [ ] **Step 6: 报告**

每步写命令与输出；未观察到的写"未观察到"。

---

## Self-Review

**Spec coverage:** 判定先于下载（Task 1 Step 3 的复用-only 循环 + 既有 `paint_if_changed` 的 RTC 比对 ✓）、只取当前页（`ensure_bitmap` ✓，Task 1 的 `the_paint_path_fetches_exactly_the_target_page` 断言它 ✓）、提交契约新定义（Step 3 的无条件提交 + 注释说明重试去哪里 ✓）、不新增持久化（未引入任何 ✓）、不改哈希（未触及 ✓）、空页/不可达/手动翻页不回归（Task 2 Step 5 ✓）、验收可服务端观察（Task 2 Step 3 ✓ 主验收）。

**Placeholder scan:** 无 TBD/TODO。所有代码步骤给出可执行代码；夹具 API（`script_ok`/`script_get`/`calls_matching`/`stage_panel_record`/`lock`/`reset_for_test`/`page0_byte`/`schedule_json`/`bitmap_body`/`md5hex`）都取自本文件既有用例 ✓。

**Type consistency:** `sync_schedule` 仍返回 `bool` ✓；`with_table` 的闭包返回类型不变 ✓；`ensure_bitmap(idx, &[u8; 32]) -> *mut u8` 未改签名 ✓；测试替换的两条用例仍用同一批夹具名 ✓。

**一处诚实标注：** Task 1 Step 3 里把提交改为无条件，**删掉了一个原有保护**（"缺位图就不提交"）。它的保护意图被绘制路径接管（失败 ⇒ 不记录 ⇒ 下轮重试），这条等价性由 `a_failed_page_fetch_is_retried_on_the_next_wake` 用例钉住 ✓；如果评审认为该等价性不成立，就必须改回带条件的提交并在 spec 里重写契约。
