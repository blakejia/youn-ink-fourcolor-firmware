"""Per-WebSocket conversation orchestrator.

State machine for one device connection:

    IDLE
      └─ hello → SEND hello_ack, server_ready → IDLE
      └─ ptt_start → RECORDING
    RECORDING
      └─ binary frames (PCM) appended to in-memory buffer
      └─ ptt_stop → PROCESSING (lock further ptt_start until done)
    PROCESSING
      └─ ASR via Whisper
      └─ send asr_final
      └─ LLM stream via chat/completions
      └─ send cli_summary tokens incrementally + llm_done at end
      └─ TTS via audio/speech, resample 24k → 16k if needed
      └─ stream binary TTS frames; send tts_start/tts_end markers
      └─ IDLE
"""
from __future__ import annotations

import asyncio
import json
import logging
import time
import uuid
from dataclasses import dataclass, field
from typing import Optional

from fastapi import WebSocket

from . import protocol as P
from .ai import LLMClient, TTSClient, WhisperClient, resample_pcm_24k_to_16k
from .config import settings

log = logging.getLogger(__name__)


# System prompt is intentionally minimal — Chinese-first since the device
# firmware and project are Chinese-language.
DEFAULT_SYSTEM_PROMPT = (
    "你是 Youn Ink 墨水屏设备的语音助手。"
    "请用简短、自然、口语化的中文回答，适合在小型墨水屏上朗读。"
    "回答尽量控制在 60 字以内。"
)


@dataclass
class ChatMessage:
    role: str
    content: str


@dataclass
class Session:
    ws: WebSocket
    device_id: str
    board_type: str
    session_id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])
    state: str = "idle"  # idle | recording | processing
    pcm_buffer: bytearray = field(default_factory=bytearray)
    chat_history: list[ChatMessage] = field(default_factory=list)
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    last_activity_ms: int = field(default_factory=lambda: int(time.time() * 1000))
    closed: bool = False

    async def send_json(self, obj: dict) -> None:
        if self.closed:
            return
        await self.ws.send_text(json.dumps(obj, ensure_ascii=False))

    async def send_bytes(self, data: bytes) -> None:
        if self.closed:
            return
        await self.ws.send_bytes(data)


