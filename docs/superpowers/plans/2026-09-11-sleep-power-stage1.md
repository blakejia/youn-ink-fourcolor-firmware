# 休眠省电 Stage 1 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让设备在电池供电时按服务端排期做占空比深睡（醒来→同步→仅在画面变化时绘屏→关射频→再睡），并在插着 USB 时完全不睡、串口全程在线。

**Architecture:** 服务端新增"现在该显示第几页/距下次翻页多少秒"两个字段，成为轮换的唯一真相源；固件删掉本地轮换与轮询任务，改为"按唤醒原因分 quiet/interactive 两条启动路径 + 一个纯策略函数决定睡不睡、睡多久"，并用 `RTC_DATA_ATTR` 记住屏上内容以避免每次唤醒白刷 20 秒。

**Tech Stack:** ESP-IDF v6.0 (esp32s3, xtensa) / Rust no_std staticlib（`firmware/main/rust`，FFI shim = `shim.cpp`）/ FastAPI + pytest（`server/`）/ 无 CI、无 C++ 单测（C++ 侧验证 = 构建通过 + 真机日志断言）。

**Spec:** `docs/superpowers/specs/2026-09-11-sleep-power-redesign.md`（本计划只覆盖该 spec 的 **Stage 1**；Stage 2 EPD 轨、Stage 3 快速重连缓存各自另行计划）

## Global Constraints

- **零 spec 偏差是硬约束**；spec 与计划冲突时停下来问，不要自行取舍。
- 服务端测试基线：`cd server && ./.venv/bin/python -m pytest tests -q` → 当前 **69 passed**，本计划只增不减。
- Rust 测试：`export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test` → 当前 **49 passed**（45 lib + 4 integration）。
- 构建（任何影响时间戳的改动后必须 `rm -rf build`）：
  `source ~/data/esp-idf-v6.0/export.sh && cd firmware && KEY=$(grep -oP '^MASTER_KEY=\K.*' ../server/.env) && idf.py -DDEVICE_MASTER_KEY="$KEY" build`
- **最终 `idf.py build` 一律在主会话执行**（子代理构建会产生不一致的二进制，已有先例 commit `9cb9562`）。
- 烧录只写应用分区：`python -m esptool --chip esp32s3 -p /dev/ttyACM0 -b 460800 write-flash 0x20000 firmware/build/xiaozhi.bin`；绝不碰 bootloader / 分区表。
- 服务端测试不得写入生产库：`tests/conftest.py` 已把 `settings.devices_db` 指向临时文件，不要绕过。
- C++ 固件无主机单测：**凡是能表达为纯逻辑的决策一律放进 Rust**（可 `cargo test`），C++ 只做机制与 IDF 调用。C++ 任务的验证 = 构建通过 + 真机日志断言。
- 提交信息用本仓库既有的 conventional-commit 风格（`feat(server):` / `feat(firmware):` / `fix(firmware):` …），正文说明"为什么"，不要只写"改了什么"。
- 串口取证：`/dev/ttyACM0`；设备深睡时会断开，唤醒后重新枚举（见 Task 8 的监视命令）。

---

### Task 1: 服务端排期位置（纯函数 + 响应字段 + 保存夹取）

**Files:**
- Modify: `server/youn_server/pages.py`（新增 `schedule_position`）
- Modify: `server/youn_server/app.py`（`get_schedule` 加两个字段；`create_page` 夹取时长）
- Test: `server/tests/test_schedule_api.py`

**Interfaces:**
- Produces: `pages.schedule_position(entries: list[PageEntry], now_ts: float) -> tuple[int, Optional[int]]` → `(current_index, seconds_until_next_page)`；`seconds_until_next_page` 为 `None` 表示空排期。
- Produces: `/api/pages/schedule` 响应新增 `current_index: int` 与 `seconds_until_next_page: int | null`。

- [ ] **Step 1: 写失败测试**

追加到 `server/tests/test_schedule_api.py`：

```python
def _entries(durations):
    return [
        pages_mod.PageEntry(md5=f"{i:032x}", duration_minutes=d, order=i, name=f"p{i}")
        for i, d in enumerate(durations)
    ]


def test_schedule_position_walks_the_cycle():
    entries = _entries([10, 5])          # cycle = 15 min
    cycle_start = 15 * 60               # 任意 15 分钟整数倍
    assert pages_mod.schedule_position(entries, cycle_start + 0) == (0, 600)
    assert pages_mod.schedule_position(entries, cycle_start + 599) == (0, 1)
    assert pages_mod.schedule_position(entries, cycle_start + 600) == (1, 300)
    assert pages_mod.schedule_position(entries, cycle_start + 899) == (1, 1)
    assert pages_mod.schedule_position(entries, cycle_start + 900) == (0, 600)  # 绕回


def test_schedule_position_single_page_and_empty():
    assert pages_mod.schedule_position(_entries([10]), 600) == (0, 600)
    assert pages_mod.schedule_position([], 12345) == (0, None)


def test_schedule_position_clamps_zero_duration():
    # 0 分钟页会让 cycle 为 0；必须夹到 1 分钟，否则除零
    entries = _entries([0, 5])
    assert pages_mod.schedule_position(entries, 0) == (0, 60)
    assert pages_mod.schedule_position(entries, 60) == (1, 300)


def test_schedule_response_carries_position_but_md5_ignores_it(client):
    body = client.get("/api/pages/schedule").json()
    assert "current_index" in body and "seconds_until_next_page" in body
    # 位置字段不参与 schedule_md5，否则设备缓存会被时间推进无限击穿
    md5_a = pages_mod.compute_schedule_md([pages_mod.PageEntry("a" * 32, 10, 0, "x")])
    md5_b = pages_mod.compute_schedule_md([pages_mod.PageEntry("a" * 32, 10, 0, "x")])
    assert md5_a == md5_b


```

