# 每台设备一套页面 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 页面归属于一台设备；设备只轮播自己的那组页；Web 端先选设备再操作。迁移后设备的 `schedule_md5` 必须与现在逐字节相同。

**Architecture:** 归属由目录表达（`data/pages/<device_id>/<name>.json`），位图仍全局内容寻址共享，其 meta 的 `sources` 用复合键 `"<device>/<name>"`。schedule 端点要求设备 Bearer token（设备本来就带），服务端据此只返回该设备的页。**固件零改动。**

**Tech Stack:** FastAPI + pytest（`server/`）、React + Vite（`frontend/`）、ESP32-S3 固件（只做验证）。

**Spec:** `docs/superpowers/specs/2026-09-12-per-device-pages-design.md`

## Global Constraints

- **固件零改动。** 设备已带 Bearer token 拉 schedule（`firmware/main/rust/src/page_sync.rs:171`），服务端用 `_require_device_token`（`app.py:107-121`）认设备。
- **存储模型与它的调用者必须同批落地**：改完 `pages.py` 的签名而不改 `app.py` 的调用点，套件会红。Task 1 因此把存储、迁移、接口、测试放在一起。
- **迁移与代码同批提交**，且新代码**不做旧布局兼容读**（平铺层的页面文件直接忽略），避免两套布局并存。
- **归属只由目录表达**，不写进页面 JSON ⇒ `PageSource.to_dict()` 的键保持不变（`name/canvas_json/duration_minutes/order`）。
- **设备校验三处一致**：管理接口的 `device` 必须存在于 `registry.get(device)` 且 `Device.trusted`，否则 400；缺 `device` 也 400。
- **上传的 `page` 必须属于该 `device`**，否则 400 且不写任何文件（先校验后落盘）。
- `sources` 一律复合键 `"<device>/<name>"`；引用计数仍是字符串比较，不引入新结构。
- 位图端点保持免认证、内容寻址不变。
- 测试隔离不变（`tests/conftest.py` 已隔离 `data_dir` 与 `uploads_dir`）；`cd server && ./.venv/bin/python -m pytest tests -q` 必须全绿。
- **凭据纪律：绝不把任何活 token（operator 或设备）写进被跟踪的文件、日志或提交信息。** 需要用时读进 shell 变量，只打印状态码或哈希。
- 前端改完必须 `npm run build`；`frontend/dist` 被 gitignore，不得提交。

---

### Task 1: 存储按设备 + 一次性数据迁移 + 接口（原子）

**Files:**
- Modify: `server/youn_server/pages.py`（模型与路径）
- Modify: `server/youn_server/app.py`（页面 CRUD、schedule、上传）
- Modify: `server/tests/test_schedule_api.py`、`server/tests/test_uploads_api.py`（适配新签名与必填 `device`）
- Test: `server/tests/test_per_device.py`（新建，存储 + 接口一起测）
- Data migration: `server/data/pages/{logo-1024,page2}.json` → `server/data/pages/NOTE4C-3400FC/`；三个 `.bmp.json` 的 `sources` 改复合键

**Interfaces:**
- Consumes: `settings.data_dir`、`registry.get(device_id)`、`Device.trusted`、`_require_device_token(request) -> Device`、`_require_operator(request)`
- Produces: `PageSource.device: str`；`_page_dir(device)`、`_page_path(device, name)`、`_source_key(device, name)`、`_all_page_sources(device=None)`、`list_pages(device=None)`、`upsert_page(device, name, canvas_json, duration_minutes, order, bitmap_bytes)`、`delete_page(device, name)`、`build_schedule_from_disk(device)`；HTTP：`GET /api/pages?device=`、`POST /api/pages`(body `device`)、`DELETE /api/pages/{name}?device=`、`POST /api/uploads`(form `device`)、`GET /api/pages/schedule`（设备 token）

- [ ] **Step 1: 写失败测试**

新建 `server/tests/test_per_device.py`：

