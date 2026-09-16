# 串口控制台与固件刷写 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在设备管理后台新增「串口 / 固件」页：桌面浏览器经 WebSerial 直连本机串口读设备日志，并用 esptool-js 把固件刷进 NOTE4C 的活动 OTA 槽。

**Architecture:** 串口**不经服务端**——浏览器用 WebSerial 打开本机端口、用 esptool-js 就地写入。服务端只新增一个 fail-closed 的「固件仓库」（列表 / 下载 / 上传），零串口代码、零新 Python 依赖。上传件存 `data/serial-firmware/`，与 OTA 的 `data/firmware/` **严格分离**。

**Tech Stack:** FastAPI + pydantic-settings（服务端）；React 18 + react-router 6 + Vite 5（前端）；`esptool-js`（浏览器侧 flasher）；Web Serial API。

**Spec:** `docs/superpowers/specs/2026-09-16-serial-console-and-flash-design.md`

## Global Constraints

- **只刷活动槽的应用分区**：写入偏移只能是 `0x20000`（ota_0）或 `0x410000`（ota_1）；`bootloader` / 分区表 / `nvs` 一律不碰。
- **应用分区上限 `0x3F0000`（4032 KB）**，镜像头首字节必须是 `0xE9`。
- **上传件目录 = `data/serial-firmware/`**，**禁止**写入 `data/firmware/`（OTA 频道）。
- **三个新端点必须 fail-closed**：`OPERATOR_TOKEN` 未配置 ⇒ 503，不得放行。
- **打开串口即复位设备**；**绝不自动重连**；同时只能有一个串口持有者。
- 服务端不新增任何 Python 依赖；前端只新增 `esptool-js`。
- **本仓库没有 JS 测试框架**（`frontend/package.json` 无 test 脚本）。前端纯逻辑用**一次性 node 脚本**验证（不进仓库），不新增 vitest/jest。
- 服务端测试用 `cd server && .venv/bin/python -m pytest ...`；测试不得写生产 `data/`。
- 提交信息用本仓库既有风格（`feat(server):` / `feat(web):` / `test(server):` …），正文说「为什么」。

---

## File Structure

| 文件 | 职责 | 动作 |
|---|---|---|
| `server/youn_server/config.py` | 新增 `serial_firmware_dir` 设置 | Modify |
| `server/youn_server/serial_firmware.py` | 固件仓库的纯逻辑：校验 / 列举 / 定位 / 落盘 | Create |
| `server/youn_server/app.py` | 严格鉴权 + 3 个端点 + SPA index 路由 | Modify |
| `server/tests/test_serial_firmware.py` | 仓库逻辑单测 | Create |
| `server/tests/test_firmware_api.py` | 端点 HTTP 测试（含 fail-closed） | Create |
| `frontend/src/api.js` | 新增 3 个客户端方法 | Modify |
| `frontend/src/App.jsx` | 导航项 + 路由 | Modify |
| `frontend/src/pages/Serial.jsx` | 页面壳（两个页签 + 共享端口状态） | Create |
| `frontend/src/serialLog.js` | 日志管线纯逻辑（解码 / 分行 / 环形缓冲） | Create |
| `frontend/src/flashTarget.js` | 目标槽判定纯逻辑（otadata 解析）+ 写入参数 | Create |
| `frontend/src/SerialConsole.jsx` | WebSerial 读日志组件 | Create |
| `frontend/src/FirmwareFlash.jsx` | 固件清单 / 备份 / 确认 / 写入组件 | Create |
| `frontend/package.json` | 新增 `esptool-js` 依赖 | Modify |
| `README.md` / `server/DEPLOY.md` | 记录新页面与新目录 | Modify |

---

## Task 1: 固件仓库的存储与校验逻辑

**Files:**
- Create: `server/youn_server/serial_firmware.py`
- Modify: `server/youn_server/config.py:46-51`（新增字段）、`:82-96`（加入 resolve/mkdir 列表）
- Test: `server/tests/test_serial_firmware.py`

**Interfaces:**
- Consumes: `youn_server.config.settings`
- Produces:
  - `IMAGE_MAGIC: int = 0xE9`、`MAX_IMAGE_BYTES: int = 0x3F0000`、`BUILD_ID: str = "build:xiaozhi.bin"`
  - `FirmwareItem` dataclass，字段 `id, name, source, size, mtime, sha256, image_ok`，方法 `to_json() -> dict`
  - `safe_filename(name: str) -> str`
  - `validate_image(data: bytes) -> None`（失败抛 `ValueError`）
  - `list_items() -> list[FirmwareItem]`
  - `resolve_item(item_id: str) -> Optional[Path]`
  - `save_upload(filename: str, data: bytes) -> FirmwareItem`

- [ ] **Step 1: 先改设置（加字段与建目录）**

`server/youn_server/config.py`，在 `uploads_dir` 那一行后面加：

```python
    firmware_dir: Path = Field(default=Path("./data/firmware"))
    uploads_dir: Path = Field(default=Path("./data/uploads"))
    # 浏览器手动刷写用的固件仓库。与 firmware_dir（OTA 频道）严格分离：
    # 上传件若落进 OTA 目录，设备会经 /api/ota/check 当正式更新拉走并自动刷。
    serial_firmware_dir: Path = Field(default=Path("./data/serial-firmware"))
```

同一文件里，把新字段加进 `resolve_paths` 的字段元组与新建目录循环（两处都改）：

```python
        for field in ("data_dir", "devices_db", "images_dir", "firmware_dir", "uploads_dir", "log_dir", "serial_firmware_dir"):
```

- [ ] **Step 2: 写失败的测试**

`server/tests/test_serial_firmware.py`：

```python
"""Serial firmware store unit tests.

Covers the untrusted-input gate (magic/size), the storage split from the OTA
channel, and the path-traversal hardening of item ids.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from youn_server import serial_firmware as sf
from youn_server.config import settings

HEADER = bytes([0xE9]) + b"\x00" * 31


@pytest.fixture(autouse=True)
def _tmp_store(tmp_path, monkeypatch):
    """Point the store at a throwaway dir; never touch real data/."""
    d = tmp_path / "serial-firmware"
    monkeypatch.setattr(settings, "serial_firmware_dir", d)
    yield d


def test_rejects_a_file_that_is_not_an_esp_image(_tmp_store):
    with pytest.raises(ValueError, match="0xE9"):
        sf.validate_image(b"not-an-image")


def test_rejects_an_image_larger_than_the_app_partition(_tmp_store):
    with pytest.raises(ValueError, match="过大"):
        sf.validate_image(HEADER + b"\x00" * sf.MAX_IMAGE_BYTES)


def test_accepts_a_minimal_valid_image(_tmp_store):
    sf.validate_image(HEADER)  # 不抛即通过


def test_upload_lands_in_the_serial_dir_and_never_in_the_ota_dir(_tmp_store, tmp_path, monkeypatch):
    ota_dir = tmp_path / "ota-firmware"
    monkeypatch.setattr(settings, "firmware_dir", ota_dir)

    item = sf.save_upload("my build.bin", HEADER + b"payload")

    assert item.source == "upload"
    assert (_tmp_store / item.name).is_file()
    assert not ota_dir.exists() or list(ota_dir.glob("*")) == []


def test_upload_shares_the_content_hash_it_reported(_tmp_store):
    import hashlib

    data = HEADER + b"payload"
    item = sf.save_upload("x.bin", data)
    assert item.sha256 == hashlib.sha256(data).hexdigest()
    assert (_tmp_store / f"{item.name}.sha256").read_text().strip() == item.sha256


def test_item_id_cannot_escape_the_store(_tmp_store):
    assert sf.resolve_item("upload:../../etc/passwd") is None
    assert sf.resolve_item("upload:nope.bin") is None


def test_resolving_a_valid_upload_returns_its_path(_tmp_store):
    item = sf.save_upload("ok.bin", HEADER)
    p = sf.resolve_item(item.id)
    assert p is not None and p.name == item.name


def test_listing_includes_uploads_and_reports_image_ok(_tmp_store):
    sf.save_upload("ok.bin", HEADER)
    names = {i.name for i in sf.list_items()}
    assert any(n.endswith("-ok.bin") for n in names)
    assert all(i.image_ok for i in sf.list_items())
```

- [ ] **Step 3: 跑测试，确认失败**

Run: `cd server && .venv/bin/python -m pytest tests/test_serial_firmware.py -v`
Expected: FAIL —— `ModuleNotFoundError: No module named 'youn_server.serial_firmware'`

- [ ] **Step 4: 实现模块**

`server/youn_server/serial_firmware.py`：

