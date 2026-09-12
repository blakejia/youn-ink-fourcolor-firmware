# 上传绑定页面 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Web 上传的图片必须绑定到一个已有页面，服务端把它做成该页面的画板并重新渲染位图，设备在下一轮轮询后显示它。

**Architecture:** 复用现有且唯一有效的投递链路（页面 → canvas 渲染 → 内容寻址位图 → `/api/pages/schedule` → 设备）。新端点 `POST /api/uploads` 校验目标页面存在、把图片归一化成 400×300 的 PNG 落盘到 `data/uploads/`、生成一块引用 `uploads://<id>` 的画板，然后走与 `POST /api/pages` 完全相同的 `render_canvas_to_bitmap` + `upsert_page` 两步。页面的 `duration_minutes`/`order` 从被替换页面的 `PageSource` 读回原样传回，所以轮播位置不变。同时删除今天那条断掉的上传路径（WS 推送）。

**Tech Stack:** FastAPI + Pillow（服务端，`server/youn_server/`）、pytest + `fastapi.testclient`（测试）、React + Vite（`frontend/`）、ESP32-S3 固件（只做验证，不改代码）。

**Spec:** `docs/superpowers/specs/2026-09-12-upload-to-page-binding-design.md`

## Global Constraints

- **只替换、不新建页面。** 目标页面不存在 ⇒ 400，且不写任何文件（先校验后落盘）。
- `duration_minutes` 与 `order` 必须从该页现有 `{name}.json`（`PageSource`）读出后原样传回，保证逐字节不变。
- `uploads://<id>` 的 `id` 只接受 `^[0-9a-f]{32}$`；拼接后的路径必须落在 `settings.uploads_dir` 内。
- 位图必须恰好 30000 字节（`upsert_page` 内部已校验，不得绕过）。
- 画布方言是 **Tailwind/flex**：根为 `{"default": [ ... ]}`，节点是 `{"type": ..., "props": {"tw": ..., "children": [...]}}`，图片节点为 `{"type": "img", "props": {"src": "<uri>"}}`。**不要**使用 `x/y/w/h`。
- `_render_img` 的缩放是 `min(box.w/iw, box.h/ih, 1.0)` —— **从不放大**，所以必须在上传时把图归一化到 400×300 之内，画板里才是 1:1。
- 图片适配方式：**contain**（整图可见、居中、四周补白 `#ffffff`）。
- 非目标（本次不做）：S3/CDN、上传即新建页面、"上传后立刻轮到这一页"。
- 测试基线命令：`cd server && ./.venv/bin/python -m pytest tests -q`（必须保持全绿）。跑测试前先看 `server/data/devices.db` 不要被写（conftest 已隔离）。
- 前端产物：`frontend/dist`，由 `server/youn_server/app.py:749` 挂载；改完前端必须 `npm run build`。

---

### Task 1: 渲染器认识 `uploads://`

**Files:**
- Modify: `server/youn_server/canvas_render.py:535-549`（`_load_image`）
- Modify: `server/tests/conftest.py`（把 `uploads_dir` 也纳入隔离）
- Test: `server/tests/test_canvas_render.py`（追加用例）

**Interfaces:**
- Consumes: `settings.uploads_dir`（`server/youn_server/config.py:51`，已声明但至今未被任何代码使用；目录在启动时已创建）
- Produces: `_load_image` 支持 `uploads://<32hex>`，读取 `settings.uploads_dir / f"{id}.png"`

- [ ] **Step 1: 让测试也隔离 uploads 目录**

`tests/conftest.py` 里 `isolate_pages_dir` 只改了 `settings.data_dir`，而 `uploads_dir` 是独立配置项，默认指向真实的 `./data/uploads`。不隔离的话测试会把文件写进生产目录。把该 fixture 改成同时隔离两者：

```python
@pytest.fixture(autouse=True)
def isolate_pages_dir():
    """Isolate pages/ and uploads/ storage to per-test temp dirs."""
    tmp = tempfile.mkdtemp(prefix="pages_test_")
    orig_data, orig_uploads = settings.data_dir, settings.uploads_dir
    settings.data_dir = Path(tmp)
    settings.uploads_dir = Path(tmp) / "uploads"
    (Path(tmp) / "pages").mkdir(parents=True, exist_ok=True)
    settings.uploads_dir.mkdir(parents=True, exist_ok=True)
    yield
    settings.data_dir = orig_data
    settings.uploads_dir = orig_uploads
    shutil.rmtree(tmp, ignore_errors=True)
```