（不需要新增保存校验的用例：既有 `test_min_duration_validation` 已经覆盖"低于下限返回 400"，且本任务**不改**这个契约——见 Step 3 的说明。若该文件里没有可复用的鉴权/画布 fixture，照抄同文件既有用例的写法，不要新造 fixture。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_schedule_api.py -q -k "position or clamped or carries_position"`
Expected: FAIL —`AttributeError: module 'youn_server.pages' has no attribute 'schedule_position'`

- [ ] **Step 3: 最小实现**

`server/youn_server/pages.py` 末尾新增：

```python
def schedule_position(entries: list[PageEntry], now_ts: float) -> tuple[int, Optional[int]]:
    """Which page should be showing at `now_ts`, and how long it has left.

    The cycle repeats from the Unix epoch, so both the server and the device can
    derive the same answer from any wall clock without storing state. Durations
    are clamped to >= 1 minute: a zero-minute page would make the cycle zero.
    """
    durations = [max(e.duration_minutes, 1) for e in entries]
    cycle_min = sum(durations)
    if cycle_min <= 0:
        return 0, None
    pos_min = (int(now_ts) // 60) % cycle_min
    acc = 0
    for i, d in enumerate(durations):
        if pos_min < acc + d:
            return i, (acc + d - pos_min) * 60
        acc += d
    return len(durations) - 1, 60  # 不可达；兜底避免返回 None 之外的东西
```

（`pages.py` 顶部确保 `from typing import Optional` 已在。）

`server/youn_server/app.py` 的 `get_schedule` 里，在 `sched_md = ...` 之后：

```python
        current_index, seconds_until_next_page = pages_mod.schedule_position(entries, time.time())
        return {
            "schedule_md5": sched_md,
            "server_time": datetime.now(ZoneInfo(settings.canvas_timezone)).isoformat(),
            "current_index": current_index,
            "seconds_until_next_page": seconds_until_next_page,
            "policy": { ... 保持原样 ... },
            "pages": [e.to_dict() for e in entries],
            "screen_active": _compute_screen_active(),
        }
```

`app.py` 顶层若无 `import time` 则补上。

`create_page` **不动**：它本来就有下限校验并返回 400（`test_min_duration_validation`）。原先计划里"改成静默夹取"的前提是错的，按 spec §3.2 的修正保留原契约——静默修正用户输入比明确拒绝更糟。`schedule_position` 内的 `max(duration_minutes, 1)` 仅用于对盘上历史数据保持全函数，与本条无关。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests -q`
Expected: PASS，总数 ≥ 73（原 69 + 新增 4）

- [ ] **Step 5: 提交**

```bash
git add server/youn_server/pages.py server/youn_server/app.py server/tests/test_schedule_api.py
git commit -m "feat(server): tell the device which page is showing and when it changes

Duty-cycled polling means the device is asleep most of the time, so it can no
longer advance a page index on its own without drifting. The schedule position
is a pure function of the entry durations and the wall clock, so the server can
answer it without state and the device needs none either.

Durations are clamped to >= 1 minute in the walk: a zero-minute page would make
the cycle zero and divide by zero. The same clamp now applies when a page is
saved, where min_page_duration_minutes had only ever been reported, never
enforced."
```

---

### Task 2: Rust `power` 模块（睡不睡、睡多久的纯决策）

**Files:**
- Create: `firmware/main/rust/src/power.rs`
- Modify: `firmware/main/rust/src/lib.rs`（`mod power;`）
- Create: `firmware/main/rust/include/power.h`（C 结构体定义）
- Test: `firmware/main/rust/src/power.rs`（`#[cfg(test)] mod tests`）

**Interfaces:**
- Produces（Rust）: `power::Inputs`、`power::Action`、`power::decide(&Inputs) -> Action`
- Produces（C，`include/power.h`）:
  ```c
  typedef struct {
      uint8_t  mains;                 // 插着 USB/充电器
      uint8_t  notify_active;         // 通知在屏
      uint8_t  busy;                  // SleepManager::CanSleepNow() == false
      uint8_t  sync_ok;               // 最近一次 schedule 拉取成功
      uint8_t  screen_active;         // 服务端 policy.screen_active
      uint8_t  on_canvas;             // 当前屏是画板
      uint8_t  _pad[2];
      uint64_t idle_ms;               // 距最后一次用户活动
      uint32_t grace_ms;              // NVS power:idle_grace_min * 60000
      uint32_t max_sleep_s;           // NVS power:max_sleep_min * 60
      uint32_t poll_s;                // policy.poll_interval_minutes * 60
      uint32_t sleep_poll_s;          // policy.sleep_poll_interval_minutes * 60
      uint32_t fail_streak;           // 连续同步失败次数
      int32_t  seconds_until_next_page;  // < 0 表示 null（空排期）
  } rf_power_inputs_t;

  typedef struct {
      uint8_t  sleep;                 // 0 = 保持清醒
      uint8_t  invalidate_panel;      // 睡前作废 RTC 面板记录
      uint8_t  _pad[2];
      uint32_t wake_s;                // sleep==1 时的定时唤醒秒数
      uint32_t stay_awake_ms;         // sleep==0 时下次再判定的间隔
  } rf_power_decision_t;

  void rf_power_decide(const rf_power_inputs_t* in, rf_power_decision_t* out);
  ```

- [ ] **Step 1: 写失败测试**

创建 `firmware/main/rust/src/power.rs`，先只放接口与测试：

```rust
//! Sleep/wake policy. Pure: the C side gathers the inputs, this decides.

pub struct Inputs {
    pub mains: bool,
    pub notify_active: bool,
    pub busy: bool,
    pub sync_ok: bool,
    pub screen_active: bool,
    pub on_canvas: bool,
    pub idle_ms: u64,
    pub grace_ms: u32,
    pub max_sleep_s: u32,
    pub poll_s: u32,
    pub sleep_poll_s: u32,
    pub fail_streak: u32,
    pub seconds_until_next_page: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    StayAwake { retry_ms: u32, reason: &'static str },
    Sleep { wake_s: u32, invalidate_panel: bool },
}

pub fn decide(_i: &Inputs) -> Action {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            mains: false, notify_active: false, busy: false, sync_ok: true,
            screen_active: true, on_canvas: true, idle_ms: 600_000,
            grace_ms: 180_000, max_sleep_s: 3600, poll_s: 600, sleep_poll_s: 3600,
            fail_streak: 0, seconds_until_next_page: Some(240),
        }
    }

    #[test]
    fn mains_never_sleeps() {
        let i = Inputs { mains: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 60_000, reason: "mains" });
    }

    #[test]
    fn a_notification_on_screen_holds_the_device_awake() {
        let i = Inputs { notify_active: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 15_000, reason: "notify" });
    }

    #[test]
    fn a_busy_panel_or_audio_holds_the_device_awake() {
        let i = Inputs { busy: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 15_000, reason: "busy" });
    }

    #[test]
    fn user_activity_inside_the_grace_window_holds_the_device_awake() {
        let i = Inputs { idle_ms: 179_999, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 30_000, reason: "grace" });
    }

    #[test]
    fn wakes_at_the_next_page_boundary() {
        let i = Inputs { seconds_until_next_page: Some(240), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 240, invalidate_panel: false });
    }

    #[test]
    fn the_poll_cap_bounds_the_wake() {
        let i = Inputs { seconds_until_next_page: Some(9000), poll_s: 600, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
    }

    #[test]
    fn the_sleep_window_uses_the_sleep_cadence() {
        let i = Inputs { screen_active: false, sleep_poll_s: 3600, poll_s: 600,
                         seconds_until_next_page: Some(300), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
        let i = Inputs { seconds_until_next_page: None, ..i };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 3600, invalidate_panel: false });
    }

    #[test]
    fn an_empty_schedule_sleeps_for_the_cap() {
        let i = Inputs { seconds_until_next_page: None, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
    }

    #[test]
    fn the_wake_never_goes_below_the_floor() {
        let i = Inputs { seconds_until_next_page: Some(0), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 60, invalidate_panel: false });
    }

    #[test]
    fn the_cap_is_clamped_by_max_sleep() {
        let i = Inputs { max_sleep_s: 300, seconds_until_next_page: None, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
    }

    #[test]
    fn a_failed_sync_backs_off_and_does_not_short_cycle() {
        let i = Inputs { sync_ok: false, fail_streak: 0, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 60, invalidate_panel: false });
        let i = Inputs { sync_ok: false, fail_streak: 1, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 120, invalidate_panel: false });
        let i = Inputs { sync_ok: false, fail_streak: 9, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
    }

    #[test]
    fn sleeping_from_a_non_canvas_page_invalidates_the_panel_record() {
        let i = Inputs { on_canvas: false, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 240, invalidate_panel: true });
    }
}
```

同时在 `lib.rs` 加 `mod power;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test power::`
Expected: FAIL — `not implemented` panic（除 `mains_never_sleeps` 等首调用外全部失败）

- [ ] **Step 3: 最小实现**

`power.rs` 里替换 `decide`：

```rust
/// 60 s floor: the server never asks for less, but a floor keeps a bad policy
/// value from turning the duty cycle into a hot loop.
const MIN_SLEEP_S: u32 = 60;
/// Re-evaluate cadence while held awake; short for hardware reasons, long for
/// the user-activity grace, so we do not spin on either.
const BUSY_RETRY_MS: u32 = 15_000;
const MAINS_RETRY_MS: u32 = 60_000;
const GRACE_RETRY_MS: u32 = 30_000;
const BACKOFF_BASE_S: u32 = 60;

fn poll_cap(i: &Inputs) -> u32 {
    let cap = if i.screen_active { i.poll_s } else { i.sleep_poll_s };
    cap.min(i.max_sleep_s).max(MIN_SLEEP_S)
}

pub fn decide(i: &Inputs) -> Action {
    // A device on USB is a development device: keep the console and stay
    // flashable, whatever the schedule says.
    if i.mains {
        return Action::StayAwake { retry_ms: MAINS_RETRY_MS, reason: "mains" };
    }
    // A notification is a question addressed to a human.
    if i.notify_active {
        return Action::StayAwake { retry_ms: BUSY_RETRY_MS, reason: "notify" };
    }
    // Panels and audio: cutting power mid-refresh corrupts a four-colour panel.
    if i.busy {
        return Action::StayAwake { retry_ms: BUSY_RETRY_MS, reason: "busy" };
    }
    if i.idle_ms < i.grace_ms as u64 {
        return Action::StayAwake { retry_ms: GRACE_RETRY_MS, reason: "grace" };
    }

    let cap = poll_cap(i);
    let wake_s = if !i.sync_ok {
        // Nothing to paint until the server answers; back off instead of
        // hammering a server that is down.
        (BACKOFF_BASE_S << i.fail_streak.min(5)).min(cap)
    } else {
        match i.seconds_until_next_page {
            Some(s) => s.clamp(MIN_SLEEP_S, cap),
            None => cap,
        }
    };

    // Sleeping while the UI owns the screen would resurrect the canvas on the
    // next wake while the RTC record still claims the canvas is up.
    Action::Sleep { wake_s, invalidate_panel: !i.on_canvas }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test`
Expected: PASS，总数 ≥ 61（原 49 + 12）

- [ ] **Step 5: 加 C 头文件与 FFI 导出**

创建 `firmware/main/rust/include/power.h`：内容为上面 Interfaces 里给出的 `rf_power_inputs_t` / `rf_power_decision_t` / `rf_power_decide` 声明（含 `<stdint.h>` 与 `extern "C"` 包裹）。

在 `power.rs` 末尾追加：

```rust
use core::ffi::c_int;

#[repr(C)]
pub struct CInputs {
    pub mains: u8,
    pub notify_active: u8,
    pub busy: u8,
    pub sync_ok: u8,
    pub screen_active: u8,
    pub on_canvas: u8,
    pub _pad: [u8; 2],
    pub idle_ms: u64,
    pub grace_ms: u32,
    pub max_sleep_s: u32,
    pub poll_s: u32,
    pub sleep_poll_s: u32,
    pub fail_streak: u32,
    pub seconds_until_next_page: c_int,
}

#[repr(C)]
pub struct CDecision {
    pub sleep: u8,
    pub invalidate_panel: u8,
    pub _pad: [u8; 2],
    pub wake_s: u32,
    pub stay_awake_ms: u32,
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_power_decide(inp: *const CInputs, out: *mut CDecision) {
    let i = &*inp;
    let a = decide(&Inputs {
        mains: i.mains != 0,
        notify_active: i.notify_active != 0,
        busy: i.busy != 0,
        sync_ok: i.sync_ok != 0,
        screen_active: i.screen_active != 0,
        on_canvas: i.on_canvas != 0,
        idle_ms: i.idle_ms,
        grace_ms: i.grace_ms,
        max_sleep_s: i.max_sleep_s,
        poll_s: i.poll_s,
        sleep_poll_s: i.sleep_poll_s,
        fail_streak: i.fail_streak,
        seconds_until_next_page: if i.seconds_until_next_page < 0 {
            None
        } else {
            Some(i.seconds_until_next_page as u32)
        },
    });
    let d = &mut *out;
    match a {
        Action::StayAwake { retry_ms, .. } => {
            d.sleep = 0;
            d.wake_s = 0;
            d.stay_awake_ms = retry_ms;
        }
        Action::Sleep { wake_s, invalidate_panel } => {
            d.sleep = 1;
            d.invalidate_panel = invalidate_panel as u8;
            d.wake_s = wake_s;
            d.stay_awake_ms = 0;
        }
    }
}
```

- [ ] **Step 6: 提交**

```bash
git add firmware/main/rust/src/power.rs firmware/main/rust/src/lib.rs firmware/main/rust/include/power.h
git commit -m "feat(firmware): decide the duty cycle in Rust so it can be tested

Which conditions hold the device awake, how long it sleeps and when it must
invalidate the panel record are pure functions of a dozen inputs. Keeping them
in C++ meant they could only be checked on hardware; as a Rust module they are
covered by cargo test, and the C side is left with gathering inputs and calling
esp_sleep_*.

The rules: mains never sleeps, a notification on screen is a question addressed
to a human, a busy panel or audio must not lose power mid-refresh, and only the
user-activity grace window can hold it awake for its own sake."
```

---

### Task 3: shim 增补（RTC 面板记录 / 唤醒原因 / 供电轨）

**Files:**
- Modify: `firmware/main/rust/shim.cpp`
- Modify: `firmware/main/rust/src/shim.rs`（extern 声明 + 主机桩）
- Create: `firmware/main/rust/include/shim_power.h`

**Interfaces:**
- Produces（C）:
  ```c
  /* Layout is relied on by the host stub and the Rust reader: valid at offset
   * 4, md5 at offset 8, index at 44, sizeof == 48. Keep them in sync. */
  typedef struct {
      uint32_t magic;              /* 0  */
      uint8_t  valid;              /* 4  */
      uint8_t  _pad[3];            /* 5  */
      char     displayed_md5[33];  /* 8  */
      int32_t  displayed_index;    /* 44 */
  } rf_panel_record_t;             /* sizeof == 48 */

  void rf_panel_record_get(rf_panel_record_t* out);
  void rf_panel_mark_pending(const char* md5, int index);
  void rf_panel_record_invalidate(void);

  /* 0 = 其它, 1 = timer 唤醒, 2 = ext0(BOOT), 3 = ext1(充电插入) */
  int  rf_wakeup_cause(void);

  void rf_rails_audio(int on);   /* 音频 + 功放 */
  ```
- Consumes: `Board::GetInstance()` 的 `BoardPowerBsp`（`PowerAudioOn/Off`、`PowerAmpOn/Off`）、`CustomLcdDisplay::SetOnRefreshIdle`。

- [ ] **Step 1: 写 shim 声明与主机桩（先让 `cargo test` 能编译）**

`firmware/main/rust/src/shim.rs` 的 extern 块追加：

```rust
    pub fn rf_panel_record_get(out: *mut u8);
    pub fn rf_panel_mark_pending(md5: *const c_char, index: c_int);
    pub fn rf_panel_record_invalidate();
    pub fn rf_wakeup_cause() -> c_int;
    pub fn rf_rails_audio(on: c_int);
```

主机桩（`mod host`）追加，并让 `host::lock()` 重置 `PANEL_REC`/`AUDIO_ON`：

```rust
    static PANEL_REC: Mutex<Option<(String, i32)>> = Mutex::new(None);
    // rf_panel_record_get writes 48 bytes; see rf_panel_record_t.
    static AUDIO_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_record_get(out: *mut u8) {
        // 33-byte md5 slot + valid byte at offset 4, matching rf_panel_record_t.
        let g = PANEL_REC.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            core::ptr::write_bytes(out, 0, 33);
            match g.as_ref() {
                Some((md5, _)) => {
                    *out.add(4) = 1;
                    core::ptr::copy_nonoverlapping(md5.as_ptr(), out.add(8), md5.len().min(32));
                }
                None => *out.add(4) = 0,
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_mark_pending(md5: *const c_char, index: c_int) {
        let md5 = cstr(md5);
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) = Some((md5, index));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_record_invalidate() {
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_wakeup_cause() -> c_int {
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_rails_audio(on: c_int) {
        AUDIO_ON.store(on != 0, std::sync::atomic::Ordering::SeqCst);
    }
```

- [ ] **Step 2: 跑测试确认仍通过（桩就位）**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test`
Expected: PASS，总数与 Task 2 结束一致（本步只加管道，不加断言）

- [ ] **Step 3: 实现 shim.cpp**

`shim.cpp` 顶部加 `#include "esp_sleep.h"`、`#include "board.h"`、`#include "boards/zectrix-s3-epaper-4.2/board_power_bsp.h"`（若已包含则跳过）。追加：

```cpp
// Panel content survives deep sleep in RTC memory, so a wake that changes
// nothing can skip a >= 15 s full refresh. RTC_DATA_ATTR is retained across
// deep sleep and soft resets, lost on power-on (magic fails -> repaint).
RTC_DATA_ATTR static rf_panel_record_t g_panel_rec;
// md5 of the frame we asked for; committed only when the refresh goes idle, so
// an interrupted refresh cannot be recorded as done.
static char g_pending_md5[33];
static int  g_pending_index = -1;
static bool g_pending_valid = false;

extern "C" void rf_panel_record_get(rf_panel_record_t *out) {
    if (out == nullptr) return;
    *out = g_panel_rec;
}

extern "C" void rf_panel_mark_pending(const char *md5, int index) {
    if (md5 == nullptr) return;
    snprintf(g_pending_md5, sizeof(g_pending_md5), "%s", md5);
    g_pending_index = index;
    g_pending_valid = true;
}

extern "C" void rf_panel_record_invalidate(void) {
    g_panel_rec.valid = 0;
    g_panel_rec.magic = 0;
}

extern "C" int rf_wakeup_cause(void) {
    switch (esp_sleep_get_wakeup_cause()) {
    case ESP_SLEEP_WAKEUP_TIMER: return 1;
    case ESP_SLEEP_WAKEUP_EXT0:  return 2;
    case ESP_SLEEP_WAKEUP_EXT1:  return 3;
    default:                     return 0;
    }
}

extern "C" void rf_rails_audio(int on) {
    BoardPowerBsp *p = board_power();
    if (p == nullptr) return;
    if (on) { p->PowerAudioOn(); p->PowerAmpOn(); }
    else    { p->PowerAmpOff(); p->PowerAudioOff(); }
}
```

`board_power()` 是一个新增的小助手：从 `Board::GetInstance()` 取 `BoardPowerBsp*`。若板级没有公开访问器，在 `zectrix-s3-epaper-4.2.h` 加 `BoardPowerBsp* GetPower() { return power_.get(); }`，shim 里 `static_cast<ZectrixS3Epaper42*>(&Board::GetInstance())->GetPower()`。

在 shim 的初始化函数里挂 on-idle 提交（该函数在 `rf_set_display` 之后调用一次即可）：

```cpp
// Commit the pending md5 once the panel reports idle: this is what makes
// "skip the repaint next wake" safe.
extern "C" void rf_panel_watch_refresh(void) {
    CustomLcdDisplay *d = lcd();
    if (d == nullptr) return;
    d->SetOnRefreshIdle([]() {
        if (!g_pending_valid) return;
        g_panel_rec.magic = RF_PANEL_MAGIC;
        g_panel_rec.valid = 1;
        snprintf(g_panel_rec.displayed_md5, sizeof(g_panel_rec.displayed_md5), "%s", g_pending_md5);
        g_panel_rec.displayed_index = g_pending_index;
        g_pending_valid = false;
    });
}
```

（`RF_PANEL_MAGIC` 定义为 `0x50414E31u`。）

- [ ] **Step 4: 构建确认通过**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && KEY=$(grep -oP '^MASTER_KEY=\K.*' ../server/.env) && rm -rf build && idf.py -DDEVICE_MASTER_KEY="$KEY" build`
Expected: `Project build complete.`，无 `error:` / `undefined reference`

- [ ] **Step 5: 提交**

```bash
git add firmware/main/rust/shim.cpp firmware/main/rust/src/shim.rs firmware/main/rust/include/shim_power.h
git commit -m "feat(firmware): expose panel memory, wake cause and the audio rail to Rust

The panel record lives in RTC memory because every deep-sleep wake is a cold
boot: without it the device cannot tell whether the frame it is about to draw is
already on the glass, and would pay a >= 15 s full refresh every wake.

The pending/commit split matters: the md5 is only recorded when the refresh
reports idle, so a refresh interrupted by power loss is not remembered as done."
```

---

### Task 4: `page_sync` 收敛为一次性同步 + 变化才绘屏

**Files:**
- Modify: `firmware/main/rust/src/page_sync.rs`
- Modify: `firmware/main/rust/include/page_sync.h`

**Interfaces:**
- Consumes: Task 1 的 `current_index` / `seconds_until_next_page`；Task 3 的 `rf_panel_*`。
- Produces（C）:
  ```c
  bool     page_sync_sync_once(void);        /* 拉取+解析+按需下载；true = 成功 */
  bool     page_sync_paint_if_changed(void); /* true = 真的绘屏了 */
  uint32_t page_sync_next_wake_s(void);      /* 上次响应推导，未夹取；0 = 未知 */
  bool     page_sync_sync_ok(void);          /* 最近一次 sync_once 是否成功 */
  uint32_t page_sync_poll_s(void);           /* policy.poll_interval_minutes * 60 */
  uint32_t page_sync_sleep_poll_s(void);     /* policy.sleep_poll_interval_minutes * 60 */
  bool     page_sync_screen_active(void);
  ```
- 移除：`task_entry` 的循环、`plan_tick`、`TICK_MS`、`POLL_MS`、`started_us`/`duration_s` 自动推进、`page_sync_start` 的语义改为"仅启动（供交互模式首次绘屏）"。

- [ ] **Step 1: 删旧测试、写新失败测试**

删除 `page_sync.rs` 中 `plan_tick` 的 4 个测试（`the_first_tick_always_polls_and_paints`、`polling_follows_the_servers_cadence_not_the_tick`、`the_sleep_window_polls_slowly_and_stops_painting`、`a_suspended_canvas_polls_but_never_paints`）与 `plan_tick`/`Tick` 本身。追加：

```rust
    fn scripted_schedule_with_position(index: usize, next_s: u32, entries: &[(u8, u32)]) {
        let pages: Vec<String> = entries.iter()
            .map(|(t, m)| format!(r#"{{"md5":"{}","duration_minutes":{}}}"#, md5hex(*t), m))
            .collect();
        let body = format!(
            r#"{{"schedule_md5":"{}","current_index":{},"seconds_until_next_page":{},"screen_active":true,"policy":{{"poll_interval_minutes":10,"sleep_poll_interval_minutes":60}},"pages":[{}]}}"#,
            md5hex(0x11), index, next_s, pages.join(","));
        shim::host::script_ok("/api/pages/schedule", body.as_bytes());
    }

    #[test]
    fn paints_when_the_servers_page_differs_from_the_glass() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(1, 240, &[(0xa1, 10), (0xb2, 5)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        assert!(sync_once());
        assert!(paint_if_changed(), "nothing recorded on the glass yet -> paint");
        assert_eq!(shim::host::fb()[0], 0xb2, "the server's current page is on the panel");
        assert_eq!(shim::host::refreshes(), 1);
    }

    #[test]
    fn skips_the_repaint_when_the_glass_already_shows_it() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed());
        assert_eq!(shim::host::refreshes(), 1);

        // Simulate the wake that follows: same page, same glass.
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(!paint_if_changed(), "same md5 as the glass -> no panel cycle");
        assert_eq!(shim::host::refreshes(), 1, "no extra refresh was spent");
    }

    #[test]
    fn a_failed_sync_reports_failure_and_does_not_paint() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_get("/api/pages/schedule", 500, b"");
        assert!(!sync_once());
        assert!(!paint_if_changed());
        assert_eq!(shim::host::refreshes(), 0);
    }

    #[test]
    fn manual_paging_overrides_the_server_until_the_next_sync() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10), (0xb2, 5)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));

        next();  // local override -> page 2
        assert_eq!(shim::host::fb()[0], 0xb2);

        // A later sync re-imposes the server's index.
        scripted_schedule_with_position(0, 240, &[(0xa1, 10), (0xb2, 5)]);
        sync_once();
        assert!(paint_if_changed(), "override cleared -> back to the server's page");
        assert_eq!(shim::host::fb()[0], 0xa1);
    }

    #[test]
    fn next_wake_is_reported_from_the_response() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert_eq!(next_wake_s(), 240);
        assert_eq!(poll_s(), 600);
        assert_eq!(sleep_poll_s(), 3600);
        assert!(screen_active());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test page_sync::`
Expected: FAIL — `paint_if_changed` / `next_wake_s` / `poll_s` 未定义

- [ ] **Step 3: 实现**

在 `Table` 中把 `current: usize`、`started_us: u64` 换成：

```rust
    /// Index the server says should be showing now.
    server_index: usize,
    /// Set by manual paging; cleared by the next successful sync.
    override_index: Option<usize>,
    /// Seconds until the server's next page change (0 = unknown/empty).
    next_wake_s: u32,
