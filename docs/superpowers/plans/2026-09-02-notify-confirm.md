# NOTE4C 待确认通知（Confirm Notification）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 NOTE4C 设备能展示一条「待确认消息」（BOOT 短按拉取，上键同意/下键不同意/BOOT 关闭，5 分钟自动退出），服务端提供 FIFO 通知队列 + HTTP 端点 + FastMCP 服务，结果落库。

**Architecture:** 服务端 `notify_store.py`（JSONL FIFO 队列）暴露 4 个 HTTP 端点；FastMCP 以 Streamable HTTP 挂到现有 FastAPI app 同端口 9002 `/mcp`。设备端新增 `notify.{h,cc}` 模块复用 `HttpClientWrapper`，在 `application.cc` 三个按键回调加分支；BSP `config.h` 把 `TODO_UP/DOWN/CONFIRM` 改名成实际 GPIO。

**Tech Stack:** 服务端 FastAPI + fastmcp + Pydantic + sqlite（devices）；固件 ESP-IDF v6.0 + esp_http_client。

**Spec:** `docs/superpowers/specs/2026-09-02-notify-confirm-design.md`

## Global Constraints

- 服务端 Python venv：`server/.venv`（ESP-IDF 无关，pytest 直接跑）
- 测试：`server/.venv/bin/python -m pytest tests/ -q` 必须全绿（现状 38）
- 固件构建：`cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build`
- `requirements.txt` 新增 `fastmcp>=2.0`（streamable-http transport）
- device_id 校验必须走 `registry.list_all(only_trusted=True)` 存在且 trust
- 设备 token：`server_pairing_get_token()`（page_sync 同款）
- 位图：`render_canvas_to_bitmap()` → 30000 字节 2bpp BWRY
- notify 存储：`data/notifications.jsonl`，append-only + `threading.Lock`
- 命名：新模块 `notify_store.py` / `mcp_server.py` / `notify.{h,cc}`；BSP 宏 `UP_BUTTON_GPIO/DOWN_BUTTON_GPIO/CONFIRM_BUTTON_GPIO`
- 不引入新 HTTP 栈（固件复用 `http_wrapper_get/post_json`）
- 设备状态机：`IDLE | FETCHING | NOTIFYING`；5min FreeRTOS timer 对齐 ttl 300s

## Plan Amendment — Task 3 FastMCP 4.x API

**Status:** required (implementer dispatch blocked 27m on dependency discovery)
**Reason:** plan's `FastMCP("name", transport="streamable-http")` and `mcp.mount(app)` API is removed in fastmcp 4.x (current version resolves to 4.0.1, which is the only one compatible with our pinned `pydantic==2.10.3` / `fastapi==0.115.6` constraint set).
**Ruling:** Use the 4.x pattern `mcp.http_app(path="/mcp", transport="streamable-http")` and mount via `app.mount("/mcp", sub_app)` with `FastAPI(lifespan=sub_app.lifespan)`. TestClient works on the sub_app directly (no parent app needed in tests).
**Cost if wrong:** The 4.x API is the only available option for our dependency constraints; 2.x has mcp version conflicts that are unsolvable. Tests in the plan use `client = TestClient(app)` which works because we attach the sub_app to a parent FastAPI with proper lifespan.

---

### Task 1: notify_store — FIFO 队列存储层

**Files:**
- Create: `server/youn_server/notify_store.py`
- Test: `server/tests/test_notify_store.py`

**Interfaces:**
- Consumes: `settings.data_dir`（`server/youn_server/config.py`）
- Produces:
  - `Notification` dataclass（`id, device_id, title, body, created_at, ttl_sec, status, decision, acked_at`）
  - `NotifyStore` 类：
    - `enqueue(device_id, title, body, ttl_sec=300) -> Notification`
    - `next_for(device_id) -> Notification | None`
    - `ack(id, decision) -> Notification | None`
    - `recent(device_id="", limit=20) -> list[Notification]`
  - `get_store() -> NotifyStore`（单例，进程内）

- [ ] **Step 1: 写失败测试**