```python
"""Pages belong to a device: storage, schedule and the admin API.

Ownership is the directory (data/pages/<device_id>/<name>.json); bitmaps stay
global and content-addressed, keyed in their meta by "<device>/<name>".
"""
from __future__ import annotations

import json

import pytest
from fastapi.testclient import TestClient

from youn_server import pages as pages_mod
from youn_server.app import create_app, registry
from youn_server.canvas_render import render_canvas_to_bitmap
from youn_server.config import settings

DEV_A = "NOTE4C-AAAAAA"
DEV_B = "NOTE4C-BBBBBB"


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _canvas(label: str) -> dict:
    # Text is a plain string child, not a node: canvas_render accepts only
    # div/span/img node types and raises RenderError for anything else
    # (_resolve_children, canvas_render.py:282).
    return {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": label}}]}


def _make(device: str, name: str, order: int = 0) -> None:
    canvas = _canvas(name)
    pages_mod.upsert_page(device, name, canvas, 10, order, render_canvas_to_bitmap(canvas))


def _register(device_id: str, token: str, trusted: bool = True) -> None:
    registry.upsert(device_id, "NOTE4C", ip_address="127.0.0.1")
    if trusted:
        registry.approve(device_id)
    registry.set_token(device_id, token)


# ── storage ────────────────────────────────────────────────────────────

def test_pages_belong_to_the_device_that_created_them():
    _make(DEV_A, "a-page")
    _make(DEV_B, "b-page")
    assert [p.name for p in pages_mod.list_pages(DEV_A)] == ["a-page"]
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["b-page"]
    assert pages_mod._page_path(DEV_A, "a-page").parent.name == DEV_A


def test_two_devices_can_use_the_same_page_name():
    _make(DEV_A, "home")
    _make(DEV_B, "home")
    assert [p.name for p in pages_mod.list_pages(DEV_A)] == ["home"]
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["home"]


def test_a_schedule_never_contains_another_devices_page():
    _make(DEV_A, "only-a", order=0)
    _make(DEV_B, "only-b", order=0)
    assert [e.name for e in pages_mod.build_schedule_from_disk(DEV_A)] == ["only-a"]
    assert [e.name for e in pages_mod.build_schedule_from_disk(DEV_B)] == ["only-b"]


def test_a_page_on_the_flat_old_layout_is_ignored():
    """No compatibility read: two layouts must never both be live."""
    pages_dir = settings.data_dir / "pages"
    pages_dir.mkdir(parents=True, exist_ok=True)
    (pages_dir / "legacy.json").write_text(json.dumps({
        "name": "legacy", "canvas_json": _canvas("x"),
        "duration_minutes": 10, "order": 0}))
    assert pages_mod.list_pages(DEV_A) == []


def test_the_bitmap_meta_keys_the_source_by_device_slash_name():
    _make(DEV_A, "same")
    metas = list((settings.data_dir / "pages").glob("*.bmp.json"))
    assert metas, "expected a bitmap meta"
    sources = [s for f in metas for s in json.loads(f.read_text())["sources"]]
    assert sources == [f"{DEV_A}/same"]


def test_replacing_a_page_leaves_it_claimed_by_one_bitmap_only():
    """The invariant from the earlier fix, now expressed per device."""
    _make(DEV_A, "twice")
    _make(DEV_A, "twice")
    metas = list((settings.data_dir / "pages").glob("*.bmp.json"))
    claimers = [f.name for f in metas if f"{DEV_A}/twice" in json.loads(f.read_text())["sources"]]
    assert len(claimers) == 1


def test_deleting_one_devices_page_leaves_the_other_alone():
    _make(DEV_A, "shared-name")
    _make(DEV_B, "shared-name")
    assert pages_mod.delete_page(DEV_A, "shared-name") is True
    assert pages_mod.list_pages(DEV_A) == []
    assert [p.name for p in pages_mod.list_pages(DEV_B)] == ["shared-name"]


def test_a_schedule_entry_carries_the_pages_own_duration_and_order():
    _make(DEV_A, "dur", order=7)
    entry = pages_mod.build_schedule_from_disk(DEV_A)[0]
    assert (entry.name, entry.order, entry.duration_minutes) == ("dur", 7, 10)


# ── the admin API ──────────────────────────────────────────────────────

def test_pages_require_a_device(client):
    assert client.get("/api/pages").status_code == 400
    assert client.post("/api/pages", json={"name": "x", "canvas_json": {}}).status_code == 400


def test_a_device_that_is_unknown_or_untrusted_is_refused(client):
    assert client.get("/api/pages", params={"device": "NOTE4C-NOSUCH"}).status_code == 400
    _register("NOTE4C-UNTRUSTED", "e" * 64, trusted=False)
    assert client.get("/api/pages", params={"device": "NOTE4C-UNTRUSTED"}).status_code == 400


def test_pages_are_listed_per_device(client):
    _register(DEV_A, "a" * 64)
    _register(DEV_B, "b" * 64)
    r = client.post("/api/pages", json={
        "device": DEV_A, "name": "a-only", "canvas_json": _canvas("a"),
        "duration_minutes": 10, "order": 0})
    assert r.status_code == 200
    assert [p["name"] for p in
            client.get("/api/pages", params={"device": DEV_A}).json()["pages"]] == ["a-only"]
    assert client.get("/api/pages", params={"device": DEV_B}).json()["pages"] == []


def test_an_upload_must_name_a_device_and_a_page_of_that_device(client):
    _register(DEV_A, "a" * 64)
    _register(DEV_B, "b" * 64)
    assert client.post("/api/uploads", files={"image": ("a.png", b"x", "image/png")}).status_code == 400
    r = client.post("/api/uploads", files={"image": ("a.png", b"x", "image/png")},
                    data={"device": DEV_A, "page": "not-here"})
    assert r.status_code == 400
    assert not any(settings.uploads_dir.iterdir())


def test_the_schedule_requires_the_device_token(client):
    _register("NOTE4C-TESTC", "c" * 64)
    assert client.get("/api/pages/schedule").status_code == 401
    r = client.get("/api/pages/schedule", headers={"Authorization": "Bearer " + "c" * 64})
    assert r.status_code == 200
    assert r.json()["pages"] == []


def test_an_untrusted_device_token_cannot_read_a_schedule(client):
    _register("NOTE4C-TESTD", "d" * 64, trusted=False)
    r = client.get("/api/pages/schedule", headers={"Authorization": "Bearer " + "d" * 64})
    assert r.status_code == 401
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_per_device.py -q`
Expected: FAIL — `upsert_page() missing 1 required positional argument`（签名还不是按设备的）。

