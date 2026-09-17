# NOTE4C 固件省电（唤醒期为主）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不影响功能的前提下降低 NOTE4C 唤醒期能耗：刷屏期间无射频、唤醒期射频省电模式、省掉空的通知查询、开 DFS、去掉 LED 常循环与多余的音频电源脚拉高，并加入可对比的省电计数器。

**Architecture:** 保持「一次唤醒跑一次 `sync_once` → `paint_if_changed` → `ServicePowerPolicy`」的既有形状不变，只改时序里的三处：① 把已在深睡路径里存在的射频拆除（`rf_rails_audio(0)` / `esp_wifi_disconnect()` / `esp_wifi_stop()`）**提前到刷屏之前**，但必须先让 Rust 侧把要画的那页**预取到 RAM**（因为位图原本是在 paint 路径内部下载的）② 连上后切 `WIFI_PS_MAX_MODEM` ③ 服务端在 `schedule` 响应里多给一个 `notify_pending`，设备据它决定要不要发 `/api/notifications/next`。CPU 侧只加配置（`CONFIG_PM_ENABLE` + `esp_pm_configure`，**不开 light sleep**），驱动自带的 PM 锁足以保住 SPI/I2S 时序。

**Tech Stack:** ESP-IDF v6.0（tag `662a3be`，target `esp32s3`）、C++ 主程序 + Rust 静态库（`main/rust/`，`crate-type=["staticlib","rlib"]` ⇒ 可 `cargo test`）、FastAPI + pytest（`server/`）。

**Spec:** `docs/superpowers/specs/2026-09-17-firmware-power-design.md`（commit `c7a87bb`）

## Global Constraints

- **红线（每个任务都隐含包含）**：
  1. 网页版「串口 / 固件刷写」工具必须照常可用 ⇒ 不开 light sleep、不关 USB-Serial-JTAG / UART0 控制台、日志保持 `INFO`。
  2. 新内容的可见延迟不得变差 ⇒ 不拉长轮询周期、不改变 `seconds_until_next_page` 语义。
  3. 按键 / 配对 / 通知确认手感不变 ⇒ 唤醒判定、去抖、确认窗口时序不动。
  4. 首屏与页面切换的画质不降 ⇒ 不做降质刷新。
- **不做**：EPD 轨空闲切断（已是现状）、light sleep、事件驱动唤醒重排、刷新路径重构、`/api/photos*` 死代码清理。
- 构建纪律：`rm -rf build && idf.py build`（**最终构建必须在主会话执行**，子代理构建会产生分歧二进制）；有 `undefined reference` 即失败。
- 推送/提交：每个任务结束提交一次；**共享分支推送要先问用户**。
- 无电流表 ⇒ 只报「时长记账」（射频开秒数 / 清醒毫秒 / 每次唤醒请求数），**不得**声称实测 mA。

---

## File Structure

| 文件 | 责任 | 本计划中的角色 |
|---|---|---|
| `firmware/main/rust/src/page_sync.rs` | 排期同步与刷屏（纯逻辑 + FFI 导出） | 新增「预取」FFI；`ParsedSchedule` 增 `notify_pending` |
| `firmware/main/rust/src/shim.rs` | Rust → C++ 的 `extern "C"` 声明 + **host 测试桩** | 新增计数器 FFI 的声明与桩 |
| `firmware/main/rust/shim.cpp` / `include/shim_power.h` | C++ 侧实现 RF_* FFI | 新增计数器实现 |
| `firmware/main/application.cc` | 唤醒循环、电源策略、生命周期 | 改 `RunPowerCycle` 时序；切射频省电模式；计数器自增点 |
| `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc/.h` | LED 与电源轨 | `PowerLedTask` 事件驱动化 |
| `firmware/main/boards/zectrix-s3-epaper-4.2/i2c_power_hook.cc` | I2C 传输前的电源脚钩子 | 不再每次重复拉高 |
| `firmware/main/main.cc` | `app_main` | 放 `esp_pm_configure` |
| `firmware/sdkconfig*` | 构建配置 | `CONFIG_PM_ENABLE=y` |
| `server/youn_server/notify_store.py` | 通知队列 | 新增 `has_pending()` |
| `server/youn_server/app.py` | 路由 | `schedule` 响应加 `notify_pending`；接收计数器 |