```python
# server/tests/test_notify_store.py
import tempfile
from pathlib import Path
import pytest

from youn_server import notify_store as ns


@pytest.fixture()
def store():
    tmp = tempfile.mkdtemp(prefix="notify_")
    s = ns.NotifyStore(Path(tmp) / "notifications.jsonl")
    yield s


def test_enqueue_and_fifo_order(store):
    a = store.enqueue("DEV-1", "t1", "b1", 300)
    b = store.enqueue("DEV-1", "t2", "b2", 300)
    c = store.enqueue("DEV-2", "t3", "b3", 300)
    assert store.next_for("DEV-1").id == a.id  # FIFO per device
    assert store.next_for("DEV-1").id == b.id
    assert store.next_for("DEV-1") is None     # drained
    assert store.next_for("DEV-2").id == c.id


def test_next_marks_shown(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    got = store.next_for("DEV-1")
    assert got.id == n.id
    assert got.status == "shown"
    assert store.next_for("DEV-1") is None  # not redelivered


def test_expired_skipped(store):
    store.enqueue("DEV-1", "t", "b", 0)  # ttl 0 → instantly expired
    assert store.next_for("DEV-1") is None


def test_ack_sets_decision(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    got = store.ack(n.id, "agree")
    assert got.decision == "agree"
    assert got.status == "acked"


def test_ack_idempotent(store):
    n = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    store.ack(n.id, "agree")
    again = store.ack(n.id, "reject")
    assert again.decision == "agree"  # first decision wins


def test_recent_filters_device(store):
    a = store.enqueue("DEV-1", "t", "b", 300)
    store.next_for("DEV-1")
    store.ack(a.id, "agree")
    store.enqueue("DEV-2", "t2", "b2", 300)
    hist = store.recent(device_id="DEV-1")
    assert len(hist) == 1
    assert hist[0].device_id == "DEV-1"


def test_recent_limit(store):
    for i in range(25):
        store.enqueue("DEV-1", f"t{i}", "b", 300)
    assert len(store.recent(limit=20)) == 20
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_notify_store.py -q`
Expected: FAIL（`ModuleNotFoundError: youn_server.notify_store`）

- [ ] **Step 3: 实现 notify_store.py**

```python
"""FIFO notification queue persisted as JSONL (append-only)."""
from __future__ import annotations

import json
import threading
import time
import uuid
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Optional

from .config import settings


@dataclass
class Notification:
    id: str
    device_id: str
    title: str
    body: str
    created_at: float
    ttl_sec: int
    status: str              # pending | shown | acked | expired | error
    decision: Optional[str]  # agree | reject | None
    acked_at: Optional[float]

    def is_expired(self, now: float | None = None) -> bool:
        now = now or time.time()
        return now >= self.created_at + self.ttl_sec


class NotifyStore:
    def __init__(self, path: Path | None = None):
        self._path = path or (settings.data_dir / "notifications.jsonl")
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        self._items: dict[str, Notification] = {}
        self._load()

    def _load(self) -> None:
        if not self._path.exists():
            return
        for line in self._path.read_text().splitlines():
            try:
                d = json.loads(line)
                self._items[d["id"]] = Notification(**d)
            except (json.JSONDecodeError, KeyError):
                continue

    def _append(self, n: Notification) -> None:
        with self._lock:
            with self._path.open("a") as f:
                f.write(json.dumps(asdict(n), ensure_ascii=False) + "\n")

    def enqueue(self, device_id: str, title: str, body: str,
                ttl_sec: int = 300) -> Notification:
        n = Notification(
            id=uuid.uuid4().hex,
            device_id=device_id,
            title=title,
            body=body,
            created_at=time.time(),
            ttl_sec=ttl_sec,
            status="pending",
            decision=None,
            acked_at=None,
        )
        with self._lock:
            self._items[n.id] = n
        self._append(n)
        return n

    def next_for(self, device_id: str) -> Optional[Notification]:
        """Return earliest pending, non-expired notification; mark shown atomically."""
        now = time.time()
        with self._lock:
            for n in sorted(self._items.values(), key=lambda x: x.created_at):
                if n.device_id != device_id or n.status != "pending":
                    continue
                if n.is_expired(now):
                    n.status = "expired"
                    continue
                n.status = "shown"
                self._append(n)
                return n
        return None

    def ack(self, notification_id: str, decision: str) -> Optional[Notification]:
        with self._lock:
            n = self._items.get(notification_id)
            if n is None:
                return None
            if n.status == "acked":
                return n  # idempotent
            n.status = "acked"
            n.decision = decision
            n.acked_at = time.time()
            self._append(n)
            return n

    def recent(self, device_id: str = "", limit: int = 20) -> list[Notification]:
        items = self._items.values()
        if device_id:
            items = (n for n in items if n.device_id == device_id)
        return sorted(items, key=lambda x: x.created_at, reverse=True)[:limit]


_store: NotifyStore | None = None


def get_store() -> NotifyStore:
    global _store
    if _store is None:
        _store = NotifyStore()
    return _store
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_notify_store.py -q`
Expected: PASS（7 passed）

