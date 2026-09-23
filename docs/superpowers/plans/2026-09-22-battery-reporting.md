# 固件电量上报机制 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 设备每次唤醒采样电池（mv/pct/charge），搭车 schedule GET 上报；服务端存 90 天历史并提供 `power-history` API；前端设备页手绘 SVG 折线。

**Architecture:** 复用 `rf_power_counters` 的既有三层模式：C++ 机制（ADC+charge 快照一次采样）→ shim FFI `rf_battery_sample` → Rust 策略（page_sync 拼 URL）。服务端在 schedule GET 处理器解析四键 `v/p/c` 落新表 `battery_history`（含当次 power 计数器快照列），插入顺手 PURGE 90 天前旧行；新查询端点降采样后供前端 SVG。

**Tech Stack:** ESP-IDF v6.0 C++ / Rust staticlib（host 桩测试）/ FastAPI + SQLite / React + 手绘 SVG。

**Spec:** `docs/superpowers/specs/2026-09-22-battery-reporting-design.md`

## Global Constraints

- Rust 管 policy、C++ 管 mechanism：采样判定与 URL 拼接在 Rust；ADC/charge 读取在 C++。
- 缺键语义与 `rr` 一致：`mv=0`/GET 无 `v` 键 = 没有样本，**不是 0**；服务端不追加、快照不加字段。
- charge 编码（spec 定稿）：`0=unknown, 1=no-power, 2=charging, 3=full, 4=discharging`。
- 服务端校验：`mv ∈ [2500,5000]`、`pct ∈ [0,100]`、`charge ∈ [0,4]`，越界整点丢弃。
- 历史保留 90 天（`ts < now - 90*86400` 删除），插入时顺手 PURGE。
- `power-history` 鉴权走 operator token（同 `/api/devices`）；`hours` 默认 24、上限 2160。
- 服务端路由写在 `create_app()` 内 `@app.get` 直挂（无 APIRouter），端点命名 `/api/...`。
- 每个 server 端点同 commit 带 pytest；Rust 测试 host 桩驱动，不碰真机。
- 服务端改动验证后 `systemctl --user restart youn-ink-server`。
- 测试命令：server `cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/ -q`；rust `export PATH="$HOME/.cargo/bin:$PATH" && cd firmware/main/rust && cargo test`。
- 固件最终构建门禁（Task 6）：主会话 `export PATH="$HOME/.cargo/bin:$PATH"; source ~/data/esp-idf-v6.0/export.sh; cd firmware && idf.py build`，成功标准 `Project build complete` + 零 `undefined reference`。
- 编辑纪律：同文件多处改动优先整块重写；每次 edit 后复读 + `git diff` 核删减行。

---

### Task 1: 固件 Rust 侧——battery 采样 FFI + schedule URL 拼接

**Files:**
- Modify: `firmware/main/rust/src/shim.rs`（extern 块 :96-99 附近 + host 桩区 :330-395 附近）
- Modify: `firmware/main/rust/src/page_sync.rs`（`fetch_schedule` :168-180 附近）
- Test: `firmware/main/rust/src/page_sync.rs` 测试模块（:1640-1730 既有测试旁）

**Interfaces:**
- Consumes: 既有 `shim::rf_power_counters`、`shim::rf_last_reset_reason` 模式。
- Produces: `shim::rf_battery_sample(mv: *mut u16, pct: *mut u8, charge: *mut u8) -> i32`（1=有样本，0=无）；host 桩 `host::set_battery_sample(mv: u16, pct: u8, charge: u8)` 与 `host::set_battery_sample_none()`；URL 新增参数 `v`(mv)/`p`(pct)/`c`(charge)。

- [ ] **Step 1: 写失败测试**（加在 `schedule_url_carries_the_last_reset_reason` 旁）