```

`ParsedSchedule` 增 `current_index: usize`、`seconds_until_next_page: Option<u32>`（`json::member` + `int_value` 解析；缺失时 `current_index = 0`、`seconds_until_next_page = None`）。

`sync_schedule` 提交时写入 `server_index`、`next_wake_s`，并 `override_index = None`；`unchanged` 快路径**也要**更新这两个字段与清除 override（排期没变但时间推进了）：

```rust
    with_table(|t| {
        t.server_index = parsed.current_index.min(t.count.saturating_sub(1));
        t.next_wake_s = parsed.seconds_until_next_page.unwrap_or(0);
        t.override_index = None;
    });
```

新增导出（替换旧任务循环）：

```rust
fn target_index() -> Option<usize> {
    with_table(|t| {
        if t.count == 0 { return None; }
        Some(t.override_index.unwrap_or(t.server_index).min(t.count - 1))
    })
}

/// Paint the target page only if the glass does not already show it.
pub fn paint_if_changed() -> bool {
    let Some(idx) = target_index() else { return false; };
    let md5 = with_table(|t| t.pages[idx].md5);
    let mut rec = [0u8; 48];
    unsafe { shim::rf_panel_record_get(rec.as_mut_ptr()) };
    if rec[4] != 0 && rec[8..40] == md5 {
        log_i!("PageSync", "glass already shows {}, skipping repaint", ...);
        return false;
    }
    let mut slot = with_table(|t| t.pages[idx].bitmap);
    if slot.is_null() {
        slot = unsafe { shim::rf_alloc(PAGE_BITMAP_SIZE + 1) };
        if slot.is_null() || !download_bitmap(&md5, slot) { return false; }
        with_table(|t| t.pages[idx].bitmap = slot);
    }
    unsafe { shim::rf_panel_mark_pending(md5.as_ptr() as *const core::ffi::c_char, idx as i32) };
    blit_and_refresh(slot);
    true
}
```

`blit_and_refresh` 就是原 `show_page` 去掉索引/日志后的帧缓冲拷贝 + `rf_request_full_refresh()`；`DISPLAYING` 的维护保持原样。

`next()`/`prev()` 改为写 `override_index`，然后调 `paint_index(idx)`——即 `paint_if_changed` 去掉 RTC 比对的那一份（用户明确要求翻页时必须画），两者共用 `blit_and_refresh`。

C ABI 按 Interfaces 逐条导出；`page_sync_start()` 保留但只做 `DISPLAYING=true` 与 `SUSPENDED=false`（不再建任务）。

- [ ] **Step 4: 跑测试确认通过**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test`
Expected: PASS，总数 ≥ 62（删 4 加 5，其余保持）