---

## Task 1: 计数器与上报（在已有 schedule GET 上搭车，零新增往返）

**Files:**
- Modify: `firmware/main/rust/src/shim.rs`（extern 块 + host 桩）
- Modify: `firmware/main/rust/shim.cpp`、`firmware/main/rust/include/shim_power.h`
- Modify: `firmware/main/application.cc`（自增点）
- Modify: `firmware/main/rust/src/page_sync.rs`（把计数挂在 URL 查询串上）
- Test: `firmware/main/rust/src/page_sync.rs`（`#[cfg(test)] mod tests`，与既有用例同风格）
- Modify: `server/youn_server/app.py`、`server/youn_server/devices.py`
- Test: `server/tests/test_power_counters.py`

**Interfaces:**
- Consumes: 既有 `shim::rf_build_endpoint(path, url, cap)`、`shim::rf_http_get(url, token, buf, len, timeout)`（`page_sync.rs:158-180` 的用法）
- Produces:
  - C++: `extern "C" void rf_power_counters(uint32_t* wakes, uint32_t* awake_ms, uint32_t* radio_ms, uint32_t* http_gets, uint32_t* refresh_ms)`，声明在 `shim_power.h`
  - Rust: `shim::rf_power_counters(...)` + host 桩；`pub fn counters_query(buf: &mut [u8])` 生成 `?w=…&a=…&r=…&g=…&f=…`

- [ ] **Step 1: 写失败测试（Rust 侧，host 上可跑）**

在 `page_sync.rs` 的 `mod tests` 里加：

```rust
    #[test]
    fn schedule_url_carries_power_counters() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(7, 1234, 567, 2, 890);   // 需在 host 桩里新增
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");   // 既有助手（:1233 已在用）
        assert!(gets.iter().any(|c| c.contains("/api/pages/schedule?")),
                "schedule GET without a query string: {gets:?}");
        for kv in ["w=7", "a=1234", "r=567", "g=2", "f=890"] {
            assert!(gets.iter().any(|c| c.contains(kv)), "{kv} missing from {gets:?}");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && cargo test schedule_url_carries_power_counters`
Expected: FAIL（`set_counters` / `last_url` 不存在）

- [ ] **Step 3: 实现**

a) `shim.rs` 的 `extern "C"` 块加：

```rust
    pub fn rf_power_counters(wakes: *mut u32, awake_ms: *mut u32, radio_ms: *mut u32,
                             http_gets: *mut u32, refresh_ms: *mut u32);
```

b) `shim.rs` 的 `#[cfg(test)]` host 桩区（与既有 `rf_rails_audio` 桩 `:319`、`calls_matching` 桩 `:338` 同风格）加一个 `COUNT` 静态量与 `set_counters(...)` 辅助，并让 `rf_power_counters` 桩读出它。
   若 `calls_matching` 返回的字符串**不含 URL**（只需看一眼 `:338` 的实现即可判定），就在同一桩区补一个同风格的 `last_url()`，把上面的 `gets` 换成它 —— 两处改动都在同一个桩区，不要另起机制。

c) `shim.cpp` 实现（计数器本体放这里，Rust 只读）：

```cpp
static uint32_t g_wakes, g_awake_ms, g_radio_ms, g_http_gets, g_refresh_ms;
extern "C" void rf_power_counters(uint32_t* w, uint32_t* a, uint32_t* r, uint32_t* g, uint32_t* f) {
    if (w) *w = g_wakes; if (a) *a = g_awake_ms; if (r) *r = g_radio_ms;
    if (g) *g = g_http_gets; if (f) *f = g_refresh_ms;
}
extern "C" void rf_power_count_wake(void)   { g_wakes++; }
extern "C" void rf_power_add_awake_ms(uint32_t ms) { g_awake_ms += ms; }
```

