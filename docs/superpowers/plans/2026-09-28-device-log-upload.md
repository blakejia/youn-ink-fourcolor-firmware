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
| `server/youn_server/app.py`（改） | `POST /api/device-log`、`GET /api/devices/{id}/logs`、`policy.log_upload` 三态下发、开关读写端点、schedule 上行解析 `&lo=` |
| `server/tests/test_device_log.py`（新） | 服务端全部用例 |
| `firmware/main/rust/include/log_upload_policy.h`（新） | ABI 定义（Rust 策略输入/输出） |
| `firmware/main/rust/src/log_upload_policy.rs`（新） | 纯策略：决策表、退避、分段 |
| `firmware/main/rust/tests/log_upload_policy.rs`（新） | 策略单测 + 布局契约 |
| `firmware/main/rust/include/shim_log.h`（新） | 环形缓冲 C ABI 声明 |
| `firmware/main/rust/shim_log.cpp`（新） | `.rtc_noinit` 缓冲、锁、`esp_log_set_vprintf` 钩子、链式转发 |
| `firmware/main/rust/src/shim.rs`（改） | 声明 `rf_logbuf_*` 外部符号 |
| `firmware/main/application.cc`（改） | 装配：装钩子、读两个开关、周期内调用上报 |
| `firmware/main/rust/src/settings.rs` + `include/settings_menu.h`（改） | 设备侧本地开关的设置项与菜单（`Kind::Toggle`） |
| `firmware/main/settings.cc` / `firmware/main/application.cc`（改） | 本地开关的 NVS 读写，走既有 `Settings` 类（`GetBool/SetBool`） |
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


def test_rotation_moves_the_full_file_aside(monkeypatch):
    """Design §7 lists rotation as a required case. Shrink the threshold rather
    than writing 5 MB; the constant is the module's only knob."""
    monkeypatch.setattr(devicelog, "_ROTATE_BYTES", 64)
    devicelog.append_lines("NOTE4C-TEST", "t0", "a" * 100 + "\n", dropped=0)
    devicelog.append_lines("NOTE4C-TEST", "t1", "second\n", dropped=0)
    base = settings.device_log_dir / "NOTE4C-TEST.log"
    assert base.read_text() == "t1 second\n", "current holds only the fresh append"
    assert (settings.device_log_dir / "NOTE4C-TEST.log.1").read_text().startswith("t0 aaa")


def test_rotation_keeps_newest_previous_at_1_and_caps(monkeypatch):
    monkeypatch.setattr(devicelog, "_ROTATE_BYTES", 8)
    for i in range(5):
        devicelog.append_lines("NOTE4C-TEST", f"t{i}", "x" * 20 + "\n", dropped=0)
    d = settings.device_log_dir
    assert (d / "NOTE4C-TEST.log.1").read_text().startswith("t3")
    assert (d / "NOTE4C-TEST.log.2").read_text().startswith("t2")
    assert (d / "NOTE4C-TEST.log.3").read_text().startswith("t1")
    assert not (d / "NOTE4C-TEST.log.4").exists(), "capped at .3"


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
Expected: PASS（8 passed）

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
  - `registry.set_log_upload(device_id: str, value: Optional[int]) -> None`（`None` = 无意见）
  - `registry.get_log_upload(device_id: str) -> Optional[int]`（`None` = 无意见 / 未知设备）
  - HTTP `POST /api/device-log` → 201 `{"written": N}`；另读 schedule 上行的 `lo=`
  - HTTP `POST /api/devices/{device_id}/log-upload` body `{"value": 1|0|null}` → 200 `{"log_upload": 1|0|null}`
  - HTTP `GET /api/devices/{device_id}/logs?tail=N` → 200 `{"lines": [...], "truncated": bool}`
  - schedule 响应 `policy.log_upload: null | 1 | 0`
  - schedule 响应顶层 `local_log_upload: 0|1|2`（`lo=` 回显）
  - `registry.set_local_log_upload(device_id, value: Optional[int])` / `registry.get_local_log_upload(device_id) -> Optional[int]`——**持久化**最近一次 `lo=`，并随 `/api/devices` 行输出（列 `local_log_upload`，可空）。原因：`local_log_upload` 若只活在设备 token 的 schedule 响应里，运维端永远读不到（`/api/pages/schedule` 对 operator token 返回 401），Task 7 的覆盖提示就永远渲染不出来。

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


def test_log_upload_defaults_to_no_opinion(client, trusted_device):
    """Both sides silent => off. The stored value must be NULL, not 0: a
    DEFAULT 0 would read as an opinion and void the local switch."""
    device_id, _ = trusted_device
    r = client.get("/api/devices", headers={"X-Operator-Token": ""})
    dev = next(d for d in r.json()["devices"] if d["device_id"] == device_id)
    assert dev["log_upload"] is None