```python
"""串口控制台用的固件仓库（与 OTA 频道严格分离）。

存储布局（settings.serial_firmware_dir）：

    serial-firmware/
        <ts>-<safe>.bin
        <ts>-<safe>.bin.sha256

只读来源（构建产物，不复制）：

    <repo>/firmware/build/xiaozhi.bin

约束来源见 spec：上传件按不可信处理；本模块**绝不**写 settings.firmware_dir。
"""
from __future__ import annotations

import hashlib
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

from .config import settings

#: ESP 应用镜像头首字节（其余 8 字节头字段不校验：浏览器侧只做粗筛，
#: 真正的芯片/镜像校验由 esptool-js 在设备上完成）
IMAGE_MAGIC = 0xE9
#: 应用分区实际大小（自 firmware/build/partition_table/partition-table.bin 解析）
MAX_IMAGE_BYTES = 0x3F0000
#: 仓库根（app.py 在 create_app 内直接 import 本模块）
_REPO_ROOT = Path(__file__).resolve().parents[2]
BUILD_ARTIFACT = _REPO_ROOT / "firmware" / "build" / "xiaozhi.bin"
BUILD_ID = "build:xiaozhi.bin"


@dataclass
class FirmwareItem:
    id: str
    name: str
    source: str  # "build" | "upload"
    size: int
    mtime: float
    sha256: str
    image_ok: bool

    def to_json(self) -> dict:
        return asdict(self)


def _sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _first_byte_is_magic(p: Path) -> bool:
    with p.open("rb") as f:
        return f.read(1) == bytes([IMAGE_MAGIC])


def safe_filename(name: str) -> str:
    """与 ota._safe_filename 同构：只保留字母数字与 . _ -"""
    return "".join(c for c in name if c.isalnum() or c in "._-").strip() or "firmware"


def validate_image(data: bytes) -> None:
    """不可信输入的唯一校验入口；失败抛 ValueError（中文原因，直接回给前端）。"""
    if not data:
        raise ValueError("空文件")
    if data[0] != IMAGE_MAGIC:
        raise ValueError("这不是 ESP 应用镜像（首字节应为 0xE9）")
    if len(data) > MAX_IMAGE_BYTES:
        raise ValueError(
            f"镜像过大：{len(data)} 字节，应用分区上限 {MAX_IMAGE_BYTES} 字节"
        )


def _item_from_path(p: Path, item_id: str, source: str) -> FirmwareItem:
    st = p.stat()
    return FirmwareItem(
        id=item_id,
        name=p.name,
        source=source,
        size=st.st_size,
        mtime=st.st_mtime,
        sha256=_sha256_file(p),
        image_ok=_first_byte_is_magic(p),
    )


def list_items() -> list[FirmwareItem]:
    items: list[FirmwareItem] = []
    if BUILD_ARTIFACT.is_file():
        items.append(_item_from_path(BUILD_ARTIFACT, BUILD_ID, "build"))
    d = settings.serial_firmware_dir
    if d.is_dir():
        for p in sorted(d.glob("*.bin")):
            items.append(_item_from_path(p, f"upload:{p.name}", "upload"))
    return items


def resolve_item(item_id: str) -> Optional[Path]:
    """把不透明 id 解析成白名单内的真实路径；解析不出返回 None。

    **不**对 id 后半段跑 ``safe_filename``：``list_items`` 用的是磁盘原始文件名，
    二次净化会让 id 无法往返（``a b.bin`` → ``ab.bin``，甚至解析到另一个文件），
    含非法字符的名字一律拒绝。解析后再做父目录校验（照 ota.get_file_path）。
    """
    if item_id == BUILD_ID:
        return BUILD_ARTIFACT if BUILD_ARTIFACT.is_file() else None
    if item_id.startswith("upload:"):
        name = item_id[len("upload:"):]
        # 不再净化：list_items 用的是磁盘原始名，二次净化会让 id 无法往返
        # （`a b.bin` → `ab.bin`，甚至解析到另一个文件）。非法名一律拒绝。
        if not name or name in (".", "..") or "/" in name or "\\" in name or "\x00" in name:
            return None
        p = settings.serial_firmware_dir / name
        if not p.is_file():
            return None
        if settings.serial_firmware_dir.resolve() not in p.resolve().parents:
            return None
        return p
    return None


def save_upload(filename: str, data: bytes) -> FirmwareItem:
    validate_image(data)
    d = settings.serial_firmware_dir
    d.mkdir(parents=True, exist_ok=True)
    stem = Path(safe_filename(filename)).stem or "firmware"
    name = f"{int(time.time() * 1000)}-{stem}.bin"
    p = d / name
    p.write_bytes(data)
    sha = hashlib.sha256(data).hexdigest()
    (d / f"{name}.sha256").write_text(sha + "\n")
    return FirmwareItem(
        id=f"upload:{name}",
        name=name,
        source="upload",
        size=len(data),
        mtime=p.stat().st_mtime,
        sha256=sha,
        image_ok=True,
    )
```

- [ ] **Step 5: 跑测试，确认通过**

Run: `cd server && .venv/bin/python -m pytest tests/test_serial_firmware.py -v`
Expected: PASS（8 passed）

> **任务执行后更正（Ruling A/B，见账本）**：`resolve_item` 对 id 二次净化会让
> id 无法往返（`a b.bin` → `ab.bin`，实测甚至会解析到另一个文件，导致下载字节与
> 列表的 size/sha256 不符），已改为"不净化 + 显式拒绝非法名"；`save_upload` 的命名
> 从秒级改毫秒，避免同秒同名上传静默互覆。评审轮另补 3 条用例（原始名往返、符号
> 链接逃逸、构建产物 size/sha256 一致），实测反向变异时会恰好变红。

- [ ] **Step 6: 跑既有套件，确认没弄坏别的东西**

Run: `cd server && .venv/bin/python -m pytest tests/ -q`
Expected: 全绿（此前基线 111 passed）

- [ ] **Step 7: 提交**

```bash
git add server/youn_server/config.py server/youn_server/serial_firmware.py server/tests/test_serial_firmware.py
git commit -m "feat(server): 固件仓库的存储与校验逻辑

与 OTA 频道分离的目录、镜像头/大小校验、路径遍历加固。
三个端点后续挂在这套纯逻辑上，便于先测规则再接 HTTP。"
```

---

## Task 2: 三个 HTTP 端点（fail-closed）

**Files:**
- Modify: `server/youn_server/app.py`（新增 `hashlib` import；`_require_operator_strict` 紧随 `_require_operator`；新端点节放在 `# ── Web admin UI ──` **之前**）
- Test: `server/tests/test_firmware_api.py`

**Interfaces:**
- Consumes: Task 1 的 `serial_firmware.list_items/resolve_item/save_upload`、`app._operator_token`
- Produces: `GET /api/firmware` → `{"items": [...]}`；`GET /api/firmware/{id}/download` → `application/octet-stream` + `X-SHA256`；`POST /api/firmware`（multipart `file`）→ 条目 JSON；`_require_operator_strict(request) -> None`

- [ ] **Step 1: 写失败的测试**

`server/tests/test_firmware_api.py`：