d) `shim_power.h` 补上这两个 `rf_power_count_*` 的声明（`rf_power_counters` 只被 Rust 调用，无需给 C++ 用）。

e) `page_sync.rs`：`fetch_schedule` 里把查询串拼上（`path` 的 `CBuf::<64>` 容量需相应放大到 `CBuf::<160>`，并用现有 `core::fmt::Write` 写法）：

```rust
    let mut path = CBuf::<160>::new();
    path.push("/api/pages/schedule");
    let mut c = [0u32; 5];
    unsafe { shim::rf_power_counters(&mut c[0], &mut c[1], &mut c[2], &mut c[3], &mut c[4]) };
    let _ = write!(path, "?w={}&a={}&r={}&g={}&f={}", c[0], c[1], c[2], c[3], c[4]);
```

f) 服务端 `app.py` 的 `get_schedule` 里读 `request.query_params`，把五个值存进该设备的计数快照（`devices.py` 加 `power_counters` TEXT 列 + `set_power_counters(device_id, json_text)`，`upsert` 关键字是 `ip_address`，勿写错），并在响应里回带 `"power": {...}`。

g) `application.cc` 的 `RunPowerCycle` 首尾计数器：

```cpp
    const int64_t aw0 = esp_timer_get_time() / 1000;
    ...
    rf_power_add_awake_ms((uint32_t)(esp_timer_get_time() / 1000 - aw0));
```

并在 `rf_rails_audio(0); esp_wifi_disconnect(); esp_wifi_stop();` 之前记录 `radio_ms`（用同一 `esp_timer_get_time()` 差值口径）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd firmware/main/rust && cargo test`
Expected: PASS（既有 155+ 条全绿）

Run: `cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/test_power_counters.py -q`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/rust/src/shim.rs firmware/main/rust/shim.cpp firmware/main/rust/include/shim_power.h firmware/main/application.cc server/youn_server/app.py server/youn_server/devices.py server/tests/test_power_counters.py
git commit -m "feat(power): 设备侧省电计数器（唤醒/清醒毫秒/射频开秒数/请求数/刷屏毫秒）搭车 schedule GET 上报"
```

> **失败路径**：若 `cargo test` 在本机跑不起来（工具链缺失），如实报告「未观察到」，**不要**跳过测试直接改实现。

---

## Task 2: 刷屏前断射频（**必须先预取，否则会打断页面更新**）

**Files:**
- Modify: `firmware/main/rust/src/page_sync.rs`（新增 FFI `page_sync_prepare_paint`）
- Modify: `firmware/main/application.cc:926-965`（`RunPowerCycle`）
- Test: `firmware/main/rust/src/page_sync.rs`

**Interfaces:**
- Consumes: Task 1 的 `shim::rf_power_counters`；既有 `ensure_bitmap(idx, md5)`（`page_sync.rs:475`）、`target_index()`（`:465`）、`paint_if_changed()`（`:536`）
- Produces: `#[unsafe(no_mangle)] pub extern "C" fn page_sync_prepare_paint() -> bool` —— 只做「把要画的那页取到 RAM」，**不碰面板**；返回 `true` 表示「这一页已就绪或压根不需要下载」