def test_operator_enable_reaches_schedule_policy(client, trusted_device):
    device_id, token = trusted_device
    r = client.post(f"/api/devices/{device_id}/log-upload",
                    json={"value": 1},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    assert r.json()["log_upload"] == 1

    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["policy"]["log_upload"] == 1


def test_operator_can_clear_back_to_no_opinion(client, trusted_device):
    """Clearing the service setting hands control back to the device."""
    device_id, token = trusted_device
    client.post(f"/api/devices/{device_id}/log-upload",
                json={"value": 1}, headers={"X-Operator-Token": ""})
    r = client.post(f"/api/devices/{device_id}/log-upload",
                    json={"value": None}, headers={"X-Operator-Token": ""})
    assert r.json()["log_upload"] is None

    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["policy"]["log_upload"] is None


def test_operator_explicit_off_is_distinct_from_no_opinion(client, trusted_device):
    """0 and NULL are different states: 0 overrides the local switch."""
    device_id, token = trusted_device
    client.post(f"/api/devices/{device_id}/log-upload",
                json={"value": 0}, headers={"X-Operator-Token": ""})
    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["policy"]["log_upload"] == 0


def test_schedule_echoes_local_opinion(client, trusted_device):
    """The uplink carries the device's own switch so the UI can explain why an
    enabled service setting has no effect."""
    device_id, token = trusted_device
    r = client.get("/api/pages/schedule?lo=2",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["local_log_upload"] == 2
    # Absent means "no opinion", and it must not be confused with 0.
    r = client.get("/api/pages/schedule",
                   headers={"Authorization": f"Bearer {token}"})
    assert r.json()["local_log_upload"] == 0


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

`server/youn_server/devices.py` 的 `__init__` 末尾（`battery_history` 迁移块之后）加列，照 `power_counters` 的写法：

**必须是可空列，且不带 `DEFAULT 0`。** 服务端的 `0` 是「明确关」这一种意见，不是默认值；`DEFAULT 0` 会让服务端永远有意见，本地开关彻底失效（与「两侧都能开」矛盾），所以默认值是 `NULL`。

```python
        # Per-device log-upload switch (spec 2026-09-28). NULLABLE on purpose:
        # NULL = no opinion (the device's own switch decides), 1 = force on,
        # 0 = force off. A DEFAULT 0 would read as a standing opinion and
        # silently void the local switch.
        try:
            self._conn.execute("ALTER TABLE devices ADD COLUMN log_upload INTEGER")
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
        # The device's own switch opinion, persisted from the `lo=` uplink so
        # the operator UI can explain a service setting that appears to do
        # nothing. It must be durable: the only other carrier is the
        # device-token schedule response, which an operator token cannot read.
        try:
            self._conn.execute("ALTER TABLE devices ADD COLUMN local_log_upload INTEGER")
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
```

然后加两个方法（注意 `None` 的语义是「无意见」，不是「关」）：

```python
    def set_log_upload(self, device_id: str, value: Optional[int]) -> None:
        """Persist the service-side log-upload opinion.

        ``None`` clears it back to "no opinion" so the device's own switch
        decides again; ``1``/``0`` force on/off and override the device.
        """
        with self._lock:
            self._conn.execute(
                "UPDATE devices SET log_upload = ? WHERE device_id = ?",
                (value, device_id),
            )

    def set_local_log_upload(self, device_id: str, value: Optional[int]) -> None:
        """Persist the device's own opinion from the `lo=` uplink (0/1/2)."""
        with self._lock:
            self._conn.execute(
                "UPDATE devices SET local_log_upload = ? WHERE device_id = ?",
                (value, device_id),
            )

    def get_local_log_upload(self, device_id: str) -> Optional[int]:
        with self._lock:
            row = self._conn.execute(
                "SELECT local_log_upload FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
        if row is None:
            return None
        try:
            raw = row["local_log_upload"]
        except (IndexError, KeyError):
            return None
        return None if raw is None else int(raw)

    def get_log_upload(self, device_id: str) -> Optional[int]:
        """The service-side opinion: None (no opinion) / 1 / 0.

        Unset, unknown device, or SQL NULL all read as None. A stored 0 is a
        real opinion and is returned as 0 -- the two must not collapse.
        """
        with self._lock:
            row = self._conn.execute(
                "SELECT log_upload FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
        if row is None:
            return None
        try:
            raw = row["log_upload"]
        except (IndexError, KeyError):
            return None
        return None if raw is None else int(raw)
```

`Device` 数据类加字段 `log_upload: Optional[int] = None`（`devices.py` 的 `@dataclass` 块内，`trust` 之后）。`/api/devices` 用 `dict(d.__dict__)` 整体序列化（`app.py:210`），所以加字段即自动出现在响应里，**不需要改 `app.py` 的序列化**——`test_log_upload_defaults_to_no_opinion` 依赖这一点。同时确认设备行→`Device` 的构造处（`get`/`list_all`/`get_device_by_token` 共用的行映射）带上该列，否则读到的恒为 `None`。

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
        raw = body.get("value", None)
        if raw is not None and raw not in (0, 1):
            raise HTTPException(400, "value must be 1, 0 or null")
        registry.set_log_upload(device_id, None if raw is None else int(raw))
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

`get_schedule` 的两处改动。**下发**（`policy` 块内，三态原样透传——不要把 `None` 折成 0）：

```python
                "log_upload": registry.get_log_upload(dev.device_id),
```

**上行回显**（响应顶层，供后台解释「为什么服务端开了没效果」）：

```python
        # `lo` is the device's own switch opinion: 0 = none, 1 = off, 2 = on.
        # Echoed back so the admin UI can say "the device has it off" instead
        # of showing an enabled service setting that appears to do nothing.
        # Absent (old firmware) reads as 0 = no opinion.
        local_lo = _u32("lo")
```

并在返回值里加 `"local_log_upload": local_lo,`（与 `"power": power` 同级）。**同时把 `local_lo` 持久化**——运维端读不到这个响应，它必须落到设备行上：

```python
        # The device's opinion must outlive this request: an operator token
        # cannot read this endpoint (401), so the UI's only source is the
        # device row. Absent `lo=` (old firmware) leaves the stored value
        # alone rather than clearing it.
        if qp.get("lo") is not None:
            registry.set_local_log_upload(dev.device_id, local_lo)
```

`Device` 数据类再加字段 `local_log_upload: Optional[int] = None`，并在三处行映射（`get`/`get_device_by_token`/`list_all`）带上该列（照 `log_upload=self.get_log_upload(...)` 的写法）。`dict(d.__dict__)` 会自动把它输出到 `/api/devices`，Task 7 的 `d.local_log_upload` 因此可用。

- [ ] **Step 5: 跑测试确认通过**

Run: `cd server && .venv/bin/python -m pytest tests/test_device_log.py -q`
Expected: PASS（18 passed = Task 1 的 8 + Task 2 的 10）

- [ ] **Step 6: 跑全量服务端测试确认没打破既有契约**

Run: `cd server && .venv/bin/python -m pytest tests/ -q`
Expected: PASS（302 + 18 = 320 左右）

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

/* Copy the oldest un-acked contiguous run into `out`, NUL-terminated within
 * `cap` bytes (so `out` must hold `cap` bytes; cap == 0 or 1 yields an empty
 * string). Does NOT advance the tail: only rf_logbuf_ack does, so a failed
 * upload leaves the lines for the next attempt.
 *
 * `*out_seq_lo` is meaningful only when `*out_lines > 0` — seq 0 is a real
 * frame number, so a caller that acks after a zero-line read would compute
 * seq_lo + 0 - 1 and ack everything. Ack with `*out_seq_lo + *out_lines - 1`. */
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

// One byte is reserved (kCapBytes = kDataBytes - 1) so that head == tail
// unambiguously means EMPTY. Occupancy is then DERIVED from the two cursors
// rather than stored: a stored counter is updated non-atomically with the bytes
// it describes, and this buffer lives in `.rtc_noinit` — the one region a reset
// mid-write is guaranteed to preserve — so a stored counter can go stale and
// stay stale into the next boot. Cursor-derived occupancy cannot.
constexpr int kCapBytes = kDataBytes - 1;
// An absurd (or torn) `len` field can make the drop step a multiple of
// kDataBytes, which would stall the make-room loop forever; every walk is
// bounded by this, the most frames that can physically fit.
constexpr int kMaxFrames = kDataBytes / 6 + 1;

struct Ring {
    uint32_t magic;
    uint32_t seq;      // next line number to assign
    uint32_t head;     // write offset into data[]
    uint32_t tail;     // acked offset into data[]
    uint32_t dropped;  // lines lost to overwrite
    uint8_t  data[kDataBytes];
};

// Bytes currently held: derived, so it cannot disagree with the cursors.
inline uint32_t ring_used_locked() {
    return (g_ring.head + kDataBytes - g_ring.tail) % kDataBytes;
}

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

// Copy `n` bytes into the ring at `at`, wrapping past the end. Every copy in
// this file goes through here: a multi-byte memcpy at an index near kDataBytes
// would otherwise run past data[] into the neighbouring `.rtc.force_slow`
// variable, which shares the RTC region with no padding between them.
void ring_copy_in(uint32_t at, const uint8_t* src, int n) {
    const int first = (at + (uint32_t)n <= kDataBytes) ? n : (kDataBytes - (int)at);
    std::memcpy(&g_ring.data[at], src, first);
    if (first < n) std::memcpy(&g_ring.data[0], src + first, n - first);
}

// Read `n` bytes out of the ring at `at`, wrapping. Mirror of ring_copy_in.
void ring_copy_out(uint8_t* dst, uint32_t at, int n) {
    const int first = (at + (uint32_t)n <= kDataBytes) ? n : (kDataBytes - (int)at);
    std::memcpy(dst, &g_ring.data[at], first);
    if (first < n) std::memcpy(dst + first, &g_ring.data[0], n - first);
}

// Append one framed line. Caller holds the mux.
//
// The frame is written at `head` and `head` is advanced LAST: a reset inside
// this function therefore leaves no half-frame inside [tail, head), so the next
// boot simply reads the previous backlog.
void push_locked(const char* text, int len) {
    if (len <= 0) return;
    if (len > kMaxLine - 1) len = kMaxLine - 1;
    const int frame = 4 + 2 + len;
    if (frame > kCapBytes) return;  // cannot be stored at all

    // Make room from the tail. Bounded twice: by the cursor-derived occupancy
    // (so it stops when genuinely empty) and by an explicit frame cap (so a
    // garbage `len` whose step is a multiple of kDataBytes cannot stall it).
    for (int guard = 0; guard <= kMaxFrames; guard++) {
        if (ring_used_locked() + (uint32_t)frame <= (uint32_t)kCapBytes) break;
        if (g_ring.tail == g_ring.head) break;  // empty and still too big
        uint16_t old_len = 0;
        ring_copy_out((uint8_t*)&old_len, (g_ring.tail + 4) % kDataBytes, 2);
        uint32_t step = 4 + 2 + old_len;
        // `>= kDataBytes`, not `>`: a step of exactly kDataBytes moves the
        // cursor nowhere (mod kDataBytes), so the loop would burn every guard
        // iteration without freeing a byte, inflate `dropped`, and — because
        // head then advances by frame onto tail — leave the ring reading as
        // empty and lose the whole backlog. Skip only the header in that case.
        if (step < 6 || step >= (uint32_t)kDataBytes) step = 6;
        g_ring.tail = (g_ring.tail + step) % kDataBytes;
        g_ring.dropped++;
    }
    // Belt and braces: the loop above is bounded, so it can exit without having
    // made room (garbage metadata). Never publish a frame that would overflow
    // the capacity — dropping this one line is the honest failure.
    if (ring_used_locked() + (uint32_t)frame > (uint32_t)kCapBytes) return;

    const uint32_t seq = g_ring.seq++;
    uint16_t len16 = (uint16_t)len;
    ring_copy_in(g_ring.head, (const uint8_t*)&seq, 4);
    ring_copy_in((g_ring.head + 4) % kDataBytes, (const uint8_t*)&len16, 2);
    ring_copy_in((g_ring.head + 6) % kDataBytes, (const uint8_t*)text, len);
    g_ring.head = (g_ring.head + (uint32_t)frame) % kDataBytes;  // publish last
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
    // The check AND the exchange are under the lock: a concurrent second caller
    // could otherwise observe the hook installed and capture capture_hook as
    // `g_prev_vprintf`, making the hook forward to itself (unbounded recursion).
    // esp_log_set_vprintf is a single __atomic_exchange_n, so holding this leaf
    // lock across it is safe.
    portENTER_CRITICAL(&g_mux);
    if (!g_hook_installed) {
        g_prev_vprintf = esp_log_set_vprintf(&capture_hook);
        g_hook_installed = true;
        ensure_init_locked();
    }
    portEXIT_CRITICAL(&g_mux);
}

// `out` holds exactly `cap` bytes INCLUDING the NUL terminator — the same
// convention as CBuf::push_bytes (`len + n >= N` reserves the last byte), so a
// caller can pass a CBuf's size and C satisfies the whole contract within it.
// Admitting a line only when `written + len + 1 < cap` keeps out[written]
// in bounds.
extern "C" void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines) {
    if (out_seq_lo) *out_seq_lo = 0;
    if (out_lines) *out_lines = 0;
    if (out == nullptr || cap <= 1) return;

    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int written = 0;
    int at = (int)g_ring.tail;
    // Walk the occupancy span, bounded by both the span and a frame cap: the
    // span alone would stall on a torn/garbage length field.
    uint32_t remaining = ring_used_locked();
    uint32_t lines = 0;
    uint32_t first_seq = 0;
    for (int guard = 0; guard <= kMaxFrames && remaining >= 6; guard++) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        at = (at + 2) % kDataBytes;
        if (written + (int)len + 1 >= cap) break;
        if (6u + len > remaining) break;  // never read past the span
        ring_copy_out((uint8_t*)out + written, (uint32_t)at, len);
        written += len;
        out[written++] = '\n';
        at = (at + len) % kDataBytes;
        remaining -= 6u + len;
        if (lines == 0) first_seq = seq;
        lines++;
    }
    out[written] = 0;
    portEXIT_CRITICAL(&g_mux);

    // Meaningful only when *out_lines > 0: seq 0 is a real frame number, so a
    // caller that acks with lines == 0 would compute seq_lo + 0 - 1 and ack
    // everything.
    if (out_seq_lo) *out_seq_lo = first_seq;
    if (out_lines) *out_lines = lines;
}