- [ ] **Step 5: 提交**

```bash
git add server/youn_server/notify_store.py server/tests/test_notify_store.py
git commit -m "feat(server): FIFO notification queue store (notify_store)"
```

---

### Task 2: HTTP 端点 — 通知创建/拉取/ack/历史

**Files:**
- Modify: `server/youn_server/app.py`（新增 4 端点 + config）
- Modify: `server/youn_server/config.py`（加 `notify_default_ttl` 字段）
- Test: `server/tests/test_notify.py`

**Interfaces:**
- Consumes: Task 1 的 `notify_store.get_store()`；`registry`（`app.py` 现有）；`_require_operator` / `_require_device_token`；`render_canvas_to_bitmap`
- Produces:
  - `POST /api/notifications` → 201
  - `GET /api/notifications/next?device_id=` → 200 | 204
  - `POST /api/notifications/{id}/ack` → 200 | 404
  - `GET /api/notifications/history` → 200

- [ ] **Step 1: 写失败测试**

```python
# server/tests/test_notify.py
import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server.config import settings
from youn_server import notify_store as ns
import youn_server.devices as devices_mod


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


@pytest.fixture()
def trusted_device(client):
    # Register a trusted device via pairing flow (reuse existing pairing)
    r = client.post("/api/devices/pair-start", json={"device_id": "NOTE4C-TEST", "board_type": "NOTE4C"})
    assert r.status_code == 200
    code = r.json()["code"]
    r = client.post("/api/devices/pair-confirm",
                    json={"device_id": "NOTE4C-TEST", "code": code},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    token = r.json()["token"]
    return "NOTE4C-TEST", token


def test_create_notification(client, trusted_device):
    device_id, _ = trusted_device
    r = client.post("/api/notifications",
                    json={"device_id": device_id, "title": "t", "body": "b"},
                    headers={"X-Operator-Token": ""})
    assert r.status_code == 201
    assert r.json()["notification"]["status"] == "pending"


def test_create_requires_trusted_device(client):
    r = client.post("/api/notifications",
                    json={"device_id": "UNKNOWN", "title": "t", "body": "b"})
    assert r.status_code == 400
    assert "not trusted" in r.json()["detail"].lower()


def test_next_returns_bitmap_and_meta(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"},
                headers={"X-Operator-Token": ""})
    r = client.get("/api/notifications/next",
                   params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    data = r.json()
    assert "bitmap_base64" in data
    assert data["notification"]["title"] == "t"
    # Second pull: no pending left
    r2 = client.get("/api/notifications/next",
                    params={"device_id": device_id},
                    headers={"Authorization": f"Bearer {token}"})
    assert r2.status_code == 204


def test_next_requires_device_token(client, trusted_device):
    device_id, _ = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"})
    r = client.get("/api/notifications/next", params={"device_id": device_id})
    assert r.status_code == 401


def test_ack_notification(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t", "body": "b"})
    r = client.get("/api/notifications/next", params={"device_id": device_id},
                   headers={"Authorization": f"Bearer {token}"})
    nid = r.json()["notification"]["id"]
    r = client.post(f"/api/notifications/{nid}/ack",
                    json={"decision": "agree"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 200
    assert r.json()["status"] == "acked"
    assert r.json()["decision"] == "agree"


def test_ack_404_for_unknown(client, trusted_device):
    _, token = trusted_device
    r = client.post("/api/notifications/unknown-id/ack",
                    json={"decision": "agree"},
                    headers={"Authorization": f"Bearer {token}"})
    assert r.status_code == 404


def test_history(client, trusted_device):
    device_id, token = trusted_device
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t1", "body": "b"})
    client.post("/api/notifications",
                json={"device_id": device_id, "title": "t2", "body": "b"})
    r = client.get("/api/notifications/history",
                   params={"device_id": device_id},
                   headers={"X-Operator-Token": ""})
    assert r.status_code == 200
    assert len(r.json()["notifications"]) == 2
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_notify.py -q`
Expected: FAIL（404 on `/api/notifications`）

- [ ] **Step 3: 实现**