```rust
    #[test]
    fn schedule_url_carries_battery_sample_when_one_exists() {
        let _g = shim::host::lock();
        shim::host::set_counters(7, 1234, 567, 2, 890);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample(3980, 76, 4); // discharging
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("v=3980&p=76&c=4")),
                "battery sample missing from the schedule URL: {gets:?}");
    }

    #[test]
    fn schedule_url_omits_battery_params_when_no_sample() {
        // mv=0 / 未设样本 = 传感器缺席或缺电读数：缺键而非零值（与 rr 语义一致）。
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample_none();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        let sched = gets.iter().find(|c| c.contains("/api/pages/schedule?"))
            .expect("schedule GET");
        assert!(!sched.contains("v=") && !sched.contains("&p=") && !sched.contains("&c="),
                "no-sample must omit v/p/c entirely: {sched}");
    }
```

- [ ] **Step 2: 跑红** — `cargo test schedule_url_carries_battery` → 编译失败（`set_battery_sample` 未定义）。先在 host 桩区加：

```rust
    /// Scripted battery sample for the `?v=&p=&c=` query params. `None` = no
    /// valid reading this cycle (sensor absent, ADC failure, or mains-powered
    /// skip) — the URL omits the params entirely rather than sending zeros.
    static BATTERY: Mutex<Option<(u16, u8, u8)>> = Mutex::new(None);

    pub fn set_battery_sample(mv: u16, pct: u8, charge: u8) {
        *BATTERY.lock().unwrap_or_else(|e| e.into_inner()) = Some((mv, pct, charge));
    }
    pub fn set_battery_sample_none() {
        *BATTERY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_battery_sample(mv: *mut u16, pct: *mut u8, charge: *mut u8) -> i32 {
        let b = *BATTERY.lock().unwrap_or_else(|e| e.into_inner());
        match b {
            Some((m, p, c)) => {
                unsafe {
                    if !mv.is_null() { *mv = m; }
                    if !pct.is_null() { *pct = p; }
                    if !charge.is_null() { *charge = c; }
                }
                1
            }
            None => 0,
        }
    }
```

- [ ] **Step 3: 跑红（链接期）** — 测试仍红：URL 断言失败或 `rf_battery_sample` 未在 extern 块声明。在 shim.rs extern 块 `rf_last_reset_reason() -> u32;` 后加：

```rust
    pub fn rf_battery_sample(mv: *mut u16, pct: *mut u8, charge: *mut u8) -> i32;
```

- [ ] **Step 4: 实现 URL 拼接**（`fetch_schedule`，`rr` 写入行之后）

```rust
    let mut battery = [0u16 as u32; 0]; // placeholder removed — see real code below
```
实际代码（替换上面占位，写入 `path` 于 `rr` 之后）：

```rust
    let mut b_mv: u16 = 0;
    let mut b_pct: u8 = 0;
    let mut b_chg: u8 = 0;
    if unsafe { shim::rf_battery_sample(&mut b_mv, &mut b_pct, &mut b_chg) } == 1 && b_mv > 0 {
        let _ = write!(path, "&v={}&p={}&c={}", b_mv, b_pct, b_chg);
    }
```
（删除 Step 4 开头的占位行；`path` 容量 160 足够：既有串 ~40 字节 + 电池 ~18 字节。）

- [ ] **Step 5: 跑绿** — `cargo test` 全量（基线 179+4）全绿。

- [ ] **Step 6: 提交**

```bash
git add firmware/main/rust/src/shim.rs firmware/main/rust/src/page_sync.rs
git commit -m "feat(firmware): schedule GET 搭车电池采样 ?v=&p=&c=（host 桩 TDD）"
```

### Task 2: 固件 C++ 侧——rf_battery_sample 真实现（机制层）