// Contract: pass seq_lo + lines - 1 from the read that produced the payload.
// Acks only frames the reader actually returned (contiguous from the tail).
extern "C" void rf_logbuf_ack(uint32_t seq_hi) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int at = (int)g_ring.tail;
    // Same bounded walk as read.
    uint32_t remaining = ring_used_locked();
    for (int guard = 0; guard <= kMaxFrames && remaining >= 6; guard++) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        if (seq > seq_hi) break;
        if (6u + len > remaining) break;
        // `at` points at the len field; the next frame starts 2 + len later.
        at = (at + 2 + len) % kDataBytes;
        g_ring.tail = at;
        remaining -= 6u + len;
    }
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_stats(uint32_t* dropped, uint32_t* used) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    if (dropped) *dropped = g_ring.dropped;
    if (used) *used = ring_used_locked();
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

- [ ] **Step 5: 用 nm 确认符号进了目标文件（最终 ELF 里是空的，见下）**

**注意：本步在 Task 3 内只验证到目标文件级。** `libmain.a` 用 `-Wl,--gc-sections` 链接，而 Task 3 没有任何调用方，归档成员会被回收 → 最终 ELF 里 `rf_logbuf_*` 计数为 0、`.rtc_noinit` 读作 0 B。这是链接器的正常行为，不是缺陷。证据取目标文件：