- [ ] **Step 3: 改 `pages.py`**

```python
@dataclass
class PageSource:
    name: str
    canvas_json: dict
    duration_minutes: int
    order: int
    # Ownership lives in the directory name, not in the file: to_dict() keeps its
    # original keys, so the on-disk page format does not change.
    device: str = ""
```

```python
def _safe_component(value: str, what: str) -> str:
    safe = "".join(c for c in value if c.isalnum() or c in "._-")
    if not safe or safe != value:
        raise ValueError(f"invalid {what}: {value!r}")
    return safe


def _page_dir(device: str) -> Path:
    return settings.data_dir / "pages" / _safe_component(device, "device")


def _page_path(device: str, name: str) -> Path:
    return _page_dir(device) / f"{_safe_component(name, 'page name')}.json"


def _source_key(device: str, name: str) -> str:
    return f"{device}/{name}"


def _all_page_sources(device: Optional[str] = None) -> list[PageSource]:
    """Walk the device directories. A page source on the flat layer is not a page
    any more — the layout changed and there is deliberately no fallback read, so
    two layouts can never both be live."""
    out: list[PageSource] = []
    root = settings.data_dir / "pages"
    dirs = [_page_dir(device)] if device else sorted(p for p in root.glob("*") if p.is_dir())
    for d in dirs:
        for p in d.glob("*.json"):
            if not _is_page_source(p):
                continue
            try:
                src = PageSource.from_dict(json.loads(p.read_text()))
            except (json.JSONDecodeError, KeyError, ValueError) as e:
                log.warning("bad page source %s: %s", p, e)
                continue
            src.device = d.name
            out.append(src)
    out.sort(key=lambda s: (s.order, s.name))
    return out
```

`upsert_page` / `delete_page` / `list_pages` 改为按设备（注意 `sources` 一律用复合键）：