**Files:**
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc`（`ReadBatteryStatus` :567 附近后新增导出函数）
- Modify: `firmware/main/rust/shim.cpp`（`rf_last_reset_reason` :411 后新增弱链接采样器）
- Test: 无单测（C++ 机制层无 host 测试设施，靠 Task 6 编译门禁 + 真机）

**Interfaces:**
- Consumes: 板级 `ReadBatteryStatus(uint16_t&, uint8_t&)`、`ChargeStatus::Snapshot`。
- Produces: `extern "C" int ZectrixReadBatterySample(uint16_t* mv, uint8_t* pct, uint8_t* charge)`（1=成功）；shim.cpp 内 `rf_battery_sample` 改调它（host 桩由 Rust cfg 控制，二者不共存——见 Step 2 说明）。

- [ ] **Step 1: 板级导出采样函数**（zectrix-s3-epaper-4.2.cc，`ReadBatteryPercentForFactoryTest` 之后，紧邻既有 `ZectrixReadBatteryPercentForFactoryTest` 导出模式 :628）

```cpp
extern "C" bool ZectrixReadBatterySample(uint16_t* mv, uint8_t* pct, uint8_t* charge) {
    if (mv == nullptr || pct == nullptr || charge == nullptr) return false;
    ZectrixBoard* board = ZectrixBoard::GetInstance();  // 既有单例访问模式，见文件内其他导出
    charge_status_.Tick(GetNowMs());  // 若非成员可达则走 board 公有刷新入口
    ChargeStatus::Snapshot s = charge_status_.Get();
    uint16_t v = 0; uint8_t p = 0;
    if (!ReadBatteryStatus(v, p)) return false;
    *mv = v; *pct = p;
    // charge 编码（spec）: 0=unknown 1=no-power 2=charging 3=full 4=discharging
    if (s.charging && !s.full)       *charge = 2;
    else if (s.full)                 *charge = 3;
    else if (!s.power_present)       *charge = 4;
    else                             *charge = 0;
    return true;
}
```
注意：实现者先读文件确认单例与成员可见性的真实写法（`ZectrixReadBatteryPercentForFactoryTest` :628 如何拿到 board 内部就照抄），上面 charge 分支语义必须保留：**power_present=0 且非充电 ⇒ 放电(4)**；full 优先于 charging。

- [ ] **Step 2: shim.cpp 接线**（`rf_last_reset_reason` 之后）。关键结构：`rf_battery_sample` 的 host 桩在 Rust（Task 1），真机实现在 C++——用既有弱符号模式（本文件 `Board::GetInstance()` 直调先例 :58/:388），**C++ 版仅设备构建需要，host 桩 Rust `#[unsafe(no_mangle)]` 在固件链接时会与 C++ 强符号冲突**。因此 Rust host 桩加 `#[cfg(test)]`（cargo test 走桩、固件链接走 C++）——回 Task 1 文件给桩加该属性并 `cargo test` 复跑确认仍绿：

```cpp
// Battery telemetry: page_sync rides ?v=&p=&c= on the schedule GET. Values
// come from the board's ADC + charge snapshot; 0 return = no valid reading
// (no battery / ADC failure), which Rust maps to "omit the params".
extern "C" int rf_battery_sample(uint16_t* mv, uint8_t* pct, uint8_t* charge) {
    return ZectrixReadBatterySample(mv, pct, charge) ? 1 : 0;
}
```
（若链接报 `ZectrixReadBatterySample` 未定义：该符号按板目录条件编译，本板 CMake 已编入 main；实现者以 `idf.py build` 实际报错为准，必要时在 shim.cpp 用 `extern "C" bool ZectrixReadBatterySample(...)` 前向声明。）

- [ ] **Step 3: mains 跳过采样**（spec：插电不采样）。在 `ZectrixReadBatterySample` 开头加：mains/充电中直接 `return false`（读 `s.power_present`，true 即不产样本——充电段曲线本来就该是空档）。把 charge 分支简化为只在 `!s.power_present` 时继续读 ADC。

- [ ] **Step 4: 提交**