```python
"""Firmware repository endpoint tests.

The gate is fail-closed *by design*: unlike every other operator endpoint in
this app, an unconfigured OPERATOR_TOKEN must not open firmware read/write.
"""
from __future__ import annotations

import hashlib
import os

import pytest
from fastapi.testclient import TestClient

from youn_server import serial_firmware as sf
from youn_server.app import create_app
from youn_server.config import settings

HEADER = bytes([0xE9]) + b"\x00" * 31
TOKEN = "test-operator-token"


@pytest.fixture(autouse=True)
def _store(tmp_path, monkeypatch):
    monkeypatch.setattr(settings, "serial_firmware_dir", tmp_path / "serial-firmware")
    os.environ["OPERATOR_TOKEN"] = TOKEN
    yield
    os.environ.pop("OPERATOR_TOKEN", None)


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def test_listing_requires_the_operator_token(client):
    assert client.get("/api/firmware").status_code == 401


def test_download_requires_the_operator_token(client):
    assert client.get("/api/firmware/build:xiaozhi.bin/download").status_code == 401


def test_upload_requires_the_operator_token(client):
    r = client.post("/api/firmware", files={"file": ("x.bin", HEADER)})
    assert r.status_code == 401


def test_endpoints_refuse_when_no_token_is_configured(client, monkeypatch):
    monkeypatch.delenv("OPERATOR_TOKEN", raising=False)
    monkeypatch.setattr(settings, "operator_token", "")
    assert client.get("/api/firmware").status_code == 503
    assert client.post("/api/firmware", files={"file": ("x.bin", HEADER)}).status_code == 503
    # 下载是最会泄漏字节的那条路径：若它降级为 fail-open 且未配置令牌，
    # build:xiaozhi.bin 会直接 200 返回整份镜像。这条断言必须单独钉住。
    assert client.get("/api/firmware/build:xiaozhi.bin/download").status_code == 503


def test_upload_then_list_then_download_round_trip(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("demo.bin", HEADER + b"payload")},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 200
    item = r.json()
    assert item["source"] == "upload"
    assert item["size"] == len(HEADER) + 7

    listing = client.get("/api/firmware", headers={"X-Operator-Token": TOKEN}).json()
    assert any(i["id"] == item["id"] for i in listing["items"])

    d = client.get(f"/api/firmware/{item['id']}/download", headers={"X-Operator-Token": TOKEN})
    assert d.status_code == 200
    assert d.content == HEADER + b"payload"
    assert d.headers["x-sha256"] == hashlib.sha256(HEADER + b"payload").hexdigest()
    assert d.headers["content-type"] == "application/octet-stream"


def test_upload_rejects_a_non_esp_image(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("bad.bin", b"nope")},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 400
    assert "0xE9" in r.json()["detail"]


def test_upload_rejects_an_oversized_image(client):
    r = client.post(
        "/api/firmware",
        files={"file": ("big.bin", HEADER + b"\x00" * sf.MAX_IMAGE_BYTES)},
        headers={"X-Operator-Token": TOKEN},
    )
    assert r.status_code == 400


def test_download_rejects_traversal_and_unknown_ids(client):
    h = {"X-Operator-Token": TOKEN}
    # 正控：真实存在的上传件必须下载成功。没有这条，下面那几个 404 断言
    # 会被"路由缺失/守卫缺失"同样满足，测试就失去鉴别力。
    item = client.post(
        "/api/firmware", files={"file": ("ok.bin", HEADER)}, headers=h
    ).json()
    assert client.get(f"/api/firmware/{item['id']}/download", headers=h).status_code == 200

    for bad in ("upload:..%2F..%2Fetc%2Fpasswd", "upload:nope.bin", "bogus:x"):
        r = client.get(f"/api/firmware/{bad}/download", headers=h)
        assert r.status_code == 404
        # 断言 handler 自己的 detail，才能与 FastAPI 路由缺失的 "Not Found" 区分开
        assert r.json()["detail"] == "firmware not found"
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cd server && .venv/bin/python -m pytest tests/test_firmware_api.py -v`
Expected: FAIL —— 全部 404（端点还不存在）

- [ ] **Step 3: 实现端点**

在 `app.py` 的 `_require_operator` 之后（约 85 行）插入：

```python
def _require_operator_strict(request: Request) -> None:
    """固件仓库专用：未配置令牌时**拒绝**，而不是放行。

    与 _require_operator 的 fail-open 语义刻意相反 —— 本仓库承载设备映像，
    且后台域名公网可达；没有令牌等于把刷写素材暴露给任何人。
    """
    if not _operator_token():
        raise HTTPException(status_code=503, detail="operator token not configured")
    _require_operator(request)
```

在 `import secrets` 一行下面加 `import hashlib`，并在 `from . import ota as ota_mod` 附近加：

```python
from . import serial_firmware as serial_fw
```

然后在 `# ── Web admin UI (Vite build output) ──` 注释**之前**插入一整节：

```python
    # ── Serial firmware repository (operator, fail-closed) ──
    # Serial flashing itself happens entirely in the browser (WebSerial +
    # esptool-js); the server only hands out image bytes. No port is opened,
    # no esptool runs here.
    @app.get("/api/firmware")
    async def list_firmware(request: Request) -> dict:
        _require_operator_strict(request)
        return {"items": [i.to_json() for i in serial_fw.list_items()]}

    @app.get("/api/firmware/{item_id:path}/download")
    async def download_firmware(item_id: str, request: Request) -> Response:
        _require_operator_strict(request)
        p = serial_fw.resolve_item(item_id)
        if p is None:
            raise HTTPException(status_code=404, detail="firmware not found")
        data = p.read_bytes()
        return Response(
            content=data,
            media_type="application/octet-stream",
            headers={
                "Content-Length": str(len(data)),
                "X-SHA256": hashlib.sha256(data).hexdigest(),
                "Cache-Control": "no-store",
            },
        )

    @app.post("/api/firmware")
    async def upload_firmware(
        request: Request,
        file: UploadFile = File(...),
    ) -> dict:
        _require_operator_strict(request)
        data = await file.read()
        try:
            item = serial_fw.save_upload(file.filename or "firmware.bin", data)
        except ValueError as e:
            raise HTTPException(status_code=400, detail=str(e)) from e
        return item.to_json()
```

- [ ] **Step 4: 跑测试，确认通过**

Run: `cd server && .venv/bin/python -m pytest tests/test_firmware_api.py -v`
Expected: PASS（8 passed）

- [ ] **Step 5: 跑全量服务端套件**

Run: `cd server && .venv/bin/python -m pytest tests/ -q`
Expected: 全绿（111 + 8 + 8）

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/app.py server/tests/test_firmware_api.py
git commit -m "feat(server): 固件仓库端点（列表/下载/上传）与 fail-closed 鉴权

后台域名公网可达，固件素材的读写不能沿用 _require_operator 的
fail-open 语义；三个端点未配置令牌时一律 503。"
```

---

## Task 3: 前端 API 客户端、页面壳与路由

**Files:**
- Modify: `frontend/src/api.js`（`api` 对象末尾加 3 个方法）
- Modify: `frontend/src/App.jsx`（import、`NAV`、`routes`）
- Modify: `server/youn_server/app.py`（SPA index 路由列表加 `"/serial"`）
- Create: `frontend/src/pages/Serial.jsx`

**Interfaces:**
- Consumes: Task 2 的端点
- Produces: `api.firmwareList() -> Promise<Item[]>`、`api.firmwareBytes(id) -> Promise<ArrayBuffer>`、`api.uploadFirmware(file) -> Promise<Item>`；`pages/Serial.jsx` 默认导出组件，接收两个页签渲染

- [ ] **Step 1: 加客户端方法**

`frontend/src/api.js`，在 `api` 对象里 `otaCheck` 之前插入：

```js
  firmwareList: async () => (await request('/firmware')).items ?? [],
  // request() 对非 JSON 响应直接返回 Response，这里取字节
  firmwareBytes: (id) => request(`/firmware/${encodeURIComponent(id)}/download`).then((r) => r.arrayBuffer()),
  uploadFirmware: async (file) => {
    const fd = new FormData();
    fd.append('file', file);
    const headers = {};
    const token = getToken();
    if (token) headers['X-Operator-Token'] = token;
    const res = await fetch(`${BASE}/firmware`, { method: 'POST', body: fd, headers });
    if (!res.ok) {
      let d = res.statusText;
      try { d = (await res.json()).detail || d; } catch (e) {}
      throw new Error(d);
    }
    return res.json();
  },
```

- [ ] **Step 2: 注册路由（两处都要改，漏一处刷新 404）**

`frontend/src/App.jsx`：import 区加 `import Serial from './pages/Serial.jsx';`；`NAV` 末尾加：

```js
  { to: '/serial', label: '串口 / 固件' },
```

`routes` 的 children 里 `ota` 之后加：

```js
      { path: 'serial', element: <Serial /> },
```

`server/youn_server/app.py` 的 SPA index 列表改成：

```python
        for _spa_path in ("/", "/login", "/devices", "/pages", "/images", "/ota", "/serial"):
```

- [ ] **Step 3: 写页面壳**

`frontend/src/pages/Serial.jsx`：

```jsx
import React, { useState } from 'react';
import SerialConsole from '../SerialConsole.jsx';
import FirmwareFlash from '../FirmwareFlash.jsx';

const TABS = [
  { id: 'console', label: '串口监视' },
  { id: 'flash', label: '固件刷写' },
];

export default function Serial() {
  const [tab, setTab] = useState('console');

  return (
    <div>
      <h1>串口 / 固件</h1>
      <p className="muted">
        串口由<strong>你的浏览器</strong>直接打开（WebSerial），不经服务端。
        所以本页必须运行在<strong>设备所插的那台机器</strong>上，且使用桌面
        Chrome / Edge 89+ 或 Firefox 151+。
      </p>

      <div className="row" role="group" aria-label="串口工具">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            aria-pressed={tab === t.id}
            className={tab === t.id ? 'btn' : 'btn secondary'}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="card">
        {tab === 'console' ? <SerialConsole /> : <FirmwareFlash />}
      </div>
    </div>
  );
}
```

同时创建两个占位导出，让本步就能构建（下一步会替换成真实现）：

`frontend/src/SerialConsole.jsx`：

```jsx
import React from 'react';