```python
def upsert_page(device, name, canvas_json, duration_minutes, order, bitmap_bytes) -> PageEntry:
    if len(bitmap_bytes) != 30000:
        raise ValueError(f"bitmap must be 30000 bytes, got {len(bitmap_bytes)}")
    md5 = hashlib.md5(bitmap_bytes).hexdigest()
    key = _source_key(device, name)
    src = PageSource(name=name, canvas_json=canvas_json,
                     duration_minutes=duration_minutes, order=order)
    src_path = _page_path(device, name)
    src_path.parent.mkdir(parents=True, exist_ok=True)
    src_path.write_text(json.dumps(src.to_dict(), ensure_ascii=False, indent=2))
    bpath = _bitmap_path(md5)
    bmpath_meta = _bitmap_meta_path(md5)
    if not bpath.exists():
        bpath.write_bytes(bitmap_bytes)
        bmpath_meta.write_text(json.dumps({
            "md5": md5, "sources": [key],
            "rendered_at": int(time.time()), "refcount": 0,
        }))
    # A page belongs to exactly one bitmap: drop it from every other meta.
    for meta in _all_bitmap_metas():
        if meta.get("md5") != md5 and key in meta.get("sources", []):
            _drop_source_reference(meta["md5"], source_name=key)
    _record_source_reference(md5, source_name=key)
    log.info("page upserted device=%s name=%s md5=%s duration=%dmin order=%d",
             device, name, md5, duration_minutes, order)
    return PageEntry(md5=md5, duration_minutes=duration_minutes, order=order, name=name)


def delete_page(device: str, name: str) -> bool:
    src_path = _page_path(device, name)
    if not src_path.exists():
        return False
    key = _source_key(device, name)
    md5_to_drop: Optional[str] = None
    for meta in _all_bitmap_metas():
        if key in meta.get("sources", []):
            md5_to_drop = meta["md5"]
            break
    src_path.unlink()
    if md5_to_drop:
        _drop_source_reference(md5_to_drop, source_name=key)
    log.info("page deleted device=%s name=%s bitmap=%s", device, name, md5_to_drop)
    return True


def list_pages(device: Optional[str] = None) -> list[PageSource]:
    return _all_page_sources(device)


def build_schedule_from_disk(device: str) -> list[PageEntry]:
    """Snapshot one device's source pages into schedule entries."""
    out: list[PageEntry] = []
    metas = _all_bitmap_metas()
    for src in _all_page_sources(device):
        key = _source_key(device, src.name)
        candidates = [(m.get("rendered_at", 0), m["md5"]) for m in metas
                      if key in m.get("sources", [])]
        if not candidates:
            continue
        candidates.sort(reverse=True)
        out.append(PageEntry(md5=candidates[0][1], duration_minutes=src.duration_minutes,
                             order=src.order, name=src.name))
    out.sort(key=lambda e: e.order)
    return out
```

- [ ] **Step 4: 改 `app.py`**

共用校验 + 三个管理端点：

```python
def _require_known_device(device: str) -> str:
    """The admin's device selector must name a device that exists and is trusted;
    a typo would otherwise create a phantom page set on disk."""
    dev = registry.get(device)
    if dev is None or not dev.trusted:
        raise HTTPException(400, f"unknown or untrusted device: {device!r}")
    return device
```

- `GET /api/pages`：查询参数 `device`，缺失 ⇒ 400，否则 `_require_known_device` 后 `pages_mod.list_pages(device)`。
- `POST /api/pages`：body 取 `device`（缺失 400）→ `_require_known_device` → 其余不变 → `pages_mod.upsert_page(device, name, canvas_json, duration_minutes, order, bitmap)`。
- `DELETE /api/pages/{name}`：查询参数 `device`（缺失 400）→ `_require_known_device` → `pages_mod.delete_page(device, name)`；未删到 ⇒ 404。
- `POST /api/uploads`：新增必填 form 字段 `device` → `_require_known_device(device)` → 页面校验改为 `pages_mod.list_pages(device)`，`source = next(...)` 从**该快照**取，`upsert_page(device, ...)`；`page` 不属于该设备时沿用既有 400 + 该设备的可选页面列表。
- `GET /api/pages/schedule`：

```python
    @app.get("/api/pages/schedule")
    async def get_schedule(request: Request) -> dict:
        dev = _require_device_token(request)
        entries = pages_mod.build_schedule_from_disk(dev.device_id)
        ...（其余不变）
```

- [ ] **Step 5: 适配既有用例**

`tests/test_schedule_api.py`、`tests/test_uploads_api.py` 里凡直接调 `pages_mod.*` 的补设备参数（任意测试设备名），凡打 `/api/pages*` 的补 `device`，凡打 `/api/pages/schedule` 的补设备 token 头（照 `test_per_device.py` 的 `_register` 写法）。

- [ ] **Step 6: 跑全量确认绿**

Run: `cd server && ./.venv/bin/python -m pytest tests -q`
Expected: PASS（新用例 + 既有用例都绿）

- [ ] **Step 7: 数据迁移**