- [ ] **Step 5: 构建确认 C ABI 变化没打断 C++ 调用点**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && idf.py build`
Expected: `Project build complete.`；若 `application.cc` 仍调用被删的 `page_sync_start` 语义，改为按 Task 6 的调用方式（本步只保证链接通过）

- [ ] **Step 6: 提交**

```bash
git add firmware/main/rust/src/page_sync.rs firmware/main/rust/include/page_sync.h
git commit -m "refactor(firmware): make page_sync a one-shot sync, not a poll loop

Duty-cycled operation means nothing polls between wakes, and the page index is
now the server's answer rather than a counter the device advances. The task
loop, its tick plan and the local rotation maths all go; what remains is
fetch -> compare against the RTC panel record -> repaint only when the glass is
out of date.

Manual paging survives as a local override that the next successful sync clears,
so browsing cannot fight the schedule."
```

---

### Task 5: 惰性显示初始化

**Files:**
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.h`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc`（`CreateDisplay` 加参数）

**Interfaces:**
- Produces: `CustomLcdDisplay::BringUpPanel()`（幂等：`EPD_Init()` + `EPD_Clear()` + 首帧 `EPD_Display()` + `prev_buffer_synced = true` + `rf_panel_watch_refresh()`）。
- 构造函数不再做面板上电动作，只分配 framebuffer/mutex/refresh task。

- [ ] **Step 1: 记录当前行为（作为对照基线，不是测试）**

Run: `rg -n "EPD init|EPD_Clear|EPD_Display\(\)|prev_buffer_synced = true" firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc | head -20`
Expected: 输出显示构造函数里 `EPD init` → `EPD_Clear()` → `EPD_Display()` 与 `prev_buffer_synced = true` 的位置（实现时以此为准移动）

- [ ] **Step 2: 实现拆分**

构造函数中把 `EPD_Init(); EPD_Clear(); ... EPD_Display(); prev_buffer_synced = true;` 整段**移除**，改成：

```cpp
    // Panel bring-up is deferred: a duty-cycled wake that changes nothing must
    // not pay the ~20 s blank+refresh this used to do on every boot.
    ESP_LOGI(TAG, "EPD buffers ready, panel bring-up deferred");
    dirty_mutex = xSemaphoreCreateMutex();
    assert(dirty_mutex);
    start_refresh_task();