export default function SerialConsole() {
  return <p className="muted">串口监视（待实现）</p>;
}
```

`frontend/src/FirmwareFlash.jsx`：

```jsx
import React from 'react';

export default function FirmwareFlash() {
  return <p className="muted">固件刷写（待实现）</p>;
}
```

- [ ] **Step 4: 构建并验证页面可达**

```bash
cd frontend && npm run build
systemctl --user restart youn-ink-server
sleep 4
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9002/serial
```

Expected: 构建成功；`/serial` 返回 **200**（SPA index）。直接刷新该路径若返回 404，说明第 2 步的 `app.py` 列表漏改。

- [ ] **Step 5: 提交**

```bash
git add frontend/src/api.js frontend/src/App.jsx frontend/src/pages/Serial.jsx frontend/src/SerialConsole.jsx frontend/src/FirmwareFlash.jsx server/youn_server/app.py
git commit -m "feat(web): 串口/固件页骨架、客户端方法与路由注册

SPA 路由必须同时在 App.jsx 与 app.py 的 index 列表登记，否则刷新 404。"
```

---

## Task 4: 日志管线的纯逻辑（解码 / 分行 / 环形缓冲）

**Files:**
- Create: `frontend/src/serialLog.js`
- Test: 一次性 node 脚本 `/tmp/seriallog_check.mjs`（**不进仓库**）

**Interfaces:**
- Produces: `class LineDecoder { push(bytes: Uint8Array) -> string[] }`、`class RingBuffer { push(lines: string[]) ; toText(opts) ; truncated: boolean }`、`const RING_LINE_CAP = 5000`、`const RING_BYTE_CAP = 2 * 1024 * 1024`

- [ ] **Step 1: 写一次性验证脚本（先让它红）**

写入 `/tmp/seriallog_check.mjs`：

```js
import assert from 'node:assert/strict';
import { LineDecoder, RingBuffer } from '/mnt/data/project/youn-ink-fourcolor-firmware/frontend/src/serialLog.js';

const enc = new TextEncoder();

// ① 一个中文字被拆进两个 USB 包，不得乱码
{
  const d = new LineDecoder();
  const bytes = enc.encode('温度正常\n');
  assert.deepEqual(d.push(bytes.slice(0, 2)), []);
  assert.deepEqual(d.push(bytes.slice(2)), ['温度正常']);
}

// ② \r 也结束一行（进度行）
{
  const d = new LineDecoder();
  assert.deepEqual(d.push(enc.encode('a\rb\n')), ['a', 'b']);
}

// ②b CRLF 是一个断行，不得多出空行
{
  const d = new LineDecoder();
  assert.deepEqual(d.push(enc.encode('a\r\nb')), ['a']);
  assert.deepEqual(d.flush(), ['b']);
}

// ③ 剥离 ANSI 色码
{
  const d = new LineDecoder();
  assert.deepEqual(d.push(enc.encode('\x1b[0;32mI (1) boot\x1b[0m\n')), ['I (1) boot']);
}

// ④ 环形缓冲按行数截断，并在 toText 头部标注
{
  const r = new RingBuffer();
  r.push(['1', '2', '3']);
  const txt = r.toText({ cap: 2, header: 'HEADER' });
  assert.match(txt, /HEADER/);
  assert.equal(txt.split('\n').slice(-2).join('\n'), '2\n3');
}

console.log('serialLog OK');
```

- [ ] **Step 2: 跑脚本，确认失败**

Run: `node /tmp/seriallog_check.mjs`
Expected: FAIL —— `ERR_MODULE_NOT_FOUND`（`serialLog.js` 还不存在）

- [ ] **Step 3: 实现**

`frontend/src/serialLog.js`：

```js
// 串口字节流 → 行。三条规则都来自真机教训：
//  1) 一个多字节字符可能被拆进两个 USB 包 ⇒ TextDecoder 必须 stream:true
//  2) 进度行用 \r 刷新 ⇒ \r 也结束一行，否则永远不刷新
//  3) ESP-IDF 开 CONFIG_LOG_COLORS 时带 ANSI 色码 ⇒ 渲染前剥掉

export const RING_LINE_CAP = 5000;
export const RING_BYTE_CAP = 2 * 1024 * 1024;