```bash
source ~/data/esp-idf-v6.0/export.sh
O=$(find firmware/build -name "shim_log.cpp.obj" | head -1)
xtensa-esp32s3-elf-nm "$O" | grep rf_logbuf     # 期望 4 个 T
xtensa-esp32s3-elf-size -A "$O" | grep noinit   # 期望 .rtc_noinit.0 ≈ 2068
```


Run:
```bash
source ~/data/esp-idf-v6.0/export.sh
xtensa-esp32s3-elf-nm firmware/build/xiaozhi.elf | grep rf_logbuf
```
Expected: 4 个 `T rf_logbuf_*`（`install_hook` / `read` / `ack` / `stats`）

- [ ] **Step 6: 最终 ELF 的符号与段大小（本步在 Task 5 之前必为空）

这两项**在 Task 3 内必然为空**，由 Task 5 的接线负责让它们出现；Task 5 Step 8 会重跑同一检查。此处仅记录预期：


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
  - `struct Inputs { local_set: u8, server_set: u8, has_pending: u8, wifi_ready: u8, pending_bytes: u32, pending_lines: u32, fail_streak: u32, _pad: [u8;4], last_fail_s: i64, now_s: i64 }`（size 40）
  - `struct Decision { action: u8, _pad: [u8;3], max_bytes: u32 }`（size 8）
  - `OPINION_NONE/OFF/ON: u8`，`SKIP_DISABLED/EMPTY/NO_NET/BACKOFF: u8`，`UPLOAD: u8`
  - `fn resolve_enabled(local_set: u8, server_set: u8) -> bool` — 服务端明确则服务端为准；否则本地明确则本地；都无意见则关
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
        local_set: OPINION_NONE,
        server_set: OPINION_ON,
        has_pending: 1,
        wifi_ready: 1,
        // ABOVE MAX_UPLOAD_BYTES (1024) so the happy path actually exercises the
        // cap; `max_bytes_is_capped_by_pending_bytes` pins the other bound of
        // the min. (The first draft used 100, which cannot yield max_bytes ==
        // MAX_UPLOAD_BYTES and made the happy-path assertion unsatisfiable.)
        pending_bytes: 1500,
        pending_lines: 2,
        fail_streak: 0,
        _pad: [0; 4],
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
fn service_switch_alone_enables() {
    // Neither side needs the other: the service can turn it on by itself.
    let i = Inputs { local_set: OPINION_NONE, server_set: OPINION_ON, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn local_switch_alone_enables() {
    // And so can the device holder, with no service involvement.
    let i = Inputs { local_set: OPINION_ON, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn neither_side_speaking_leaves_it_off() {
    let i = Inputs { local_set: OPINION_NONE, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn service_off_overrides_local_on() {
    // The documented conflict rule: the service wins.
    let i = Inputs { local_set: OPINION_ON, server_set: OPINION_OFF, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn service_on_overrides_local_off() {
    let i = Inputs { local_set: OPINION_OFF, server_set: OPINION_ON, ..base() };
    assert_eq!(decide(&i).action, UPLOAD);
}

#[test]
fn local_off_alone_disables() {
    let i = Inputs { local_set: OPINION_OFF, server_set: OPINION_NONE, ..base() };
    assert_eq!(decide(&i).action, SKIP_DISABLED);
}

#[test]
fn resolve_enabled_is_the_conflict_rule() {
    assert!(resolve_enabled(OPINION_NONE, OPINION_ON));
    assert!(resolve_enabled(OPINION_ON, OPINION_NONE));
    assert!(!resolve_enabled(OPINION_NONE, OPINION_NONE));
    assert!(!resolve_enabled(OPINION_ON, OPINION_OFF), "service off wins");
    assert!(resolve_enabled(OPINION_OFF, OPINION_ON), "service on wins");
}

#[test]
fn disabled_switch_skips() {
    let i = Inputs { server_set: OPINION_OFF, ..base() };
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
    // Precedence: the composed switch is checked before pending/network so a
    // disabled device never even inspects the buffer.
    let i = Inputs {
        server_set: OPINION_OFF, has_pending: 0, wifi_ready: 0, ..base()
    };
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
    assert_eq!(offset_of!(Inputs, local_set), 0);
    assert_eq!(offset_of!(Inputs, server_set), 1);
    assert_eq!(offset_of!(Inputs, has_pending), 2);
    assert_eq!(offset_of!(Inputs, wifi_ready), 3);
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
    uint8_t  local_set;      /* rf_log_upload_opinion_t: device settings menu */
    uint8_t  server_set;     /* rf_log_upload_opinion_t: schedule policy */
    uint8_t  has_pending;
    uint8_t  wifi_ready;
    uint32_t pending_bytes;
    uint32_t pending_lines;
    uint32_t fail_streak;
    uint32_t _pad;
    int64_t  last_fail_s;    /* negative = none yet */
    int64_t  now_s;          /* negative = wall clock unset */
} rf_log_upload_inputs_t;    /* sizeof == 40 */

typedef struct {
    uint8_t  action;         /* rf_log_upload_action_t */
    uint8_t  _pad[3];
    uint32_t max_bytes;
} rf_log_upload_decision_t;  /* sizeof == 8 */

/* Both switches are three-state. "No opinion" is not "off": it is what lets
 * the other side decide. See rf_log_upload_resolve below. */
typedef enum {
    RF_LOG_OPINION_NONE = 0,
    RF_LOG_OPINION_OFF = 1,
    RF_LOG_OPINION_ON = 2,
} rf_log_upload_opinion_t;

typedef enum {
    RF_LOG_UPLOAD = 0,
    RF_LOG_SKIP_DISABLED = 1,
    RF_LOG_SKIP_EMPTY = 2,
    RF_LOG_SKIP_NO_NET = 3,
    RF_LOG_SKIP_BACKOFF = 4,
} rf_log_upload_action_t;

/* The conflict rule: an explicit service opinion wins, otherwise an explicit
 * local one, otherwise off. Exposed so C++ can render the same answer the
 * decision used. */
uint8_t rf_log_upload_resolve(uint8_t local_set, uint8_t server_set);
rf_log_upload_decision_t rf_log_upload_decide(const rf_log_upload_inputs_t* in);
uint32_t rf_log_upload_backoff_s(uint32_t streak, uint32_t base_s, uint32_t max_s);

#endif  /* LOG_UPLOAD_POLICY_H */
```

- [ ] **Step 4: 实现策略**

`firmware/main/rust/src/log_upload_policy.rs`：

```rust
//! Log-upload gate. Pure: no I/O, no globals — C++ gathers the facts (both
//! switch opinions, ring occupancy, link state, RTC failure stamps) and this
//! decides whether to upload and how many bytes.
//!
//! Backoff reuses the shape already in `notify_policy` (`base * 2^(n-1)`,
//! capped) rather than inventing a second convention.

/// Single-upload cap. Bounded by the deep-sleep cycle's HTTP budget and by the
/// fixed request buffer on the C++ side.
pub const MAX_UPLOAD_BYTES: u32 = 1024;

/// Both switches are three-state. "No opinion" is not "off": it is precisely
/// what lets the other side decide.
pub const OPINION_NONE: u8 = 0;
pub const OPINION_OFF: u8 = 1;
pub const OPINION_ON: u8 = 2;

pub const UPLOAD: u8 = 0;
pub const SKIP_DISABLED: u8 = 1;
pub const SKIP_EMPTY: u8 = 2;
pub const SKIP_NO_NET: u8 = 3;
pub const SKIP_BACKOFF: u8 = 4;

pub const BASE_BACKOFF_S: u32 = 60;
pub const MAX_BACKOFF_S: u32 = 900;

/// The conflict rule, in one place: an explicit service opinion wins,
/// otherwise an explicit local one, otherwise off. The service is the
/// authority (it is the side an operator reaches remotely); the local switch
/// is the device holder's own consent, honoured when the service is silent.
pub fn resolve_enabled(local_set: u8, server_set: u8) -> bool {
    if server_set == OPINION_ON {
        return true;
    }
    if server_set == OPINION_OFF {
        return false;
    }
    local_set == OPINION_ON
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Inputs {
    pub local_set: u8,
    pub server_set: u8,
    pub has_pending: u8,
    pub wifi_ready: u8,
    pub pending_bytes: u32,
    pub pending_lines: u32,
    pub fail_streak: u32,
    pub _pad: [u8; 4],
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
    // Composed switch first: a disabled device must not even look at the
    // buffer. Both sides silent resolves to false (off by default).
    if !resolve_enabled(i.local_set, i.server_set) {
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
Expected: PASS（18 passed）

- [ ] **Step 7: 哨兵变异验证（证明测试不是假的）**

把 `decide` 里的 `!resolve_enabled(i.local_set, i.server_set)` 临时改成 `if false`，重跑：
Expected: `disabled_switch_skips`、`disabled_beats_everything`、`service_off_overrides_local_on`、`local_off_alone_disables`、`neither_side_speaking_leaves_it_off` FAIL，其余仍 PASS。改回。

把 `resolve_enabled` 里的 `if server_set == OPINION_OFF { return false; }` 删掉，重跑：
（注：直接删掉会让 `OPINION_OFF` 分支落到末尾的 `local_set == OPINION_ON` 上，而这在 Rust 里是**另一个分支**、删掉后 `server_set == OPINION_OFF` 只剩一个不可达的 match 臂 —— 用 if 形式时它会编译为 E0317「if may be missing an else clause」。等价且可编译的变异：把该 `if` 的 body 改成 `return true;`（即让 OFF 不再否决），效果相同。）
Expected: 只有 `service_off_overrides_local_on` 与 `resolve_enabled_is_the_conflict_rule` FAIL——**这两个测试正是「冲突以服务端为准」这条规则的守卫**。改回。

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
    fn schedule_policy_log_upload_is_parsed_as_three_states() {
        // The service switch arrives in the policy block (the query string has
        // no room: CBuf::<160> is already ~93 bytes and overflow voids the
        // whole GET). null must survive as OPINION_NONE -- collapsing it to 0
        // would make the service permanently override the local switch.
        let _g = shim::host::lock();

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload(1));
        sync_once();
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_ON);

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload(0));
        sync_once();
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_OFF);

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload_null());
        sync_once();
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_NONE,
                   "null is 'no opinion', not 'off'");

        // Old firmware / old server: the key is absent entirely. Same meaning.
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_NONE);
    }
```

（`schedule_json_log_upload(n)` 照同模块既有 `schedule_json` 辅助的写法构造，`policy` 里多一个 `"log_upload": n` 字段；`schedule_json_log_upload_null()` 是同一个辅助、值为 JSON `null`。）

- [ ] **Step 3: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test schedule_policy_log_upload_is_parsed_as_three_states`
Expected: FAIL — `cannot find function log_upload_server_get`

- [ ] **Step 4: 实现开关存储与解析**

在 `page_sync.rs` 加 RTC 侧存储的薄包装（经 `shim` 转发，Rust 不自己碰 RTC）：

```rust
/// The service's log-upload opinion from the last schedule response, as a
/// three-state value. Kept in the RTC snapshot with the power counters, so a
/// deep-sleep wake (RTC cleared) defaults to OPINION_NONE until the next sync.
pub fn log_upload_server_get() -> u8 {
    unsafe { shim::rf_log_upload_server_get() }
}

pub fn log_upload_server_set(v: u8) {
    unsafe { shim::rf_log_upload_server_set(v) }
}
```

在 `Protocol`/`Policy` 解析处读取 `policy.log_upload`，映射 `null`/缺失 → `OPINION_NONE`、`0` → `OPINION_OFF`、`1` → `OPINION_ON`；`sync_once` 成功回调里 `log_upload_server_set(mapped)`。

**这里必须显式处理 JSON `null`，且已核实可行。** `json.rs` 的扫描器本身识别 `null`（`json.rs:85`），但 `int_value` 对 `null` 与「键不存在」都返回 `None`（它走 `parse::<i64>()`，`null` 解析失败）——**直接用它会把两种语义折平**。区分办法：用 `json::member()` 拿值的起始位置，`None` = 键不存在，`Some(pos)` = 键存在（此时再看该位置是否 `n`）。

C++ 侧 `shim.cpp` 加两个一行转发：

```cpp
RTC_DATA_ATTR static uint8_t g_log_upload_server_set;
extern "C" uint8_t rf_log_upload_server_get(void) { return g_log_upload_server_set; }
extern "C" void rf_log_upload_server_set(uint8_t v) { g_log_upload_server_set = v; }
```

同时加本地开关的 NVS 读写——**本任务拥有它**（上行要读它才能编译；Task 6 只加菜单与切换规则）：

```cpp
// 本地日志上报开关，三态。用两个布尔表达：present 记"用户是否表过态"，
// on 记方向。未设置 => 0（无意见），所以服务端开关能生效。
//
// 与 Wi-Fi 开关的关键差别：那个是 RAM 静态量（g_wifi_switch_intent），
// 本项必须落 NVS —— 设备每 10 分钟深睡一次会掉 RAM。
extern "C" uint8_t rf_log_upload_local_get(void) {{
    Settings s("wifi");
    if (!s.GetBool("log_up_present", false)) return 0;   // OPINION_NONE
    return s.GetBool("log_up_on", false) ? 2 : 1;        // ON / OFF
}}

extern "C" void rf_log_upload_local_set(uint8_t v) {{
    Settings s("wifi", /* read_write = */ true);
    if (v == 0) {{
        s.SetBool("log_up_present", false);
        return;
    }}
    s.SetBool("log_up_present", true);
    s.SetBool("log_up_on", v == 2);
}}
```

（`#include "settings.h"` 加到 `shim.cpp` 顶部 include 区。）

在 `shim.rs` 声明这四个符号（server get/set + local get/set）。

- [ ] **Step 5: 实现上报函数**

在 `page_sync.rs` 加（照 `notify.rs` 的 POST 范式：`CBuf` 建 URL、`rf_build_endpoint`、base64、`rf_http_post_json`）：

```rust
/// One upload attempt. Returns true when the server accepted the batch.
/// Caller must already hold no lock: HTTP can block for seconds.
fn log_upload_try_once() -> bool {
    let mut dropped = 0u32;
    let mut used = 0u32;
    unsafe { shim::rf_logbuf_stats(&mut dropped, &mut used) };
    // Read BEFORE building Inputs: `rf_logbuf_read` is the only source that
    // knows how many whole frames the pending bytes contain, and that count
    // (`lines`) is what the ack arithmetic later needs. Reading is a bounded
    // byte copy under the ring lock — cheap, unlike the HTTP that `decide`
    // gates. `seq_lo` is meaningful only when `lines > 0`.
    let mut read_buf = CBuf::<1536>::new();
    let mut seq_lo = 0u32;
    let mut lines = 0u32;
    unsafe {
        shim::rf_logbuf_read(read_buf.as_mut_ptr(), 1536, &mut seq_lo, &mut lines);
    }
    read_buf.set_len_from_terminator();
    let (last_fail_s, fail_streak) = unsafe { shim::rf_log_upload_fail_state() };
    let inputs = log_upload_policy::Inputs {
        // Both opinions are three-state: the schedule policy is 0=none/1=off/
        // 2=on, and the device's own NVS switch uses the same encoding.
        local_set: shim::rf_log_upload_local_get(),
        server_set: log_upload_server_get(),
        has_pending: if used > 0 { 1 } else { 0 },
        wifi_ready: 1,  // the caller only reaches here on a completed sync
        pending_bytes: used,
        pending_lines: lines,
        fail_streak,
        _pad: [0; 4],
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

三个取值点的来源（都要在 `shim.rs` 声明，C++ 侧实现）：

| 符号 | 来源 |
| --- | --- |
| `rf_log_upload_server_get() -> u8`（`page_sync.rs` 内包一层 `log_upload_server_get()`） | schedule 响应的 `policy.log_upload`（`null`→0、`0`→1、`1`→2），存 RTC |
| `rf_log_upload_local_get() -> u8` | 读 NVS 的本地开关，映射成 `0/1/2`（未设置 = 0 无意见）。**本任务实现它**——上行要读它才能编译；Task 6 只加菜单项与切换规则，不重复实现 |
| `rf_log_upload_fail_state(out_last_s: *mut i64, out_streak: *mut u32)` | RTC 里的退避戳与失败计数，与 `rf_fail_streak_*` 同位置 |

上面代码里的 `log_upload_server_get()` 就是 `page_sync.rs` 对 `rf_log_upload_server_get()` 的薄包装（与既有的 `log_upload_enabled()` 同一手法）。**三态映射不能折平**：`policy.log_upload` 的 `null` 必须落成 `OPINION_NONE`，否则本地开关永远被覆盖。

实现要点：`rf_logbuf_stats` 拿 `used`/`dropped`；`rf_logbuf_read` 取字节（缓冲用 `CBuf::<1536>`，同时拿 `out_seq_lo` 与 `out_lines`）；base64 照 `notify.rs` 用 `B64`；body 用 `CBuf::<2048>`；URL 用 `CBuf::<320>` + `rf_build_endpoint("/api/device-log", ...)`；HTTP 201 才 `rf_logbuf_ack(seq_lo + lines - 1)` 并清零 streak，否则 `fail_streak += 1`、`last_fail_s = now`。

**顺序与零行约定**：先 `rf_logbuf_read` 拿 `(seq_lo, lines)`，再用它填 `Inputs.pending_lines`，然后才 `decide`；`lines == 0` 时**不要** ack（`seq_lo` 在零行时无意义，`seq_lo + 0 - 1` 会下溢成 `0xFFFFFFFF`，而 `ack` 的 `seq > seq_hi` 判断会因此 ack 掉全部）。`shim_log.h` 已声明该约定。

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

## Task 6: 设备侧本地开关（设置菜单 + NVS）

**Files:**
- Modify: `firmware/main/rust/include/settings_menu.h`（item 枚举）
- Modify: `firmware/main/rust/src/settings.rs`（菜单项 + `ITEM_*` 常量 + 切换规则 + 单测）
- Modify: `firmware/main/application.cc`（Toggle 分支：写 NVS + 重绘）
- Test: `firmware/main/rust/src/settings.rs` 的 `mod tests`

**Interfaces:**
- Consumes: Task 4 的 `OPINION_NONE/OFF/ON`；**Task 5 已实现的** `rf_log_upload_local_get()` / `rf_log_upload_local_set(u8)`（NVS 读写归 Task 5，本任务不重复实现）
- Produces:
  - 设置项 id `RF_SETTINGS_ITEM_LOG_UPLOAD = 12`（Rust `ITEM_LOG_UPLOAD: u8 = 12`）
  - `fn settings::log_upload_toggle_target(current: u8) -> u8` — 三态切换规则

- [ ] **Step 1: 写失败测试**

追加到 `firmware/main/rust/src/settings.rs` 的 `mod tests`：

```rust
    #[test]
    fn log_upload_row_exists_as_a_toggle() {
        let items: Vec<_> = NETWORK_ITEMS.iter().filter(|i| i.id == ITEM_LOG_UPLOAD).collect();
        assert_eq!(items.len(), 1, "exactly one log-upload row");
        assert_eq!(items[0].kind, Kind::Toggle, "it is a switch, not an action");
    }

    #[test]
    fn toggle_target_cycles_none_on_off() {
        // Three states, so the press has to say where it goes. NONE is the
        // "let the service decide" position and must be reachable -- without
        // it, a user who once pressed the switch could never hand control back.
        assert_eq!(log_upload_toggle_target(OPINION_NONE), OPINION_ON);
        assert_eq!(log_upload_toggle_target(OPINION_ON), OPINION_OFF);
        assert_eq!(log_upload_toggle_target(OPINION_OFF), OPINION_NONE);
    }

    #[test]
    fn toggle_target_is_total() {
        // An out-of-range byte must not panic.
        assert_eq!(log_upload_toggle_target(99), OPINION_ON);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test log_upload`
Expected: FAIL — `cannot find value ITEM_LOG_UPLOAD` / `cannot find function log_upload_toggle_target`

- [ ] **Step 3: 加设置项与切换规则**

`firmware/main/rust/include/settings_menu.h` 的 item 枚举末尾加：

```c
    /* 日志上报：设备侧的同意开关（NVS）。三态，见 log_upload_policy.h 的
     * opinion 编码。第三种状态（无意见 = 让服务端决定）由按键循环抵达。 */
    RF_SETTINGS_ITEM_LOG_UPLOAD = 12,
```

`firmware/main/rust/src/settings.rs` 加常量、切换规则与菜单项：

```rust
pub const ITEM_LOG_UPLOAD: u8 = 12;

/// Cycles the device-side log-upload opinion. Three states, so a single press
/// walks NONE -> ON -> OFF -> NONE. The NONE position is what hands control
/// back to the service.
pub fn log_upload_toggle_target(current: u8) -> u8 {
    match current {
        crate::log_upload_policy::OPINION_ON => crate::log_upload_policy::OPINION_OFF,
        crate::log_upload_policy::OPINION_OFF => crate::log_upload_policy::OPINION_NONE,
        _ => crate::log_upload_policy::OPINION_ON,
    }
}
```

在 `NETWORK_ITEMS` 里加一行（放在 `ITEM_WIFI_ERROR` 之后）：

```rust
    I { id: ITEM_LOG_UPLOAD, label: c"日志上报", kind: Kind::Toggle },
```

- [ ] **Step 4: 接设置页的 Toggle 分支**

`firmware/main/application.cc` 的设置项 switch（`RF_SETTINGS_ITEM_WIFI_TOGGLE` 兄弟分支）加：

```cpp
                case RF_SETTINGS_ITEM_LOG_UPLOAD: {
                    const uint8_t next =
                        rf_settings_log_upload_toggle(rf_log_upload_local_get());
                    rf_log_upload_local_set(next);
                    // The row shows the opinion just written.
                    sr->SetItemChecked(RF_SETTINGS_ITEM_LOG_UPLOAD, next == 2);
                    break;
                }
```

（`rf_settings_log_upload_toggle` 是 `settings.rs` 的 `log_upload_toggle_target` 导出的 C ABI 包装，与既有 `rf_settings_wifi_switch_shown` 同一手法。）

- [ ] **Step 5: 双门禁**

Run:
```bash
cd firmware/main/rust && export PATH="$HOME/.cargo/bin:$PATH" && cargo test
```
Expected: PASS

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"
source ~/data/esp-idf-v6.0/export.sh
cd firmware
IDF_TARGET=esp32s3 idf.py build
```
Expected: `Project build complete`

- [ ] **Step 6: 提交**

```bash
git add firmware/main/rust/include/settings_menu.h firmware/main/rust/src/settings.rs firmware/main/application.cc
git commit -m "feat(firmware): device-side log-upload switch in the settings menu"
```

---

## Task 7: 前端开关与日志查看

**Files:**
- Modify: `frontend/src/api.js`
- Modify: `frontend/src/pages/Devices.jsx`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: `POST /api/devices/{id}/log-upload`、`GET /api/devices/{id}/logs?tail=N`（Task 2）

- [ ] **Step 1: 加 api 方法**

`frontend/src/api.js`，在 `api` 对象里加：

```js
  // value is a three-state opinion: 1 = force on, 0 = force off, null = let
  // the device decide. Not a boolean -- passing false would mean "force off"
  // and permanently override the device's own switch.
  setLogUpload: (id, value) =>
    request(`/devices/${encodeURIComponent(id)}/log-upload`, { method: 'POST', body: { value } }),
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
                      {/* Three-state control: 开启 (1) / 关闭 (0) / 跟随设备 (null).
                          A boolean switch cannot express "no opinion", and
                          without that state the service could never hand
                          control back to the device. */}
                      <select
                        value={d.log_upload === null || d.log_upload === undefined ? 'auto' : String(d.log_upload)}
                        disabled={busy === 'log:' + d.device_id}
                        onChange={(e) => setLogUpload(d, e.target.value)}
                        style={{ fontSize: 12 }}
                      >
                        <option value="1">开启</option>
                        <option value="0">关闭</option>
                        <option value="auto">跟随设备</option>
                      </select>
                      {localOverrideNote(d) && (
                        <div className="muted" style={{ fontSize: 11 }}>{localOverrideNote(d)}</div>
                      )}
                    </td>
```

- 加状态与处理器（照 `setTrust` 的写法）：

```jsx
  async function setLogUpload(d, raw) {
    const value = raw === 'auto' ? null : Number(raw);
    setBusy('log:' + d.device_id);
    try {
      await api.setLogUpload(d.device_id, value);
      await refresh('devices', fetchDevices);
    } catch (e) {
      setErr(e.message);
    } finally {
      setBusy('');
    }
  }
```

- 加 `localOverrideNote` 辅助：**只显示服务端值时，运维会以为自己没点上**。该列必须同时说明设备侧的意见与最终结果。

```jsx
// Explains why an enabled service setting may do nothing: the service only
// wins when it has an opinion, and it always wins when it does. Returns ''
// when there is nothing surprising to say.
function localOverrideNote(d) {
  // local_log_upload comes from the schedule uplink (?lo=): 0 none, 1 off, 2 on.
  const local = d.local_log_upload;
  const svc = d.log_upload;
  if (local === undefined || local === 0) return '';
  const localOn = local === 2;
  if (svc === null || svc === undefined) {
    return localOn ? '设备端已开' : '设备端已关';
  }
  const svcOn = svc === 1;
  if (svcOn === localOn) return '';
  return `服务端已覆盖：设备端${localOn ? '开' : '关'}`;
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
1. 设备页出现"日志上报"列，三态控件，默认选中"跟随设备"。
2. 选"开启"→ 刷新页面后仍为"开启"（已持久化）。
3. 选"关闭"→ 刷新后仍为"关闭"。
4. 选"跟随设备"→ 刷新后仍为"跟随设备"（`null` 未被折成 `0`）。
5. 设备上报过一次后，点"日志"能看到行。
6. **覆盖提示**：设备侧开关置为"开"、服务端选"关闭"时，该列下方出现"服务端已覆盖：设备端开"。

- [ ] **Step 5: 提交**

```bash
git add frontend/src/api.js frontend/src/pages/Devices.jsx .gitignore
git commit -m "feat(frontend): per-device log-upload switch and log viewer"
```

---

## Task 8: 端到端真机验收

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

- [ ] **Step 2: 四种开关组合逐一验证**

这是本功能的核心判据——**两侧都是信任点，冲突时服务端为准**。每种组合等 ≥2 个轮询周期（≤20 分钟），再比对日志文件的末尾时间戳：

```bash
tail -n 3 server/data/devicelogs/NOTE4C-3400FC.log
```

| 服务端 | 本地（设置菜单） | 期望：是否上报 | 测法 |
| --- | --- | --- | --- |
| 跟随设备 | 开 | **上报** | 设置页按到"开"，后台设"跟随设备" |
| 跟随设备 | 关 | **不上报** | 设置页按到"关" |
| 跟随设备 | 无意见 | **不上报** | 设置页再按一次回到"无意见" |
| 开启 | 关 | **上报**（服务端赢） | 后台设"开启"，设置页保持"关" |
| 关闭 | 开 | **不上报**（服务端赢） | 后台设"关闭"，设置页设"开" |
| 关闭 | 无意见 | **不上报** | 后台设"关闭" |

后台 `local_log_upload` 的显示也一并核对（对应 Task 7 的覆盖提示）。

同时确认服务端确实收到了请求：
```bash
journalctl --user -u youn-ink-server --since "30 min ago" | grep device-log
```
Expected: 上报为真的组合各至少一条 `POST /api/device-log HTTP/1.1" 201`；上报为假的组合在等待窗口内没有新增行

- [ ] **Step 3: 崩溃前日志存活验证（本设计的核心价值）**

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

- [ ] **Step 4: 记录真机结论**

把观察写进 `docs/superpowers/progress/`（照 `2026-09-24-rr4-diagnosis.md` 的格式），至少包含：
- 四种开关组合各自的实测结果，特别是「服务端明确关 vs 本地开」是否确实停止上报（冲突规则）；
- 复位前日志是否确实出现在复位后第一次上报里（这是设计成立与否的判据）；
- 若出现环形覆盖丢行（日志里见到 `dropped N lines`），记录当时的行率与时间。

- [ ] **Step 5: 提交验证记录**

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
| §4 Rust 策略（合成规则/决策表/退避/分段/base64） | Task 4；Task 5 Step 5 |
| §4 `resolve_enabled` 冲突规则 | Task 4 Step 1（`resolve_enabled_is_the_conflict_rule`）、Step 7 哨兵 |
| §4 接线进 schedule 周期 | Task 5 Step 5-6 |
| §5 双侧三态合成语义 | Task 4（规则）+ Task 5 Step 2-4（服务端意见解析）+ Task 6（本地意见） |
| §5 服务端可空三态列 + 下发 | Task 2 Step 3-4 |
| §5 上行 `lo=` 回传 + 后台可观测 | Task 2 Step 4；Task 7 `localOverrideNote` |
| §5 上行端点 + 存储 + 轮转 | Task 1、Task 2 Step 4 |
| §5 读取端点 | Task 2 Step 4 |
| §5 `device_log_dir` 配置 + mkdir | Task 1 Step 3 |
| §5 `.gitignore` | Task 7 Step 3 |
| §6 前端三态控件与弹窗 | Task 7 |
| §6 设备侧本地开关（设置菜单 + NVS） | Task 6 |
| §7 测试要求（布局契约/哨兵/fixture 隔离） | Task 4 Step 1/7；Task 1 `_isolate_device_log_dir` |
| §7 实施顺序 | 任务编号即顺序（本地开关排在固件装配之后、前端之前） |
| §7 `shim_log.cpp` 独立文件 + CMake 登记 | Task 3 Step 3 |
| §7 钩子链式转发 | Task 3 Step 2（`g_prev_vprintf`） |
| §8 风险 | Task 8 Step 4 观测项 |

无遗漏。

**② 占位符扫描**：无 "TBD/TODO/类似 Task N"。Task 6 的 NVS 三态编码（两个布尔）与 Task 5 的服务端三态映射都给了完整代码；Task 5 Step 4 提到「查一下 `json.rs` 是否已有 null 识别」已就地核实并写明结论（扫描器识别 `null`，但 `int_value` 会把 `null` 与缺键折平，须用 `member()` 区分）。Task 5 Step 5 的 `log_upload_try_once` 给出了完整签名、输入构造、成功/失败分支与每条约束（CBuf 尺寸、base64、ack 时机、streak 更新）；省略的只有逐行样板，已指明照 `notify.rs` 的哪一处。Task 5 Step 2 的测试辅助 `schedule_json_with_policy_log_upload` 已说明构造方式（照同模块既有 `schedule_json` 加一个字段）。

**③ 类型一致性**：`Inputs` 字段顺序/偏移在 Task 4 的 Rust 与 `log_upload_policy.h` 两处逐字一致（size 40，`local_set`@0、`server_set`@1、`has_pending`@2、`wifi_ready`@3、`pending_bytes`@4、`pending_lines`@8、`fail_streak`@12、`last_fail_s`@24、`now_s`@32）；`Decision` size 8、`max_bytes`@4 两处一致；三态枚举 `OPINION_NONE/OFF/ON` 与 `RF_LOG_OPINION_NONE/OFF/ON` 数值一致（0/1/2）；`rf_logbuf_read/ack/stats/install_hook` 在 Task 3 头文件、Task 3 实现、Task 5 的 `shim.rs` 声明三处同名同参；`rf_log_upload_server_get/set` 在 Task 5 的 `shim.rs` 与计划给出的 C++ 定义两处一致；`rf_log_upload_local_get/set` 在 Task 5 的 `shim.rs` 声明与 C++ 定义两处一致，Task 6 只消费；`MAX_UPLOAD_BYTES` 在 Task 4 定义为 1024，Task 5 的 body 缓冲 2048 与其 base64 膨胀一致；`devicelog.append_lines/tail_lines` 在 Task 1 定义、Task 2 消费，参数名与类型一致；服务端 `value` 与前端 `setLogUpload(id, value)` 的三态（`1`/`0`/`null`）贯通。

**④ 一处已知的实现风险（不阻塞，Task 7 观测）**：`capture_hook` 内用 `vsnprintf`，而钩子可能落在 cache 关闭窗口。若真机出现该窗口下的异常，Task 7 Step 5 记录后另开任务处理——本计划不预先加防护，因为没有证据表明它在实践中发生。