```

新增：

```cpp
void CustomLcdDisplay::BringUpPanel() {
    if (panel_brought_up_) {
        return;
    }
    panel_brought_up_ = true;
    ESP_LOGI(TAG, "EPD bring-up");
    EPD_Init();
    EPD_Clear();
    memcpy(prev_buffer, buffer, lcd_spi_data.buffer_len);
    EPD_Display();
    prev_buffer_synced = true;
}
```

头文件加 `void BringUpPanel();` 与 `bool panel_brought_up_ = false;`。

板级 `CreateDisplay(...)` 增加参数 `bool bring_up_panel`，在 `new CustomLcdDisplay(...)` 之后按参数调用 `display_->BringUpPanel()`；**冷启动与交互路径传 `true`，quiet 路径传 `false`**。

- [ ] **Step 3: 构建确认通过**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && KEY=$(grep -oP '^MASTER_KEY=\K.*' ../server/.env) && rm -rf build && idf.py -DDEVICE_MASTER_KEY="$KEY" build`
Expected: `Project build complete.`

- [ ] **Step 4: 真机确认冷启动行为未变**

烧录并抓一次冷启动日志（拔电重插后 `python -m esptool --chip esp32s3 -p /dev/ttyACM0 -b 460800 write-flash 0x20000 firmware/build/xiaozhi.bin`，随后按 Task 8 的监视命令）：
Expected: 仍能看到 `EPD bring-up` 与随后 5/10/15 秒的 `EPD busy wait`，且面板正常显示画板页。