const ANSI = /\x1b\[[0-9;]*m/g;
// 保留 \t，丢掉其它 C0 控制字符（\n \r 在分行阶段已消费）
const CTRL = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

export class LineDecoder {
  constructor() {
    this.decoder = new TextDecoder('utf-8', { fatal: false });
    this.pending = '';
    // 上一条记录以孤立 \r 结束在缓冲区末尾 ⇒ 下一包开头的 \n 是同一个断行的另一半，
    // 必须吞掉，否则跨 USB 包断开的 CRLF 会多出一个空行（约每 64 行一次）。
    this.sawCR = false;
  }

  /** @param {Uint8Array} bytes @returns {string[]} 完整行 */
  push(bytes) {
    this.pending += this.decoder.decode(bytes, { stream: true });
    if (this.sawCR && this.pending.startsWith('\n')) this.pending = this.pending.slice(1);
    this.sawCR = false;
    const out = [];
    let idx;
    while ((idx = this.pending.search(/[\r\n]/)) !== -1) {
      const line = this.pending.slice(0, idx);
      const isCR = this.pending[idx] === '\r';
      // CRLF 是一个断行，不是两个：否则每行后面都会多一个空行
      const joined = isCR && this.pending[idx + 1] === '\n';
      const next = joined ? idx + 2 : idx + 1;
      this.pending = this.pending.slice(next);
      // 孤立 \r 且它就是缓冲区最后一个字符 ⇒ 下一包开头的 \n 属同一断行
      this.sawCR = isCR && !joined && this.pending.length === 0;
      out.push(clean(line));
    }
    return out;
  }

  flush() {
    if (!this.pending) {
      this.sawCR = false;
      return [];
    }
    const line = clean(this.pending);
    this.pending = '';
    this.sawCR = false;
    return [line];
  }
}

const BYTE_ENCODER = new TextEncoder();
const byteLength = (s) => BYTE_ENCODER.encode(s).length;

function clean(line) {
  return line.replace(ANSI, '').replace(CTRL, '');
}

export class RingBuffer {
  constructor() {
    this.lines = [];
    this.bytes = 0;
    this.truncated = false;
  }

  push(lines) {
    for (const l of lines) {
      this.lines.push(l);
      this.bytes += byteLength(l) + 1;
    }
    this.enforce();
  }

  pushRaw(text) {
    for (const l of text.split('\n')) this.push([l]);
  }

  enforce() {
    while (
      this.lines.length > RING_LINE_CAP ||
      (this.bytes > RING_BYTE_CAP && this.lines.length > 1)
    ) {
      const dropped = this.lines.shift();
      this.bytes -= byteLength(dropped) + 1;
      this.truncated = true;
    }
  }

  clear() {
    this.lines = [];
    this.bytes = 0;
    this.truncated = false;
  }

  /** @param {{cap?: number, header?: string}} opts @returns {string} */
  toText({ cap = Infinity, header = '' } = {}) {
    const tail = cap === Infinity ? this.lines : this.lines.slice(-cap);
    const parts = [];
    if (header) {
      parts.push(header);
      if (this.truncated) parts.push('（更早的行已被丢弃）');
    }
    return parts.concat(tail).join('\n');
  }
}
```

- [ ] **Step 4: 跑脚本，确认通过**

Run: `node /tmp/seriallog_check.mjs`
Expected: `serialLog OK`

- [ ] **Step 5: 提交**

```bash
git add frontend/src/serialLog.js
git commit -m "feat(web): 串口日志管线纯逻辑（中文解码/CR 分行/环形缓冲）"
```

---

## Task 5: 目标槽判定与写入参数（纯逻辑）

**Files:**
- Create: `frontend/src/flashTarget.js`
- Test: 一次性 node 脚本 `/tmp/flashtarget_check.mjs`（**不进仓库**）

**Interfaces:**
- Produces: `SLOTS = [{name:'ota_0', offset:0x20000}, {name:'ota_1', offset:0x410000}]`、`OTADATA_OFFSET=0xd000`、`OTADATA_SIZE=0x2000`、`APP_PARTITION_SIZE=0x3F0000`、`parseOtadata(bytes) -> {slotIndex, name, offset, seq, evidence, verified}`（`name` 来自 `...SLOTS[i]`，**不是** `slotName`）

- [ ] **Step 1: 写一次性验证脚本（先红）**

写入 `/tmp/flashtarget_check.mjs`：

```js
import assert from 'node:assert/strict';
import { parseOtadata, SLOTS, APP_PARTITION_SIZE, OTADATA_OFFSET } from '/mnt/data/project/youn-ink-fourcolor-firmware/frontend/src/flashTarget.js';

assert.equal(OTADATA_OFFSET, 0xd000);
assert.equal(APP_PARTITION_SIZE, 0x3f0000);

// ① 全 0xFF（擦除态）⇒ ota_0，且证据字符串可显示给用户
{
  const r = parseOtadata(new Uint8Array(0x2000).fill(0xff));
  assert.equal(r.offset, SLOTS[0].offset);
  assert.match(r.evidence, /擦除/);
}

// ② 第一条有效条目 ⇒ 落到某个槽，且带上 seq 证据
{
  const b = new Uint8Array(0x2000).fill(0xff);
  new DataView(b.buffer).setUint32(0, 2, true);
  const r = parseOtadata(b);
  assert.ok([0x20000, 0x410000].includes(r.offset));
  assert.equal(r.seq, 2);
  assert.match(r.evidence, /ota_seq=2/);
}

// ③ 空/短输入不得抛异常
assert.equal(parseOtadata(new Uint8Array(0)).offset, SLOTS[0].offset);

console.log('flashTarget OK');
```

- [ ] **Step 2: 跑脚本，确认失败**

Run: `node /tmp/flashtarget_check.mjs`
Expected: FAIL —— `ERR_MODULE_NOT_FOUND`

- [ ] **Step 3: 实现**

`frontend/src/flashTarget.js`：

```js
// 刷写目标判定。分区表实测值（由 firmware/build/partition_table/partition-table.bin 解析）：
//   ota_0 @0x20000 4032K   ota_1 @0x410000 4032K   otadata @0xd000 8K
//
// 为什么需要它：OTA 更新过之后 otadata 指向 ota_1，此时往 0x20000 刷会
// "刷写成功但设备照旧跑老固件"。所以目标是"活动槽"，不是常量 0x20000。

export const SLOTS = [
  { name: 'ota_0', offset: 0x20000 },
  { name: 'ota_1', offset: 0x410000 },
];
export const OTADATA_OFFSET = 0xd000;
export const OTADATA_SIZE = 0x2000;
export const APP_PARTITION_SIZE = 0x3f0000;

// 单条槽位记录 32 字节：ota_seq(u32) seq_label[20] ota_state(u32) crc(u32)。
// ⚠️ 两条记录**各占一个 4 KiB 扇区**，不是连续的两条 32 字节：
// ESP-IDF 的 bootloader_common_read_otadata() 从 ota_select_map + SPI_SEC_SIZE
// 读第二条；写入侧 rewrite_ota_seq() 整扇区擦除后只写 32 字节，所以 0x20.. 恒为
// 0xFF。按记录长度步进会永远只看到第一条记录，从第二次 OTA 起的每轮交替都判错槽
// —— 那正是"刷了但没生效"的根因（评审实测并给出 ESP-IDF 出处）。
const SECTOR_SIZE = OTADATA_SIZE / 2; // 0x1000
const ENTRY_SIZE = 32;
const ERASED = 0xffffffff;

/**
 * 解析 otadata，给出刷写目标。
 * ESP-IDF 的 seq→槽位映射**未在本项目真机上验证过**，因此结果必须连同证据一起
 * 展示，并允许用户在 UI 里手动覆盖 —— 这是 spec「已知风险」里写明的那一条。
 */
export function parseOtadata(bytes) {
  if (!bytes || bytes.byteLength < SECTOR_SIZE + ENTRY_SIZE) {
    return {
      slotIndex: 0,
      ...SLOTS[0],
      seq: null,
      evidence: 'otadata 读不到内容 ⇒ 按 ota_0 处理（请人工确认）',
      verified: false,
    };
  }
  const entries = [0, 1].map((i) => ({
    index: i,
    seq: new DataView(bytes.buffer, bytes.byteOffset + i * SECTOR_SIZE, ENTRY_SIZE).getUint32(0, true),
  }));
  const valid = entries.filter((e) => e.seq !== ERASED && e.seq !== 0);
  if (valid.length === 0) {
    return {
      slotIndex: 0,
      ...SLOTS[0],
      seq: null,
      evidence: 'otadata 为擦除态 ⇒ bootloader 回落到 ota_0',
      verified: true,
    };
  }
  const active = valid.reduce((a, b) => (b.seq > a.seq ? b : a));
  const slotIndex = (active.seq - 1) % 2;
  return {
    slotIndex,
    ...SLOTS[slotIndex],
    seq: active.seq,
    evidence: `otadata ota_seq=${active.seq} ⇒ 推断 ${SLOTS[slotIndex].name}（未真机验证，请对照开机日志的 "Loaded app from partition at offset"）`,
    verified: false,
  };
}

/** 写入参数：与项目既有 esptool 命令行一致（esp32s3 / dio / 80m / 16MB）。 */
export const FLASH_PARAMS = {
  flashMode: 'dio',
  flashFreq: '80m',
  flashSize: '16MB',
  chip: 'esp32s3',
};
```

- [ ] **Step 4: 跑脚本，确认通过**

Run: `node /tmp/flashtarget_check.mjs`
Expected: `flashTarget OK`

- [ ] **Step 5: 提交**

```bash
git add frontend/src/flashTarget.js
git commit -m "feat(web): 刷写目标槽判定（otadata 解析）与写入参数"
```

---

## Task 6: 串口监视组件

> **Ruling G（评审 T3 时发现，同样适用于本任务）**：表单控件必须按仓库既有结构写 ——
> `<label className="field"><div className="field-label">标题</div><input/></label>`
> （见 `frontend/src/pages/Ota.jsx:49-51`、`Devices.jsx:121-123`）。裸 `<label>文字<input/></label>`
> 不会命中 `.field > input` 的样式。另外：不存在的 `className="hint"` 应写 `.muted`；
> 不存在的 `className="log"` 应写 `.mono`（`.log` 从来不存在，只有 `.logout`）。

**Files:**
- Modify: `frontend/src/SerialConsole.jsx`（替换 Task 3 的占位）
- Modify: `frontend/package.json`（本任务不加依赖，仅此说明：esptool-js 在 Task 7 加）

**Interfaces:**
- Consumes: `serialLog.js` 的 `LineDecoder` / `RingBuffer` / `RING_LINE_CAP`
- Produces: 默认导出 `SerialConsole`（无 props）

- [ ] **Step 1: 实现组件**

`frontend/src/SerialConsole.jsx`：

```jsx
import React, { useEffect, useRef, useState } from 'react';
import { Banner, BusyButton } from './ui.jsx';
import { LineDecoder, RingBuffer, RING_LINE_CAP } from './serialLog.js';

const BAUD_CHOICES = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];

