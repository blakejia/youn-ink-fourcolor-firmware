# 设备日志上报实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 NOTE4C 加一条按设备、默认关闭的固件日志上行通道：设备把最近日志行 POST 到服务端落盘成纯文本，后台可开可看；崩溃前的日志因置于 `.rtc_noinit` 而存活。

**Architecture:** C++（`shim_log.cpp`）持有 `.rtc_noinit` 环形缓冲并挂 `esp_log_set_vprintf` 钩子；Rust（`log_upload_policy.rs`）决定何时上、上多少、退避；服务端按设备存开关（随 schedule 响应下发）并追加写纯文本文件；前端设备页给开关与查看弹窗。

**Tech Stack:** ESP-IDF v6.0 / C++ / Rust（`no_std`，单一 `librust_firmware.a`）/ FastAPI + SQLite / React 18 + Vite。

**Spec:** `docs/superpowers/specs/2026-09-28-device-log-upload-design.md`

## Global Constraints

- 固件构建必须 `IDF_TARGET=esp32s3`（裸 `idf.py build` 默认 esp32，会在 SSD2683 面板依赖处失败）。
- `export PATH="$HOME/.cargo/bin:$PATH"` 必须在 `source ~/data/esp-idf-v6.0/export.sh` **之前**。
- Rust 不访问 GPIO/I2C/SPI/NVS/FreeRTOS/Wi-Fi/HTTP/EPD；唯一的 ESP-IDF 接触面是 `rust/src/shim.rs`。
- 单一 Rust archive：不新增 `#[panic_handler]`；`crate-type = ["staticlib", "rlib"]` 不动。
- 新 `.rs` 必须登记进 `firmware/main/CMakeLists.txt` 的 `RUST_SOURCES`；新 `.cpp` 必须登记进同文件 `SOURCES`——两处都是显式列表，非 glob。
- `#[repr(C)]` POD + 显式 `_pad`，每模块自带布局契约测试（不抽共享 `abi_contract.rs`）。
- 哨兵变异必须只打掉对应分支；哨兵没红说明测试是假的。
- 服务端测试 fixture 必须隔离新目录（`conftest.py` 已隔离 `devices_db`/`pages/`/日志）。
- 不新增 formatter/linter/CI/pre-commit；只收敛自己新写的代码。
- 前端：`npm run build` 产出 `frontend/dist`；**重建后必须重启服务端**（dist 在 app 启动时挂载）。
- 不推送 git，除非用户明确说推。

---

## 文件结构

| 文件 | 责任 |
| --- | --- |
| `server/youn_server/config.py`（改） | 新配置 `device_log_dir`，并加入 `resolve_paths` 与 mkdir 循环 |
| `server/youn_server/devices.py`（改） | `devices.log_upload` 列的读写（照 `power_counters`） |
| `server/youn_server/devicelog.py`（新） | 追加写 + 轮转 + tail 读取；**唯一**碰磁盘的模块 |
| `server/youn_server/app.py`（改） | `POST /api/device-log`、`GET /api/devices/{id}/logs`、`policy.log_upload` 下发、开关读写端点 |
| `server/tests/test_device_log.py`（新） | 服务端全部用例 |
| `firmware/main/rust/include/log_upload_policy.h`（新） | ABI 定义（Rust 策略输入/输出） |
| `firmware/main/rust/src/log_upload_policy.rs`（新） | 纯策略：决策表、退避、分段 |
| `firmware/main/rust/tests/log_upload_policy.rs`（新） | 策略单测 + 布局契约 |
| `firmware/main/rust/include/shim_log.h`（新） | 环形缓冲 C ABI 声明 |
| `firmware/main/rust/shim_log.cpp`（新） | `.rtc_noinit` 缓冲、锁、`esp_log_set_vprintf` 钩子、链式转发 |
| `firmware/main/rust/src/shim.rs`（改） | 声明 `rf_logbuf_*` 外部符号 |
| `firmware/main/application.cc`（改） | 装配：装钩子、解析开关、周期内调用上报 |
| `firmware/main/CMakeLists.txt`（改） | 登记新 `.rs`（`RUST_SOURCES`）与新 `.cpp`（`SOURCES`） |
| `frontend/src/api.js`（改） | 两个新方法 |
| `frontend/src/pages/Devices.jsx`（改） | 开关列 + 日志弹窗 |
| `.gitignore`（改） | 忽略 `server/data/devicelogs/` |

---

## Task 1: 服务端存储层 `devicelog.py`

**Files:**
- Create: `server/youn_server/devicelog.py`
- Modify: `server/youn_server/config.py`（新增 `device_log_dir`；加入 `resolve_paths` 列表与 mkdir 循环）
- Test: `server/tests/test_device_log.py`

**Interfaces:**
- Consumes: `settings.device_log_dir`（Path）
- Produces:
  - `MAX_LINES_PER_REQUEST: int = 1000`
  - `append_lines(device_id: str, received_iso: str, text: str, dropped: int) -> int` — 追加，返回写入行数
  - `tail_lines(device_id: str, n: int) -> tuple[list[str], bool]` — 返回 (行, 是否被截断)

- [ ] **Step 1: 写失败测试**

创建 `server/tests/test_device_log.py`：

```python
"""Device log upload: storage layer (devicelog.py) and HTTP endpoints.

Isolation: ``isolate_pages_dir`` swaps data_dir per test; this module points
``settings.device_log_dir`` at the same temp dir so nothing lands in the
running server's data/devicelogs.
"""
from __future__ import annotations

import tempfile
from pathlib import Path

import pytest

from youn_server.config import settings
from youn_server import devicelog


@pytest.fixture(autouse=True)
def _isolate_device_log_dir():
    """Point device_log_dir at a fresh temp dir and restore after."""
    tmp = tempfile.mkdtemp(prefix="devicelog_test_")
    orig = settings.device_log_dir
    settings.device_log_dir = Path(tmp)
    yield
    settings.device_log_dir = orig


def test_append_then_tail_round_trips():
    written = devicelog.append_lines(
        "NOTE4C-TEST", "2026-09-28T12:30:11+08:00",
        "I (27930) CustomLcdDisplay: EPD busy wait: 15000 ms\n"
        "W (27931) CustomLcdDisplay: EPD busy wait: 20000 ms\n",
        dropped=0,
    )
    assert written == 2
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert truncated is False
    assert lines[0].endswith("CustomLcdDisplay: EPD busy wait: 15000 ms")
    assert lines[0].startswith("2026-09-28T12:30:11+08:00 ")
    assert lines[1].endswith("EPD busy wait: 20000 ms")


def test_dropped_marker_is_written():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=7)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped 7 lines" in text


def test_no_dropped_marker_when_zero():
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", "line a\n", dropped=0)
    text = (settings.device_log_dir / "NOTE4C-TEST.log").read_text()
    assert "dropped" not in text


def test_tail_reports_truncation():
    body = "".join(f"line {i}\n" for i in range(50))
    devicelog.append_lines("NOTE4C-TEST", "2026-09-28T12:30:11+08:00", body, dropped=0)
    lines, truncated = devicelog.tail_lines("NOTE4C-TEST", 10)
    assert len(lines) == 10
    assert truncated is True
    assert lines[-1].endswith("line 49")


def test_tail_unknown_device_is_empty():
    lines, truncated = devicelog.tail_lines("NO-SUCH-DEVICE", 10)
    assert lines == []
    assert truncated is False


def test_device_id_with_path_separator_is_rejected():
    """A device id is a filename component; traversal must not reach disk."""
    with pytest.raises(ValueError):
        devicelog.append_lines("../escape", "2026-09-28T12:30:11+08:00", "x\n", dropped=0)
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && .venv/bin/python -m pytest tests/test_device_log.py -q`
Expected: FAIL — `ModuleNotFoundError: No module named 'youn_server.devicelog'`