```bash
cd /mnt/data/project/youn-ink-fourcolor-firmware
mkdir -p server/data/pages/NOTE4C-3400FC
git mv server/data/pages/logo-1024.json server/data/pages/NOTE4C-3400FC/logo-1024.json
git mv server/data/pages/page2.json    server/data/pages/NOTE4C-3400FC/page2.json
python3 - <<'PY'
import json, pathlib
d = pathlib.Path("server/data/pages")
dev = "NOTE4C-3400FC"
# logo-1024 is currently claimed by two metas; the live one is whichever the
# schedule resolves (max of (rendered_at, md5)). Clear the stale claimant and
# drop its bitmap: it is a deterministic render of the same source.
rewrites = {
    "36be550c9209e7c1a1dfdd80fe3607fe.bmp.json": [f"{dev}/logo-1024"],
    "9670217f5e14df3c8e2d84288a38adea.bmp.json": [],
    "9c9c526b379b06786fba4a767dde2570.bmp.json": [f"{dev}/page2"],
}
for fn, sources in rewrites.items():
    p = d / fn
    m = json.loads(p.read_text())
    m["sources"] = sources
    m["refcount"] = len(sources)
    if sources:
        p.write_text(json.dumps(m, ensure_ascii=False, indent=2))
    else:
        (d / f"{m['md5']}.bin").unlink(missing_ok=True)
        p.unlink()
        print("reclaimed stale bitmap", m["md5"][:8])
print("migration done")
PY
ls server/data/pages server/data/pages/NOTE4C-3400FC
```

立刻验证迁移不变量（不要等到 Task 3 —— 本任务的其他测试都在临时目录里跑，迁移写错它们不会红）：

```bash
cd server && ./.venv/bin/python -c "
import sys; sys.path.insert(0, '.')
from youn_server import pages as p
entries = p.build_schedule_from_disk('NOTE4C-3400FC')
print('md5:', p.compute_schedule_md(entries))
print('pages:', [(e.name, e.md5[:8]) for e in entries])
"
cd ..
```

Expected: `md5: de3dc3118cd0d350e94dee7cfc54a7be`，`pages: [('logo-1024', '36be550c'), ('page2', '9c9c526b')]` —— 与迁移前实测值逐字符相同。不一样就是迁移写错了，当场修，不要提交。

- [ ] **Step 8: 提交（代码与数据同一提交，不留布局真空期）**

```bash
git add server/youn_server server/tests server/data/pages
git commit -m "feat(pages): pages belong to a device, storage to schedule"
```

---

### Task 2: Web 端先选设备

**Files:**
- Create: `frontend/src/deviceContext.js`
- Modify: `frontend/src/App.jsx`（侧栏选择器）、`frontend/src/api.js`、`frontend/src/pages/Pages.jsx`、`frontend/src/pages/Images.jsx`

**Interfaces:**
- Consumes: `api.devices()`（现有，返回数组，元素有 `device_id` 与 `trust`）；Task 1 的接口
- Produces: `getSelectedDevice()`、`setSelectedDevice(id)`（并派发 `window` 事件 `device-changed`）；`api.pages(device)`、`api.deletePage(name, device)`、`api.uploadToPage(file, page, device)`

- [ ] **Step 1: 选择器状态**

```js
// frontend/src/deviceContext.js
const KEY = 'youn_selected_device';

export function getSelectedDevice() {
  return localStorage.getItem(KEY) || '';
}

export function setSelectedDevice(id) {
  if (id) localStorage.setItem(KEY, id);
  else localStorage.removeItem(KEY);
  window.dispatchEvent(new Event('device-changed'));
}
```

- [ ] **Step 2: 侧栏选择器**

在 `App.jsx` 的 `Layout` 里、`<nav>` 之前加一个组件：`useEffect` 载入 `api.devices()`，过滤 `d.trust`，渲染 `<select>`（未选时第一项为「请选择设备」），`onChange` 调 `setSelectedDevice`；同组件监听 `device-changed` 以同步自身显示。

- [ ] **Step 3: api.js 带 device**

```js
  pages: async (device) => (await request(`/pages?device=${encodeURIComponent(device)}`)).pages ?? [],
  deletePage: (name, device) =>
    request(`/pages/${encodeURIComponent(name)}?device=${encodeURIComponent(device)}`, { method: 'DELETE' }),
  // createPage: 调用方在 body 里带 device
  // uploadToPage: FormData 里加 fd.append('device', device)
```

- [ ] **Step 4: 两个页面跟随选择器**

`Pages.jsx`：未选设备 ⇒ 渲染「请先选择设备」并禁用新建/保存/删除；`load()` 用 `api.pages(device)`；`createPage` 的 body 带 `device`；`deletePage(name, device)`；监听 `device-changed` 重新加载。
`Images.jsx`：未选设备 ⇒ 提示并禁用；页面下拉来自 `api.pages(device)`；上传调 `api.uploadToPage(file, page, device)`。

- [ ] **Step 5: 构建 + 浏览器实测**