export default function SerialConsole() {
  const [supported] = useState(() => typeof navigator !== 'undefined' && !!navigator.serial);
  const [baud, setBaud] = useState(115200);
  const [open, setOpen] = useState(false);
  const [err, setErr] = useState('');
  const [paused, setPaused] = useState(false);
  const [filter, setFilter] = useState('');
  const [stamp, setStamp] = useState(false);
  const [text, setText] = useState('');
  const [lineCount, setLineCount] = useState(0);

  const portRef = useRef(null);
  const readerRef = useRef(null);
  const bufferRef = useRef(new RingBuffer());
  const decoderRef = useRef(new LineDecoder());
  const pausedRef = useRef(false);
  const stampRef = useRef(false);
  const rafRef = useRef(0);
  const preRef = useRef(null);
  const stickRef = useRef(true);

  useEffect(() => { pausedRef.current = paused; }, [paused]);
  useEffect(() => { stampRef.current = stamp; }, [stamp]);

  // 批量渲染：每帧最多重画一次，避免高频启动日志把 DOM 打爆
  const scheduleRender = () => {
    if (rafRef.current) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = 0;
      const buf = bufferRef.current;
      let lines = buf.lines;
      if (filter) lines = lines.filter((l) => l.includes(filter));
      setText(lines.slice(-RING_LINE_CAP).join('\n'));
      setLineCount(buf.lines.length);
      const el = preRef.current;
      if (el && stickRef.current) el.scrollTop = el.scrollHeight;
    });
  };

  const readLoop = async (port) => {
    while (port.readable && readerRef.current) {
      const reader = port.readable.getReader();
      readerRef.current = reader;
      try {
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          const lines = decoderRef.current.push(value);
          if (lines.length) {
            if (stampRef.current) {
              const t = new Date().toISOString().slice(11, 23);
              bufferRef.current.push(lines.map((l) => `[${t}] ${l}`));
            } else {
              bufferRef.current.push(lines);
            }
            if (!pausedRef.current) scheduleRender();
          }
        }
      } catch (e) {
        setErr(`读取中断：${e.message}`);
        break;
      } finally {
        reader.releaseLock();
        readerRef.current = null;
      }
    }
  };

  const connect = async () => {
    setErr('');
    try {
      const port = await navigator.serial.requestPort();
      await port.open({ baudRate: baud });
      portRef.current = port;
      setOpen(true);
      // 打开会复位设备：清空旧内容，从头开始看
      bufferRef.current.clear();
      decoderRef.current = new LineDecoder();
      scheduleRender();
      readLoop(port);
    } catch (e) {
      setErr(`打开失败：${e.message}（本机另一个程序可能占着这个端口）`);
    }
  };

  const disconnect = async () => {
    try {
      await readerRef.current?.cancel();
    } catch (e) { /* 已经断了 */ }
    try {
      await portRef.current?.close();
    } catch (e) { /* 已经断了 */ }
    portRef.current = null;
    readerRef.current = null;
    setOpen(false);
  };

  useEffect(() => {
    if (!supported) return undefined;
    const onDisconnect = (e) => {
      if (portRef.current && e.target === portRef.current) {
        setErr('设备已断开（USB 掉线或设备进入深睡）。不会自动重连 —— 点「重新连接」。');
        setOpen(false);
        portRef.current = null;
      }
    };
    navigator.serial.addEventListener('disconnect', onDisconnect);
    return () => navigator.serial.removeEventListener('disconnect', onDisconnect);
  }, [supported]);

  useEffect(() => () => { disconnect(); }, []);

  const download = () => {
    const header = `# 串口日志 ${new Date().toISOString()} baud=${baud} lines=${lineCount}`;
    const blob = new Blob([bufferRef.current.toText({ header })], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `serial-${Date.now()}.log`;
    a.click();
    URL.revokeObjectURL(a.href);
  };

  if (!supported) {
    return (
      <Banner>
        这个浏览器没有 Web Serial。请用桌面版 Chrome / Edge 89+ 或 Firefox 151+
        打开本页，并且必须是 HTTPS 或 localhost（局域网 IP 的 http 页面拿不到串口）。
      </Banner>
    );
  }

  return (
    <div>
      <Banner>{err}</Banner>
      <p className="muted">
        打开串口会<strong>复位设备一次</strong>；监视期间设备不会进深睡，关闭本页即恢复。
      </p>

      <div className="field">
        <label>
          波特率
          <select value={baud} disabled={open} onChange={(e) => setBaud(Number(e.target.value))}>
            {BAUD_CHOICES.map((b) => <option key={b} value={b}>{b}</option>)}
          </select>
        </label>
        <BusyButton busy={false} onClick={open ? disconnect : connect}>
          {open ? '断开' : '打开串口并复位设备'}
        </BusyButton>
        <button type="button" className="btn secondary" onClick={() => setPaused((p) => !p)}>
          {paused ? '继续显示' : '暂停显示'}
        </button>
        <button type="button" className="btn secondary" onClick={() => { bufferRef.current.clear(); scheduleRender(); }}>
          清屏
        </button>
        <button type="button" className="btn secondary" onClick={download} disabled={!lineCount}>
          下载 .log
        </button>
        <label>
          <input type="checkbox" checked={stamp} onChange={(e) => setStamp(e.target.checked)} />
          每行加接收时间戳
        </label>
        <label>
          过滤
          <input value={filter} onChange={(e) => { setFilter(e.target.value); scheduleRender(); }} />
        </label>
      </div>

      <pre
        ref={preRef}
        className="mono"
        style={{ maxHeight: 420, overflow: 'auto', whiteSpace: 'pre-wrap' }}
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
        }}
      >
        {text}
      </pre>
      <p className="muted">
        缓冲上限 {RING_LINE_CAP} 行；暂停只停渲染，读取仍在继续（否则设备侧写日志会被拖住）。
      </p>
    </div>
  );
}
```

- [ ] **Step 2: 构建**

Run: `cd frontend && npm run build`
Expected: 构建成功，无 JSX/ESLint 层错误。

- [ ] **Step 3: 验证「不支持」降级路径（可自动化）**

在设备旁之外的任意环境都能测：用无头浏览器打开 `http://127.0.0.1:9002/serial`，并在页面加载前把 `navigator.serial` 删掉，断言页面出现「这个浏览器没有 Web Serial」提示、且不出现「打开串口」按钮。

用浏览器工具执行（等价的一次性脚本）：

```js
// 打开 http://127.0.0.1:9002/serial（登录后），先 login（localStorage youn_operator_token）
// 然后在页面上下文注入：
//   Object.defineProperty(navigator, 'serial', { value: undefined });
// 重新加载，断言 document.body.innerText 含 '没有 Web Serial'
```

Expected: 断言通过（降级提示存在，按钮不存在）。

- [ ] **Step 4: 提交**

```bash
git add frontend/src/SerialConsole.jsx
git commit -m "feat(web): 串口监视组件（中文解码、暂停只停渲染、不自动重连）

打开即复位、单读者、深睡会断开 —— 三条都来自真机教训，UI 文案与
行为按此写死，避免使用者误判设备故障。"
```

---

## Task 7: 固件刷写组件

> **Ruling I（预扫漏判，已更正）**：本任务原先写的是 `target.slotName`，而 T5 的
> `parseOtadata` 返回的是 `...SLOTS[i]` 展开出来的 **`name`** —— 下游会渲染 `undefined`。
> 计划已统一为 `name`（T5 的接口声明同步更正）。

> **Ruling G（评审 T3 时发现，同样适用于本任务）**：表单控件必须按仓库既有结构写 ——
> `<label className="field"><div className="field-label">标题</div><input/></label>`
> （见 `frontend/src/pages/Ota.jsx:49-51`、`Devices.jsx:121-123`）。裸 `<label>文字<input/></label>`
> 不会命中 `.field > input` 的样式。另外：不存在的 `className="hint"` 应写 `.muted`；
> 不存在的 `className="log"` 应写 `.mono`（`.log` 从来不存在，只有 `.logout`）。

**Files:**
- Modify: `frontend/src/FirmwareFlash.jsx`（替换占位）
- Modify: `frontend/package.json`（新增 `esptool-js`）
- Modify: `frontend/src/api.js`（如 Task 3 已加完则不动）

**Interfaces:**
- Consumes: `flashTarget.js` 的 `parseOtadata` / `SLOTS` / `OTADATA_OFFSET` / `OTADATA_SIZE` / `APP_PARTITION_SIZE` / `FLASH_PARAMS`；`api.firmwareList/firmwareBytes/uploadFirmware`
- Produces: 默认导出 `FirmwareFlash`（无 props）

- [ ] **Step 1: 装依赖**

```bash
cd frontend && npm install --save esptool-js && node -e "console.log(require('./package.json').dependencies)"
```

Expected: `dependencies` 里出现 `esptool-js`；记下装到的版本号写进提交信息。

- [ ] **Step 2: 实现组件**

`frontend/src/FirmwareFlash.jsx`：

