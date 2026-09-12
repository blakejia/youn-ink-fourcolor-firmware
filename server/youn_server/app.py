"""FastAPI app factory.

Builds a single ASGI app exposing:

  WebSocket  /ws                 device-side AI conversation endpoint
  GET        /api/health         liveness
  GET        /api/devices        device registry (operator)
  POST       /api/devices/{id}/approve
  POST       /api/devices/{id}/revoke
  POST       /api/ota            upload .bin + version + notes
  GET        /api/ota/check      latest firmware metadata
  GET        /api/ota/download/{filename}  signed download

All HTTP routes require an operator token via header `X-Operator-Token` when
OPERATOR_TOKEN is set in env. WS handshake requires trust=1 in the device
registry, or a one-time pairing secret via `?secret=<hex>` query parameter.
"""
from __future__ import annotations

import asyncio
import base64
import os
import io
import json
import logging
import re
import secrets
import time
from dataclasses import asdict
from pathlib import Path
from typing import Optional

from datetime import datetime
from zoneinfo import ZoneInfo

from fastapi import (
    Body,
    FastAPI,
    File,
    Form,
    HTTPException,
    Query,
    Request,
    UploadFile,
    WebSocket,
    WebSocketDisconnect,
    status,
)
from pydantic import BaseModel
from PIL import Image
from fastapi.responses import FileResponse, JSONResponse
from fastapi.responses import Response

from .canvas_render import RenderError, render_canvas_to_bitmap, render_canvas_to_png
from . import pairing as pairing_mod

from . import notify_store as ns

from . import pages as pages_mod

from . import ota as ota_mod
from . import protocol as P
from .ai import LLMClient, TTSClient, WhisperClient
from .config import settings, setup_logging
from .devices import Device, registry
from .session import Session, SessionManager

log = logging.getLogger(__name__)


# ── operator auth (HTTP only) ─────────────────────────────────────────
_OPERATOR_TOKEN = secrets.compare_digest  # type: ignore[attr-defined]


def _operator_token() -> Optional[str]:
    """Read operator token: environment first (test override), then .env settings."""
    return os.environ.get("OPERATOR_TOKEN") or settings.operator_token or None


def _require_operator(request: Request) -> None:
    expected = _operator_token()
    if not expected:
        return  # operator API disabled if no token configured
    provided = request.headers.get("X-Operator-Token", "")
    if not secrets.compare_digest(provided.encode(), expected.encode()):
        raise HTTPException(status_code=401, detail="bad operator token")

# ── pairing (device auth) ───────────────────────────────────────────
_pairing_store = pairing_mod.PairingStore(settings.devices_db)


class _PairStartBody(BaseModel):
    device_id: str
    board_type: str = "unknown"


class _PairConfirmBody(BaseModel):
    device_id: str
    code: str


class _PairClaimBody(BaseModel):
    device_id: str
    code: str


def _require_device_token(request: Request) -> "Device":
    """Validate Authorization: Bearer <token> against device_secrets + trust.

    Returns the authenticated ``Device`` on success so callers that need
    to cross-check the request's ``device_id`` against the token holder
    can do so without re-querying the registry.
    """
    auth = request.headers.get("Authorization", "")
    if not auth.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="unauthorized")
    token = auth[7:]
    dev = registry.get_device_by_token(token)
    if dev is None or not dev.trusted:
        raise HTTPException(status_code=401, detail="unauthorized")
    return dev