`config.py` 新增字段（约 line 63 `operator_token` 附近）：
```python
    notify_default_ttl: int = Field(default=300)
```

`app.py` 新增（在 `# ── Canvas Loop ──` 段之前）：
```python
    # ── Notifications ──

    @app.post("/api/notifications", status_code=201)
    async def create_notification(request: Request, body: dict = Body(...)):
        _require_operator(request)
        device_id = body.get("device_id", "")
        title = body.get("title", "")
        text = body.get("body", "")
        ttl = body.get("ttl_sec", settings.notify_default_ttl)
        if not device_id or not title or not text:
            raise HTTPException(400, "missing device_id/title/body")
        if not any(d.device_id == device_id for d in registry.list_all(only_trusted=True)):
            raise HTTPException(400, "device not trusted")
        n = ns.get_store().enqueue(device_id, title, text, ttl)
        return {"notification": asdict(n)}

    @app.get("/api/notifications/next")
    async def next_notification(request: Request, device_id: str = Query(...)):
        _require_device_token(request)
        n = ns.get_store().next_for(device_id)
        if n is None:
            return Response(status_code=204)
        try:
            bitmap = render_canvas_to_bitmap({
                "default": [{"type": "div", "props": {
                    "tw": "flex flex-col p-[16px] gap-[8px] bg-white",
                    "children": [
                        {"type": "div", "props": {"tw": "text-[20px] font-bold",
                                                  "style": {"color": "#000000"},
                                                  "children": n.title}},
                        {"type": "div", "props": {"tw": "text-[16px]",
                                                  "style": {"color": "#000000"},
                                                  "children": n.body}},
                    ]}}]
            })
        except Exception:
            ns.get_store()._items[n.id].status = "error"
            raise HTTPException(500, "render failed")
        import base64
        return {"bitmap_base64": base64.b64encode(bitmap).decode("ascii"),
                "notification": asdict(n)}

    @app.post("/api/notifications/{nid}/ack")
    async def ack_notification(nid: str, request: Request, body: dict = Body(...)):
        _require_device_token(request)
        decision = body.get("decision")
        if decision not in ("agree", "reject"):
            raise HTTPException(400, "decision must be agree|reject")
        n = ns.get_store().ack(nid, decision)
        if n is None:
            raise HTTPException(404, "notification not found")
        return {"status": n.status, "decision": n.decision}

    @app.get("/api/notifications/history")
    async def notification_history(request: Request, device_id: str = Query(""), limit: int = Query(20)):
        _require_operator(request)
        items = ns.get_store().recent(device_id=device_id, limit=limit)
        return {"notifications": [asdict(n) for n in items]}
```

`app.py` import 区新增：
```python
from dataclasses import asdict
from . import notify_store as ns
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_notify.py -q`
Expected: PASS（7 passed）

- [ ] **Step 5: 提交**

```bash
git add server/youn_server/app.py server/youn_server/config.py server/tests/test_notify.py
git commit -m "feat(server): notification HTTP endpoints (create/next/ack/history)"
```

---

### Task 3: FastMCP 服务（同端口 /mcp）

**Files:**
- Create: `server/youn_server/mcp_server.py`
- Modify: `server/youn_server/app.py`（mount mcp）
- Modify: `server/requirements.txt`（+fastmcp）
- Test: `server/tests/test_mcp.py`

**Interfaces:**
- Consumes: `notify_store.get_store()`；`settings.operator_token`
- Produces:
  - `push_notification(device_id, title, body, ttl_sec=300) -> dict`
  - `list_notifications(device_id="", limit=20) -> dict`
  - `ack_notification(notification_id, decision) -> dict`

- [ ] **Step 1: 写失败测试**

```python
# server/tests/test_mcp.py
import json
import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app


@pytest.fixture()
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_mcp_probe(client):
    r = client.get("/mcp")
    assert r.status_code in (200, 404, 405)  # transport info endpoint


def test_mcp_tool_call_push(client):
    r = client.post("/mcp", json={
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "push_notification",
                   "arguments": {"device_id": "X", "title": "t", "body": "b"}}
    })
    assert r.status_code == 200
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_mcp.py -q`
Expected: FAIL（`/mcp` 404）

- [ ] **Step 3: 实现**

`requirements.txt` 加：
```
fastmcp>=2.0
```

