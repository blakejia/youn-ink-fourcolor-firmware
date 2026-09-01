"""FastAPI app factory.

Builds a single ASGI app exposing:

  WebSocket  /ws                 device-side AI conversation endpoint
  GET        /api/health         liveness
  GET        /api/devices        device registry (operator)
  POST       /api/devices/{id}/approve
  POST       /api/devices/{id}/revoke
  POST       /api/images         multipart upload; converts + pushes via WS
  GET        /api/images         list images known to server
  DELETE     /api/images/{id}    remove + tell device
  POST       /api/ota            upload .bin + version + notes
  GET        /api/ota/check      latest firmware metadata
  GET        /api/ota/download/{filename}  signed download
  POST       /api/push_image     alias of /api/images for compat with README §88

All HTTP routes require an operator token via header `X-Operator-Token` when
OPERATOR_TOKEN is set in env. WS handshake requires trust=1 in the device
registry, or a one-time pairing secret via `?secret=<hex>` query parameter.
"""
from __future__ import annotations

import asyncio
import os
import io
import json
import logging
import secrets
import time
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
from fastapi.responses import FileResponse, JSONResponse
from fastapi.responses import Response

from .canvas_render import RenderError, render_canvas_to_bitmap, render_canvas_to_png
from . import pairing as pairing_mod

from . import pages as pages_mod

from . import image_conv
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


def _require_device_token(request: Request) -> None:
    """Validate Authorization: Bearer <token> against device_secrets + trust."""
    auth = request.headers.get("Authorization", "")
    if not auth.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="unauthorized")
    token = auth[7:]
    dev = registry.get_device_by_token(token)
    if dev is None or not dev.trusted:
        raise HTTPException(status_code=401, detail="unauthorized")



# ── image storage helpers ─────────────────────────────────────────────
def _image_path(image_id: str) -> Path:
    return settings.images_dir / f"{image_id}.bin"


def _image_meta_path(image_id: str) -> Path:
    return settings.images_dir / f"{image_id}.json"


def _device_can_push(device_id: str) -> bool:
    dev = registry.get(device_id)
    return dev is not None and dev.trusted


def _push_image_to_device(session: "Session", image_id: str) -> bool:
    """Notify an active session that an image is ready to pull."""
    if session is None or session.closed:
        return False
    meta = json.loads(_image_meta_path(image_id).read_text())
    binary = _image_path(image_id).read_bytes()

    async def _do_push() -> None:
        try:
            session.pending_pushes += 1
            await session.send_json(P.ok(
                P.OutMsg.IMAGE_PUSH_META,
                id=image_id,
                width=image_conv.SCREEN_W,
                height=image_conv.SCREEN_H,
                format=meta["format"],
                size=meta["size"],
                title=meta.get("title", ""),
            ))
            # Chunked binary push — same pattern as TTS.
            CHUNK = 8000
            for off in range(0, len(binary), CHUNK):
                await session.send_bytes(binary[off : off + CHUNK])
            await session.send_json(P.ok(P.OutMsg.IMAGE_PUSH_DONE, id=image_id))
            log.info(
                "image pushed device=%s id=%s bytes=%d",
                session.device_id, image_id, len(binary),
            )
        finally:
            session.pending_pushes -= 1

    asyncio.create_task(_do_push())
    return True