```jsx
import React, { useEffect, useRef, useState } from 'react';
import { api } from './api.js';
import { Banner, BusyButton } from './ui.jsx';
import { formatBytes } from './format.js';
import {
  APP_PARTITION_SIZE, FLASH_PARAMS, OTADATA_OFFSET, OTADATA_SIZE, SLOTS, parseOtadata,
} from './flashTarget.js';
import { md5Hex } from './md5.js';

const STEP_IDLE = 'idle';

export default function FirmwareFlash() {
  const [supported] = useState(() => typeof navigator !== 'undefined' && !!navigator.serial);
  const [items, setItems] = useState([]);
  const [selected, setSelected] = useState('');
  const [err, setErr] = useState('');
  const [step, setStep] = useState(STEP_IDLE);
  const [confirmText, setConfirmText] = useState('');
  const [target, setTarget] = useState(null);
  const [deviceInfo, setDeviceInfo] = useState(null);
  const [log, setLog] = useState('');
  const [advanced, setAdvanced] = useState(false);
  const logRef = useRef(null);
  const fileRef = useRef(null);

  const say = (s) => setLog((prev) => `${prev}${s}\n`);

  const load = async () => {
    try { setItems(await api.firmwareList()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); }, []);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [log]);

  // 刷写中关标签页 = 半写状态，必须拦住
  useEffect(() => {
    if (step === STEP_IDLE) return undefined;
    const h = (e) => { e.preventDefault(); e.returnValue = ''; };
    window.addEventListener('beforeunload', h);
    return () => window.removeEventListener('beforeunload', h);
  }, [step]);

  const upload = async () => {
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择 .bin 文件'); return; }
    setErr('');
    try {
      const item = await api.uploadFirmware(f);
      say(`已上传 ${item.name}（${formatBytes(item.size)}，sha256 ${item.sha256.slice(0, 12)}…）`);
      await load();
      setSelected(item.id);
    } catch (e) { setErr(e.message); }
  };

  const expectedConfirm = deviceInfo ? deviceInfo.mac.replace(/:/g, '').slice(-4).toUpperCase() : '';

  const flash = async () => {
    const item = items.find((i) => i.id === selected);
    if (!item) { setErr('请先选择固件'); return; }
    if (confirmText.trim().toUpperCase() !== expectedConfirm) {
      setErr(`确认串不匹配：请输入设备 MAC 末四位 ${expectedConfirm}`);
      return;
    }
    setErr('');
    setLog('');
    let port = null;
    let transport = null;
    let esploader = null;
    try {
      setStep('打开串口');
      const esptool = await import('esptool-js'); // 懒加载，不进首屏 bundle

      const bytes = await api.firmwareBytes(item.id);
      const data = new Uint8Array(bytes);
      say(`固件 ${item.name} ${formatBytes(data.length)} sha256 ${item.sha256.slice(0, 12)}…`);

      port = await navigator.serial.requestPort();
      const terminal = {
        clean: () => {},
        writeLine: (s) => say(s),
        write: (s) => setLog((prev) => prev + s),
      };
      transport = new esptool.Transport(port, true);
      esploader = new esptool.ESPLoader({ transport, baudrate: 115200, terminal, debugLogging: false });

      setStep('识别芯片');
      const chip = await esploader.main();
      say(`芯片：${chip}`);
      if (!/ESP32-S3/i.test(chip)) throw new Error(`这不是 ESP32-S3（读到 ${chip}），已中止，未写入任何字节`);

      const mac = (esploader.chip?.MAC || '').toUpperCase();
      setDeviceInfo({ chip, mac });
      say(`MAC：${mac}`);
      if (!mac || mac.replace(/:/g, '').slice(-4).toUpperCase() !== expectedConfirm) {
        throw new Error(`确认串对应的是 ${expectedConfirm}，与本机设备 ${mac} 不符，已中止`);
      }

      setStep('读 otadata 判定目标槽');
      const otaBytes = await esploader.readFlash(OTADATA_OFFSET, OTADATA_SIZE, () => {});
      const t = parseOtadata(new Uint8Array(otaBytes));
      setTarget(t);
      say(`目标槽：${t.name} @0x${t.offset.toString(16)}（${t.evidence}）`);

      setStep('备份当前固件');
      const backup = new Uint8Array(
        await esploader.readFlash(t.offset, APP_PARTITION_SIZE, () => {}),
      );
      const blob = new Blob([backup], { type: 'application/octet-stream' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = `backup-${mac.replace(/:/g, '')}-${t.name}-${Date.now()}.bin`;
      a.click();
      URL.revokeObjectURL(a.href);
      say(`已下载备份 ${formatBytes(backup.length)}`);

      setStep('写入');
      await esploader.writeFlash({
        fileArray: [{ data, address: t.offset }],
        flashMode: FLASH_PARAMS.flashMode,
        flashFreq: FLASH_PARAMS.flashFreq,
        flashSize: FLASH_PARAMS.flashSize,
        eraseAll: false,
        compress: true,
        reportProgress: (i, written, total) => {
          if (written === total) say(`写入完成 ${formatBytes(total)}`);
        },
        calculateMD5Hash: (image) => md5Hex(image),
      });

      setStep('复位');
      await esploader.after('hard_reset');
      setStep(STEP_IDLE);
      say('刷写结束：设备已复位。若要验证，去「串口监视」页签重新打开串口看开机日志。');
    } catch (e) {
      setErr(`刷写中止：${e.message}；如果已经进入写入阶段，设备可能处于半写状态 —— 不要断电，用备份回滚。`);
      setStep(STEP_IDLE);
      try { await transport?.disconnect?.(); } catch (x) { /* ignore */ }
    }
  };

  if (!supported) {
    return (
      <Banner>
        这个浏览器没有 Web Serial，无法刷写。请用桌面版 Chrome / Edge 89+ 或
        Firefox 151+，并确保页面是 HTTPS 或 localhost。
      </Banner>
    );
  }

  const busy = step !== STEP_IDLE;

  return (
    <div>
      <Banner>{err}</Banner>
      <p className="muted">
        默认只写<strong>活动 OTA 槽的应用分区</strong>（bootloader / 分区表 / NVS 不碰），
        刷前自动读出当前固件并下载为备份。写入前需输入设备 MAC 末四位确认。
      </p>

      <div className="field">
        <label>
          服务器上的固件
          <select value={selected} onChange={(e) => setSelected(e.target.value)} disabled={busy}>
            <option value="">— 选择 —</option>
            {items.map((i) => (
              <option key={i.id} value={i.id}>
                {i.source === 'build' ? '[构建产物] ' : '[上传] '}{i.name} · {formatBytes(i.size)}
              </option>
            ))}
          </select>
        </label>
        <button type="button" className="btn secondary" onClick={load} disabled={busy}>刷新清单</button>
      </div>

      <div className="field">
        <label>
          或上传一个 .bin
          <input type="file" accept=".bin" ref={fileRef} disabled={busy} />
        </label>
        <BusyButton busy={busy} onClick={upload}>上传</BusyButton>
      </div>

      {target && (
        <p className="muted">
          目标：<strong>{target.name} @0x{target.offset.toString(16)}</strong>
          {' '}（{target.evidence}）
        </p>
      )}

      <div className="field">
        <label>
          输入设备 MAC 末四位以确认
          <input
            value={confirmText}
            onChange={(e) => setConfirmText(e.target.value)}
            disabled={busy}
            placeholder="例：3400"
          />
        </label>
        <BusyButton busy={busy} busyText={step === STEP_IDLE ? '' : `${step}…`} onClick={flash}>
          开始刷写
        </BusyButton>
      </div>

      <label>
        <input type="checkbox" checked={advanced} onChange={(e) => setAdvanced(e.target.checked)} />
        手动覆盖目标槽
      </label>
      {advanced && (
        <div className="field">
          {SLOTS.map((s) => (
            <button
              key={s.name}
              type="button"
              className="btn secondary"
              disabled={busy}
              onClick={() => setTarget({ slotIndex: SLOTS.indexOf(s), ...s, seq: null, evidence: '手动覆盖', verified: false })}
            >
              {s.name} @0x{s.offset.toString(16)}
            </button>
          ))}
        </div>
      )}

      <pre ref={logRef} className="mono" style={{ maxHeight: 320, overflow: 'auto' }}>{log}</pre>
      <p className="muted">写入完成前不要关页面、不要拔线。</p>
    </div>
  );
}
// esptool-js 要求调用方提供 MD5 实现（它自己不打包 crypto）；
// md5Hex 来自 ./md5.js，见下一步。ESM 下只能用 import，不能用 require。
```

- [ ] **Step 2b: 核实 esptool-js 的 MAC 读取 API（不要照抄猜的属性名）**

```bash
cd frontend && grep -rnE "readMac|MAC" node_modules/esptool-js/lib/index.d.ts | head -20
```

Expected: 找到读取 MAC 的公开入口（形如 `readMac(loader)` 或 loader 上的属性）。
按实际签名改写 `flash()` 里那两行（`esploader.chip?.MAC` 只是占位猜测）；若不存在
公开入口，改用 `esploader.readFlash` 读 eFuse MAC 区块之外的办法 —— **要么核实，要么
把确认串改成"输入目标偏移"**，不要留一个可能永远为空的确认串。

- [ ] **Step 3: 提供 MD5 实现**

`frontend/src/md5.js`（下面这份已用 5 组已知答案 + node `crypto` 交叉校验验证过，照抄即可）：