- [ ] **Step 2: 写失败测试**

追加到 `server/tests/test_canvas_render.py`：

```python
def _write_upload(upload_id: str) -> None:
    """A 400x300 solid-red PNG sits in the uploads dir."""
    from youn_server.config import settings
    img = Image.new("RGB", (400, 300), (220, 30, 30))
    settings.uploads_dir.mkdir(parents=True, exist_ok=True)
    img.save(settings.uploads_dir / f"{upload_id}.png", format="PNG")


def test_uploads_scheme_renders_the_stored_image():
    from youn_server.config import settings
    up_id = "a" * 32
    _write_upload(up_id)
    canvas = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": [{"type": "img", "props": {"src": f"uploads://{up_id}"}}]}}]}
    bitmap = render_canvas_to_bitmap(canvas)
    assert len(bitmap) == 30000

    blank = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": []}}]}
    # A red picture must differ from an empty white canvas.
    assert bitmap != render_canvas_to_bitmap(blank)


def test_uploads_scheme_rejects_an_id_that_could_escape_the_directory():
    canvas = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": [{"type": "img", "props": {"src": "uploads://../../etc/passwd"}}]}}]}
    # _render_img logs and skips an image it cannot load; the page still renders.
    bitmap = render_canvas_to_bitmap(canvas)
    assert len(bitmap) == 30000


def test_uploads_scheme_skips_a_missing_file():
    canvas = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": [{"type": "img", "props": {"src": "uploads://" + "b" * 32}}]}}]}
    assert len(render_canvas_to_bitmap(canvas)) == 30000
```

（文件顶部若没有 `from PIL import Image` 就加上；`render_canvas_to_bitmap` 已在该文件被导入。）

- [ ] **Step 3: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_canvas_render.py -q -k uploads`
Expected: FAIL —— `unsupported scheme: uploads://aa...`，渲染被跳过 ⇒ 与空画布相同 ⇒ 第一个用例断言失败。

- [ ] **Step 4: 实现 `uploads://` 分支**

在 `canvas_render.py` 的 `_load_image` 中，把 `http(s)` 分支与 `else` 之间插入：

```python
        elif src.startswith("uploads://"):
            ident = src[len("uploads://"):]
            if not re.fullmatch(r"[0-9a-f]{32}", ident):
                raise RenderError("src", f"invalid upload id: {ident[:40]}")
            path = settings.uploads_dir / f"{ident}.png"
            if not path.exists():
                raise RenderError("src", f"upload not found: {ident}")
            img = Image.open(path)
```

确认 `canvas_render.py` 顶部已 `import re` 与 `from youn_server.config import settings`（`_load_image` 已在用其它 settings，按文件现状补齐）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_canvas_render.py -q`
Expected: PASS（含原有用例）

- [ ] **Step 6: Commit**

```bash
git add server/youn_server/canvas_render.py server/tests/conftest.py server/tests/test_canvas_render.py
git commit -m "feat(canvas): teach the renderer the uploads:// reference"
```

---

### Task 2: `POST /api/uploads`（必须绑定）

**Files:**
- Modify: `server/youn_server/app.py`（新增端点，放在页面端点附近，约 617 行后）
- Test: `server/tests/test_uploads_api.py`（新建）

**Interfaces:**
- Consumes: `pages_mod.list_pages()`、`pages_mod._page_path`（读现有 `PageSource`）、`pages_mod.upsert_page(name, canvas_json, duration_minutes, order, bitmap_bytes)`、`render_canvas_to_bitmap(canvas_json)`、`_require_operator(request)`、`settings.uploads_dir`
- Produces: `POST /api/uploads`（multipart `image` + `page`）⇒ `{"page", "md5", "duration_minutes", "order"}`；磁盘上 `data/uploads/{id}.png`（归一化后，画板引用的那张）与 `data/uploads/{id}.src.png`（原图）

- [ ] **Step 1: 写失败测试**

新建 `server/tests/test_uploads_api.py`：

```python
"""Upload must bind to an existing page.

Covers: the refusal when no page is named, the refusal when the named page does
not exist, the happy path replacing a page's picture while its identity
(duration, order, name) is untouched, and the fact that a bad image leaves the
page alone.
"""
from __future__ import annotations