```bash
git add firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc firmware/main/rust/shim.cpp firmware/main/rust/src/shim.rs
git commit -m "feat(firmware): 板级电池采样 ZectrixReadBatterySample + shim 接线（充电/插电不产样本）"
```

### Task 3: 服务端——battery_history 表 + schedule GET 解析入库

**Files:**
- Modify: `server/youn_server/devices.py`（schema/迁移区 :107-130）
- Modify: `server/youn_server/app.py`（schedule 处理器 power 段 :516-557 之后）
- Modify: `server/youn_server/registry.py` 若 devices.py 的类名是 Registry（实现者按实际类落方法）
- Test: Create `server/tests/test_battery_history.py`

**Interfaces:**
- Consumes: 既有 `registry.set_power_counters/get_power_counters`、`_require_device_token`、power dict（`w/a/r/g/f` 已解析）。
- Produces: `registry.add_battery_sample(device_id: str, ts: int, mv: int, pct: int, charge: int, counters: dict) -> None`；`registry.battery_history(device_id: str, since_ts: int) -> list[dict]`（升序，dict 含 ts/mv/pct/charge/五计数器键）；表 `battery_history(device_id, ts, mv, pct, charge, wakes, awake_ms, radio_ms, http_gets, refresh_submit_ms, PRIMARY KEY(device_id, ts))`。

- [ ] **Step 1: 写失败测试**（沿 `tests/test_power_counters.py` 的 fixture 风格——实现者先读该文件拿 `registry` fixture 写法照抄）

```python
"""battery_history 表 + schedule GET ?v=&p=&c= 入库（spec 2026-09-22）。"""
import time
import pytest

pytestmark = pytest.mark.usefixtures("client")  # 按 test_power_counters.py 实际 fixture 调整


def _get_schedule(client, query=""):
    return client.get(f"/api/pages/schedule{query}",
                      headers={"X-Device-Token": client.device_token})


def test_full_battery_params_insert_one_row(client, registry):
    now = int(time.time())
    r = _get_schedule(client, f"?w=3&a=900&r=400&g=1&f=0&rr=3&v=3980&p=76&c=4")
    assert r.status_code == 200
    rows = registry.battery_history(client.device_id, since_ts=now - 60)
    assert len(rows) == 1
    row = rows[0]
    assert (row["mv"], row["pct"], row["charge"]) == (3980, 76, 4)
    assert row["wakes"] == 3 and row["awake_ms"] == 900 and row["radio_ms"] == 400


def test_missing_battery_params_change_nothing(client, registry):
    now = int(time.time())
    assert _get_schedule(client, "?w=1&a=2&r=3&g=4&f=5&rr=3").status_code == 200
    assert registry.battery_history(client.device_id, since_ts=now - 60) == []


def test_partial_battery_params_change_nothing(client, registry):
    now = int(time.time())
    assert _get_schedule(client, "?w=1&v=3900&p=70").status_code == 200  # 缺 c
    assert registry.battery_history(client.device_id, since_ts=now - 60) == []


def test_out_of_range_values_dropped(client, registry):
    now = int(time.time())
    for bad in ("?v=100&p=50&c=4", "?v=6000&p=50&c=4", "?v=3900&p=101&c=4",
                "?v=3900&p=50&c=9"):
        assert _get_schedule(client, bad).status_code == 200
    assert registry.battery_history(client.device_id, since_ts=now - 60) == []


def test_purge_drops_rows_older_than_90_days(client, registry):
    now = int(time.time())
    old = now - 91 * 86400
    registry.add_battery_sample(client.device_id, old, 4000, 90, 4, {})
    _get_schedule(client, "?w=1&a=1&r=1&g=1&f=0&v=3980&p=76&c=4")
    rows = registry.battery_history(client.device_id, since_ts=now - 100 * 86400)
    assert [r["ts"] for r in rows] == [now] or all(
        r["ts"] >= now - 90 * 86400 - 5 for r in rows)


def test_snapshot_gains_battery_fields_only_with_params(client, registry):
    _get_schedule(client, "?w=1&a=2&r=3&g=4&f=5&v=3980&p=76&c=4")
    snap = registry.get_power_counters(client.device_id)
    assert snap["battery_mv"] == 3980 and snap["battery_pct"] == 76
    assert snap["battery_charge"] == 4
```
（fixture 名以 `test_power_counters.py` / `tests/conftest.py` 实际为准；`client.device_token` 若不存在按该文件既有的 token 注入方式改写。断言不得改：四键齐备才入库、缺/部分/越界全阴性、90 天 PURGE、快照三字段。）