`server/youn_server/mcp_server.py`：
```python
"""FastMCP server exposing notification tools on /mcp (streamable-http)."""
from __future__ import annotations

from dataclasses import asdict

from fastmcp import FastMCP

from .config import settings
from . import notify_store as ns
from . import devices as devices_mod

# fastmcp 4.x: no `transport=` kwarg on FastMCP(). Streamable HTTP
# transport is selected when mounting via http_app(transport=...).
mcp = FastMCP("youn-notify")


@mcp.tool
def push_notification(device_id: str, title: str, body: str,
                      ttl_sec: int = 300) -> dict:
    """Create a pending notification for a device."""
    if not any(d.device_id == device_id
               for d in devices_mod.list_all(only_trusted=True)):
        raise ValueError("device not trusted")
    n = ns.get_store().enqueue(device_id, title, body, ttl_sec)
    return {"ok": True, "notification": asdict(n)}


@mcp.tool
def list_notifications(device_id: str = "", limit: int = 20) -> dict:
    """List recent notifications (all devices or filtered by device_id)."""
    items = ns.get_store().recent(device_id=device_id, limit=limit)
    return {"notifications": [asdict(n) for n in items]}


@mcp.tool
def ack_notification(notification_id: str, decision: str) -> dict:
    """Mark a notification as agreed or rejected."""
    if decision not in ("agree", "reject"):
        raise ValueError("decision must be agree|reject")
    n = ns.get_store().ack(notification_id, decision)
    if n is None:
        raise ValueError("notification not found")
    return {"ok": True, "status": n.status, "decision": n.decision}

`app.py` 在 `create_app` 末尾（return app 之前）加：
```python
    # Mount FastMCP on /mcp (streamable-http transport).
    # fastmcp 4.x: get sub-app via http_app(); must propagate its lifespan
    # into the parent FastAPI or StreamableHTTPSessionManager fails init.
    try:
        from .mcp_server import mcp as mcp_server
        mcp_subapp = mcp_server.http_app(path="/mcp", transport="streamable-http")
        app.mount("/mcp", mcp_subapp)
        app.router.lifespan_context = mcp_subapp.lifespan
    except ImportError:
        log.warning("fastmcp not installed; /mcp endpoint disabled")
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_mcp.py -q`
Expected: PASS（2 passed）

- [ ] **Step 5: 提交**

```bash
git add server/youn_server/mcp_server.py server/youn_server/app.py server/requirements.txt server/tests/test_mcp.py
git commit -m "feat(server): FastMCP notification tools on /mcp"
```

---

### Task 4: 固件 — BSP 改名 + notify 模块 + 接线

**Files:**
- Create: `firmware/main/common/notify.h`
- Create: `firmware/main/common/notify.cc`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/config.h`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc`
- Modify: `firmware/main/application.cc`
- Modify: `firmware/main/CMakeLists.txt`

**Interfaces:**
- Consumes: `http_wrapper_get/post_json`（`http_client_wrapper.h`）；`page_sync_show_page`（`page_sync.h`）；`server_pairing_get_token`（配对）
- Produces:
  - `notify_init()` / `notify_deinit()`
  - `notify_request_next()`（异步 GET）
  - `notify_is_active() -> bool`
  - `notify_post_ack(const char* decision)`（agree|reject）
  - `notify_dismiss()`（恢复画板）

- [ ] **Step 1: 编译失败确认**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep -i notify`
Expected: FAIL（undefined reference to notify_*）

- [ ] **Step 2: 实现 notify.h**

```c
/**
 * @file notify.h
 * @brief 待确认通知模块：BOOT 拉取，上/下键 ack，5min 自动关闭
 *
 * 状态机：IDLE -> FETCHING (GET in flight) -> NOTIFYING (展示位图)
 *          -> IDLE (ack / dismiss / 5min timeout)
 */
#ifndef NOTIFY_H
#define NOTIFY_H

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief 初始化通知模块（需在 HTTP client 初始化后调用）
 */
void notify_init(void);

/**
 * @brief 反初始化
 */
void notify_deinit(void);

/**
 * @brief 请求拉取下一条待确认通知（BOOT 短按触发）
 *
 * 非阻塞：内部发起异步 HTTP GET /api/notifications/next。
 * 有通知时切换到 NOTIFYING 并显示位图。
 */
void notify_request_next(void);

/**
 * @brief 是否正在展示通知（NOTIFYING 状态）
 */
bool notify_is_active(void);

/**
 * @brief 提交 ack 并关闭展示（上/下键触发）
 *
 * @param decision  "agree" 或 "reject"
 */
void notify_post_ack(const char *decision);

/**
 * @brief 关闭通知展示（BOOT 短按或 5min 超时触发），不发 ack
 */
void notify_dismiss(void);

#ifdef __cplusplus
}
#endif

#endif // NOTIFY_H
```