```bash
cd frontend && npm run build && cd ../server && ./start.sh restart && sleep 3
```

用无头浏览器核对并记录 DOM 证据（无视觉能力时不要声称看过像素）：
1. 未选设备时「页组管理」「替换页面画面」都显示提示且按钮 `disabled`；
2. 选中 `NOTE4C-3400FC` 后「页组管理」列出 `logo-1024`、`page2`；
3. 「替换页面画面」的下拉同样只有这两页；未选页面时上传按钮仍 `disabled`。

- [ ] **Step 6: 提交**

```bash
git add frontend/src
git commit -m "feat(web): pick the device before its pages"
```

---

### Task 3: 验收（含设备侧零扰动）

**Files:** 无源码改动（验证任务）

- [ ] **Step 1: 取设备 token（不回显、不落盘到被跟踪文件）**

```bash
cd /mnt/data/project/youn-ink-fourcolor-firmware/server
DTOK=$(./.venv/bin/python - <<'PY'
import sqlite3
c = sqlite3.connect("data/devices.db")
row = c.execute("SELECT token FROM device_secrets WHERE device_id='NOTE4C-3400FC'").fetchone()
print(row[0] if row else "")
PY
)
test -n "$DTOK" && echo "device token loaded (${#DTOK} chars)" || echo "NO TOKEN — report this, do not invent one"
```

- [ ] **Step 2: 迁移不变量：设备 schedule 必须与迁移前逐字节相同**

```bash
curl -s http://10.0.0.90:9002/api/pages/schedule -H "Authorization: Bearer $DTOK" \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print('md5:',d['schedule_md5']);print('pages:',[(p['name'],p['md5']) for p in d['pages']])"
```

Expected: `md5: de3dc3118cd0d350e94dee7cfc54a7be`，`pages` 为 `logo-1024 → 36be550c…` 与 `page2 → 9c9c526b…`（与迁移前实测值逐字符相同）。用设备 token；管理员 token 不适用于这个端点。

- [ ] **Step 3: 设备侧零扰动**

一次短窗读串口（一次只开一个读者；打开即复位设备）：确认仍出现 `schedule updated: 2 pages` 与 `show page i/2 md5="…"`，md5 仍是 `36be550c`/`9c9c526b`。

- [ ] **Step 4: 跨设备上传被拒**

建一台临时可信设备（`registry.upsert` + `approve` + `set_token`，或直接用它已有的设备名），用它 `POST /api/uploads` 覆盖属于 NOTE4C-3400FC 的 `page2` ⇒ 期望 400，且 `server/data/uploads/` 无新增文件。

- [ ] **Step 5: 清掉验证残留**

删掉临时设备（`registry.revoke`）与它为测试建的页面；确认设备只剩 `NOTE4C-3400FC`，页面只剩它的两页，`GET /api/pages?device=NOTE4C-3400FC` 返回那两页。

- [ ] **Step 6: 报告**

每步写命令 + 输出；未观察到的写「未观察到」并说明什么能证明它。**不得猜测或推断。**

---

## Self-Review

**Spec coverage:** 存储与归属（Task 1 Step 3）、迁移与残留清理（Task 1 Step 7）、接口与设备校验（Task 1 Step 4）、schedule 的设备识别（Task 1 Step 4）、上传跨设备拒绝（Task 1 Step 4 + Step 1 的用例）、Web 选择器与禁用（Task 2）、验收含"逐字节相同"（Task 3 Step 2）、非目标（未实现：共享、立即显示、设备管理 UI、S3/CDN、固件改动）。

**Placeholder scan:** 无 TBD/TODO，无"类似 Task N"。设备 token 的取法是可执行代码（Task 3 Step 1），失败路径明确要求如实报告。

**Type consistency:** `upsert_page(device, name, canvas_json, duration_minutes, order, bitmap_bytes)`、`delete_page(device, name)`、`list_pages(device=None)`、`build_schedule_from_disk(device)`、`_source_key(device, name)` 在 Task 1 定义、Task 1/3 使用，签名一致；前端 `pages(device)` / `deletePage(name, device)` / `uploadToPage(file, page, device)` 与 Task 1 的接口形状一致。

**与上一版计划的差异（自查时修正）：** 原先把存储改动与接口改动分成两个任务，那样第一个任务结束时套件必红（`app.py` 仍用旧签名），违反"每个任务独立可测"。已合并为 Task 1（存储 + 迁移 + 接口 + 测试）；并删掉了一处测试里的 `__import__("hashlib")` 垃圾写法。