- [ ] **Step 2: 跑红** — `pytest tests/test_battery_history.py -q` → FAIL（`battery_history` 属性不存在）。

- [ ] **Step 3: 实现 devices.py**（迁移区 `power_counters` ALTER 之后）

```python
        # Battery telemetry history (spec 2026-09-22): one row per wake that
        # carried ?v=&p=&c= on the schedule GET, plus that GET's power-counter
        # snapshot so adjacent-row deltas explain the discharge rate.
        self._conn.execute(
            """CREATE TABLE IF NOT EXISTS battery_history (
                   device_id TEXT NOT NULL,
                   ts INTEGER NOT NULL,
                   mv INTEGER NOT NULL,
                   pct INTEGER NOT NULL,
                   charge INTEGER NOT NULL,
                   wakes INTEGER NOT NULL DEFAULT 0,
                   awake_ms INTEGER NOT NULL DEFAULT 0,
                   radio_ms INTEGER NOT NULL DEFAULT 0,
                   http_gets INTEGER NOT NULL DEFAULT 0,
                   refresh_submit_ms INTEGER NOT NULL DEFAULT 0,
                   PRIMARY KEY (device_id, ts)
               )""")
```

Registry 方法（放 `get_power_counters` 之后，同一把 `self._lock`）：

```python
    _BATTERY_VALID = ("mv", 2500, 5000), ("pct", 0, 100), ("charge", 0, 4)

    def add_battery_sample(self, device_id: str, ts: int, mv: int, pct: int,
                           charge: int, counters: dict) -> None:
        """Append one battery sample; drop out-of-range values silently."""
        for name, lo, hi in (("mv", 2500, 5000), ("pct", 0, 100), ("charge", 0, 4)):
            v = {"mv": mv, "pct": pct, "charge": charge}[name]
            if not (lo <= v <= hi):
                return
        c = counters or {}
        with self._lock:
            self._conn.execute(
                "INSERT OR REPLACE INTO battery_history VALUES (?,?,?,?,?,?,?,?,?,?)",
                (device_id, ts, mv, pct, charge, c.get("wakes", 0),
                 c.get("awake_ms", 0), c.get("radio_ms", 0), c.get("http_gets", 0),
                 c.get("refresh_submit_ms", 0)))
            self._conn.execute(
                "DELETE FROM battery_history WHERE device_id = ? AND ts < ?",
                (device_id, ts - 90 * 86400))

    def battery_history(self, device_id: str, since_ts: int) -> list:
        with self._lock:
            cur = self._conn.execute(
                "SELECT ts, mv, pct, charge, wakes, awake_ms, radio_ms, http_gets,"
                " refresh_submit_ms FROM battery_history"
                " WHERE device_id = ? AND ts >= ? ORDER BY ts", (device_id, since_ts))
            return [dict(zip([d[0] for d in cur.description], row)) for row in cur.fetchall()]
```
（删掉类属性 `_BATTERY_VALID` 占位——范围校验就地写在方法内，别留死代码。）

- [ ] **Step 4: 实现 app.py 解析**（`set_power_counters` 调用之后、`entries = ...` 之前）