class SessionManager:
    """Tracks active sessions and runs the AI pipeline for each."""

    def __init__(self, ai_whisper: WhisperClient, ai_llm: LLMClient, ai_tts: TTSClient) -> None:
        self._asr = ai_whisper
        self._llm = ai_llm
        self._tts = ai_tts
        self._sessions: dict[str, Session] = {}
        self._lock = asyncio.Lock()

    async def register(self, session: Session) -> None:
        async with self._lock:
            self._sessions[session.session_id] = session
        log.info(
            "session open device=%s board=%s session=%s",
            session.device_id, session.board_type, session.session_id,
        )

    async def unregister(self, session_id: str) -> None:
        async with self._lock:
            s = self._sessions.pop(session_id, None)
        if s is not None:
            log.info("session close device=%s session=%s", s.device_id, s.session_id)

    def get(self, session_id: str) -> Optional[Session]:
        return self._sessions.get(session_id)

    def all_sessions(self) -> list[Session]:
        return list(self._sessions.values())

    # ── message router ──
    async def handle_text(self, session: Session, data: str) -> None:
        session.last_activity_ms = int(time.time() * 1000)
        try:
            msg = json.loads(data)
        except ValueError:
            await session.send_json(P.ok(P.OutMsg.ERROR, message="invalid json"))
            return

        mtype = msg.get("type", "")

        if mtype == P.MsgType.HELLO:
            await session.send_json(P.ok(
                P.OutMsg.HELLO_ACK,
                session_id=session.session_id,
                server_version="1.0.0",
                transport="ws",
            ))
            await session.send_json(P.ok(P.OutMsg.SERVER_READY))
            return

        if mtype == P.MsgType.PTT_START:
            if session.state != "idle":
                log.warning("ptt_start in state=%s device=%s", session.state, session.device_id)
            async with session.lock:
                session.state = "recording"
                session.pcm_buffer.clear()
            await session.send_json(P.ok(P.OutMsg.STATUS, status="recording"))
            return

        if mtype == P.MsgType.PTT_STOP:
            if session.state != "recording":
                log.warning("ptt_stop in state=%s device=%s", session.state, session.device_id)
                return
            asyncio.create_task(self._run_ppt_pipeline(session))
            return

        if mtype == P.MsgType.PONG:
            return

        if mtype == P.MsgType.OTA_CHECK:
            # Defer to OTA module — handler injected at app boot.
            handler = getattr(self, "_ota_handler", None)
            if handler:
                asyncio.create_task(handler(session, msg))
            return

        log.debug("unhandled inbound type=%s device=%s", mtype, session.device_id)

    async def handle_binary(self, session: Session, data: bytes) -> None:
        session.last_activity_ms = int(time.time() * 1000)
        if session.state != "recording":
            log.debug(
                "binary frame in state=%s len=%d device=%s",
                session.state, len(data), session.device_id,
            )
            return
        async with session.lock:
            session.pcm_buffer.extend(data)

    # ── the pipeline itself ──
    async def _run_ppt_pipeline(self, session: Session) -> None:
        async with session.lock:
            if session.state != "recording":
                return
            session.state = "processing"
            pcm = bytes(session.pcm_buffer)
            session.pcm_buffer.clear()

        await session.send_json(P.ok(P.OutMsg.STATUS, status="processing"))
        t0 = time.time()

        # 1. ASR
        try:
            text = await self._asr.transcribe_pcm(
                pcm,
                sample_rate=settings.device_sample_rate,
                sample_width=settings.device_sample_width,
                channels=settings.device_channels,
                language="zh",
            )
        except Exception as e:  # noqa: BLE001
            log.exception("ASR failed device=%s: %s", session.device_id, e)
            await session.send_json(P.ok(P.OutMsg.ERROR, message=f"asr: {e}"))
            async with session.lock:
                session.state = "idle"
            return

        text = (text or "").strip()
        if not text:
            log.info("ASR empty device=%s", session.device_id)
            await session.send_json(P.ok(P.OutMsg.ASR_FINAL, text=""))
            async with session.lock:
                session.state = "idle"
            return

        await session.send_json(P.ok(P.OutMsg.ASR_FINAL, text=text))
        log.info("ASR ok device=%s text=%r (%.2fs)", session.device_id, text, time.time() - t0)

        # 2. LLM streaming
        session.chat_history.append(ChatMessage("user", text))
        messages = [{"role": "system", "content": DEFAULT_SYSTEM_PROMPT}]
        # Keep last 6 turns to bound context.
        for m in session.chat_history[-12:]:
            messages.append({"role": m.role, "content": m.content})

        full = ""
        t_llm = time.time()
        try:
            await session.send_json(P.ok(P.OutMsg.STATUS, status="speaking"))
            async for chunk in self._llm.stream_chat(messages, temperature=0.7):
                full += chunk
                # Streaming hint to UI — firmware may ignore.
                await session.send_json(P.ok(
                    P.OutMsg.CLI_SUMMARY,
                    latestAssistantText=full,
                    done=False,
                ))
            await session.send_json(P.ok(P.OutMsg.LLM_DONE, full_text=full))
            session.chat_history.append(ChatMessage("assistant", full))
            log.info(
                "LLM ok device=%s %d chars (%.2fs)",
                session.device_id, len(full), time.time() - t_llm,
            )
        except Exception as e:  # noqa: BLE001
            log.exception("LLM failed device=%s: %s", session.device_id, e)
            await session.send_json(P.ok(P.OutMsg.ERROR, message=f"llm: {e}"))
            async with session.lock:
                session.state = "idle"
            return

        # 3. TTS — send chunks as binary frames. Skip silently if reply empty.
        if not full.strip():
            async with session.lock:
                session.state = "idle"
            return

        try:
            await session.send_json(P.ok(P.OutMsg.TTS_START))
            seq = 0
            t_tts = time.time()
            async for pcm24k in self._tts.stream_speech(full):
                pcm16k = await resample_pcm_24k_to_16k(pcm24k)
                if not pcm16k:
                    continue
                frame = P.pack_tts_frame(
                    pcm16k,
                    P.TtsFrameHeader(
                        sample_rate=settings.device_sample_rate,
                        sample_width=settings.device_sample_width,
                        channels=settings.device_channels,
                        seq=seq,
                    ),
                )
                await session.send_bytes(frame)
                seq += 1
            await session.send_json(P.ok(P.OutMsg.TTS_END))
            log.info(
                "TTS ok device=%s %d frames (%.2fs)",
                session.device_id, seq, time.time() - t_tts,
            )
        except Exception as e:  # noqa: BLE001
            log.exception("TTS failed device=%s: %s", session.device_id, e)
            await session.send_json(P.ok(P.OutMsg.ERROR, message=f"tts: {e}"))

        async with session.lock:
            session.state = "idle"