**为什么必须先预取**：`page_sync.rs:343` 与 `:446-449` 写明「位图不在 sync 里下载，由 paint 路径按需取」；`:1233` 的用例断言一次正常重画有 **2 个 GET**（schedule + 那一页）。若在 `paint_if_changed()` 之前关掉射频，这一页就永远取不到 —— 直接踩红线 2。

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn prepare_paint_fetches_the_page_without_touching_the_panel() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        let before = shim::host::refreshes();
        assert!(prepare_paint(), "the bitmap must be resident");
        assert_eq!(shim::host::refreshes(), before, "prepare must not refresh the panel");
        assert!(paint_if_changed(), "then the paint still happens");
    }

    #[test]
    fn prepare_paint_is_true_when_nothing_needs_downloading() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        prepare_paint();
        paint_if_changed();                       // glass now shows page 0
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        sync_once();
        assert!(prepare_paint(), "same page on the glass -> no download needed");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && cargo test prepare_paint`
Expected: FAIL（`prepare_paint` 未定义）

- [ ] **Step 3: 实现**

a) Rust 侧（`paint_if_changed` 上方）：

```rust
/// Fetch the page that is about to be painted, without touching the panel.
///
/// The power wiring turns the radio off before a full refresh (15-25 s of
/// waveform), so the download must happen first — `paint_if_changed` would
/// otherwise try to fetch it with the radio down and the page would never land.
/// Returns false only when the page is missing and could not be fetched.
pub fn prepare_paint() -> bool {
    let Some(idx) = target_index() else { return true };   // empty hint: nothing to fetch
    let md5 = with_table(|t| t.pages[idx].md5);
    if !with_table(|t| t.pages[idx].bitmap.is_null()) { return true; }
    !ensure_bitmap(idx, &md5).is_null()
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_prepare_paint() -> bool {
    prepare_paint()
}
```

b) `application.cc` 的 `RunPowerCycle`，把 `page_sync_paint_if_changed();` 一行改成三行（保持既有注释块完整）：

```cpp
    // Fetch the page BEFORE the radio comes down: `paint_if_changed` downloads
    // the target page on demand (see page_sync.rs), so tearing the radio down
    // first would strand the update. Only cut the radio once it is resident —
    // on a failed fetch we keep it, so the existing retry semantics are intact.
    const bool paint_ready = page_sync_prepare_paint();
    if (paint_ready) {
        StopRadioForPaint();
    }
    page_sync_paint_if_changed();
```

c) 新增私有方法 `StopRadioForPaint()`（放在 `RunPowerCycle` 上方），**它就是今天深睡路径里那三行的提取**（`application.cc:1043-1045` 逐字）：

```cpp
void Application::StopRadioForPaint() {
    wifi_connected_.store(false, std::memory_order_release);
    rf_rails_audio(0);          // amp off before audio power off (silent)
    esp_wifi_disconnect();
    esp_wifi_stop();
}
```

并在两条深睡路径里把原来的三行替换为 `StopRadioForPaint();`（重复调用是安全的：两次 `esp_wifi_stop()` 返回 `ESP_ERR_WIFI_NOT_STARTED`，现有代码是裸调用、无 `ESP_ERROR_CHECK`）。

d) **守卫用已核实的 quiet 标志**（已查：`quiet_boot_` 原子量在 `application.h:94`，访问器 `IsQuietBoot()` 在 `application.h:35`；`RunPowerCycle()` 的唯一调用点在 `application.cc:920`，`quiet_boot_` 的分支出现在 `:303/:323/:339/:442/:592/:606`）：

```cpp
    const bool paint_ready = page_sync_prepare_paint();
    if (paint_ready && IsQuietBoot()) {
        StopRadioForPaint();
    }
    page_sync_paint_if_changed();
```

   加 `IsQuietBoot()` 守卫是**保守方向**：duty-cycle 唤醒（本计划的省电目标）一定会走这条路并受益；若交互会话也会调到这里，它只是不做提前断射频（行为与今天一致，不回归）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd firmware/main/rust && cargo test`
Expected: PASS（含 `:1233` 那条「2 个 GET」的既有断言仍绿 —— 预取只把下载提前，GET 次数不变）

- [ ] **Step 5: 提交**

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/application.cc
git commit -m "feat(power): 刷屏前先预取位图再断射频 —— 全刷窗口内彻底无射频"
```

---

## Task 3: 唤醒期射频省电模式（`WIFI_PS_MAX_MODEM`）

**Files:**
- Modify: `firmware/main/application.cc`（Wi-Fi 连上回调，`:355-381` 附近）
- Test: 无自动化（见 Step 4 的失败路径）

**Interfaces:**
- Consumes: `Board::GetInstance().SetPowerSaveLevel(PowerSaveLevel::LOW_POWER)`（`boards/common/board.h:26-30,89` → `zectrix-s3-epaper-4.2.cc:199-211` → `wifi_manager.cc:370-376` → `wifi_station.cc:957-975`，全链路已存在、目前零调用者）
- Produces: 无（纯调用）

- [ ] **Step 1: 加调用**

在 `application.cc` 的 Wi-Fi 连上回调里（连上后、任何 HTTP 之前）加：

```cpp
    // Duty-cycled device: nothing interactive is served while this wake is in
    // flight, so the radio may sleep between beacons. LATENCY: each request pays
    // tens-to-hundreds of ms more; measured against the "no visible-latency
    // regression" red line via the awake-ms counter (Task 1).
    Board::GetInstance().SetPowerSaveLevel(PowerSaveLevel::LOW_POWER);
```

- [ ] **Step 2: 编译**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && rm -rf build && idf.py build`
Expected: 退出码 0、无 `undefined reference`

- [ ] **Step 3: 计数器对比**

真机跑 ≥1 小时的正常轮询，取 Task 1 的 `awake_ms` 与 `radio_ms`：与开启前同长度基线对比。**期望**：`radio_ms` 下降；若 `awake_ms` 增长幅度把收益吃掉，则回滚本任务。

- [ ] **Step 4: 提交**

```bash
git add firmware/main/application.cc
git commit -m "feat(power): 唤醒期把 Wi-Fi 切到 MAX_MODEM（用既有零调用的 SetPowerSaveLevel 链路）"
```

> **失败路径**：真机测不了（无设备在旁）时，如实登记为「未验证」，**不要**用编译通过冒充有效。

---

## Task 4: 通知查询条件化（服务端加字段 + 设备跳过空查询）

**Files:**
- Modify: `server/youn_server/notify_store.py`（新增 `has_pending`）
- Modify: `server/youn_server/app.py:506-540`（schedule 响应）
- Test: `server/tests/test_notify_pending_field.py`
- Modify: `firmware/main/rust/src/page_sync.rs`（`ParsedSchedule` 增字段 + 解析）
- Modify: `firmware/main/rust/src/notify.rs` / `application.cc:954`（按字段决定是否调用）

**Interfaces:**
- Consumes: `NotifyStore.next_for(device_id)`（`notify_store.py:110`）、`parse_schedule`（`page_sync.rs:296`）、`notify_request_next()`（`notify.rs:481`）
- Produces:
  - 服务端：`NotifyStore.has_pending(device_id: str) -> bool`；schedule 响应新增 `"notify_pending": bool`
  - 设备：`ParsedSchedule.notify_pending: bool`；FFI `page_sync_notify_pending() -> bool`

- [ ] **Step 1: 写失败测试（服务端）**

```python
def test_schedule_reports_pending_notification(client, device_token):
    """队列非空 ⇒ notify_pending=true；空 ⇒ false。"""
    body = client.get("/api/pages/schedule", headers=device_token).json()
    assert body["notify_pending"] is False
    enqueue_one_notification(device_id)
    body = client.get("/api/pages/schedule", headers=device_token).json()
    assert body["notify_pending"] is True
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/test_notify_pending_field.py -q`
Expected: FAIL（`KeyError: 'notify_pending'`）

- [ ] **Step 3: 实现服务端**

`notify_store.py`（紧邻 `next_for`，同样只读不标记）：

```python
    def has_pending(self, device_id: str) -> bool:
        """True when `next_for` would return something — without marking it shown."""
        now = time.time()
        with self._lock:
            self._sweep(now)
            return any(
                n.device_id == device_id and n.status == "pending" and not n.is_expired(now)
                for n in self._items.values()
            )
```

`app.py` 的 `get_schedule` 返回体加一行（与既有字段并列）：

```python
            "notify_pending": notify_store.has_pending(dev.device_id),
```

（模块名已查实：`app.py:58` 是 `from . import notify_store as ns` ⇒ 写成 `ns.has_pending(dev.device_id)`。）

- [ ] **Step 4: 跑测试确认通过 → 提交**

Run: `cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/ -q`
Expected: PASS（既有全绿 + 新文件）

```bash
git add server/youn_server/notify_store.py server/youn_server/app.py server/tests/test_notify_pending_field.py
git commit -m "feat(power): schedule 响应携带 notify_pending（用于省掉空的通知查询）"
```

- [ ] **Step 5: 写设备侧失败测试**

先照 `schedule_json_with_md5`（`page_sync.rs:1057`）的样子加一个同风格助手 `schedule_json_with_notify(sched_tag: u8, entries: &[(u8, u32)], pending: Option<bool>) -> Vec<u8>`（`None` ⇒ 不出该字段），再写：

```rust
    #[test]
    fn schedule_reports_whether_a_notification_is_waiting() {
        let _g = shim::host::lock();
        reset_for_test();
        assert!(parse_schedule(&schedule_json_with_notify(0x11, &[], Some(true)))
            .unwrap().notify_pending);
        assert!(!parse_schedule(&schedule_json_with_notify(0x11, &[], Some(false)))
            .unwrap().notify_pending);
        assert!(!parse_schedule(&schedule_json_with_notify(0x11, &[], None))
            .unwrap().notify_pending, "absent field defaults to false -> no wasted request");
    }
```

- [ ] **Step 6: 跑测试确认失败**

Run: `cd firmware/main/rust && cargo test notify_pending`
Expected: FAIL（结构体无该字段）

- [ ] **Step 7: 实现设备侧**

`ParsedSchedule` 加 `pub notify_pending: bool`；`parse_schedule` 里按既有写法：

```rust
    let notify_pending = json::member(body, 0, "notify_pending")
        .and_then(|at| json::bool_value(body, at))
        .unwrap_or(false);
```

并加 FFI `page_sync_notify_pending() -> bool`（读表，与 `page_sync_sync_ok` 同风格）。

`application.cc` 的 `notify_request_next();` 改为：

```cpp
    // Only when the server says something is waiting: an empty poll is a whole
    // radio round-trip for nothing. The field is absent-safe (absent = false).
    if (page_sync_notify_pending()) {
        notify_request_next();
    }
```

- [ ] **Step 8: 跑测试 → 编译 → 提交**

Run: `cd firmware/main/rust && cargo test`
Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && rm -rf build && idf.py build`

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/rust/src/notify.rs firmware/main/application.cc
git commit -m "feat(power): 通知队列为空时跳过 /api/notifications/next（少一次射频往返）"
```

> **兼容性**：设备可能先于服务端升级 ⇒ 字段缺失必须默认 `false`（已按此实现）；老设备忽略新字段，行为不变。

---

## Task 5: 开 DFS（`CONFIG_PM_ENABLE` + `esp_pm_configure`，**不开 light sleep**）

**Files:**
- Modify: `firmware/sdkconfig.defaults`、`firmware/sdkconfig`
- Modify: `firmware/main/main.cc:37-84`（`app_main`，NVS 之后、`Application::Initialize` 之前）

**Interfaces:**
- Consumes: IDF `components/esp_pm/include/esp_pm.h:22-26`（`esp_pm_config_t{max_freq_mhz, min_freq_mhz, light_sleep_enable}`）
- Produces: 无对外接口

- [ ] **Step 1: 改配置**

`firmware/sdkconfig`：`CONFIG_PM_ENABLE=y`（`:2627` 由 `# CONFIG_PM_ENABLE is not set` 改）。定位时**以 `grep -n` 得到的实际行号为准**，不要硬编行号。保持 `CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ=240` 不变（`:2749-2751`）。

- [ ] **Step 2: 加运行期配置**

`main.cc` 的 `app_main` 内、`Application::Initialize(...)` 之前：

```cpp
    // DFS only: the USB-Serial-JTAG console must survive idle (project red
    // line), and light sleep would tear that pad down on esp32s3. The SPI
    // master (EPD) and I2S drivers hold their own PM locks, so their timing is
    // unaffected — no locks are added here on purpose.
    esp_pm_config_t pm = {};
    pm.max_freq_mhz = 240;
    pm.min_freq_mhz = 80;
    pm.light_sleep_enable = false;
    ESP_ERROR_CHECK(esp_pm_configure(&pm));
```

（补 `#include "esp_pm.h"`。）

- [ ] **Step 3: 构建**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && rm -rf build && idf.py build`
Expected: 退出码 0；若 `CONFIG_PM_ENABLE` 与 PSRAM/DFS 冲突，**构建日志会给出确切的冲突项** —— 按日志原文处理并记录。

- [ ] **Step 4: 真机长跑（本任务的核心验收）**

刷入后跑 ≥2 小时正常轮询，逐条确认：音频无爆音、EPD 无花屏、串口工具照常可用、页面照常更新、`rf_power_counters` 的 `awake_ms` 不恶化。

- [ ] **Step 5: 提交**

```bash
git add firmware/sdkconfig firmware/sdkconfig.defaults firmware/main/main.cc
git commit -m "feat(power): 开启 DFS（240/80MHz，light_sleep_enable=false 保住 USB 控制台）"
```

> **失败路径**：这是本计划风险最高的一项。任何一条红线被破坏 ⇒ 立即 `git revert` 本提交并如实报告，**不要**带着花屏/爆音继续。

---

## Task 6: LED 事件驱动（保留用户可见语义）

**Files:**
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc:8-66`（`PowerLedTask`）
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.h:8-15`（状态原子）

**Interfaces:**
- Consumes: 既有状态原子 `led_override_enabled_` / `led_override_blink_` / `led_override_phase_` / `led_activity_pulses_`（`board_power_bsp.h:8-15`）、`ChargeStatus::Tick/Get`、状态设置函数（`board_power_bsp.cc:137-145`）
- Produces: 无对外接口变化（GPIO3 的观感不变）

**当前语义（必须 1:1 保留）**：`full` ⇒ 常亮(0)；`charging` ⇒ 200ms 灭 / 2800ms 亮；其余 ⇒ 常灭(1)；有活动脉冲且未充电 ⇒ 120ms 亮 + 180ms 灭，每次消耗一个脉冲；override 分支 ⇒ 闪烁或常亮。全部写 GPIO3 且用 `gpio_hold_*` 包住。

- [ ] **Step 1: 抽纯函数（本仓**没有** C++ host 测试入口：`main/test/` 不存在，`CMakeLists.txt` 里无 gtest/unity ⇒ 这一步**不写 C++ 单测**，改以 Step 3 的真机三态对照为证据；Rust 侧的 TDD 不受影响）

把 `PowerLedTask` 里的判定抽成**纯函数**（同文件内 `static`），供 Step 2 调用：

```cpp
// 输入：override 开关/blink、充电快照、活动脉冲数；输出：这次要做什么
struct LedAction { bool level; uint32_t on_ms; uint32_t off_ms; bool consume_pulse; };
static LedAction led_decide(bool ovr, bool ovr_blink, const ChargeStatus::Snapshot& s,
                            uint32_t pulses);
```

该函数必须能一一映出下表（Step 3 真机逐态对照时按此表验收）：

| 输入 | 期望 |
|---|---|
| `ovr && ovr_blink` | 500ms 翻转一次，电平取反 |
| `ovr && !ovr_blink` | 常亮(1) |
| `!ovr && !charging && !full && pulses>0` | 亮 120ms / 灭 180ms，且 `consume_pulse = true` |
| `!ovr && full` | 常亮(0) |
| `!ovr && charging` | 灭 200ms / 亮 2800ms |
| 其余 | 常灭(1) |

- [ ] **Step 2: 改造任务**

`PowerLedTask` 改为：**等待事件**（充电状态变化回调 / 活动脉冲计数变化 / override 变化），每次唤醒后按 `led_decide` 应用一次电平与 `gpio_hold`，然后用 `ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(...))` 阻塞（充电中保留 2800ms 节奏的定时等待）。**不得**保留无条件 500ms 空转。

- [ ] **Step 3: 构建 + 真机对照**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && rm -rf build && idf.py build`
真机逐态对照（充电中 / 充满 / 放电空闲 / 有活动 / override 闪烁）与改前观感是否一致。**不一致就不合入。**