```python
        # Battery telemetry (spec 2026-09-22): all three keys present and
        # in-range → one history row carrying this GET's counter snapshot.
        # Anything less = old firmware or bad reading: no row, no snapshot
        # fields (same absent-means-absent semantics as `rr`).
        try:
            v, p, c = int(qp["v"]), int(qp["p"]), int(qp["c"])
        except (KeyError, TypeError, ValueError):
            v = 0
        if v:
            registry.add_battery_sample(dev.device_id, int(time.time()), v, p, c, power)
            power["battery_mv"], power["battery_pct"], power["battery_charge"] = v, p, c
```

- [ ] **Step 5: 跑绿** — `pytest tests/test_battery_history.py -q` 全绿；再全量 `pytest tests/ -q` 确认基线 282+ 无回归。

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/devices.py server/youn_server/app.py server/tests/test_battery_history.py
git commit -m "feat(server): battery_history 表 + schedule GET 电池参数入库（TDD，含 PURGE 与越界阴性对照）"
```

### Task 4: 服务端——power-history 查询端点（降采样 + 鉴权）

**Files:**
- Modify: `server/youn_server/app.py`（devices 路由区，`create_app()` 内）
- Test: Modify `server/tests/test_battery_history.py`（追加）

**Interfaces:**
- Consumes: Task 3 `registry.battery_history(device_id, since_ts)`。
- Produces: `GET /api/devices/{device_id}/power-history?hours=N` → `{"points": [...]}`（升序，每点 `ts/mv/pct/charge/wakes/awake_ms/radio_ms/http_gets/refresh_submit_ms`）；>500 点按桶平均、charge 多数位、计数器取桶末值。

- [ ] **Step 1: 写失败测试**（追加到 test_battery_history.py）

```python
def test_power_history_requires_operator_token(client):
    r = client.get(f"/api/devices/{client.device_id}/power-history")
    assert r.status_code == 401


def test_power_history_returns_points_ascending(client, registry):
    import time as _t
    now = int(_t.time())
    for i, (mv, p) in enumerate([(4000, 90), (3900, 80), (3800, 70)]):
        registry.add_battery_sample(client.device_id, now - 300 + i * 60, mv, p, 4,
                                    {"wakes": i, "awake_ms": 100 * i})
    r = client.get(f"/api/devices/{client.device_id}/power-history?hours=1",
                   headers={"X-Operator-Token": client.operator_token})
    assert r.status_code == 200
    pts = r.json()["points"]
    assert [p["mv"] for p in pts] == [4000, 3900, 3800]
    assert pts[0]["awake_ms"] == 0 and pts[2]["awake_ms"] == 200