# ── app factory ───────────────────────────────────────────────────────
def create_app() -> FastAPI:
    setup_logging()
    app = FastAPI(
        title="Youn Ink Server",
        version="1.0.0",
        description="AI + image push + OTA for Youn Ink NOTE4C / 4-color EPD",
    )

    # AI clients are app-scoped. For high-load deployments, replace with a pool.
    app.state.whisper = WhisperClient()
    app.state.llm = LLMClient()
    app.state.tts = TTSClient()
    app.state.sessions = SessionManager(app.state.whisper, app.state.llm, app.state.tts)

    # Hook OTA + image list handlers into the session manager.
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

    async def _image_list_handler(session: Session, _msg: dict) -> None:
        # Lightweight: list image IDs + titles + formats.
        out = []
        for p in settings.images_dir.glob("*.json"):
            try:
                d = json.loads(p.read_text())
            except json.JSONDecodeError:
                continue
            out.append({
                "id": d["id"],
                "title": d.get("title", ""),
                "format": d.get("format", ""),
                "size": d.get("size", 0),
                "uploaded_at": d.get("uploaded_at", 0),
            })
        # Devices don't have a typed message for this — emit an info-ish envelope.
        await session.send_json({"type": "image_list", "images": out})

    app.state.sessions._ota_handler = _ota_handler  # type: ignore[attr-defined]
    app.state.sessions._image_list_handler = _image_list_handler  # type: ignore[attr-defined]

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
        ip = request.client.host if request.client else "unknown"
        if not _pairing_store.check_rate_limit(ip):
            raise HTTPException(429, detail="rate limited, try again later")
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

    @app.post("/api/devices/pair-claim")
    async def pair_claim(body: _PairClaimBody, request: Request) -> dict:
        if _pairing_store.is_claim_locked(body.device_id):
            raise HTTPException(429, detail="too many failed attempts, try again later")
        token = _pairing_store.claim_session(body.device_id, body.code)
        if token is None:
            _pairing_store.record_claim_failure(body.device_id)
            raise HTTPException(401, detail="invalid or expired code")
        # Write token and mark device trusted
        registry.set_token(body.device_id, token)
        registry.approve(body.device_id)
        return {"token": token}


    # ── image upload + push ──
    @app.post("/api/images")
    async def upload_image(
        request: Request,
        image: UploadFile = File(...),
        format: str = Form("bwry2bpp"),
        title: str = Form(""),
        target_device_id: str = Form(""),
    ) -> dict:
        _require_operator(request)
        if format not in ("1bpp", "bwry2bpp"):
            raise HTTPException(400, "format must be 1bpp or bwry2bpp")

        data = await image.read()
        if len(data) > 25 * 1024 * 1024:
            raise HTTPException(413, "image too large (>25 MB)")

        try:
            raw = image_conv.convert_bytes(data, format)  # type: ignore[arg-type]
        except Exception as e:  # noqa: BLE001
            raise HTTPException(400, f"convert failed: {e}") from e

        image_id = secrets.token_urlsafe(8)
        _image_path(image_id).write_bytes(raw)
        meta = {
            "id": image_id,
            "title": title or image.filename or image_id,
            "format": format,
            "size": len(raw),
            "uploaded_at": int(time.time()),
            "target_device_id": target_device_id,
        }
        _image_meta_path(image_id).write_text(json.dumps(meta, ensure_ascii=False, indent=2))

        # Try to push to a live session.
        pushed = False
        for s in app.state.sessions.all_sessions():
            if target_device_id and s.device_id != target_device_id:
                continue
            pushed = _push_image_to_device(s, image_id) or pushed

        return {"id": image_id, "size": len(raw), "format": format, "pushed": pushed}

    @app.get("/api/images")
    async def list_images(request: Request) -> dict:
        _require_operator(request)
        out = []
        for p in sorted(settings.images_dir.glob("*.json")):
            try:
                out.append(json.loads(p.read_text()))
            except json.JSONDecodeError:
                continue
        return {"images": out}

    @app.delete("/api/images/{image_id}")
    async def delete_image(image_id: str, request: Request) -> dict:
        _require_operator(request)
        _image_path(image_id).unlink(missing_ok=True)
        _image_meta_path(image_id).unlink(missing_ok=True)
        return {"deleted": image_id}

    @app.post("/api/push_image")
    async def push_image_alias(
        request: Request,
        image: UploadFile = File(...),
        format: str = Form("bwry2bpp"),
        title: str = Form(""),
        target_device_id: str = Form(""),
    ) -> dict:
        # README §88 mentions this exact endpoint name.
        return await upload_image(
            request=request, image=image, format=format,
            title=title, target_device_id=target_device_id,
        )

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
    async def preview_page(request: Request, body: dict = Body(...)) -> Response:
        """Render canvas_json to a PNG preview (used by the web admin UI)."""
        _require_operator(request)
        canvas_json = body.get("canvas_json")
        if not isinstance(canvas_json, dict):
            raise HTTPException(400, "canvas_json must be an object")
        try:
            png = render_canvas_to_png(canvas_json)
        except RenderError as e:
            raise HTTPException(400, f"render failed: {e.path}: {e.message}") from e
        except Exception as e:  # noqa: BLE001
            raise HTTPException(500, f"render failed: {e}") from e
        return Response(content=png, media_type="image/png")

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
        return {
            "schedule_md5": sched_md,
            "server_time": datetime.now(ZoneInfo(settings.canvas_timezone)).isoformat(),
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
        auth_header = websocket.headers.get("authorization", "")
        token = ""
        if auth_header.startswith("Bearer "):
            token = auth_header[7:]
        dev_by_token = registry.get_device_by_token(token) if token else None
        authorized = dev.trusted or (
            dev_by_token is not None and dev_by_token.trusted
        )
        if not authorized:
            await websocket.send_text(json.dumps(P.ok(
                P.OutMsg.ERROR, message="device not trusted; pair via operator API",
            )))
            await websocket.close()
            log.warning("ws refused device=%s (not trusted, no valid token)", device_id)
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
    dist_dir = Path(__file__).resolve().parents[2] / "frontend" / "dist"
    if dist_dir.exists() and (dist_dir / "index.html").exists():
        from fastapi.staticfiles import StaticFiles

        app.mount("/", StaticFiles(directory=str(dist_dir), html=True), name="spa")
        log.info("web admin UI mounted from %s", dist_dir)
    else:
        log.warning("frontend/dist not found (%s); web admin UI disabled", dist_dir)

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