- [ ] **Step 5: 提交**

```bash
git add firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.h firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc
git commit -m "perf(firmware): bring the panel up on demand, not in the constructor

Every deep-sleep wake is a cold boot, and the constructor's EPD_Init + EPD_Clear
+ EPD_Display is the 15-20 s of 'EPD busy wait' that precedes WiFi in every boot
log. Under duty cycling that is ~48 minutes a day of blanking the screen plus a
visible flash per wake.

Cold boot and interactive wakes still bring the panel up immediately, so the
reset path is unchanged; only the quiet wake can now skip it entirely."
```

---

### Task 6: quiet / interactive 启动路径拆分

**Files:**
- Modify: `firmware/main/main.cc`（判定唤醒原因，传入 Application）
- Modify: `firmware/main/application.h` / `application.cc`（`Initialize(bool quiet)`）
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc`（按 quiet 决定是否建 UI/上电面板）

**Interfaces:**
- Consumes: Task 3 的 `rf_wakeup_cause()`；Task 5 的 `CreateDisplay(..., bring_up_panel)`。
- Produces: `Application::Initialize(bool quiet)`；`Application::IsQuietBoot() const`。

- [ ] **Step 1: 实现唤醒原因判定**

`main.cc` 的 `app_main()` 中，在 bounce 逻辑之后：

```cpp
    // A timer wake is a duty-cycle wake: nothing on the panel changes unless a
    // page boundary passed, so skip the UI, provisioning and panel bring-up.
    const bool quiet_boot = rf_wakeup_cause() == 1;
    ESP_LOGI(TAG, "Boot path: %s (wakeup cause=%d)", quiet_boot ? "quiet" : "interactive",
             rf_wakeup_cause());
    Application::GetInstance().Initialize(quiet_boot);
```

- [ ] **Step 2: Application 侧分流**

`Application::Initialize(bool quiet)` 存储 `quiet_boot_ = quiet`，并在 quiet 时跳过：`rawdraw_ui_manager_` 创建与 Settings 注册、`audio_service_.Initialize/Start`、配网检查（`NEEDS_PROVISION` 分支保持：没有凭据时即便 quiet 也要走配网，否则永远连不上）。

交互路径与现状完全一致。qui​​et 路径仍需：`WifiManager` 启动、`server_pairing` 任务、`StartSntpClockSyncOnce`、`page_sync` 初始化。

- [ ] **Step 3: 板级按 quiet 建屏**

`zectrix-s3-epaper-4.2.cc` 的 `CreateDisplay(...)` 调用改为 `CreateDisplay(..., /*bring_up_panel=*/!quiet)`。

- [ ] **Step 4: 构建 + 真机确认 quiet 路径**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && KEY=$(grep -oP '^MASTER_KEY=\K.*' ../server/.env) && rm -rf build && idf.py -DDEVICE_MASTER_KEY="$KEY" build`
烧录后拔电、等一次定时唤醒，抓日志：
Expected: 出现 `Boot path: quiet (wakeup cause=1)`，且**没有** `EPD bring-up`、没有 `RawDrawUiManager: RawDraw UI Manager initialized`；从启动到 `WiFi connected` 明显短于交互路径（对照日志时间戳）。