def test_power_history_downsamples_over_500_points(client, registry):
    import time as _t
    now = int(_t.time())
    for i in range(600):
        registry.add_battery_sample(client.device_id, now - 600 * 60 + i * 60,
                                    4000 - i // 10, 90, 4, {})
    r = client.get(f"/api/devices/{client.device_id}/power-history?hours=12",
                   headers={"X-Operator-Token": client.operator_token})
    assert r.status_code == 200
    assert len(r.json()["points"]) <= 500


def test_power_history_hours_cap_and_default(client):
    r = client.get(f"/api/devices/{client.device_id}/power-history?hours=99999",
                   headers={"X-Operator-Token": client.operator_token})
    assert r.status_code == 200  # 超上限钳到 2160，不报错
```
（`client.operator_token` 按 conftest 实际 fixture 改写——conftest 里 operator_token 被置空的话用测试内显式 token header，参考 `test_device_last_seen.py` 的做法。）

- [ ] **Step 2: 跑红** — 404（端点不存在）。

- [ ] **Step 3: 实现**（app.py devices 路由区，`@app.get` 直挂）

```python
    @app.get("/api/devices/{device_id}/power-history")
    async def get_power_history(device_id: str, request: Request,
                                hours: int = 24) -> dict:
        _require_operator(request)
        hours = max(1, min(hours, 2160))
        since = int(time.time()) - hours * 3600
        pts = registry.battery_history(device_id, since)
        if len(pts) > 500:
            pts = _downsample_battery(pts, 500)
        return {"points": pts}
```
模块级辅助（app.py 顶部函数区）：

```python
def _downsample_battery(points: list, buckets: int) -> list:
    """Time-bucket average; charge = majority in bucket, counters = bucket-last
    (keeps deltas monotonic non-decreasing across bucket boundaries)."""
    span = points[-1]["ts"] - points[0]["ts"] or 1
    width = span / buckets
    out, cur, cur_ts = [], [], None
    for p in points:
        b = int((p["ts"] - points[0]["ts"]) / width)
        if cur and b != cur_ts:
            out.append(_merge_bucket(cur))
            cur = []
        cur_ts = b
        cur.append(p)
    if cur:
        out.append(_merge_bucket(cur))
    return out


def _merge_bucket(bucket: list) -> dict:
    charges = [p["charge"] for p in bucket]
    charge = max(set(charges), key=charges.count)
    merged = {"ts": bucket[-1]["ts"], "charge": charge}
    for k in ("mv", "pct"):
        merged[k] = round(sum(p[k] for p in bucket) / len(bucket))
    for k in ("wakes", "awake_ms", "radio_ms", "http_gets", "refresh_submit_ms"):
        merged[k] = bucket[-1][k]
    return merged
```
（`_require_operator` 名按 app.py 既有鉴权辅助实际名称调整——实现者先 `grep -n "operator" server/youn_server/app.py | head` 找到真实辅助再落笔；若无现成辅助，照 `_require_device_token` 的 header 校验写一个只查 operator token 的最小版本。）

- [ ] **Step 4: 跑绿** — 新 4 条 + Task 3 全部绿；全量 pytest 基线无回归。

- [ ] **Step 5: 重启 + 活体冒烟** — `systemctl --user restart youn-ink-server`；`curl -s -H "X-Operator-Token: $TOK" 'http://127.0.0.1:9002/api/devices/NOTE4C-3400FC/power-history?hours=24'` → `{"points": []}`（新表尚无数据即空数组，非 5xx）。

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/app.py server/tests/test_battery_history.py
git commit -m "feat(server): GET /api/devices/{id}/power-history（鉴权+90天上限+>500点降采样）"
```

### Task 5: 前端——设备页 SVG 电量曲线

**Files:**
- Modify: `frontend/src/pages/Devices.jsx`（设备卡片内加 `<BatterySpark device={...}/>`）
- Modify: `frontend/src/lib/api.js`（若 API 封装层存在则加 `powerHistory(id, hours)`；不存在按 Devices.jsx 既有 fetch 模式直调）

**Interfaces:**
- Consumes: Task 4 端点 `{points: [{ts, mv, pct, charge, ...}]}`。
- Produces: 组件 `BatterySpark({ deviceId })`（本文件内定义即可，不外导）。

- [ ] **Step 1: 实现组件**（Devices.jsx 底部；窗口切换 24h/7d/30d/90d；零图表库）

```jsx
function BatterySpark({ deviceId }) {
  const [pts, setPts] = useState(null);
  const [hours, setHours] = useState(24);
  useEffect(() => {
    let alive = true;
    fetch(`/api/devices/${deviceId}/power-history?hours=${hours}`,
          { headers: { 'X-Operator-Token': localStorage.getItem('operatorToken') || '' } })
      .then(r => r.ok ? r.json() : { points: [] })
      .then(d => alive && setPts(d.points))
      .catch(() => alive && setPts([]));
    return () => { alive = false; };
  }, [deviceId, hours]);
  const W = 320, H = 60, PAD = 2;
  const span = (pts && pts.length > 1) ? pts[pts.length - 1].ts - pts[0].ts : 1;
  const xy = p => [PAD + (p.ts - pts[0].ts) / span * (W - 2 * PAD),
                   H - PAD - (p.mv - 3300) / (4200 - 3300) * (H - 2 * PAD)];
  // 放电段黑色折线，充电段（charge 2/3）断开——空档自然断线
  let segs = [], cur = [];
  (pts || []).forEach(p => {
    if (p.charge === 2 || p.charge === 3) { if (cur.length > 1) segs.push(cur); cur = []; }
    else cur.push(p);
  });
  if (cur.length > 1) segs.push(cur);
  return (
    <div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <span style={{ fontSize: 12, color: 'var(--color-text-secondary, #666)' }}>电量曲线</span>
        {[24, 168, 720, 2160].map(h => (
          <button key={h} onClick={() => setHours(h)}
                  style={{ fontWeight: h === hours ? 'bold' : 'normal', fontSize: 12 }}>
            {h === 24 ? '24h' : h === 168 ? '7d' : h === 720 ? '30d' : '90d'}
          </button>
        ))}
      </div>
      <svg width={W} height={H} role="img" aria-label="电池电压曲线">
        {pts === null && <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="12">加载中…</text>}
        {pts !== null && pts.length < 2 &&
          <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="12">暂无数据（新固件生效后逐点累积）</text>}
        {segs.map((s, i) => (
          <polyline key={i} fill="none" stroke="#000" strokeWidth="1.5"
                    points={s.map(p => xy(p).join(',')).join(' ')} />
        ))}
      </svg>
    </div>
  );
}
```
（`useState/useEffect` 若未 import 则补进文件顶部 React import；token 的 localStorage 键名以 `api.js` 现状为准，先读再写。）

- [ ] **Step 2: 挂载**（设备卡片：power 快照展示点旁 `<BatterySpark deviceId={device.device_id} />`；同时把快照 `battery_pct` 若存在则显示为「电池 xx%」——走 `device.power` JSON 的三个 battery_* 字段，缺失时整段不渲染，旧行为不变）。

- [ ] **Step 3: 构建验证** — `cd frontend && npm run build` exit 0；对 `:9002` 实 FastAPI 手动验证（`vite preview` 不继承 proxy，不采用）。

- [ ] **Step 4: 提交**

```bash
git add frontend/src/pages/Devices.jsx frontend/src/lib/api.js
git commit -m "feat(frontend): 设备页 SVG 电量曲线（24h/7d/30d/90d，零图表库）"
```

### Task 6: 全量验证 + 固件编译门禁

**Files:** 无新改动（验证任务；失败则回对应任务修复后重跑本任务）。

- [ ] **Step 1: Rust 全量** — `export PATH="$HOME/.cargo/bin:$PATH" && cd firmware/main/rust && cargo test` → 全绿（基线 179+4 + 新 2 条）。
- [ ] **Step 2: server 全量** — `cd server && PYTHONPATH=$PWD .venv/bin/python -m pytest tests/ -q` → 全绿（基线 282 + 新 9 条；MCP flake 允许孤立复跑）。
- [ ] **Step 3: 前端构建** — `npm run build` exit 0。
- [ ] **Step 4: 固件编译门禁（主会话）** —

```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware && idf.py build
```
成功标准：`Project build complete`、零 `undefined reference`、`xiaozhi.bin` ≈2.85–2.9 MB。符号级验证：`xtensa-esp32s3-elf-nm build/esp-idf/main/libmain.a | grep -i battery` 能看到 `rf_battery_sample`/`ZectrixReadBatterySample`。

- [ ] **Step 5: 真机验收清单（设备在线后，非本计划阻塞项）** —
  1. 烧录后首次电池供电轮询：schedule GET 带上 `?v=&p=&c=`（服务端日志/`power_counters` 快照三字段确认）。
  2. 插电期间：GET 不带 v/p/c（曲线空档）。
  3. 24h 后前端曲线成形，放电斜率与 `radio_ms/awake_ms` 增量方向一致。

- [ ] **Step 6: 收尾提交（若有 Task 1-5 修复回灌）+ 推送**（推送前问用户，沿会话纪律）。