# ── app factory ───────────────────────────────────────────────────────
def create_app() -> FastAPI:
    setup_logging()
    if not settings.master_key:
        log.error(
            "MASTER_KEY is empty: all pair-start requests will be rejected "
            "(set MASTER_KEY in .env; generate with secrets.token_urlsafe(32))"
        )
    if not _operator_token():
        log.error(
            "OPERATOR_TOKEN is empty: every operator endpoint (/api/devices, "
            "/api/pages, /api/ota, /api/notifications, /mcp) is "
            "OPEN to anyone who can reach this port. Set OPERATOR_TOKEN in .env."
        )
    app = FastAPI(
        title="Youn Ink Server",
        version="1.0.0",
        description="AI + OTA for Youn Ink NOTE4C / 4-color EPD",
    )

    # AI clients are app-scoped. For high-load deployments, replace with a pool.
    app.state.whisper = WhisperClient()
    app.state.llm = LLMClient()
    app.state.tts = TTSClient()
    app.state.sessions = SessionManager(app.state.whisper, app.state.llm, app.state.tts)

    # Hook the OTA handler into the session manager.
    async def _ota_handler(session: Session, _msg: dict) -> None:
        meta = ota_mod.latest()
        if meta is None:
            return
        await session.send_json(P.ok(
            P.OutMsg.OTA_AVAILABLE,
            version=meta.version,
            size=meta.size,
            sha256=meta.sha256,
            url=f"{settings.public_http_base}/api/ota/download/{meta.filename}",
        ))

    app.state.sessions._ota_handler = _ota_handler  # type: ignore[attr-defined]

    # ── HTTP routes ──
    @app.get("/api/health")
    async def health() -> dict:
        return {
            "status": "ok",
            "ts": int(time.time()),
            "sessions": len(app.state.sessions.all_sessions()),
        }

    @app.get("/api/devices")
    async def list_devices(request: Request) -> dict:
        _require_operator(request)
        return {"devices": [d.__dict__ for d in registry.list_all()]}

    @app.post("/api/devices/{device_id}/approve")
    async def approve_device(device_id: str, request: Request) -> dict:
        _require_operator(request)
        if not registry.approve(device_id):
            raise HTTPException(404, "unknown device")
        return {"device_id": device_id, "trust": True}

    @app.post("/api/devices/{device_id}/revoke")
    async def revoke_device(device_id: str, request: Request) -> dict:
        _require_operator(request)
        if not registry.revoke(device_id):
            raise HTTPException(404, "unknown device")
        return {"device_id": device_id, "trust": False}

    # ── device pairing ──
    @app.post("/api/devices/pair-start")
    async def pair_start(body: _PairStartBody, request: Request) -> dict:
        # No MASTER_KEY configured -> reject every pair-start (the startup
        # log.error explains why). Checked before header presence so an
        # unconfigured server never hints at the expected request shape.
        if not settings.master_key:
            raise HTTPException(401, detail="device authentication failed")

        mac = request.headers.get("X-Device-Mac", "")
        ts_str = request.headers.get("X-Device-Timestamp", "")
        nonce = request.headers.get("X-Device-Nonce", "")
        sig = request.headers.get("X-Device-Signature", "")

        if not (mac and ts_str and sig):
            raise HTTPException(400, detail="missing device auth headers")
        try:
            timestamp = int(ts_str)
        except ValueError:
            raise HTTPException(400, detail="invalid timestamp")

        # HMAC signature over MAC(6) || timestamp || nonce; also enforces
        # the ±30s time window and the 5-min nonce replay cache.
        if not _pairing_store.verify_device_signature(
            body.device_id, mac, timestamp, nonce, sig
        ):
            raise HTTPException(401, detail="device authentication failed")

        # Whitelist checked only after authentication so its existence is
        # not revealed to unauthenticated callers.
        if not _pairing_store.check_whitelist(body.device_id):
            raise HTTPException(401, detail="device not in whitelist")

        ip = request.client.host if request.client else "unknown"
        if not _pairing_store.check_rate_limit(ip):
            # Retry-After 让客户端知道要退避多久；此前没有该头，
            # 固件收到 429 就立即重发 → 打点循环。
            raise HTTPException(
                429, detail="rate limited, try again later",
                headers={"Retry-After": str(pairing_mod.RATE_LIMIT_WINDOW)},
            )
        # Upsert device if new
        registry.upsert(body.device_id, body.board_type)
        code, expires_in = _pairing_store.create_session(body.device_id)
        return {"code": code, "expires_in": expires_in}

    @app.post("/api/devices/pair-confirm")
    async def pair_confirm(body: _PairConfirmBody, request: Request) -> dict:
        _require_operator(request)
        if not _pairing_store.confirm_session(body.device_id, body.code):
            raise HTTPException(401, detail="invalid or expired code")
        return {"status": "ready"}

    @app.get("/api/devices/pair-pending")
    async def pair_pending(request: Request) -> dict:
        """Operator view: sessions waiting for user confirmation."""
        _require_operator(request)
        return {"sessions": _pairing_store.list_pending()}

    @app.post("/api/devices/pair-claim")
    async def pair_claim(body: _PairClaimBody, request: Request) -> dict:
        # Valid code, user hasn't confirmed yet → pending (200, no token).
        # Checked before lockout/failure accounting so normal 2s polling
        # while the user reads the code never trips the claim lockout.
        if _pairing_store.is_pending(body.device_id, body.code):
            return {"status": "pending"}
        if _pairing_store.is_claim_locked(body.device_id):
            raise HTTPException(
                429, detail="too many failed attempts, try again later",
                headers={"Retry-After": str(pairing_mod.CONFIRM_LOCKOUT_SECONDS)},
            )
        token = _pairing_store.claim_session(body.device_id, body.code)
        if token is None:
            _pairing_store.record_claim_failure(body.device_id)
            raise HTTPException(401, detail="invalid or expired code")
        # Write token and mark device trusted
        registry.set_token(body.device_id, token)
        registry.approve(body.device_id)
        return {"token": token}


    # ── OTA ──
    @app.post("/api/ota")
    async def upload_ota(
        request: Request,
        firmware: UploadFile = File(...),
        version: str = Form(...),
        channel: str = Form("stable"),
        notes: str = Form(""),
    ) -> dict:
        _require_operator(request)
        data = await firmware.read()
        if len(data) > 32 * 1024 * 1024:
            raise HTTPException(413, "firmware too large")
        meta = ota_mod.save_firmware(data, version=version, channel=channel, notes=notes)
        return meta.to_json()

    @app.get("/api/ota")
    async def ota_meta(request: Request) -> dict:
        """Operator view: current firmware metadata."""
        _require_operator(request)
        meta = ota_mod.latest("stable")
        if meta is None:
            return {"available": False}
        return {"available": True, **meta.to_json()}

    @app.get("/api/ota/check")
    async def ota_check(request: Request, channel: str = Query("stable")) -> dict:
        _require_device_token(request)
        meta = ota_mod.latest(channel)
        if meta is None:
            return {"available": False}
        return {
            "available": True,
            "version": meta.version,
            "size": meta.size,
            "sha256": meta.sha256,
            "filename": meta.filename,
            "url": f"{settings.public_http_base}/api/ota/download/{meta.filename}",
        }

    @app.get("/api/ota/download/{filename}")
    async def ota_download(filename: str, request: Request) -> FileResponse:
        _require_device_token(request)
        p = ota_mod.get_file_path(filename)
        if p is None:
            raise HTTPException(404, "unknown firmware")
        meta = ota_mod.latest()
        if meta is None or meta.filename != filename:
            # Allow historical downloads too — but require file existence check.
            pass
        # Re-verify integrity on every download so a tampered .bin can't slip out.
        if not ota_mod.verify_signature(p):
            raise HTTPException(500, "firmware signature mismatch")
        return FileResponse(p, filename=filename, media_type="application/octet-stream")

    @app.post("/api/pages/preview")
    async def preview_page(
        request: Request, body: dict = Body(...), debug: int = Query(0),
    ) -> Response:
        """Render canvas_json to a PNG preview (used by the web admin UI).

        ?debug=1 → JSON {png_b64, bounds} so the editor can overlay selection
        rectangles using the server-side layout (single source of truth).
        """
        _require_operator(request)
        canvas_json = body.get("canvas_json")
        if not isinstance(canvas_json, dict):
            raise HTTPException(400, "canvas_json must be an object")
        try:
            if debug:
                from .canvas_render import render_canvas_to_png_debug
                png, bounds = render_canvas_to_png_debug(canvas_json)
                return {"png_b64": base64.b64encode(png).decode("ascii"),
                        "bounds": bounds}
            png = render_canvas_to_png(canvas_json)
        except RenderError as e:
            raise HTTPException(400, f"render failed: {e.path}: {e.message}") from e
        except Exception as e:  # noqa: BLE001
            raise HTTPException(500, f"render failed: {e}") from e
        return Response(content=png, media_type="image/png")

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
        dev = _require_device_token(request)
        if dev.device_id != device_id:
            raise HTTPException(status_code=401, detail="device mismatch")
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
            ns.get_store().mark_error(n.id)
            raise HTTPException(500, "render failed")
        return {"bitmap_base64": base64.b64encode(bitmap).decode("ascii"),
                "notification": asdict(n)}

    @app.post("/api/notifications/{nid}/ack")
    async def ack_notification(nid: str, request: Request, body: dict = Body(...)):
        dev = _require_device_token(request)
        decision = body.get("decision")
        if decision not in ("agree", "reject"):
            raise HTTPException(400, "decision must be agree|reject")
        # 归属校验：next 端点有 device_id 交叉检查，ack 此前只验 token，
        # 任一受信设备可替任意通知回执。
        n = ns.get_store().get(nid)
        if n is not None and n.device_id != dev.device_id:
            raise HTTPException(403, "notification belongs to another device")
        n = ns.get_store().ack(nid, decision)
        if n is None:
            raise HTTPException(404, "notification not found")
        return {"status": n.status, "decision": n.decision}

    @app.get("/api/notifications/history")
    async def notification_history(request: Request, device_id: str = Query(""), limit: int = Query(20)):
        _require_operator(request)
        items = ns.get_store().recent(device_id=device_id, limit=limit)
        return {"notifications": [asdict(n) for n in items]}

    # ── Canvas Loop ──

    def _compute_screen_active() -> bool:
        tz = ZoneInfo(settings.canvas_timezone)
        now = datetime.now(tz)
        s = datetime.strptime(settings.canvas_sleep_start, "%H:%M").time()
        e = datetime.strptime(settings.canvas_sleep_end, "%H:%M").time()
        t = now.time()
        # Window crossing midnight is common: treat start <= t < end, with wrap.
        if s <= e:
            return not (s <= t < e)
        return not (t >= s or t < e)

    @app.post("/api/pages")
    async def create_page(
        request: Request,
        body: dict = Body(...),
    ) -> dict:
        _require_operator(request)
        name = str(body.get("name", "")).strip()
        canvas_json = body.get("canvas_json")
        duration_minutes = int(body.get("duration_minutes", 10))
        order = int(body.get("order", 0))
        if not name:
            raise HTTPException(400, "name required")
        if not isinstance(canvas_json, dict):
            raise HTTPException(400, "canvas_json must be an object")
        if duration_minutes < settings.canvas_min_page_duration_minutes:
            raise HTTPException(400,
                f"duration_minutes must be >= min_page_duration_minutes={settings.canvas_min_page_duration_minutes}")
        try:
            bitmap = render_canvas_to_bitmap(canvas_json)
        except RenderError as e:
            raise HTTPException(400, f"render failed: {e.path}: {e.message}") from e
        except Exception as e:  # noqa: BLE001
            raise HTTPException(500, f"render failed: {e}") from e
        entry = pages_mod.upsert_page(name, canvas_json, duration_minutes, order, bitmap)
        return entry.to_dict()

    @app.get("/api/pages")
    async def list_pages(request: Request) -> dict:
        _require_operator(request)
        out = [s.to_dict() for s in pages_mod.list_pages()]
        return {"pages": out, "count": len(out)}

    @app.delete("/api/pages/{name}")
    async def delete_page(name: str, request: Request) -> dict:
        _require_operator(request)
        if not pages_mod.delete_page(name):
            raise HTTPException(404, "unknown page")
        return {"deleted": name}

    @app.get("/api/pages/schedule")
    async def get_schedule() -> dict:
        entries = pages_mod.build_schedule_from_disk()
        sched_md = pages_mod.compute_schedule_md(entries)
        current_index, seconds_until_next_page = pages_mod.schedule_position(entries, time.time())
        return {
            "schedule_md5": sched_md,
            "server_time": datetime.now(ZoneInfo(settings.canvas_timezone)).isoformat(),
            "current_index": current_index,
            "seconds_until_next_page": seconds_until_next_page,
            "policy": {
                "sleep_window": {
                    "start": settings.canvas_sleep_start,
                    "end": settings.canvas_sleep_end,
                    "tz": settings.canvas_timezone,
                },
                "poll_interval_minutes": settings.canvas_poll_interval_minutes,
                "sleep_poll_interval_minutes": settings.canvas_sleep_poll_interval_minutes,
                "min_page_duration_minutes": settings.canvas_min_page_duration_minutes,
            },
            "pages": [e.to_dict() for e in entries],
            "screen_active": _compute_screen_active(),
        }

    @app.get("/api/pages/bitmap/{md5}.bin")
    async def get_bitmap(md5: str) -> Response:
        data = pages_mod.get_bitmap(md5)
        if data is None:
            raise HTTPException(404, "unknown bitmap")
        return Response(
            content=data,
            media_type="application/octet-stream",
            headers={"Cache-Control": "public, max-age=31536000, immutable"},
        )

    # ── upload → page binding ────────────────────────────────────────
    _UPLOAD_ID_RE = re.compile(r"[0-9a-f]{32}")
    _UPLOAD_MAX_PIXELS = 4000 * 4000

    def _upload_paths(upload_id: str) -> tuple[Path, Path]:
        if not _UPLOAD_ID_RE.fullmatch(upload_id):
            raise ValueError(f"invalid upload id: {upload_id!r}")
        return (settings.uploads_dir / f"{upload_id}.png",
                settings.uploads_dir / f"{upload_id}.src.png")

    # Panel geometry, kept local so the upload path needs no extra import.
    PANEL_WIDTH = 400
    PANEL_HEIGHT = 300

    def _normalize_upload(data: bytes) -> bytes:
        """Fit the picture inside the panel, letterboxed on white.

        The renderer never upscales (`min(box/iw, 1.0)`), so the stored file has
        to be the size the page will draw: anything larger would be scaled down
        again at render time, anything smaller would sit tiny in the middle.
        """
        img = Image.open(io.BytesIO(data))
        # Dimensions are known after open(); check the cap before load() so a
        # huge PNG is rejected without decoding it into memory.
        if img.width * img.height > _UPLOAD_MAX_PIXELS:
            raise ValueError(f"image too large: {img.width}x{img.height}")
        img.load()
        if "A" in img.getbands() or "transparency" in img.info:
            # Composite alpha onto white: convert("RGB") alone would paint
            # transparent pixels black, contradicting the white letterbox.
            base = Image.new("RGBA", img.size, (255, 255, 255, 255))
            img = Image.alpha_composite(base, img.convert("RGBA")).convert("RGB")
        elif img.mode not in ("RGB", "L"):
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
        """One contain-fitted image, drawn at panel size.

        The renderer sizes a node from its own `w-[Npx]`/`h-[Npx]` tw token or
        its `style.width/height` — `w-full`/`h-full` are not parsed and an
        unsized img measures 0x0, which pastes a single pixel. So the img gets
        the same explicit `style` the existing pages use
        (`server/data/pages/logo-1024.json`). The stored file is already
        panel-sized and letterboxed on white, so this draws 1:1."""
        return {"default": [{"type": "div", "props": {
            "tw": "flex flex-col w-full h-full items-center justify-center bg-white",
            "children": [{"type": "img",
                          "props": {"src": f"uploads://{upload_id}",
                                    "style": {"width": f"{PANEL_WIDTH}px",
                                              "height": f"{PANEL_HEIGHT}px"}}}]}}]}

    @app.post("/api/uploads")
    async def upload_to_page(
        request: Request,
        image: UploadFile = File(...),
        page: str = Form(""),
    ) -> dict:
        _require_operator(request)

        page = page.strip()
        sources = pages_mod.list_pages()
        existing = [s.name for s in sources]
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

        # Same snapshot as the validation above: re-scanning here could lose the
        # page to a concurrent delete and raise StopIteration (a 500).
        source = next(s for s in sources if s.name == page)
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

    # ── WebSocket ──
    @app.websocket("/ws")
    async def ws_endpoint(websocket: WebSocket) -> None:
        await websocket.accept()
        # 1) Receive hello within 5s.
        try:
            hello_raw = await asyncio.wait_for(websocket.receive_text(), timeout=5.0)
        except (asyncio.TimeoutError, WebSocketDisconnect):
            await websocket.close()
            return
        try:
            hello = json.loads(hello_raw)
        except ValueError:
            await websocket.send_text(json.dumps(P.ok(P.OutMsg.ERROR, message="bad hello")))
            await websocket.close()
            return

        device_id = str(hello.get("deviceId", "")).strip()
        board_type = str(hello.get("boardType", "unknown")).strip()[:64]
        if not device_id:
            await websocket.send_text(json.dumps(P.ok(P.OutMsg.ERROR, message="missing deviceId")))
            await websocket.close()
            return

        dev = registry.upsert(
            device_id, board_type,
            ws_session_id="",  # set after Session is created
            ip_address=websocket.client.host if websocket.client else None,
        )

        # 2) Auth via Bearer token in WebSocket handshake headers.
        #    The token must belong to *this* device_id: `dev.trusted` alone is
        #    not sufficient — device_id comes straight from the client's hello,
        #    so trusting it would let anyone who knows a paired device_id
        #    impersonate that device without a token.
        auth_header = websocket.headers.get("authorization", "")
        token = ""
        if auth_header.startswith("Bearer "):
            token = auth_header[7:]
        dev_by_token = registry.get_device_by_token(token) if token else None
        authorized = (
            dev_by_token is not None
            and dev_by_token.trusted
            and dev_by_token.device_id == device_id
        )
        if not authorized:
            await websocket.send_text(json.dumps(P.ok(
                P.OutMsg.ERROR, message="device not trusted; pair via operator API",
            )))
            await websocket.close()
            log.warning(
                "ws refused device=%s (no valid token bound to this device_id)",
                device_id,
            )
            return

        # 3) Hand off to session manager.
        session = Session(ws=websocket, device_id=device_id, board_type=board_type)
        registry.upsert(
            device_id, board_type,
            ws_session_id=session.session_id,
            ip_address=websocket.client.host if websocket.client else None,
        )
        await app.state.sessions.register(session)
        # Handshake response: the inbound hello was consumed above for auth,
        # so hello_ack + server_ready must be sent here — session.handle_text
        # will never see the first hello again.
        await session.send_json(P.ok(
            P.OutMsg.HELLO_ACK,
            session_id=session.session_id,
            server_version="1.0.0",
            transport="ws",
        ))
        await session.send_json(P.ok(P.OutMsg.SERVER_READY))

        try:
            while True:
                msg = await websocket.receive()
                if msg.get("type") == "websocket.disconnect":
                    break
                if "text" in msg and msg["text"] is not None:
                    await app.state.sessions.handle_text(session, msg["text"])
                elif "bytes" in msg and msg["bytes"] is not None:
                    await app.state.sessions.handle_binary(session, msg["bytes"])
        except WebSocketDisconnect:
            pass
        except Exception as e:  # noqa: BLE001
            log.exception("ws loop error device=%s: %s", device_id, e)
        finally:
            session.closed = True
            await app.state.sessions.unregister(session.session_id)
    # ── Web admin UI (Vite build output) ──
    # Explicit index routes (NOT a "/" StaticFiles mount): the MCP root mount
    # below matches every path, so a second "/" mount would be dead code.
    # Keep this list in sync with frontend/src/App.jsx <Route> paths.
    dist_dir = Path(__file__).resolve().parents[2] / "frontend" / "dist"
    if dist_dir.exists() and (dist_dir / "index.html").exists():
        from fastapi.responses import FileResponse
        from fastapi.staticfiles import StaticFiles

        app.mount("/assets", StaticFiles(directory=str(dist_dir / "assets")), name="spa-assets")
        for _spa_path in ("/", "/login", "/devices", "/pages", "/images", "/ota"):
            app.get(_spa_path, response_class=FileResponse, include_in_schema=False)(
                lambda _p=_spa_path: FileResponse(str(dist_dir / "index.html"))
            )
        log.info("web admin UI mounted from %s", dist_dir)
    else:
        log.warning("frontend/dist not found (%s); web admin UI disabled", dist_dir)

    # ── FastMCP on /mcp (streamable-http), mounted LAST ──
    # Sub-app routes already live at /mcp; mounting at "/mcp" would double
    # them to /mcp/mcp. A root mount matches every path, so it MUST stay
    # after ALL explicit routes above (API + SPA index routes).
    try:
        from .mcp_server import mcp as mcp_server
        # sub-app routes are already at /mcp; mount at root to avoid double-mount
        mcp_subapp = mcp_server.http_app(transport="streamable-http")
        app.mount("/", mcp_subapp)
        # MCP tools (push/list/ack notifications) have no auth of their own and
        # the root mount makes them world-reachable. Gate /mcp on the operator
        # token: clients must send `X-Operator-Token`. If no token is configured
        # the mount stays open (same fail-open semantics as the HTTP API).
        @app.middleware("http")
        async def _mcp_operator_gate(request: Request, call_next):
            path = request.url.path
            if path == "/mcp" or path.startswith("/mcp/"):
                expected = _operator_token()
                if expected:
                    provided = request.headers.get("X-Operator-Token", "")
                    if not secrets.compare_digest(provided.encode(), expected.encode()):
                        return JSONResponse(
                            {"detail": "bad operator token"}, status_code=401
                        )
            return await call_next(request)
        # Compose lifespans so we don't clobber any existing lifespan
        # (or legacy on_event handlers) that may be added later.
        from contextlib import asynccontextmanager
        _original_lifespan = app.router.lifespan_context

        @asynccontextmanager
        async def _composed_lifespan(app):
            async with _original_lifespan(app):
                async with mcp_subapp.lifespan(app):
                    yield

        app.router.lifespan_context = _composed_lifespan
    except ImportError:
        log.warning("fastmcp not installed; /mcp endpoint disabled")
    return app


# Convenience handle for `uvicorn youn_server.app:app --factory`.
app = create_app()


async def _shutdown_event() -> None:
    """Best-effort cleanup of httpx clients on FastAPI shutdown."""
    pass  # placeholder; uvicorn handles asyncio cleanup on signal


def install_shutdown_handlers(app: FastAPI) -> None:
    @app.on_event("shutdown")
    async def _close_clients() -> None:  # pragma: no cover
        for attr in ("whisper", "llm", "tts"):
            client = getattr(app.state, attr, None)
            if client and hasattr(client, "aclose"):
                try:
                    await client.aclose()
                except Exception:  # noqa: BLE001
                    pass


install_shutdown_handlers(app)