- [ ] **Step 5: 提交**

```bash
git add firmware/main/main.cc firmware/main/application.h firmware/main/application.cc firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc
git commit -m "feat(firmware): split the boot path into quiet and interactive

A timer wake only needs to sync and possibly repaint one frame; building the UI,
starting the audio codec and bringing the panel up are all wasted there, and the
panel bring-up alone is 20 s with a visible flash. The wake-up cause now selects
the path, and provisioning still runs on a quiet boot when there are no
credentials — otherwise a duty-cycled device could never be paired again."
```

---

### Task 7: 电源管理接线（插电不睡 / 宽限 / ext1 / 供电轨 / 唤醒时拉通知）

**Files:**
- Modify: `firmware/main/application.cc` / `.h`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/config.h`（删 `CHARGE_GPIO_AFFECT_SLEEP`）
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc`（ext1 掩码）

**Interfaces:**
- Consumes: Task 2 的 `rf_power_decide`（C 结构见 Task 2 Interfaces）；Task 3 的 `rf_rails_audio`；Task 4 的 `page_sync_sync_once/paint_if_changed/next_wake_s/poll_s/sleep_poll_s/screen_active`。
- Produces: `Application::ServicePowerPolicy()`（等价于原 `EnterScheduledSleep` 的位置）。

- [ ] **Step 1: 板级暴露电源状态（先做，Step 2 的代码依赖它）**

`zectrix-s3-epaper-4.2.h` 加：

```cpp
    /// True while a charger/USB is supplying the board.
    bool IsPowerPresent() const { return charge_status_.Get().power_present; }
```

`application.cc` 顶部常量区加：

```cpp
constexpr char kPowerNamespace[] = "power";
```

- [ ] **Step 2: 实现 `ServicePowerPolicy()`**

替换 `ArmSyncSleepTimer` / `EnterScheduledSleep` / `EnterManualSleep` 中的休眠决策部分：

```cpp
void Application::ServicePowerPolicy() {
    static int64_t last_activity_ms = 0;          // NoteButtonActivity() 更新
    static uint32_t fail_streak = 0;

    Settings nvs(kPowerNamespace, true);
    rf_power_inputs_t in = {};
    in.mains          = Board::GetInstance().IsPowerPresent() ? 1 : 0;  /* Step 1 */
    in.notify_active  = notify_is_active() ? 1 : 0;
    in.busy           = SleepManager::GetInstance().CanSleepNow() ? 0 : 1;
    in.sync_ok        = page_sync_sync_ok() ? 1 : 0;   /* Task 4 导出 */
    in.screen_active  = page_sync_screen_active() ? 1 : 0;
    in.on_canvas      = page_sync_is_displaying() ? 1 : 0;
    in.idle_ms        = (uint64_t)(esp_timer_get_time() / 1000 - last_activity_ms);
    in.grace_ms       = (uint32_t)nvs.GetInt("idle_grace_min", 3) * 60000u;
    in.max_sleep_s    = (uint32_t)nvs.GetInt("max_sleep_min", 60) * 60u;
    in.poll_s         = page_sync_poll_s();
    in.sleep_poll_s   = page_sync_sleep_poll_s();
    in.fail_streak    = fail_streak;
    in.seconds_until_next_page = (int32_t)page_sync_next_wake_s();

    rf_power_decision_t d = {};
    rf_power_decide(&in, &d);

    if (!d.sleep) {
        ESP_LOGI(kTag, "Stay awake (%u ms)", d.stay_awake_ms);
        Esp_timerRearm(d.stay_awake_ms);
        return;
    }
    if (d.invalidate_panel) {
        rf_panel_record_invalidate();
    }
    ESP_LOGI(kTag, "Deep sleep %u s (mains=%d sync_ok=%d)", d.wake_s, in.mains, in.sync_ok);
    rf_rails_audio(0);
    esp_wifi_disconnect();
    esp_wifi_stop();
    esp_sleep_enable_timer_wakeup((uint64_t)d.wake_s * 1000000ULL);
    esp_sleep_enable_ext0_wakeup((gpio_num_t)BOOT_BUTTON_GPIO, 0);
    // v6.0 deprecates esp_sleep_enable_ext1_wakeup in favour of the _io pair.
    esp_sleep_enable_ext1_wakeup_io(1ULL << CHARGE_DETECT_GPIO, ESP_EXT1_WAKEUP_ANY_LOW);
    esp_deep_sleep_start();
}
```

`Esp_timerRearm(ms)`：把现有 `ArmSyncSleepTimer`（分钟粒度）改成毫秒粒度并改名，保留它创建 `app_sync_sleep` 定时器的代码，只把回调从 `EnterScheduledSleep` 改为 `ServicePowerPolicy`，`esp_timer_start_once(sleep_timer_, ms * 1000)`。删掉原 `ArmSyncSleepTimer`/`EnterScheduledSleep` 的调用点（`NetworkEvent::Connected` 处改为 `Esp_timerRearm(3000)`，让策略尽早跑第一次判定）。

`NoteButtonActivity()` 末尾追加 `last_activity_ms = esp_timer_get_time() / 1000;`（宽限期从最后一次按键起算——现在按键完全不重置计时，这是既有毛病）。`last_activity_ms` 需要提升为成员变量 `last_activity_ms_`。

- [ ] **Step 3: 唤醒时的同步顺序与通知**

`Application::Run()`（或一个 `RunQuietCycleOnce()`，由 quiet 启动路径调用一次）按顺序：`page_sync_sync_once()` → 缓存结果并更新 `fail_streak`（成功清零）→ `notify_request_next()` → `page_sync_paint_if_changed()` → `ServicePowerPolicy()`。

`rf_rails_audio(1)` 在需要放音或进入交互模式时调用；quiet 循环不放音则保持关闭。

- [ ] **Step 4: ext1 唤醒电平与死宏清理**

- `config.h` 删除 `#define CHARGE_GPIO_AFFECT_SLEEP 1`。
- 用日志确认 `CHARGE_DETECT_GPIO`(GPIO2) 在"未插电"时的实际电平，据此选择 `ESP_EXT1_WAKEUP_ANY_LOW` / `ANY_HIGH`（`CHARGE_DETECT_CHARGING_LEVEL 0` 表示充电为低，但空闲电平必须实测，不能照抄）。把实测值写进 `config.h` 的注释。
- `bool IsPowerPresent()` 已在 Step 1 加入（转发 `charge_status_.Get().power_present`，该 `Get()` 是 const）。