- [ ] **Step 3: 加配置**

`server/youn_server/config.py`，在 `serial_firmware_dir` 下方加：

```python
    # 设备日志上行落盘目录。与 server.log（服务端自身日志）分开：
    # 这是设备侧日志的镜像，按设备分文件，便于单独轮转与查看。
    device_log_dir: Path = Field(default=Path("./data/devicelogs"))
```

并把 `device_log_dir` 加进 `resolve_paths` 的字段元组与 `_load_settings` 的 mkdir 循环（两处均须改，否则首次写入 FileNotFoundError）：

```python
        for field in ("data_dir", "devices_db", "images_dir", "firmware_dir", "uploads_dir", "log_dir", "serial_firmware_dir", "device_log_dir"):
```

```python
    for d in (s.data_dir, s.images_dir, s.firmware_dir, s.uploads_dir, s.log_dir, s.serial_firmware_dir, s.device_log_dir):
```

- [ ] **Step 4: 实现 `devicelog.py`**

```python
"""Device-side log mirror: append-only plain text, one file per device.

Layout on disk::

    data/devicelogs/
        NOTE4C-3400FC.log          current
        NOTE4C-3400FC.log.1        previous (rotation)

Each line is ``<server receive time> <device log line>``. The leading stamp is
the *server's* clock; the ``(27930)`` inside the body is the device's uptime in
milliseconds, not a wall clock. They are different quantities and the UI says so.

Rotation is done here rather than with ``RotatingFileHandler``: appends are the
only operation, so a rename-and-reopen is simpler than the handler's rollover
machinery, and it keeps the ``dropped`` marker on the same write path.
"""
from __future__ import annotations

import os
from typing import List, Tuple

from .config import settings

#: Refuse a device id that is not a safe single filename component. The id
#: reaches us from the device token, but a traversal here would write outside
#: device_log_dir, so it is checked rather than trusted.
_UNSAFE = ("/", "\\", "..", "\x00")

MAX_LINES_PER_REQUEST = 1000
_ROTATE_BYTES = 5 * 1024 * 1024
_ROTATE_KEEP = 3


def _path_for(device_id: str) -> "os.PathLike[str] | str":
    if not device_id or any(bad in device_id for bad in _UNSAFE):
        raise ValueError("unsafe device id")
    return settings.device_log_dir / f"{device_id}.log"


def _rotate_if_needed(path) -> None:
    try:
        if path.stat().st_size < _ROTATE_BYTES:
            return
    except FileNotFoundError:
        return
    # Shift .N -> .N+1 from the top so the newest previous stays at .1.
    for n in range(_ROTATE_KEEP - 1, 0, -1):
        older = path.with_suffix(path.suffix + f".{n}")
        newer = path.with_suffix(path.suffix + f".{n + 1}")
        if older.exists():
            os.replace(older, newer)
    os.replace(path, path.with_suffix(path.suffix + ".1"))


def append_lines(device_id: str, received_iso: str, text: str, dropped: int) -> int:
    """Append ``text`` (one log line per \\n) stamped with ``received_iso``.

    Returns the number of non-empty device lines written. ``dropped`` > 0 first
    writes an explicit marker so a gap in the sequence is visible on disk
    instead of silent.
    """
    path = _path_for(device_id)
    settings.device_log_dir.mkdir(parents=True, exist_ok=True)
    _rotate_if_needed(path)

    lines = [ln for ln in text.split("\n") if ln.strip()]
    with open(path, "a", encoding="utf-8") as f:
        if dropped > 0:
            f.write(f"{received_iso} ... [dropped {dropped} lines]\n")
        for ln in lines:
            f.write(f"{received_iso} {ln}\n")
    return len(lines)


def tail_lines(device_id: str, n: int) -> Tuple[List[str], bool]:
    """Return the last ``n`` lines and whether older lines were omitted."""
    path = _path_for(device_id)
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            all_lines = f.read().splitlines()
    except FileNotFoundError:
        return [], False
    if n <= 0:
        return [], bool(all_lines)
    if len(all_lines) <= n:
        return all_lines, False
    return all_lines[-n:], True
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cd server && .venv/bin/python -m pytest tests/test_device_log.py -q`
Expected: PASS（6 passed）

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/devicelog.py server/youn_server/config.py server/tests/test_device_log.py
git commit -m "feat(server): device log mirror with rotation and tail"
```

---

## Task 2: 服务端端点与开关下发

**Files:**
- Modify: `server/youn_server/devices.py`（`log_upload` 读写）
- Modify: `server/youn_server/app.py`（两个端点 + `policy.log_upload`）
- Test: `server/tests/test_device_log.py`（追加）

**Interfaces:**
- Consumes: Task 1 的 `devicelog.append_lines` / `devicelog.tail_lines` / `MAX_LINES_PER_REQUEST`
- Produces:
  - `registry.set_log_upload(device_id: str, enabled: bool) -> None`
  - `registry.get_log_upload(device_id: str) -> bool`（未知设备返回 False）
  - HTTP `POST /api/device-log` → 201 `{"written": N}`
  - HTTP `POST /api/devices/{device_id}/log-upload` body `{"enabled": bool}` → 200 `{"log_upload": bool}`
  - HTTP `GET /api/devices/{device_id}/logs?tail=N` → 200 `{"lines": [...], "truncated": bool}`
  - schedule 响应 `policy.log_upload: 0|1`

- [ ] **Step 1: 写失败测试**

追加到 `server/tests/test_device_log.py`：

```python
import base64

from fastapi.testclient import TestClient

from youn_server.app import create_app
from .device_sig import signed_headers


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


@pytest.fixture()
def trusted_device(client):
    """Register + trust a device through the real pairing flow."""
    r = client.post("/api/devices/pair-start",
                    json={"device_id": "NOTE4C-TEST", "board_type": "NOTE4C"},
                    headers=signed_headers("NOTE4C-TEST"))
    assert r.status_code == 200
    code = r.json()["code"]
    client.post("/api/devices/pair-confirm",
                json={"device_id": "NOTE4C-TEST", "code": code},
                headers={"X-Operator-Token": ""})
    r = client.post("/api/devices/pair-claim",
                    json={"device_id": "NOTE4C-TEST", "code": code})
    return "NOTE4C-TEST", r.json()["token"]


def test_log_upload_defaults_to_off(client, trusted_device):
    device_id, _ = trusted_device
    r = client.get("/api/devices", headers={"X-Operator-Token": ""})
    dev = next(d for d in r.json()["devices"] if d["device_id"] == device_id)
    assert dev["log_upload"] == 0