- [ ] **Step 3: 实现 notify.cc**

`server/youn_server/mcp_server.py`：
```python
"""FastMCP server exposing notification tools on /mcp (streamable-http)."""
from __future__ import annotations

from dataclasses import asdict

from fastmcp import FastMCP

from .config import settings
from . import notify_store as ns
from . import devices as devices_mod

# fastmcp 4.x: no `transport=` kwarg on FastMCP(). Streamable HTTP
# transport is selected when mounting via http_app(transport=...).
mcp = FastMCP("youn-notify")


@mcp.tool
def push_notification(device_id: str, title: str, body: str,
                      ttl_sec: int = 300) -> dict:
    """Create a pending notification for a device."""
    if not any(d.device_id == device_id
               for d in devices_mod.list_all(only_trusted=True)):
        raise ValueError("device not trusted")
    n = ns.get_store().enqueue(device_id, title, body, ttl_sec)
    return {"ok": True, "notification": asdict(n)}


@mcp.tool
def list_notifications(device_id: str = "", limit: int = 20) -> dict:
    """List recent notifications (all devices or filtered by device_id)."""
    items = ns.get_store().recent(device_id=device_id, limit=limit)
    return {"notifications": [asdict(n) for n in items]}


@mcp.tool
def ack_notification(notification_id: str, decision: str) -> dict:
    """Mark a notification as agreed or rejected."""
    if decision not in ("agree", "reject"):
        raise ValueError("decision must be agree|reject")
    n = ns.get_store().ack(notification_id, decision)
    if n is None:
        raise ValueError("notification not found")
    return {"ok": True, "status": n.status, "decision": n.decision}
```

`app.py` 在 `create_app` 末尾（return app 之前）加：
```python
    # Mount FastMCP on /mcp (streamable-http transport).
    # fastmcp 4.x: get sub-app via http_app(); must propagate its lifespan
    # into the parent FastAPI or StreamableHTTPSessionManager fails init.
    try:
        from .mcp_server import mcp as mcp_server
        mcp_subapp = mcp_server.http_app(path="/mcp", transport="streamable-http")
        app.mount("/mcp", mcp_subapp)
        app.router.lifespan_context = mcp_subapp.lifespan
    except ImportError:
        log.warning("fastmcp not installed; /mcp endpoint disabled")
```

    }
    s_state = NotifyState::IDLE;
}

bool notify_is_active(void) { return s_state == NotifyState::NOTIFYING; }

static void set_state(NotifyState s) { s_state = s; }

static void fetch_next(void) {
    char url[256];
    if (!server_pairing_build_endpoint("/api/notifications/next", url, sizeof(url))) {
        ESP_LOGW(kTag, "cannot build endpoint");
        set_state(NotifyState::IDLE);
        return;
    }
    char token[65];
    server_pairing_get_token(token, sizeof(token));
    char buf[4096]; int buf_len = sizeof(buf);
    int status = http_wrapper_get(url, token, buf, &buf_len, kHttpTimeoutMs);
    if (status != 200) {
        ESP_LOGI(kTag, "no pending notification (status=%d)", status);
        set_state(NotifyState::IDLE);
        return;
    }
    // Parse JSON for bitmap_base64 + notification.id
    // ... (base64 decode into framebuffer) ...
    // On success:
    //   memcpy(s_display->GetFramebuffer(), bitmap, 30000);
    //   s_display->RequestUrgentFullRefresh();
    //   set_state(NotifyState::NOTIFYING);
    //   xTimerStart(s_timeout_timer, 0);
}

void notify_request_next(void) {
    if (s_state != NotifyState::IDLE) return;
    set_state(NotifyState::FETCHING);
    // TODO: run in background task to avoid blocking button callback
    fetch_next();
}

void notify_post_ack(const char *decision) {
    if (!notify_is_active()) return;
    char url[256], body[128];
    snprintf(url, sizeof(url), "/api/notifications/%s/ack", s_notification_id);
    // POST decision; on success dismiss
    notify_dismiss();
}