import io

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from youn_server.app import create_app
from youn_server import pages as pages_mod


@pytest.fixture(scope="module")
def client():
    app = create_app()
    with TestClient(app) as c:
        yield c


def _png(size=(800, 600), color=(10, 120, 200)) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", size, color).save(buf, format="PNG")
    return buf.getvalue()


def _make_page(name: str = "test-upload-page") -> None:
    canvas = {"default": [{"type": "div", "props": {
        "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
        "children": []}}]}
    bitmap = pages_mod_render(canvas)
    pages_mod.upsert_page(name, canvas, 10, 3, bitmap)


def pages_mod_render(canvas):
    from youn_server.canvas_render import render_canvas_to_bitmap
    return render_canvas_to_bitmap(canvas)


def _page_source(name: str):
    for s in pages_mod.list_pages():
        if s.name == name:
            return s
    return None


def _schedule_md5_of(client, name: str) -> str:
    """Which bitmap the device is currently pointed at for this page."""
    for p in client.get("/api/pages/schedule").json()["pages"]:
        if p["name"] == name:
            return p["md5"]
    raise AssertionError(f"{name} is not in the schedule")


def test_upload_without_a_page_is_refused(client):
    before = {s.name for s in pages_mod.list_pages()}
    r = client.post("/api/uploads", files={"image": ("a.png", _png(), "image/png")})
    assert r.status_code == 400
    assert {s.name for s in pages_mod.list_pages()} == before


def test_upload_to_an_unknown_page_lists_the_pages_you_could_have_meant(client):
    _make_page("test-upload-known")
    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "no-such-page"})
    assert r.status_code == 400
    assert "test-upload-known" in r.json()["detail"]["pages"]


def test_upload_replaces_the_picture_but_not_the_page_identity(client):
    _make_page("test-upload-target")
    before = _page_source("test-upload-target")
    md5_before = _schedule_md5_of(client, "test-upload-target")

    r = client.post("/api/uploads",
                    files={"image": ("a.png", _png(), "image/png")},
                    data={"page": "test-upload-target"})
    assert r.status_code == 200
    body = r.json()
    assert body["page"] == "test-upload-target"
    assert body["duration_minutes"] == before.duration_minutes
    assert body["order"] == before.order

    after = _page_source("test-upload-target")
    # Identity untouched, content swapped.
    assert (after.duration_minutes, after.order, after.name) == \
           (before.duration_minutes, before.order, before.name)
    assert after.canvas_json != before.canvas_json
    src = after.canvas_json["default"][0]["props"]["children"][0]["props"]["src"]
    assert src.startswith("uploads://")

    # The page's new bitmap is a real 30000-byte frame, and the schedule now
    # points the slot at it.
    assert len(pages_mod.get_bitmap(body["md5"])) == 30000
    assert body["md5"] != md5_before
    assert _schedule_md5_of(client, "test-upload-target") == body["md5"]


def test_two_uploads_to_one_page_do_not_create_a_second_page(client):
    _make_page("test-upload-twice")
    n0 = len(pages_mod.list_pages())
    for _ in range(2):
        r = client.post("/api/uploads",
                        files={"image": ("a.png", _png(), "image/png")},
                        data={"page": "test-upload-twice"})
        assert r.status_code == 200
    assert len(pages_mod.list_pages()) == n0