```js
// RFC 1321 MD5。esptool-js 的 calculateMD5Hash 要求调用方提供实现
// （它自己不打包 crypto），而 Web Crypto 不提供 MD5。
// 用已知答案向量钉死正确性，见同目录的验证脚本。

const S = [
  7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
  5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
  4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
  6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K = new Uint32Array(64);
for (let i = 0; i < 64; i += 1) {
  K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296);
}

const rotl = (x, c) => (x << c) | (x >>> (32 - c));

/** @param {Uint8Array} bytes @returns {string} 小写 32 位十六进制 */
export function md5Hex(bytes) {
  const len = bytes.length;
  const bitLen = len * 8;
  // 填充后长度：len + 0x80 + pad + 8 字节长度字段，向上取整到 64
  const total = (((len + 8) >> 6) + 1) << 6;
  const buf = new Uint8Array(total);
  buf.set(bytes);
  buf[len] = 0x80;
  const dv = new DataView(buf.buffer);
  dv.setUint32(total - 8, bitLen >>> 0, true);
  dv.setUint32(total - 4, Math.floor(bitLen / 4294967296), true);

  let a = 0x67452301;
  let b = 0xefcdab89;
  let c = 0x98badcfe;
  let d = 0x10325476;

  const M = new Uint32Array(16);
  for (let off = 0; off < total; off += 64) {
    for (let j = 0; j < 16; j += 1) M[j] = dv.getUint32(off + j * 4, true);
    let A = a;
    let B = b;
    let C = c;
    let D = d;
    for (let i = 0; i < 64; i += 1) {
      let F;
      let g;
      if (i < 16) {
        F = (B & C) | (~B & D);
        g = i;
      } else if (i < 32) {
        F = (D & B) | (~D & C);
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        F = B ^ C ^ D;
        g = (3 * i + 5) % 16;
      } else {
        F = C ^ (B | ~D);
        g = (7 * i) % 16;
      }
      F = (F + A + K[i] + M[g]) | 0;
      A = D;
      D = C;
      C = B;
      B = (B + rotl(F, S[i])) | 0;
    }
    a = (a + A) | 0;
    b = (b + B) | 0;
    c = (c + C) | 0;
    d = (d + D) | 0;
  }

  const hex = (n) => {
    let s = '';
    for (let i = 0; i < 4; i += 1) s += ((n >>> (i * 8)) & 0xff).toString(16).padStart(2, '0');
    return s;
  };
  return hex(a) + hex(b) + hex(c) + hex(d);
}
```

- [ ] **Step 4: 用一次性脚本验证 MD5 与目标槽逻辑**

写入 `/tmp/md5_check.mjs`：

```js
import assert from 'node:assert/strict';
import { md5Hex } from '/mnt/data/project/youn-ink-fourcolor-firmware/frontend/src/md5.js';

assert.equal(md5Hex(new TextEncoder().encode('')), 'd41d8cd98f00b204e9800998ecf8427e');
assert.equal(md5Hex(new TextEncoder().encode('abc')), '900150983cd24fb0d6963f7d28e17f72');
assert.equal(md5Hex(new TextEncoder().encode('The quick brown fox jumps over the lazy dog')),
  '9e107d9d372bb6826bd81d3542a419d6');
assert.equal(md5Hex(new TextEncoder().encode('a'.repeat(1000))), 'cabe45dcc9ae5b66ba86600cca6b8ba8');
assert.equal(md5Hex(new TextEncoder().encode('中文')), 'a7bac2239fcdcb3a067903d8077c4a07');
// 跨 64 字节块边界：与 node crypto 对拍
{
  const b = new Uint8Array(200);
  for (let i = 0; i < 200; i += 1) b[i] = i & 0xff;
  const { createHash } = await import('node:crypto');
  assert.equal(md5Hex(b), createHash('md5').update(Buffer.from(b)).digest('hex'));
}
console.log('md5 OK');
```

Run: `node /tmp/md5_check.mjs`
Expected: `md5 OK`

- [ ] **Step 5: 构建**

Run: `cd frontend && npm run build`
Expected: 构建成功；提醒：`esptool-js` 走动态 import，不应进入首屏 chunk（可在 `dist/assets/` 里看到单独的 chunk）。

- [ ] **Step 6: 提交**

```bash
git add frontend/src/FirmwareFlash.jsx frontend/src/md5.js frontend/package.json frontend/package-lock.json
git commit -m "feat(web): 固件刷写组件（活动槽目标、刷前备份、MAC 末四位确认）

默认只写活动 OTA 槽：OTA 更新后 otadata 指向 ota_1，此时刷 0x20000 会
'成功但设备照旧跑老固件'。目标槽判定结果连同证据一起展示并允许覆盖。"
```

---

## Task 8: 部署、文档与真机验收

**Files:**
- Modify: `server/DEPLOY.md`、`README.md`
- （无代码改动）

- [ ] **Step 1: 部署**

```bash
cd /mnt/data/project/youn-ink-fourcolor-firmware/frontend && npm run build
systemctl --user restart youn-ink-server
sleep 4
curl -s -o /dev/null -w 'spa /serial: %{http_code}\n' http://127.0.0.1:9002/serial
TOK=$(grep -oP '^OPERATOR_TOKEN=\K.*' ../server/.env | tr -d '"')
curl -s -H "X-Operator-Token: $TOK" http://127.0.0.1:9002/api/firmware | head -c 400
```

Expected: `/serial` 200；`/api/firmware` 返回含 `build:xiaozhi.bin` 的 JSON。

- [ ] **Step 2: 文档补两句**

`server/DEPLOY.md`：新增一节说明 `/api/firmware` 三个端点、`data/serial-firmware/` 目录、以及**它为什么与 OTA 目录分离**。
`README.md`：在后台功能列表里加一行「串口 / 固件：浏览器直连本机串口（WebSerial）」。

- [ ] **Step 3: 真机验收（交付门槛，必须人工在设备旁做）**

1. 在插着 NOTE4C 的机器上用桌面 Chrome 打开 `https://note-device.1024.center:31443/serial`
2. 「串口监视」→ 打开串口并复位 ⇒ 应看到从头的启动日志，中文不乱码
3. 拔线 ⇒ 显示「设备已断开」且**不**自动重连；手动重连 ⇒ 再次从头
4. 「固件刷写」→ 选 `[构建产物] xiaozhi.bin` ⇒ 输入 MAC 末四位 ⇒ 备份下载完成 ⇒ 写入 100% ⇒ 设备复位
5. 回到「串口监视」⇒ 开机日志里能看到本次固件特征串（证明跑的是新代码）
6. **顺手把 otadata 映射的疑问结掉**：记下刷写时 UI 显示的 `ota_seq` 与目标槽，对照开机日志里 `Loaded app from partition at offset 0x…`。
   - 两者一致 ⇒ 把 `flashTarget.js` 里 `verified: false` 改成依据实测的注释，并在 spec 的「已知风险」表里标为已验证。
   - 不一致 ⇒ 修正 `parseOtadata` 的槽位映射，并补一条断言到 `/tmp/flashtarget_check.mjs`。

- [ ] **Step 4: 提交**

```bash
git add server/DEPLOY.md README.md
git commit -m "docs: 串口控制台与固件仓库的部署说明"
```

---

## Self-Review

**1. Spec coverage**

| Spec 章节 | 落实任务 |
|---|---|
| 架构（浏览器直连、服务端只做仓库、目录分离） | T1（目录）、T2（端点）、T6/T7（浏览器侧） |
| 服务端接口表 + fail-closed + 上传校验 + 路径防护 | T1、T2 |
| 前端页面/路由两处注册 | T3 |
| 日志管线（stream 解码、`\r` 分行、ANSI、环形缓冲、批量渲染、暂停只停渲染） | T4、T6 |
| 刷写状态机（校验→识别→判槽→备份→确认→写入→复位） | T5、T7 |
| 活动槽判定 + 允许覆盖 + 未验证标注 | T5、T7、T8-Step3.6 |
| 端口生命周期（关闭顺序、断开不重连、无 WebSerial 降级） | T6 |
| 安全（公网面、令牌、不可信上传） | T2、T7 |
| 测试与验收 | T1/T2（pytest）、T4/T5/T7（node 一次性脚本）、T8（真机） |
| 部署 | T3-Step4、T8-Step1 |

**2. Placeholder scan**：无 TBD/TODO；每个代码步骤都给出了完整代码；MD5 实现是唯一「按规格实现」的算法（给出了 RFC 1321 + 三条已知答案向量来钉死正确性，而非留空）。

**3. Type consistency**：`FirmwareItem.id/name/source/size/mtime/sha256/image_ok` 在 T1 定义，T2 直接 `to_json()` 透传，T3 的 `firmwareList/firmwareBytes/uploadFirmware` 与之一致；`parseOtadata` 返回的 `{slotIndex, name, offset, seq, evidence, verified}` 在 T5 定义、T7 按同名取用；`LineDecoder/RingBuffer` 在 T4 定义、T6 按同名取用。