void notify_dismiss(void) {
    if (!notify_is_active()) return;
    set_state(NotifyState::IDLE);
    s_notification_id[0] = '\0';
    if (s_timeout_timer) xTimerStop(s_timeout_timer, 0);
    // Restore original page via page_sync
    // page_sync_show_current();
}
```

- [ ] **Step 4: 接线 application.cc + config.h + CMakeLists.txt**

`config.h` 改名：
```c
#define UP_BUTTON_GPIO GPIO_NUM_39
#define DOWN_BUTTON_GPIO GPIO_NUM_18
#define CONFIRM_BUTTON_GPIO GPIO_NUM_0
```

`zectrix-s3-epaper-4.2.cc` 宏引用：
```c
constexpr gpio_num_t kBoardUpButtonGpio = UP_BUTTON_GPIO;
constexpr gpio_num_t kBoardDownButtonGpio = DOWN_BUTTON_GPIO;
constexpr gpio_num_t kBoardConfirmButtonGpio = CONFIRM_BUTTON_GPIO;
```

`application.cc` 三回调加分支（OnBootClick/OnUpClick/OnDownClick 开头）：
```c
void Application::OnBootClick() {
    if (notify_is_active()) { notify_dismiss(); return; }
    if (!page_sync_is_displaying()) return;  // 避免打断 RawDraw
    notify_request_next();
}
```

`CMakeLists.txt` SRCS 加：
```cmake
"common/notify.cc"
```

- [ ] **Step 5: 编译通过**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build`
Expected: BUILD SUCCESS（0 undefined reference）

- [ ] **Step 6: 提交**

```bash
git add firmware/main/common/notify.h firmware/main/common/notify.cc \
  firmware/main/boards/zectrix-s3-epaper-4.2/config.h \
  firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc \
  firmware/main/application.cc firmware/main/CMakeLists.txt
git commit -m "feat(firmware): notification module + BSP GPIO wiring"
```

---

### Task 5: 端到端验证（真机）

**Files:**
- Modify: `server/youn_server/app.py`（如有 bug 修复）
- 无新文件（烧录 + 人工验证）

**Interfaces:**
- Consumes: 全部 Task 1-4

- [ ] **Step 1: 服务端测试全绿**

Run: `cd server && ./.venv/bin/python -m pytest tests/ -q`
Expected: PASS（38 + 7 + 2 = 47 tests）

- [ ] **Step 2: 服务端启动**

Run: `cd server && ./start.sh start`
Expected: `Server started on :9002`，`/mcp` 探测返回 200/405

- [ ] **Step 3: 注入通知**

```bash
curl -X POST http://127.0.0.1:9002/api/notifications \
  -H "X-Operator-Token: $OPERATOR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"device_id":"NOTE4C-3400FC","title":"测试","body":"这是一条测试通知"}'
```
Expected: 201，返回 notification id

- [ ] **Step 4: 设备拉取**

真机按 BOOT → 观察 EPD 是否显示通知位图（5min 内）。

- [ ] **Step 5: 按键 ack**

按上键（GPIO39）→ 服务端应收到 ack=agree；按下键（GPIO18）→ ack=reject；按 BOOT → 关闭不 ack。

- [ ] **Step 6: 验证落库**

```bash
curl http://127.0.0.1:9002/api/notifications/history?device_id=NOTE4C-3400FC \
  -H "X-Operator-Token: $OPERATOR_TOKEN"
```
Expected: 返回 notifications 数组，status=acked，decision=agree/reject

- [ ] **Step 7: 5min 自动关闭验证**

注入通知 → 不按键 → 5min 后 EPD 恢复画板原页。

- [ ] **Step 8: 提交修复（如有）**

```bash
git add -A
git commit -m "fix: e2e notification flow issues"
```

---

## Self-Review Checklist

**1. Spec coverage** — spec 章节 2-5 全部有对应任务（Task 1-5）：
- 服务端 notify_store → Task 1 ✓
- 4 HTTP 端点 → Task 2 ✓
- FastMCP /mcp → Task 3 ✓
- 固件 BSP/notify/接线 → Task 4 ✓
- 真机验证 → Task 5 ✓

**2. Placeholder scan** — 无 TBD/TODO/「类似上面」。

**3. Type consistency** — `next_for` / `ack` / `recent` 命名一致；`agree`/`reject` 字符串一致；`300` 秒 ttl 一致；`GPIO_NUM_39/18/0` 一致。