- [ ] **Step 4: 提交**

```bash
git add firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.cc firmware/main/boards/zectrix-s3-epaper-4.2/board_power_bsp.h
git commit -m "perf(power): LED 由 500ms 常循环改为事件驱动（保持状态灯观感）"
```

---

## Task 7: 音频电源脚不再每次 I2C 重复拉高

**Files:**
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/i2c_power_hook.cc`（全文 8 行）
- Modify: `firmware/main/boards/common/i2c_device.cc:47,57,79,89`（4 个调用点保持不变）

**Interfaces:**
- Consumes: `Audio_PWR_PIN` / `AUDIO_PWR_FORCE_LEVEL`（`config.h`）、`SetAudioRail`（`zectrix-s3-epaper-4.2.cc:266-277`）
- Produces: 无对外接口变化

**判据**：该钩子的目的是「I2C 访问音频 codec 前保证它上电」，但它在**每次读写**都 `gpio_hold_dis/set_level(high)/gpio_hold_en` —— 每次都重新驱动引脚。改为幂等：只有当引脚当前不是目标电平时才真正操作。

- [ ] **Step 1: 实现幂等化**

```cpp
extern "C" void BoardI2cForcePowerOn() {
    const gpio_num_t pin = static_cast<gpio_num_t>(Audio_PWR_PIN);
    // Idempotent: this runs on EVERY register read/write, and re-driving the
    // same level each time is pure pin churn. Only act on an actual change.
    if (gpio_get_level(pin) == AUDIO_PWR_FORCE_LEVEL) {
        return;
    }
    gpio_hold_dis(pin);
    gpio_set_level(pin, AUDIO_PWR_FORCE_LEVEL);
    gpio_hold_en(pin);
}
```

- [ ] **Step 2: 先读代码确认没有「必须每次重驱动」的理由**

检查 `gpio_get_level` 在该引脚被 `gpio_hold_en` 后是否仍可用（hold 会冻结输出驱动），以及是否有别处把它拉低（`SetAudioRail` 的调用点）。若发现必须每次重驱动 ⇒ 停下如实报告，不硬改。

- [ ] **Step 3: 构建 + 真机验证音频**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && rm -rf build && idf.py build`
真机跑一次需要音频的路径（通知提示音/语音），确认无爆音、无 codec 掉电。

- [ ] **Step 4: 提交**

```bash
git add firmware/main/boards/zectrix-s3-epaper-4.2/i2c_power_hook.cc
git commit -m "perf(power): I2C 电源钩子幂等化，不再每次读写重驱动音频电源脚"
```

---

## 收尾：整体验收（不单独建任务，由执行者按此清单汇总）

- [ ] 服务器 pytest 全绿（`cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/ -q`）
- [ ] 固件构建绿（`rm -rf build && idf.py build`，主会话执行，无 `undefined reference`）
- [ ] 真机四条红线逐条过：网页串口工具 / 网页刷写工具 / 内容延迟 / 按键+配对+通知确认 / 画质
- [ ] 时长记账对比表（射频开秒数、清醒占空比、每次唤醒请求数，改前 vs 改后，同设备同页面集）
- [ ] 报告中**明确区分**「有计数器证据」与「仅真机观察」，不出现 mA 数字