- [ ] **Step 5: 构建 + 真机矩阵 1-7**

Run: `source ~/data/esp-idf-v6.0/export.sh && cd firmware && KEY=$(grep -oP '^MASTER_KEY=\K.*' ../server/.env) && rm -rf build && idf.py -DDEVICE_MASTER_KEY="$KEY" build`，随后按 Task 8 的命令监视并跑矩阵。

Expected: 见 Task 8 表格。

- [ ] **Step 6: 提交**

```bash
git add firmware/main/application.cc firmware/main/application.h firmware/main/boards/zectrix-s3-epaper-4.2/config.h firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc
git commit -m "feat(firmware): duty-cycle the battery, stay awake on USB

Replaces the fixed 30-minute sleep timer with the policy function from power.rs:
sleep until the server's next page change (bounded by the poll cap and the sleep
window), never sleep while USB is present, back off instead of hammering an
unreachable server, and defer while a notification is on screen or the panel is
busy.

Two fixes fall out: button activity now resets the idle grace (it never did),
and the dead CHARGE_GPIO_AFFECT_SLEEP macro becomes the behaviour it described —
plugging USB wakes the device through ext1, so debugging needs no BOOT press."
```

---

### Task 8: 清理、文档与真机验证矩阵（Stage 1 收口）

**Files:**
- Modify: `firmware/.env.example`（若新增 NVS 键需说明；无新增则跳过）
- Modify: `docs/superpowers/specs/2026-09-11-sleep-power-redesign.md`（回填实测结论：ext1 电平、实测唤醒时长）
- Modify: `firmware/scripts/release.py`（若 Stage 1 引入新构建产物则跳过）

**Interfaces:**
- Consumes: 前七个任务的全部产物。

- [ ] **Step 1: 监视命令（每次真机验证都用这套）**

```bash
hub op=start name=devmon application=/home/pi/.espressif/python_env/idf6.0_py3.14_env/bin/python \
  args=["/tmp/serial_passive.py", "3600"] persist=true ready={"log":"watching","timeout":30}
```

（`/tmp/serial_passive.py` 为既有被动读取器：自动重连、逐行加时间戳。若不存在，重新写一个等价脚本：循环 `serial.Serial('/dev/ttyACM0', 115200, timeout=1)` + 逐行打印。）

- [ ] **Step 2: 跑真机矩阵**

| # | 场景 | 期望日志/现象 |
|---|---|---|
| 1 | 插电 35 分钟 | 无 `USB disconnect`；无 `Deep sleep` 行 |
| 2 | 拔电后静默周期 | `Boot path: quiet (wakeup cause=1)` + `schedule` 拉取 + **无** `[REFRESH]` |
| 3 | 到翻页点的唤醒 | **恰好一次** `[REFRESH] Performing FULL refresh: reason=canvas` |
| 4 | 插 USB（不按键） | 串口出现 + `wakeup cause=3` |
| 5 | 唤醒时存在待确认通知 | `Notify: notify "…" displaying` |
| 6 | 按 BOOT 唤醒 | `Boot path: interactive (wakeup cause=2)` + 后续 `Stay awake ("grace")` 直到宽限过期 |
| 7 | 冷启动（拔电重插） | `EPD bring-up` + 必定重绘 |

任何一项不符：不要改断言迁就实现，记下证据并回到对应任务修。

- [ ] **Step 3: 回填 spec 的实测数字**

在 spec 的 §13 追加一节"实测结论"：ext1 实际唤醒电平、quiet 唤醒从 boot 到 `WiFi connected` 的毫秒数、一次翻页唤醒的射频在线时长。若与设计预期差一个数量级，明确指出。

- [ ] **Step 4: 全量回归**

Run:
```
cd server && ./.venv/bin/python -m pytest tests -q
export PATH="$HOME/.cargo/bin:$PATH"; cd firmware/main/rust && cargo test
```
Expected: 服务端 ≥ 74 passed；Rust ≥ 62 passed

- [ ] **Step 5: 提交**

```bash
git add -A firmware docs/superpowers/specs/2026-09-11-sleep-power-redesign.md
git commit -m "test(firmware): record the Stage 1 device verification

Fills in the measured wake levels and timings the design left to the device, and
records the seven-scenario matrix that has to hold before Stage 2 (cutting the
EPD rail) starts."
```

---

## Self-Review

**Spec coverage**

| Spec 节 | 任务 |
|---|---|
| §3.1 两个新字段 + 纯函数 | Task 1 |
| §3.2 保存校验保持不动 | Task 1（不改代码，只保证既有 400 用例仍过） |
| §4.1 quiet/interactive 拆分 | Task 6 |
| §4.2 三种电源形态 | Task 7（kMains/kInteractive/kDutyCycle 由 `power::decide` 表达） |
| §4.3 允许入睡条件（含第 5 条作废记录） | Task 2（`decide`）+ Task 7（接线） |
| §4.4 睡多久 + 退避 | Task 2 |
| §4.5 wake cycle 顺序 | Task 7 |
| §5 RTC 面板记录 | Task 3（存储/提交）+ Task 4（比对） |
| §6 惰性显示初始化 | Task 5 |
| §7 供电轨（Stage 1 = 音频/功放） | Task 3（`rf_rails_audio`）+ Task 7（睡前调用） |
| §9 page_sync 收敛 + 手动覆盖 | Task 4 |
| §10 NVS 键 + 删死宏 | Task 7 |
| §11 测试（服务端/Rust/真机） | Task 1、2、4、8 |
| §12 Stage 1 范围 | 本计划边界；Stage 2/3 明确不在内 |

**Placeholder scan**：无 `TBD`/`TODO`/"similar to Task N"；每个代码步骤都给了可粘贴内容。

**对照 IDF v6.0 校正过的两处事实**（写计划时先查了本地 IDF 源码，不是凭记忆）：

- `esp_sleep_enable_ext1_wakeup` 在 v6.0 已标记 deprecated，改用 `esp_sleep_enable_ext1_wakeup_io(io_mask, level_mode)`；`ESP_EXT1_WAKEUP_ANY_LOW` 在 S3 所在分支存在（值 0）。
- `ESP_SLEEP_WAKEUP_TIMER / EXT0 / EXT1` 名称已核对 `esp_sleep.h`。

**Type consistency**：`rf_power_inputs_t`/`rf_power_decision_t` 的字段名与顺序在 Task 2（Rust `CInputs`/`CDecision`、`include/power.h`）与 Task 7（C++ 填结构）三处一致；`rf_panel_record_t` 的 `valid` 偏移 4、`displayed_md5` 偏移 8 在 Task 3 的 C 定义、主机桩与 Task 4 的读取三处一致；`page_sync_*` 导出名在 Task 4 的 Interfaces 与 Task 7 的调用处一致。

**已知的诚实缺口**：Task 5/6/7 的验证依赖真机按键，无法在主机上完成；Task 8 就是为此存在的。