def test_bytes_that_are_not_an_image_leave_the_page_alone(client):
    _make_page("test-upload-bad")
    before = _page_source("test-upload-bad")
    r = client.post("/api/uploads",
                    files={"image": ("a.png", b"not an image", "image/png")},
                    data={"page": "test-upload-bad"})
    assert r.status_code == 400
    after = _page_source("test-upload-bad")
    assert after.canvas_json == before.canvas_json
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_uploads_api.py -q`
Expected: FAIL —— `/api/uploads` 返回 404（端点不存在）。

- [ ] **Step 3: 实现端点**

在 `server/youn_server/app.py` 的页面端点之后追加：

```python
    # ── upload → page binding ────────────────────────────────────────
    _UPLOAD_ID_RE = re.compile(r"[0-9a-f]{32}")
    _UPLOAD_MAX_PIXELS = 4000 * 4000

    def _upload_paths(upload_id: str) -> tuple[Path, Path]:
        if not _UPLOAD_ID_RE.fullmatch(upload_id):
            raise ValueError(f"invalid upload id: {upload_id!r}")
        return (settings.uploads_dir / f"{upload_id}.png",
                settings.uploads_dir / f"{upload_id}.src.png")

    # Constants are local: Task 3 deletes image_conv.py, which the scalar
    # aliases in this file's neighbours came from.
    PANEL_WIDTH = 400
    PANEL_HEIGHT = 300

    def _normalize_upload(data: bytes) -> bytes:
        """Fit the picture inside the panel, letterboxed on white.

        The renderer never upscales (`min(box/iw, 1.0)`), so the stored file has
        to be the size the page will draw: anything larger would be scaled down
        again at render time, anything smaller would sit tiny in the middle.
        """
        img = Image.open(io.BytesIO(data))
        img.load()
        if img.width * img.height > _UPLOAD_MAX_PIXELS:
            raise ValueError(f"image too large: {img.width}x{img.height}")
        if img.mode not in ("RGB", "L"):
            img = img.convert("RGB")
        scale = min(PANEL_WIDTH / img.width, PANEL_HEIGHT / img.height, 1.0)
        nw, nh = max(1, int(img.width * scale)), max(1, int(img.height * scale))
        img = img.resize((nw, nh), Image.Resampling.LANCZOS)
        sheet = Image.new("RGB", (PANEL_WIDTH, PANEL_HEIGHT), (255, 255, 255))
        sheet.paste(img, ((PANEL_WIDTH - nw) // 2, (PANEL_HEIGHT - nh) // 2))
        out = io.BytesIO()
        sheet.save(out, format="PNG")
        return out.getvalue()

    def _canvas_for_upload(upload_id: str) -> dict:
        """One contain-fitted image. The renderer centres and letterboxes on its
        own; `tw` is the same dialect the existing pages use."""
        return {"default": [{"type": "div", "props": {
            "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
            "children": [{"type": "img",
                          "props": {"src": f"uploads://{upload_id}"}}]}}]}

    @app.post("/api/uploads")
    async def upload_to_page(
        request: Request,
        image: UploadFile = File(...),
        page: str = Form(""),
    ) -> dict:
        _require_operator(request)

        page = page.strip()
        existing = [s.name for s in pages_mod.list_pages()]
        if not page or page not in existing:
            raise HTTPException(400, detail={
                "detail": "unknown page: uploads must name a page to replace",
                "pages": existing,
            })

        data = await image.read()
        if len(data) > 25 * 1024 * 1024:
            raise HTTPException(400, "image too large")
        try:
            normalized = _normalize_upload(data)
        except Exception as e:  # noqa: BLE001
            raise HTTPException(400, f"not a usable image: {e}") from e

        source = next(s for s in pages_mod.list_pages() if s.name == page)
        upload_id = secrets.token_hex(16)
        settings.uploads_dir.mkdir(parents=True, exist_ok=True)
        norm_path, orig_path = _upload_paths(upload_id)
        orig_path.write_bytes(data)          # keep the original for re-cropping
        norm_path.write_bytes(normalized)    # what the canvas will draw

        canvas_json = _canvas_for_upload(upload_id)
        try:
            bitmap = render_canvas_to_bitmap(canvas_json)
        except RenderError as e:
            raise HTTPException(400, f"render failed: {e.path}: {e.message}") from e
        entry = pages_mod.upsert_page(page, canvas_json, source.duration_minutes,
                                      source.order, bitmap)
        log.info("upload bound page=%s md5=%s upload=%s", page, entry.md5, upload_id)
        # PageEntry.to_dict() yields md5/duration_minutes/order/name; the caller
        # asked in terms of a page, so say `page` as well.
        return {"page": page, **entry.to_dict()}
```

补齐该文件顶部缺失的导入：`import io`、`import re`、`import secrets`、`from pathlib import Path`、`from PIL import Image`、`from youn_server.canvas_render import render_canvas_to_bitmap, RenderError`。

（`File`/`Form`/`UploadFile`/`HTTPException`/`pages_mod`/`settings`/`log` 在该文件已存在，按现状复用。）

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_uploads_api.py -q`
Expected: PASS

- [ ] **Step 5: 跑全量套件确认没有回归**

Run: `cd server && ./.venv/bin/python -m pytest tests -q`
Expected: PASS（新增用例之外全部保持通过）

- [ ] **Step 6: Commit**

```bash
git add server/youn_server/app.py server/tests/test_uploads_api.py
git commit -m "feat(server): uploads must name the page they replace"
```

---

### Task 3: 删掉断掉的图片推送路径

**Files:**
- Modify: `server/youn_server/app.py`（删 `/api/images` 三件套、`/api/push_image` 别名、`_image_path`、`_image_meta_path`、`_push_image_to_device`、`_device_can_push` 若只被它们使用）
- Modify: `server/youn_server/session.py`（删 `pending_pushes` 与 `LIST_IMAGES` 分派）
- Modify: `server/youn_server/protocol.py`（删 `IMAGE_PUSH_META`/`IMAGE_PUSH_DONE` 与 `MsgType.LIST_IMAGES`）
- Delete: `server/youn_server/image_conv.py`
- Test: `server/tests/test_uploads_api.py`（追加回归护栏）

**Interfaces:**
- Consumes: 无（纯删除）
- Produces: `POST /api/images` 等返回 404；`image_conv` 模块不存在

- [ ] **Step 1: 写回归测试**

追加到 `server/tests/test_uploads_api.py`：

```python
@pytest.mark.parametrize("method,path", [
    ("post", "/api/images"),
    ("get", "/api/images"),
    ("delete", "/api/images/deadbeef"),
    ("post", "/api/push_image"),
])
def test_the_websocket_image_push_path_is_gone(client, method, path):
    """It could never reach the panel: the firmware opens no WS and has no
    handler for the push messages. If someone adds it back, this fails."""
    r = getattr(client, method)(path)
    assert r.status_code == 404
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_uploads_api.py -q -k push_path`
Expected: FAIL —— 端点仍在，返回 200/400/405 而不是 404。

- [ ] **Step 3: 删除服务端那条路径**

- 删掉 `app.py` 中 `# ── image upload + push ──` 整段（`/api/images`、`/api/images` 列表、`/api/images/{id}` 删除、`/api/push_image` 别名）以及 `# ── image storage helpers ──` 段里的 `_image_path`、`_image_meta_path`、`_device_can_push`、`_push_image_to_device`。
- 用 `grep -n "_image_path\|_image_meta_path\|_device_can_push\|_push_image_to_device\|push_image\|images_dir\|image_conv" server/youn_server/*.py` 确认删干净，并删掉 `settings.images_dir` 的引用（`config.py` 里的字段本身可留，标为未用）。
- `session.py`：删 `pending_pushes` 计数与该模块里 `LIST_IMAGES` 的分支。
- `protocol.py`：删 `MsgType.LIST_IMAGES`、`OutMsg.IMAGE_PUSH_META`、`OutMsg.IMAGE_PUSH_DONE`。
- **不要删 `image_conv.py`**：它不是死的。`canvas_render.py:49` 从它导入 `SCREEN_W`/`SCREEN_H`、`PALETTE_BWRY` 与调色板索引，`tests/test_canvas_render.py:21,135` 也导入它。计划原先的判断（"只被 /api/images 使用"）是错的，实现者拒绝删除是对的。
- 更新 `README.md` 里描述已删上传接口的段落（`api/images` / `push_image`），使它反映现在唯一的路径 `POST /api/uploads`。
- `git rm -r --cached` 不适用；`data/images/` 是运行期目录，若被跟踪则一并删并加进 `.gitignore`。

- [ ] **Step 4: 跑全量套件确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests -q`
Expected: PASS（`test_schedule_api.py`、`test_preview.py` 等不应受影响；若有用例引用了被删符号，一并更新）

- [ ] **Step 5: Commit**

```bash
git add -A server/youn_server server/tests
git commit -m "chore(server): delete the websocket image push path"
```

---

### Task 4: Web 端改成「上传替换页面画面」

**Files:**
- Modify: `frontend/src/api.js`（`uploadImage` → `uploadToPage`，去掉 `images`/`deleteImage` 若不再使用）
- Modify: `frontend/src/pages/Images.jsx`（整页重写为上传卡片）
- Modify: `frontend/src/App.jsx:20`（导航标签）
- Test: 人工冒烟（本项目前端无自动化测试套件）

**Interfaces:**
- Consumes: `POST /api/uploads`（Task 2）、`api.pages()`（现有，返回页面列表）
- Produces: 无（终端 UI）

- [ ] **Step 1: 改 API 客户端**

`frontend/src/api.js`：删掉 `images`、`uploadImage`、`deleteImage`，新增：

```js
  uploadToPage: async (file, page) => {
    const fd = new FormData();
    fd.append('image', file);
    fd.append('page', page);
    const headers = {};
    const token = getToken();
    if (token) headers['X-Operator-Token'] = token;
    const res = await fetch(`${BASE}/uploads`, { method: 'POST', body: fd, headers });
    if (!res.ok) {
      let d = res.statusText;
      try {
        const body = await res.json();
        d = (body.detail && body.detail.detail) || body.detail || d;
        if (body.detail && body.detail.pages) d += `（可选页面：${body.detail.pages.join('、')}）`;
      } catch (e) {}
      throw new Error(d);
    }
    return res.json();
  },
```

- [ ] **Step 2: 重写上传页**

`frontend/src/pages/Images.jsx` 整体替换为：标题「替换页面画面」；一个「目标页面」下拉（`api.pages()`，值是页面名，必选，默认空且为空时禁用上传按钮）；文件选择 + 本地预览；上传按钮调 `api.uploadToPage(file, page)`；成功后提示

```
已替换页面「${page}」，设备将在下一轮轮询后更新（该页 md5 已变化）
```

并清空文件输入。错误直接显示 `e.message`。**不要**保留 format / 标题 / 目标设备 / 图片列表（图片不再是独立实体，页面列表在 `Pages.jsx`）。

- [ ] **Step 3: 改导航标签**

`frontend/src/App.jsx:20`：`<Link to="/images">图片推送</Link>` → `<Link to="/images">替换页面画面</Link>`。

- [ ] **Step 4: 构建并冒烟**

```bash
cd frontend && npm run build
cd ../server && ./start.sh restart && sleep 3 && curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:9002/
```

Expected: 构建无错误；首页 200。浏览器打开 `http://10.0.0.90:9002/`，进入「替换页面画面」：下拉里能看到现有页面名；不选页面时上传按钮不可用。

- [ ] **Step 5: Commit**

```bash
git add frontend/src/api.js frontend/src/pages/Images.jsx frontend/src/App.jsx
# frontend/dist is gitignored (.gitignore:31) - it is a build artifact, not source.
git commit -m "feat(web): upload replaces a page's picture, and must name the page"
```

---

### Task 5: 真机端到端

**Files:** 无源码改动（验证任务）

**Interfaces:**
- Consumes: 前四个任务的成果、运行中的服务端（`http://10.0.0.90:9002`）、设备 `NOTE4C-3400FC`（已配对、插电常醒）
- Produces: 一条证据链：上传 ⇒ 该页 md5 变化 ⇒ 设备拉新位图 ⇒ 屏上出现该图
- Token: every curl below sends `X-Operator-Token: $OPERATOR_TOKEN` — substitute the value of `OPERATOR_TOKEN` from `server/.env` (untracked; never paste the real value into a tracked file).

- [ ] **Step 1: 建一个一次性页面（不动用户现有页面）**

```bash
cd server && ./start.sh status
curl -s -X POST http://10.0.0.90:9002/api/pages \
  -H "X-Operator-Token: $OPERATOR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"plan-e2e","canvas_json":{"default":[{"type":"div","props":{"tw":"flex flex-col w-full h-full items-center justify-center bg-white","children":[{"type":"div","props":{"tw":"w-full h-full flex items-center justify-center bg-white","children":[{"type":"text","props":{"children":"before"}}]}}]}}]},"duration_minutes":10,"order":90}'
```

Expected: 200，返回 `{"md5": "...", ...}`。记下这个 md5（替换前的）。

- [ ] **Step 2: 上传一张图替换它**

```bash
python3 -c "
from PIL import Image
Image.new('RGB',(1200,900),(20,140,90)).save('/tmp/e2e.png')"
curl -s -X POST http://10.0.0.90:9002/api/uploads \
  -H "X-Operator-Token: $OPERATOR_TOKEN" \
  -F "page=plan-e2e" -F "image=@/tmp/e2e.png"
```

Expected: 200，`md5` 与 Step 1 不同，`order` 仍为 90。

- [ ] **Step 3: 确认服务端把它当成页面**

```bash
ls -l server/data/uploads/ | tail -3
curl -s http://10.0.0.90:9002/api/pages/schedule | python3 -c "
import json,sys; d=json.load(sys.stdin)
print([ (p['name'], p['md5']) for p in d['pages'] if p['name']=='plan-e2e' ])"
```

Expected: `data/uploads/` 下有一对 `{id}.png` 与 `{id}.src.png`；schedule 里 `plan-e2e` 的 md5 是新值。

- [ ] **Step 4: 等设备取图并上屏**

设备每次唤醒都会拉 schedule；新 md5 会触发下载。观察服务端日志：

```bash
grep -a "pages/bitmap" server/data/server.log | tail -3
```

Expected: 出现新 md5 的 `GET /api/pages/bitmap/{新 md5}.bin`。设备日志（串口）出现 `show page`，屏上出现那张绿图。

- [ ] **Step 5: 清理**

```bash
curl -s -X DELETE "http://10.0.0.90:9002/api/pages/plan-e2e" \
  -H "X-Operator-Token: $OPERATOR_TOKEN"
git add -A && git commit -m "chore: end-to-end verification notes" --allow-empty
```

Expected: 页面删除成功，schedule 回到用户原有的两页；上传文件成为孤儿（可接受，本次不做 GC）。

---

## Self-Review

**Spec coverage:** uploads 端点（Task 2）、`uploads://` 渲染分支（Task 1）、contain 归一化（Task 2 的 `_normalize_upload`，Global Constraints 里说明了为什么必须在上传时做而不是渲染时）、"必须绑定"的 400 + 可选页面列表（Task 2 Step 1/3）、页面身份不变（Task 2 断言 duration/order/name 不变，且 schedule 的 md5 换成新值）、删除断掉的推送路径（Task 3）、Web 端（Task 4）、设备侧取证（Task 5）。spec 的非目标（S3/CDN、上传即新建、立即显示）没有任务 —— 正确。

**Placeholder scan:** 无 TBD/TODO/"handle edge cases"。每个代码步骤都是可执行代码，包括测试与实现。

**Type consistency:** `upsert_page(name, canvas_json, duration_minutes, order, bitmap_bytes)` 与 `pages.py:141` 一致；`_normalize_upload` 返回 `bytes`、`_canvas_for_upload` 返回 `dict`；`PageEntry.to_dict()` 的键是 `md5/duration_minutes/order/name`（`pages.py:69`），端点显式补 `page` 键以匹配测试断言；`_schedule_md5_of` 依赖 schedule 条目里的 `name` 字段，该字段由 `PageEntry` 提供。

**一处与 spec 的偏差（有意）：** spec 说存 `{id}.{ext}`，计划改为 `{id}.png`（画板引用的归一化图，无扩展名歧义）+ `{id}.src.png`（原图，保留以便日后重新裁切）。原因：渲染器需要能凭 id 定址，而 `_render_img` 从不放大，所以渲染输入必须是归一化后的那张。

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-12-upload-to-page-binding.md`. Two execution options:

**1. Subagent-Driven (recommended)** — 每个任务派一个全新 subagent，任务之间我来评审

**2. Inline Execution** — 在本会话里按 `executing-plans` 批量执行，带检查点

Which approach?