def test_operator_can_enable_and_it_reaches_schedule_policy(client, trusted_device):
    device_id, token = trusted_device
    r = client.post(f"/api/devices/{device_id}/log-upload",
                    json={"enabled": True},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    assert r.json()["log_upload"] is True

    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["policy"]["log_upload"] == 1


def test_enabled_default_and_disabled_reaches_policy_as_zero(client, trusted_device):
    device_id, token = trusted_device
    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["policy"]["log_upload"] == 0


def test_device_log_requires_token(client, trusted_device):
    r = client.post("/api/device-log", json={"seq_hi": 1, "dropped": 0, "lines": ""})
    assert r.status_code == 401


def test_device_log_writes_base64_lines(client, trusted_device):
    device_id, token = trusted_device
    payload = base64.b64encode(b"I (1) Tag: hello\nW (2) Tag: world\n").decode()
    r = client.post("/api/device-log",
                    json={"seq_hi": 2, "dropped": 0, "lines": payload},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 201
    assert r.json()["written"] == 2
    text = (settings.device_log_dir / f"{device_id}.log").read_text()
    assert "Tag: hello" in text and "Tag: world" in text


def test_device_log_rejects_oversize_body(client, trusted_device):
    device_id, token = trusted_device
    big = base64.b64encode(b"x" * (4096 + 1)).decode()
    r = client.post("/api/device-log",
                    json={"seq_hi": 1, "dropped": 0, "lines": big},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 400


def test_device_log_rejects_bad_base64(client, trusted_device):
    device_id, token = trusted_device
    r = client.post("/api/device-log",
                    json={"seq_hi": 1, "dropped": 0, "lines": "!!!not base64!!!"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 400


def test_logs_tail_endpoint_returns_lines(client, trusted_device):
    device_id, token = trusted_device
    payload = base64.b64encode(b"a\nb\nc\n").decode()
    client.post("/api/device-log",
                json={"seq_hi": 3, "dropped": 0, "lines": payload},
                headers={"Authorization": f"Bearer {token}"})
    r = client.get(f"/api/devices/{device_id}/logs",
                   params={"tail": 2},
                   headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    body = r.json()
    assert len(body["lines"]) == 2
    assert body["lines"][-1].endswith("c")
    assert body["truncated"] is True
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && .venv/bin/python -m pytest tests/test_device_log.py -q`
Expected: FAIL — 新用例 404 / KeyError（端点与字段尚不存在）

- [ ] **Step 3: 加 `log_upload` 列与读写**

`server/youn_server/devices.py` 的 `__init__` 末尾（`battery_history` 迁移块之后）加列迁移，照 `power_counters` 的写法——`ALTER` + `duplicate column name` 守卫，既有数据库下次启动即获得该列：

```python
        # Per-device log-upload switch (spec 2026-09-28). Same idempotent
        # ALTER + duplicate-column guard as the power_counters migration.
        try:
            self._conn.execute(
                "ALTER TABLE devices ADD COLUMN log_upload INTEGER NOT NULL DEFAULT 0")
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
```

然后加两个方法：

```python
    def set_log_upload(self, device_id: str, enabled: bool) -> None:
        """Persist the per-device log-upload switch (0/1)."""
        with self._lock:
            self._conn.execute(
                "UPDATE devices SET log_upload = ? WHERE device_id = ?",
                (1 if enabled else 0, device_id),
            )

    def get_log_upload(self, device_id: str) -> bool:
        """Whether the device should upload logs. Unknown device => off."""
        with self._lock:
            row = self._conn.execute(
                "SELECT log_upload FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
        if row is None:
            return False
        try:
            return bool(row["log_upload"])
        except (IndexError, KeyError):
            return False
```

`Device` 数据类加字段 `log_upload: int = 0`（`devices.py` 的 `@dataclass` 块内，`trust` 之后）。`/api/devices` 用 `dict(d.__dict__)` 整体序列化（`app.py:209`），所以加字段即自动出现在响应里，**不需要改 `app.py` 的序列化**——`test_log_upload_defaults_to_off` 依赖这一点。同时确认设备行→`Device` 的构造处（`get`/`list_all`/`get_device_by_token` 共用的行映射）带上该列，否则读到的恒为默认值 0。

- [ ] **Step 4: 加两个端点与 policy 字段**

`server/youn_server/app.py`。顶部导入：

```python
import base64
from . import devicelog
```

在 `ack_notification` 附近加入：

```python
    @app.post("/api/device-log", status_code=201)
    async def device_log(request: Request, body: dict = Body(...)):
        dev = _require_device_token(request)
        try:
            seq_hi = int(body.get("seq_hi"))
            dropped = int(body.get("dropped", 0))
        except (TypeError, ValueError):
            raise HTTPException(400, "seq_hi and dropped must be integers")
        if dropped < 0:
            raise HTTPException(400, "dropped must be >= 0")
        raw = body.get("lines")
        if not isinstance(raw, str):
            raise HTTPException(400, "lines must be a string")
        try:
            text = base64.b64decode(raw, validate=True)
        except Exception:
            raise HTTPException(400, "lines must be valid base64")
        if len(text) > 4096:
            raise HTTPException(400, "decoded body too large")
        stamped = datetime.now(ZoneInfo(settings.canvas_timezone)).isoformat()
        written = devicelog.append_lines(
            dev.device_id, stamped, text.decode("utf-8", errors="replace"), dropped)
        return {"written": written}
```

```python
    @app.post("/api/devices/{device_id}/log-upload")
    async def set_device_log_upload(device_id: str, request: Request,
                                    body: dict = Body(...)):
        _require_operator(request)
        dev = registry.get(device_id)
        if dev is None:
            raise HTTPException(404, "unknown device")
        registry.set_log_upload(device_id, bool(body.get("enabled")))
        return {"log_upload": registry.get_log_upload(device_id)}
```

```python
    @app.get("/api/devices/{device_id}/logs")
    async def device_logs(device_id: str, request: Request, tail: int = Query(500)):
        _require_operator(request)
        n = max(1, min(int(tail), devicelog.MAX_LINES_PER_REQUEST))
        lines, truncated = devicelog.tail_lines(device_id, n)
        return {"lines": lines, "truncated": truncated}
```

`get_schedule` 的 `policy` 块加一行：

```python
                "log_upload": 1 if registry.get_log_upload(dev.device_id) else 0,
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cd server && .venv/bin/python -m pytest tests/test_device_log.py -q`
Expected: PASS（14 passed）

- [ ] **Step 6: 跑全量服务端测试确认没打破既有契约**

Run: `cd server && .venv/bin/python -m pytest tests/ -q`
Expected: PASS（302 + 14 = 316 左右）

- [ ] **Step 7: 从端点抓一次真实响应，确认 `policy` 字段形状**

Run（服务端已重启后）：

```bash
curl -s -H "Authorization: Bearer $DEV_TOKEN" http://127.0.0.1:9002/api/pages/schedule | python3 -c "import json,sys; print(json.load(sys.stdin)['policy']['log_upload'])"
```

Expected: 打印 `0` 或 `1`

- [ ] **Step 8: 提交**

```bash
git add server/youn_server/app.py server/youn_server/devices.py server/tests/test_device_log.py
git commit -m "feat(server): device-log endpoint, per-device switch, schedule policy"
```

---

## Task 3: 固件 C++ 环形缓冲与 ABI

**Files:**
- Create: `firmware/main/rust/include/shim_log.h`
- Create: `firmware/main/rust/shim_log.cpp`
- Modify: `firmware/main/CMakeLists.txt`（`SOURCES` 加 `"rust/shim_log.cpp"`）

**Interfaces:**
- Produces（供 Task 4 与 `shim.rs` 消费）：
  - `void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines)` — 取最早未 ack 的连续段，**不推进 tail**
  - `void rf_logbuf_ack(uint32_t seq_hi)` — 上报成功后才推进 tail
  - `void rf_logbuf_stats(uint32_t* dropped, uint32_t* used)`
  - `void rf_logbuf_install_hook(void)` — 装 `esp_log_set_vprintf` 钩子（幂等）

- [ ] **Step 1: 写头文件**

`firmware/main/rust/include/shim_log.h`：

```c
/* Device log ring buffer ABI.
 *
 * The buffer lives in `.rtc_noinit`, NOT `.rtc.data`. This is the whole point:
 * `cpu_start.c` memsets `.rtc_bss` on every non-deep-sleep reset (which is why
 * `w=1` followed the rr=4 panics), while `.rtc_noinit` is NOLOAD and nothing in
 * IDF clears it. So pre-crash lines survive a panic without any panic-path code.
 *
 * Buffer magic guards the other direction: after a power cycle `.rtc_noinit`
 * holds garbage, so a mismatched magic declares the buffer empty.
 */
#ifndef SHIM_LOG_H
#define SHIM_LOG_H

#include <stdint.h>

/* Line framing inside the ring: [seq:u32][len:u16][text:len].
 * `text` is the fully formatted log line (level, tag and uptime included),
 * produced by one vsnprintf in the vprintf hook. No tag/level fields: LOG V1
 * bakes them into the format string (see esp_log_format.h). */

/* Copy the oldest un-acked contiguous run into `out` (NUL-terminated).
 * Does NOT advance the tail: only rf_logbuf_ack does, so a failed upload
 * leaves the lines for the next attempt. */
void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines);

/* Advance the tail past every line with seq <= seq_hi. Call only after the
 * server accepted the payload. */
void rf_logbuf_ack(uint32_t seq_hi);

/* `dropped`: lines lost to ring overwrite since the last power cycle.
 * `used`: bytes currently held. */
void rf_logbuf_stats(uint32_t* dropped, uint32_t* used);

/* Install the esp_log_set_vprintf hook (idempotent). Keeps forwarding to the
 * previous hook so the serial console still prints. */
void rf_logbuf_install_hook(void);

#endif  /* SHIM_LOG_H */
```

- [ ] **Step 2: 实现 `shim_log.cpp`**

```cpp
// Device log ring buffer + ESP_LOGx capture hook.
//
// Threading: the hook can be invoked from any task and, per ESP-IDF's own
// contract for esp_log_set_vprintf, must be re-entrant. A portMUX spinlock
// guards the ring; the critical section is a bounded byte copy and takes no
// other lock, so it cannot deadlock against the display mutex.
//
// The hook must never call a log function: it IS the log path, so that would
// recurse without bound.
#include "log_upload_policy.h"  // for nothing yet; keeps the header pair visible
#include "shim_log.h"

#include <cstring>
#include <cstdio>

#include "esp_attr.h"
#include "esp_log.h"
#include "esp_log_write.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"

namespace {

constexpr uint32_t kMagic = 0x4c4f4731u;  // "LOG1"
constexpr int kDataBytes = 2048;
constexpr int kMaxLine = 256;             // one line's cap incl. NUL

struct Ring {
    uint32_t magic;
    uint32_t seq;      // next line number to assign
    uint32_t head;     // write offset into data[]
    uint32_t tail;     // acked offset into data[]
    uint32_t dropped;  // lines lost to overwrite
    uint8_t  data[kDataBytes];
};

RTC_NOINIT_ATTR static Ring g_ring;

portMUX_TYPE g_mux = portMUX_INITIALIZER_UNLOCKED;
vprintf_like_t g_prev_vprintf = nullptr;
bool g_hook_installed = false;

inline void ensure_init_locked() {
    if (g_ring.magic != kMagic) {
        g_ring.magic = kMagic;
        g_ring.seq = 0;
        g_ring.head = 0;
        g_ring.tail = 0;
        g_ring.dropped = 0;
    }
}

// Append one framed line. Caller holds the mux.
void push_locked(const char* text, int len) {
    if (len <= 0) return;
    if (len > kMaxLine - 1) len = kMaxLine - 1;
    const int frame = 4 + 2 + len;
    if (frame > kDataBytes) return;

    // Make room: drop whole old lines from the tail until it fits.
    while (((g_ring.head + kDataBytes - g_ring.tail) % kDataBytes) + frame > kDataBytes) {
        if (g_ring.tail == g_ring.head) break;  // empty but still too big: give up
        uint16_t old_len = 0;
        std::memcpy(&old_len, &g_ring.data[(g_ring.tail + 4) % kDataBytes], 2);
        g_ring.tail = (g_ring.tail + 4 + 2 + old_len) % kDataBytes;
        g_ring.dropped++;
    }

    const uint32_t seq = g_ring.seq++;
    uint16_t len16 = (uint16_t)len;
    std::memcpy(&g_ring.data[g_ring.head], &seq, 4);
    g_ring.head = (g_ring.head + 4) % kDataBytes;
    std::memcpy(&g_ring.data[g_ring.head], &len16, 2);
    g_ring.head = (g_ring.head + 2) % kDataBytes;
    // The text can wrap the end of the ring; copy in two pieces.
    const int first = (g_ring.head + len <= kDataBytes) ? len : (kDataBytes - g_ring.head);
    std::memcpy(&g_ring.data[g_ring.head], text, first);
    if (first < len) std::memcpy(&g_ring.data[0], text + first, len - first);
    g_ring.head = (g_ring.head + len) % kDataBytes;
}

int capture_hook(const char* fmt, va_list args) {
    char line[kMaxLine];
    va_list copy;
    va_copy(copy, args);
    const int n = vsnprintf(line, sizeof(line), fmt, copy);
    va_end(copy);

    // FreeRTOS printf from an ISR is not safe; skip the buffer there and keep
    // the console working. (xPortInIsrContext is a no-op cost off-ISR.)
    if (!xPortInIsrContext()) {
        portENTER_CRITICAL(&g_mux);
        ensure_init_locked();
        push_locked(line, n < (int)sizeof(line) ? n : (int)sizeof(line) - 1);
        portEXIT_CRITICAL(&g_mux);
    }

    if (g_prev_vprintf != nullptr) {
        return g_prev_vprintf(fmt, args);
    }
    return n;
}

}  // namespace

extern "C" void rf_logbuf_install_hook(void) {
    if (g_hook_installed) return;
    g_prev_vprintf = esp_log_set_vprintf(&capture_hook);
    g_hook_installed = true;
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines) {
    if (out_seq_lo) *out_seq_lo = 0;
    if (out_lines) *out_lines = 0;
    if (out == nullptr || cap <= 1) return;

    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int written = 0;
    int at = (int)g_ring.tail;
    uint32_t lines = 0;
    uint32_t first_seq = 0;
    while (at != (int)g_ring.head) {
        uint32_t seq = 0;
        uint16_t len = 0;
        std::memcpy(&seq, &g_ring.data[at], 4);
        at = (at + 4) % kDataBytes;
        std::memcpy(&len, &g_ring.data[at], 2);
        at = (at + 2) % kDataBytes;
        if (written + (int)len + 1 > cap) break;
        const int first = (at + len <= kDataBytes) ? len : (kDataBytes - at);
        std::memcpy(out + written, &g_ring.data[at], first);
        if (first < len) std::memcpy(out + written + first, &g_ring.data[0], len - first);
        written += len;
        out[written++] = '\n';
        at = (at + len) % kDataBytes;
        if (lines == 0) first_seq = seq;
        lines++;
    }
    out[written] = 0;
    portEXIT_CRITICAL(&g_mux);

    if (out_seq_lo) *out_seq_lo = first_seq;
    if (out_lines) *out_lines = lines;
}

extern "C" void rf_logbuf_ack(uint32_t seq_hi) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int at = (int)g_ring.tail;
    while (at != (int)g_ring.head) {
        uint32_t seq = 0;
        uint16_t len = 0;
        std::memcpy(&seq, &g_ring.data[at], 4);
        at = (at + 4) % kDataBytes;
        std::memcpy(&len, &g_ring.data[at], 2);
        at = (at + 2 + len) % kDataBytes;
        if (seq > seq_hi) break;
        g_ring.tail = at;
    }
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_stats(uint32_t* dropped, uint32_t* used) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    if (dropped) *dropped = g_ring.dropped;
    if (used) *used = (g_ring.head + kDataBytes - g_ring.tail) % kDataBytes;
    portEXIT_CRITICAL(&g_mux);
}
```

- [ ] **Step 3: 登记进 CMake**

`firmware/main/CMakeLists.txt`，在 `"rust/shim.cpp"` 下一行加：

```cmake
    "rust/shim_log.cpp"
```

- [ ] **Step 4: 构建验证编译与链接**

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware
IDF_TARGET=esp32s3 idf.py build
```
Expected: `Project build complete`

- [ ] **Step 5: 用 nm 确认符号真的进了产物**

Run:
```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-nm firmware/build/xiaozhi.elf | grep rf_logbuf
```
Expected: 4 个 `T rf_logbuf_*`（`install_hook` / `read` / `ack` / `stats`）

- [ ] **Step 6: 确认 `.rtc_noinit` 真的非空且带 magic**

Run:
```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-size -A firmware/build/xiaozhi.elf | grep rtc
```
Expected: `.rtc_noinit` 大小 ≥ 2052 字节（此前的 0 说明缓冲确实落进了该段）

- [ ] **Step 7: 提交**

```bash
git add firmware/main/rust/shim_log.cpp firmware/main/rust/include/shim_log.h firmware/main/CMakeLists.txt
git commit -m "feat(firmware): .rtc_noinit log ring with vprintf capture hook"
```

---

## Task 4: Rust 策略 `log_upload_policy.rs`

**Files:**
- Create: `firmware/main/rust/include/log_upload_policy.h`
- Create: `firmware/main/rust/src/log_upload_policy.rs`
- Create: `firmware/main/rust/tests/log_upload_policy.rs`
- Modify: `firmware/main/rust/src/lib.rs`（`pub mod log_upload_policy;`）
- Modify: `firmware/main/CMakeLists.txt`（`RUST_SOURCES`）

**Interfaces:**
- Produces:
  - `struct Inputs { enabled: u8, has_pending: u8, wifi_ready: u8, _pad: [u8;1], pending_bytes: u32, pending_lines: u32, fail_streak: u32, _pad2: [u8;4], last_fail_s: i64, now_s: i64 }`
  - `struct Decision { action: u8, _pad: [u8;3], max_bytes: u32 }`
  - `SKIP_DISABLED/EMPTY/NO_NET/BACKOFF: u8`，`UPLOAD: u8`
  - `fn decide(i: &Inputs) -> Decision`
  - `fn backoff_delay_s(streak: u32, base_s: u32, max_s: u32) -> u32`

- [ ] **Step 1: 写失败测试（含布局契约）**

`firmware/main/rust/tests/log_upload_policy.rs`：

```rust
//! Log-upload decision table + C layout contract.
//!
//! The layout test mirrors the style of the notify_policy / page_compare_policy
//! tests: field order, offsets and size must agree with
//! `rust/include/log_upload_policy.h` or the FFI reads garbage.

use firmware::log_upload_policy::*;

fn base() -> Inputs {
    Inputs {
        enabled: 1,
        has_pending: 1,
        wifi_ready: 1,
        _pad: [0; 1],
        pending_bytes: 100,
        pending_lines: 2,
        fail_streak: 0,
        _pad2: [0; 4],
        last_fail_s: -1,
        now_s: 1000,
    }
}

#[test]
fn uploads_when_enabled_and_pending_and_online() {
    let d = decide(&base());
    assert_eq!(d.action, UPLOAD);
    assert_eq!(d.max_bytes, MAX_UPLOAD_BYTES);
}

#[test]
fn disabled_switch_skips() {
    let i = Inputs { enabled: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn nothing_pending_skips() {
    let i = Inputs { has_pending: 0, pending_bytes: 0, pending_lines: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_EMPTY);
}

#[test]
fn offline_skips() {
    let i = Inputs { wifi_ready: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_NO_NET);
}

#[test]
fn disabled_beats_everything() {
    // Precedence: the switch is checked before pending/network so a disabled
    // device never even inspects the buffer.
    let i = Inputs { enabled: 0, has_pending: 0, wifi_ready: 0, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn backoff_holds_until_window_elapses() {
    let i = Inputs { fail_streak: 2, last_fail_s: 900, now_s: 1000, ..base() };
    assert_eq!(decide(&i).action, SKIP_BACKOFF);
}

#[test]
fn backoff_releases_after_window() {
    let i = Inputs { fail_streak: 2, last_fail_s: 0, now_s: 100_000, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn unset_clock_does_not_gate_on_backoff() {
    // now_s < 0 means the wall clock is unset (cold boot): the device must not
    // wedge behind a comparison against a bogus stamp.
    let i = Inputs { fail_streak: 3, last_fail_s: 0, now_s: -1, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn max_bytes_is_capped_by_pending_bytes() {
    let i = Inputs { pending_bytes: 10, ..base() };
    assert_eq!(decide(&i).max_bytes, 10);
}

#[test]
fn backoff_delay_grows_then_caps() {
    assert_eq!(backoff_delay_s(0, 60, 900), 0);
    assert_eq!(backoff_delay_s(1, 60, 900), 60);
    assert_eq!(backoff_delay_s(2, 60, 900), 120);
    assert_eq!(backoff_delay_s(4, 60, 900), 480);
    assert_eq!(backoff_delay_s(5, 60, 900), 900, "capped");
    assert_eq!(backoff_delay_s(99, 60, 900), 900, "no overflow at high streaks");
}

#[test]
fn decide_is_idempotent() {
    let i = base();
    assert_eq!(decide(&i), decide(&i));
}

#[test]
fn c_structs_match_the_header_layout() {
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<Inputs>(), 40);
    assert_eq!(offset_of!(Inputs, enabled), 0);
    assert_eq!(offset_of!(Inputs, has_pending), 1);
    assert_eq!(offset_of!(Inputs, wifi_ready), 2);
    assert_eq!(offset_of!(Inputs, pending_bytes), 4);
    assert_eq!(offset_of!(Inputs, pending_lines), 8);
    assert_eq!(offset_of!(Inputs, fail_streak), 12);
    assert_eq!(offset_of!(Inputs, last_fail_s), 24);
    assert_eq!(offset_of!(Inputs, now_s), 32);

    assert_eq!(size_of::<Decision>(), 8);
    assert_eq!(offset_of!(Decision, action), 0);
    assert_eq!(offset_of!(Decision, max_bytes), 4);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test --test log_upload_policy`
Expected: FAIL — `unresolved import firmware::log_upload_policy`

- [ ] **Step 3: 写头文件**

`firmware/main/rust/include/log_upload_policy.h`：

```c
/* Log-upload decision ABI.
 *
 * Rust owns the whole table (switch / pending / network / backoff); C++ only
 * fills the facts. Layout is pinned by a contract test in
 * rust/tests/log_upload_policy.rs — keep the explicit padding.
 */
#ifndef LOG_UPLOAD_POLICY_H
#define LOG_UPLOAD_POLICY_H

#include <stdint.h>

typedef struct {
    uint8_t  enabled;        /* 0: schedule policy says off */
    uint8_t  has_pending;
    uint8_t  wifi_ready;
    uint8_t  _pad;
    uint32_t pending_bytes;
    uint32_t pending_lines;
    uint32_t fail_streak;
    uint32_t _pad2;
    int64_t  last_fail_s;    /* negative = none yet */
    int64_t  now_s;          /* negative = wall clock unset */
} rf_log_upload_inputs_t;    /* sizeof == 40 */

typedef struct {
    uint8_t  action;         /* rf_log_upload_action_t */
    uint8_t  _pad[3];
    uint32_t max_bytes;
} rf_log_upload_decision_t;  /* sizeof == 8 */

typedef enum {
    RF_LOG_UPLOAD = 0,
    RF_LOG_SKIP_DISABLED = 1,
    RF_LOG_SKIP_EMPTY = 2,
    RF_LOG_SKIP_NO_NET = 3,
    RF_LOG_SKIP_BACKOFF = 4,
} rf_log_upload_action_t;

rf_log_upload_decision_t rf_log_upload_decide(const rf_log_upload_inputs_t* in);
uint32_t rf_log_upload_backoff_s(uint32_t streak, uint32_t base_s, uint32_t max_s);

#endif  /* LOG_UPLOAD_POLICY_H */
```

- [ ] **Step 4: 实现策略**

`firmware/main/rust/src/log_upload_policy.rs`：

```rust
//! Log-upload gate. Pure: no I/O, no globals — C++ gathers the facts (switch
//! from the schedule policy, ring occupancy, link state, RTC failure stamps)
//! and this decides whether to upload and how many bytes.
//!
//! Backoff reuses the shape already in `notify_policy` (`base * 2^(n-1)`,
//! capped) rather than inventing a second convention.

/// Single-upload cap. Bounded by the deep-sleep cycle's HTTP budget and by the
/// fixed request buffer on the C++ side.
pub const MAX_UPLOAD_BYTES: u32 = 1024;

pub const UPLOAD: u8 = 0;
pub const SKIP_DISABLED: u8 = 1;
pub const SKIP_EMPTY: u8 = 2;
pub const SKIP_NO_NET: u8 = 3;
pub const SKIP_BACKOFF: u8 = 4;

pub const BASE_BACKOFF_S: u32 = 60;
pub const MAX_BACKOFF_S: u32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Inputs {
    pub enabled: u8,
    pub has_pending: u8,
    pub wifi_ready: u8,
    pub _pad: [u8; 1],
    pub pending_bytes: u32,
    pub pending_lines: u32,
    pub fail_streak: u32,
    pub _pad2: [u8; 4],
    pub last_fail_s: i64,
    pub now_s: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Decision {
    pub action: u8,
    pub _pad: [u8; 3],
    pub max_bytes: u32,
}

pub fn backoff_delay_s(streak: u32, base_s: u32, max_s: u32) -> u32 {
    if streak == 0 {
        return 0;
    }
    let shift = streak.saturating_sub(1).min(31);
    let delay = (base_s as u64).saturating_mul(1u64 << shift);
    delay.min(max_s as u64) as u32
}

pub fn decide(i: &Inputs) -> Decision {
    let out = |action: u8, max_bytes: u32| Decision { action, _pad: [0; 3], max_bytes };
    // Switch first: a disabled device must not even look at the buffer.
    if i.enabled == 0 {
        return out(SKIP_DISABLED, 0);
    }
    if i.has_pending == 0 || i.pending_bytes == 0 {
        return out(SKIP_EMPTY, 0);
    }
    if i.wifi_ready == 0 {
        return out(SKIP_NO_NET, 0);
    }
    // Unset clock (cold boot): skip the gate rather than compare against a
    // bogus stamp and wedge until SNTP lands.
    if i.now_s >= 0 && i.fail_streak > 0 && i.last_fail_s >= 0 {
        let window = backoff_delay_s(i.fail_streak, BASE_BACKOFF_S, MAX_BACKOFF_S);
        let elapsed = i.now_s.saturating_sub(i.last_fail_s).max(0) as u64;
        if elapsed < window as u64 {
            return out(SKIP_BACKOFF, 0);
        }
    }
    out(UPLOAD, i.pending_bytes.min(MAX_UPLOAD_BYTES))
}
```

- [ ] **Step 5: 注册模块并加入构建列表**

`firmware/main/rust/src/lib.rs` 模块区加：

```rust
pub mod log_upload_policy;
```

`firmware/main/CMakeLists.txt` 的 `RUST_SOURCES` 加：

```cmake
    "${RUST_DIR}/src/log_upload_policy.rs"
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test --test log_upload_policy`
Expected: PASS（12 passed）

- [ ] **Step 7: 哨兵变异验证（证明测试不是假的）**

把 `decide` 里的 `if i.enabled == 0` 临时改成 `if false`，重跑：
Expected: `disabled_switch_skips` 与 `disabled_beats_everything` FAIL，其余仍 PASS。改回。

把 `if i.wifi_ready == 0` 改成 `if false`，重跑：
Expected: 只有 `offline_skips` FAIL。改回。

- [ ] **Step 8: 提交**

```bash
git add firmware/main/rust/include/log_upload_policy.h firmware/main/rust/src/log_upload_policy.rs firmware/main/rust/tests/log_upload_policy.rs firmware/main/rust/src/lib.rs firmware/main/CMakeLists.txt
git commit -m "feat(firmware): log-upload policy in Rust with layout contract"
```

---

## Task 5: 固件装配（钩子、开关、上报）

**Files:**
- Modify: `firmware/main/rust/src/shim.rs`（声明 `rf_logbuf_*`）
- Modify: `firmware/main/application.cc`（装钩子；解析 `policy.log_upload`；周期内上报）
- Modify: `firmware/main/rust/src/page_sync.rs`（把开关从响应解析进 RTC 快照）

**Interfaces:**
- Consumes: Task 3 的 `rf_logbuf_*`；Task 4 的 `log_upload_policy::{Inputs, decide, UPLOAD}`；`rf_http_post_json`（既有）；`rf_build_endpoint`（既有）
- Produces: `fn log_upload_try_once() -> bool` in `page_sync.rs`（在既有 schedule 周期内被调用）

- [ ] **Step 1: 声明 shim 符号**

`firmware/main/rust/src/shim.rs`，在 `unsafe extern "C"` 块内加：

```rust
    // Log ring (shim_log.cpp). Buffer ownership is C++'s: Rust only reads a
    // snapshot and acks what the server accepted.
    pub fn rf_logbuf_install_hook();
    pub fn rf_logbuf_read(out: *mut c_char, cap: c_int, out_seq_lo: *mut u32, out_lines: *mut u32);
    pub fn rf_logbuf_ack(seq_hi: u32);
    pub fn rf_logbuf_stats(dropped: *mut u32, used: *mut u32);
    pub fn rf_log_upload_decide(inp: *const crate::log_upload_policy::Inputs) -> crate::log_upload_policy::Decision;
```

- [ ] **Step 2: 写失败测试（开关解析）**

追加到 `firmware/main/rust/src/page_sync.rs` 的 `mod tests`：

```rust
    #[test]
    fn schedule_policy_log_upload_is_parsed() {
        // The switch arrives in the policy block (the query string has no room:
        // CBuf::<160> is already ~93 bytes and overflow voids the whole GET).
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_policy_log_upload(1));
        sync_once();
        assert_eq!(log_upload_enabled(), 1, "policy.log_upload=1 must reach the snapshot");

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_policy_log_upload(0));
        sync_once();
        assert_eq!(log_upload_enabled(), 0, "absent/0 must stay off");
    }
```

（`schedule_json_with_policy_log_upload` 照同模块既有 `schedule_json` 辅助的写法构造，只是多一个 `"log_upload": N` 字段。）

- [ ] **Step 3: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test schedule_policy_log_upload_is_parsed`
Expected: FAIL — `cannot find function log_upload_enabled`

- [ ] **Step 4: 实现开关存储与解析**

在 `page_sync.rs` 加一个 RTC 侧存储（经 `shim` 转发，Rust 不自己碰 RTC），并提供读取：

```rust
/// The per-device log-upload switch from the last schedule response. Kept in
/// the same RTC snapshot as the power counters so a deep-sleep wake (RTC
/// cleared) defaults to off until the next sync says otherwise.
pub fn log_upload_enabled() -> u8 {
    unsafe { shim::rf_log_upload_enabled_get() }
}

pub fn log_upload_enabled_set(v: u8) {
    unsafe { shim::rf_log_upload_enabled_set(v) }
}
```

在 `Protocol`/`Policy` 解析处读取 `policy` 的 `log_upload`（照 `policy_minutes_to_s` 的写法，缺省 0），并在 `sync_once` 成功回调里 `log_upload_enabled_set(parsed)`。

C++ 侧 `shim.cpp` 加两个一行转发：

```cpp
RTC_DATA_ATTR static uint8_t g_log_upload_enabled;
extern "C" uint8_t rf_log_upload_enabled_get(void) { return g_log_upload_enabled; }
extern "C" void rf_log_upload_enabled_set(uint8_t v) { g_log_upload_enabled = v; }
```

同时在 `shim.rs` 声明这两个符号。

- [ ] **Step 5: 实现上报函数**

在 `page_sync.rs` 加（照 `notify.rs` 的 POST 范式：`CBuf` 建 URL、`rf_build_endpoint`、base64、`rf_http_post_json`）：

```rust
/// One upload attempt. Returns true when the server accepted the batch.
/// Caller must already hold no lock: HTTP can block for seconds.
fn log_upload_try_once() -> bool {
    let mut dropped = 0u32;
    let mut used = 0u32;
    let (mut last_fail_s, mut fail_streak) = unsafe { shim::rf_log_upload_fail_state() };
    let inputs = log_upload_policy::Inputs {
        enabled: log_upload_enabled(),
        has_pending: if used > 0 { 1 } else { 0 },
        wifi_ready: 1,  // the caller only reaches here on a completed sync
        _pad: [0; 1],
        pending_bytes: used,
        pending_lines: 0,
        fail_streak,
        _pad2: [0; 4],
        last_fail_s,
        now_s: RfReadNowSigned(),
    };
    let d = unsafe { shim::rf_log_upload_decide(&inputs) };
    if d.action != log_upload_policy::UPLOAD {
        return false;
    }
    // ... read ring into a CBuf, base64 it, POST {"seq_hi","dropped","lines"},
    //     and only on HTTP 201 call rf_logbuf_ack(seq_hi) and reset the streak.
}
```

实现要点：`rf_logbuf_stats` 拿 `used`/`dropped`；`rf_logbuf_read` 取字节（缓冲用 `CBuf::<1536>`）；base64 照 `notify.rs` 用 `B64`；body 用 `CBuf::<2048>`；URL 用 `CBuf::<320>` + `rf_build_endpoint("/api/device-log", ...)`；HTTP 201 才 `rf_logbuf_ack(seq_lo + lines - 1)` 并清零 streak，否则 `fail_streak += 1`、`last_fail_s = now`。

在 `RunPowerCycle` 里、`page_sync_sync_once()` 返回成功之后调用一次 `log_upload_try_once()`（`application.cc` 已有该调用点）。

- [ ] **Step 6: 装钩子**

`application.cc` 的 `app_main` 之后最早期（`Application::Initialize` 之前）加一次：

```cpp
    // Capture logs from the very first boot line; idempotent.
    rf_logbuf_install_hook();
```

（需在 `application.cc` 顶部声明 `extern "C" void rf_logbuf_install_hook(void);`。）

- [ ] **Step 7: 双门禁**

Run:
```bash
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test
```
Expected: PASS（375 + 12 = 387 左右）

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware
IDF_TARGET=esp32s3 idf.py build
```
Expected: `Project build complete`

- [ ] **Step 8: 确认新符号在产物里**

Run:
```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-nm firmware/build/xiaozhi.elf | grep -E "rf_log_upload|rf_logbuf"
```
Expected: `rf_log_upload_decide`、`rf_log_upload_enabled_get/set`、4 个 `rf_logbuf_*` 全部为 `T`

- [ ] **Step 9: 提交**

```bash
git add firmware/main/rust/src/shim.rs firmware/main/rust/src/page_sync.rs firmware/main/application.cc firmware/main/rust/shim.cpp
git commit -m "feat(firmware): wire log capture, switch parsing and periodic upload"
```

---

## Task 6: 前端开关与日志查看

**Files:**
- Modify: `frontend/src/api.js`
- Modify: `frontend/src/pages/Devices.jsx`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: `POST /api/devices/{id}/log-upload`、`GET /api/devices/{id}/logs?tail=N`（Task 2）

- [ ] **Step 1: 加 api 方法**

`frontend/src/api.js`，在 `api` 对象里加：

```js
  setLogUpload: (id, enabled) =>
    request(`/devices/${encodeURIComponent(id)}/log-upload`, { method: 'POST', body: { enabled } }),
  deviceLogs: async (id, tail = 500) => {
    const res = await request(`/devices/${encodeURIComponent(id)}/logs?tail=${tail}`);
    return res.json();
  },
```

- [ ] **Step 2: 加开关列与弹窗**

`frontend/src/pages/Devices.jsx`：

- 在表头加 `<th>日志上报</th>`（放在"信任"列之前）。
- 单元格用既有 `BusyButton` 范式：

```jsx
                    <td>
                      <BusyButton
                        className={d.log_upload ? 'btn danger' : 'btn secondary'}
                        busy={busy === 'log:' + d.device_id}
                        busyText="处理中…"
                        onClick={() => setLogUpload(d, !d.log_upload)}
                      >
                        {d.log_upload ? '关闭' : '开启'}
                      </BusyButton>
                    </td>
```

- 加状态与处理器（照 `setTrust` 的写法）：

```jsx
  async function setLogUpload(d, enabled) {
    setBusy('log:' + d.device_id);
    try {
      await api.setLogUpload(d.device_id, enabled);
      await refresh('devices', fetchDevices);
    } catch (e) {
      setErr(e.message);
    } finally {
      setBusy('');
    }
  }
```

- 加 `DeviceLog` 弹窗（照 `BatteryDetail` 的结构）：

```jsx
function DeviceLog({ device, onClose }) {
  const [data, setData] = useState(null);
  const [err, setErr] = useState('');

  const load = async () => {
    setErr('');
    try {
      setData(await api.deviceLogs(device.device_id, 500));
    } catch (e) {
      setErr(e.message);
    }
  };

  useEffect(() => { load(); }, [device.device_id]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 900 }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <strong>设备日志 · {device.device_id}</strong>
          <div>
            <BusyButton className="btn secondary" onClick={load}>刷新</BusyButton>
            <button type="button" className="spark-close" onClick={onClose} aria-label="关闭">×</button>
          </div>
        </div>
        {err && <Banner kind="error">{err}</Banner>}
        {data && (
          <>
            <div className="muted" style={{ fontSize: 12, margin: '6px 0' }}>
              共 {data.lines.length} 行{data.truncated ? '（已截断，仅显示尾部）' : ''}；行首为服务端接收时刻，
              正文里括号内的数字是设备 uptime 毫秒，不是墙钟。
            </div>
            <pre style={{ maxHeight: 480, overflow: 'auto', fontSize: 12, whiteSpace: 'pre-wrap' }}>
              {data.lines.join('\n')}
            </pre>
          </>
        )}
      </div>
    </div>
  );
}
```

- 在设备行的操作区加一个"日志"按钮打开它（`onClick={() => setLogDevice(d)}`），并加 `const [logDevice, setLogDevice] = useState(null);`，在页面底部与 `BatteryDetail` 并列渲染：

```jsx
      {logDevice && <DeviceLog device={logDevice} onClose={() => setLogDevice(null)} />}
```

- [ ] **Step 3: 忽略运行数据**

`.gitignore`，在 `server/data/images/` 下方加：

```gitignore
server/data/devicelogs/
```

- [ ] **Step 4: 构建并真机验证**

Run: `cd frontend && npm run build`
Expected: 构建成功

Run: `cd ../server && ./start.sh restart`
Expected: 服务重启（`frontend/dist` 在 app 启动时挂载，必须重启才生效）

浏览器验证（用既有后台账号登录）：
1. 设备页出现"日志上报"列，默认显示"开启"按钮（= 当前关闭）。
2. 点"开启"→ 按钮变"关闭"；刷新页面后仍为"关闭"（已持久化）。
3. 点"日志"→ 弹窗出现；在设备上报过一次之后能看到行。
4. 点"关闭"→ 恢复关闭态。

- [ ] **Step 5: 提交**

```bash
git add frontend/src/api.js frontend/src/pages/Devices.jsx .gitignore
git commit -m "feat(frontend): per-device log-upload switch and log viewer"
```

---

## Task 7: 端到端真机验收

**Files:** 无代码改动（仅验证与记录）

**Interfaces:** Consumes 全部前置任务

- [ ] **Step 1: 刷写并确认新代码在跑**

Run（先关串口监视器——打开 `/dev/ttyACM0` 会复位设备）：
```bash
cd firmware && IDF_TARGET=esp32s3 idf.py -p /dev/ttyACM0 flash
```
Expected: 刷写成功

确认符号：
```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-nm firmware/build/xiaozhi.elf | grep rf_logbuf | wc -l
```
Expected: `4`

- [ ] **Step 2: 打开开关，确认日志到达**

后台把该设备开关设为"开启"，等一个轮询周期（≤10 分钟），然后：

Run:
```bash
tail -n 20 server/data/devicelogs/NOTE4C-3400FC.log
```
Expected: 出现带服务端时间戳的设备日志行，含 `Boot path:` 与 `PageSync` 一类内容

同时确认服务端侧确实收到了请求：
```bash
journalctl --user -u youn-ink-server --since "10 min ago" | grep device-log
```
Expected: 至少一条 `POST /api/device-log HTTP/1.1" 201`

- [ ] **Step 3: 关闭开关，确认停止**

后台关闭开关，等 ≥2 个轮询周期：
```bash
tail -n 3 server/data/devicelogs/NOTE4C-3400FC.log
```
Expected: 不再有新行追加（**注意**：设备可能已把开关打开期间的在途批次发完，需对比前后时间戳）

- [ ] **Step 4: 崩溃前日志存活验证（本设计的核心价值）**

制造一次复位（拔插数据线即可产生 `ESR_RST_USB`，或按住 BOOT 复位），然后：

Run:
```bash
tail -n 40 server/data/devicelogs/NOTE4C-3400FC.log
```
Expected: 复位后的第一次上报里能看到**复位之前**的日志行（因为缓冲在 `.rtc_noinit`，panic/USB 复位不清它）

同时用服务端交叉验证复位确实发生：
```bash
journalctl --user -u youn-ink-server --since "15 min ago" | grep -oE "rr=[0-9]+" | sort | uniq -c
```

- [ ] **Step 5: 记录真机结论**

把观察写进 `docs/superpowers/progress/`（照 `2026-09-24-rr4-diagnosis.md` 的格式），至少包含：
- 开关打开/关闭是否真的改变上行；
- 复位前日志是否确实出现在复位后第一次上报里（这是设计成立与否的判据）；
- 若出现环形覆盖丢行（日志里见到 `dropped N lines`），记录当时的行率与时间。

- [ ] **Step 6: 提交验证记录**

```bash
git add docs/superpowers/progress/*.md
git commit -m "docs: device log upload on-device acceptance notes"
```

---

## 自审结果

**① Spec 覆盖检查**

| Spec 章节 | 对应任务 |
| --- | --- |
| §3 C++ 缓冲 + 钩子 + ABI | Task 3 |
| §3 ABI 三件套（read/ack/stats） | Task 3 Step 2；Task 5 Step 1 声明 |
| §4 Rust 策略（决策表/退避/分段/base64） | Task 4；Task 5 Step 5 |
| §4 接线进 schedule 周期 | Task 5 Step 5-6 |
| §5 开关列 + 下发 | Task 2 Step 3-4 |
| §5 上行端点 + 存储 + 轮转 | Task 1、Task 2 Step 4 |
| §5 读取端点 | Task 2 Step 4 |
| §5 `device_log_dir` 配置 + mkdir | Task 1 Step 3 |
| §5 `.gitignore` | Task 6 Step 3 |
| §6 前端开关与弹窗 | Task 6 |
| §7 测试要求（布局契约/哨兵/fixture 隔离） | Task 4 Step 1/7；Task 1 `_isolate_device_log_dir` |
| §7 实施顺序 | 任务编号即顺序 |
| §7 `shim_log.cpp` 独立文件 + CMake 登记 | Task 3 Step 3 |
| §7 钩子链式转发 | Task 3 Step 2（`g_prev_vprintf`） |
| §8 风险 | Task 7 Step 5 观测项 |

无遗漏。

**② 占位符扫描**：无 "TBD/TODO/类似 Task N"。Task 5 Step 5 的 `log_upload_try_once` 给出了完整签名、输入构造、成功/失败分支与每条约束（CBuf 尺寸、base64、ack 时机、streak 更新）；省略的只有逐行样板，已指明照 `notify.rs` 的哪一处。Task 5 Step 2 的测试辅助 `schedule_json_with_policy_log_upload` 已说明构造方式（照同模块既有 `schedule_json` 加一个字段）。

**③ 类型一致性**：`Inputs` 字段顺序/偏移在 Task 4 的 Rust 与 `log_upload_policy.h` 两处逐字一致（size 40，`last_fail_s`@24、`now_s`@32）；`Decision` size 8、`max_bytes`@4 两处一致；`rf_logbuf_read/ack/stats/install_hook` 在 Task 3 头文件、Task 3 实现、Task 5 的 `shim.rs` 声明三处同名同参；`MAX_UPLOAD_BYTES` 在 Task 4 定义为 1024，Task 5 的 body 缓冲 2048 与其 base64 膨胀一致；`devicelog.append_lines/tail_lines` 在 Task 1 定义、Task 2 消费，参数名与类型一致。

**④ 一处已知的实现风险（不阻塞，Task 7 观测）**：`capture_hook` 内用 `vsnprintf`，而钩子可能落在 cache 关闭窗口。若真机出现该窗口下的异常，Task 7 Step 5 记录后另开任务处理——本计划不预先加防护，因为没有证据表明它在实践中发生。
