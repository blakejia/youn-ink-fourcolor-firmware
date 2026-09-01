"""OpenAI-compatible AI client.

Three roles: ASR (Whisper /audio/transcriptions), LLM (/chat/completions,
streaming), TTS (/audio/speech).

Uses httpx async client with timeouts. Designed to swap providers (Together /
Groq / OpenRouter / local llama.cpp /v1 proxy) by changing OPENAI_BASE_URL.
"""
from __future__ import annotations

import io
import logging
import os
import tempfile
import wave
from typing import AsyncIterator, Optional

import httpx

from .config import settings

log = logging.getLogger(__name__)


# ── ASR (Whisper) ─────────────────────────────────────────────────────
class ASRError(RuntimeError):
    pass


class WhisperClient:
    def __init__(self, *, timeout_s: float = 30.0) -> None:
        self._client = httpx.AsyncClient(
            base_url=settings.openai_base_url,
            timeout=httpx.Timeout(timeout_s, connect=10.0),
            headers={"Authorization": f"Bearer {settings.openai_api_key}"},
        )
        self._model = settings.openai_asr_model

    async def transcribe_pcm(
        self,
        pcm: bytes,
        *,
        sample_rate: int = 16000,
        sample_width: int = 2,
        channels: int = 1,
        language: Optional[str] = None,
    ) -> str:
        """Transcribe raw PCM 16-bit mono bytes via Whisper.

        Whisper requires a file-like upload — we wrap the PCM in a temporary WAV
        container so the upstream sees a valid audio file (some providers reject
        raw PCM uploads).
        """
        wav_bytes = self._pcm_to_wav(pcm, sample_rate, sample_width, channels)
        files = {"file": ("audio.wav", wav_bytes, "audio/wav")}
        data = {"model": self._model, "response_format": "json"}
        if language:
            data["language"] = language

        try:
            r = await self._client.post(
                "/audio/transcriptions", files=files, data=data
            )
        except httpx.HTTPError as e:
            raise ASRError(f"ASR transport error: {e}") from e

        if r.status_code != 200:
            raise ASRError(f"ASR {r.status_code}: {r.text[:300]}")
        try:
            return (r.json().get("text") or "").strip()
        except ValueError as e:
            raise ASRError(f"ASR invalid json: {e}") from e

    @staticmethod
    def _pcm_to_wav(pcm: bytes, sr: int, sw: int, ch: int) -> bytes:
        buf = io.BytesIO()
        with wave.open(buf, "wb") as wf:
            wf.setnchannels(ch)
            wf.setsampwidth(sw)
            wf.setframerate(sr)
            wf.writeframes(pcm)
        return buf.getvalue()

    async def aclose(self) -> None:
        await self._client.aclose()


# ── LLM (chat/completions streaming) ──────────────────────────────────
class LLMError(RuntimeError):
    pass


class LLMClient:
    """Stream chat/completions tokens. Yields decoded text chunks."""

    def __init__(self, *, timeout_s: float = 60.0) -> None:
        self._client = httpx.AsyncClient(
            base_url=settings.openai_base_url,
            timeout=httpx.Timeout(timeout_s, connect=10.0),
            headers={
                "Authorization": f"Bearer {settings.openai_api_key}",
                "Content-Type": "application/json",
            },
        )
        self._model = settings.openai_llm_model

    async def stream_chat(
        self,
        messages: list[dict],
        *,
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
    ) -> AsyncIterator[str]:
        body: dict = {
            "model": self._model,
            "messages": messages,
            "stream": True,
            "temperature": temperature,
        }
        if max_tokens:
            body["max_tokens"] = max_tokens

        try:
            async with self._client.stream(
                "POST", "/chat/completions", json=body
            ) as r:
                if r.status_code != 200:
                    text = await r.aread()
                    raise LLMError(f"LLM {r.status_code}: {text.decode('utf-8', 'replace')[:300]}")
                async for line in r.aiter_lines():
                    if not line or not line.startswith("data:"):
                        continue
                    payload = line[5:].strip()
                    if payload == "[DONE]":
                        break
                    try:
                        import json
                        obj = json.loads(payload)
                    except ValueError:
                        continue
                    delta = (obj.get("choices") or [{}])[0].get("delta") or {}
                    chunk = delta.get("content")
                    if chunk:
                        yield chunk
        except httpx.HTTPError as e:
            raise LLMError(f"LLM transport error: {e}") from e

    async def aclose(self) -> None:
        await self._client.aclose()


# ── TTS (audio/speech) ────────────────────────────────────────────────
class TTSError(RuntimeError):
    pass


class TTSClient:
    """OpenAI-compatible audio/speech. Streams raw PCM chunks back."""

    def __init__(self, *, timeout_s: float = 30.0) -> None:
        self._client = httpx.AsyncClient(
            base_url=settings.openai_base_url,
            timeout=httpx.Timeout(timeout_s, connect=10.0),
            headers={
                "Authorization": f"Bearer {settings.openai_api_key}",
                "Content-Type": "application/json",
            },
        )
        self._model = settings.openai_tts_model
        self._voice = settings.openai_tts_voice
        # PCM at 24 kHz / 16-bit / mono is what OpenAI emits. Device needs 16 kHz.
        # We expose stream_speech() returning 24 kHz PCM; the session layer
        # resamples to device rate. If you point at a provider that emits
        # device-native rate, just swap the resampler call.
        self._response_format = settings.openai_tts_response_format

    async def stream_speech(self, text: str) -> AsyncIterator[bytes]:
        """Yield PCM chunks for the given text."""
        body = {
            "model": self._model,
            "voice": self._voice,
            "input": text,
            "response_format": self._response_format,
        }
        try:
            async with self._client.stream(
                "POST", "/audio/speech", json=body
            ) as r:
                if r.status_code != 200:
                    text_body = await r.aread()
                    raise TTSError(f"TTS {r.status_code}: {text_body[:300].decode('utf-8','replace')}")
                async for chunk in r.aiter_bytes(8192):
                    if chunk:
                        yield chunk
        except httpx.HTTPError as e:
            raise TTSError(f"TTS transport error: {e}") from e

    async def aclose(self) -> None:
        await self._client.aclose()


# ── Linear resampler (24 kHz → 16 kHz) ────────────────────────────────
# OpenAI TTS emits 24 kHz mono PCM. Device expects 16 kHz mono PCM.
# We do a simple linear interpolation — good enough for voice at this ratio.
async def resample_pcm_24k_to_16k(pcm_24k: bytes) -> bytes:
    import struct
    if not pcm_24k:
        return b""
    samples = struct.unpack(f"<{len(pcm_24k) // 2}h", pcm_24k)
    n_in = len(samples)
    n_out = n_in * 2 // 3  # 16/24
    out = array.array("h", [0] * n_out)
    for i in range(n_out):
        # map output index i to input space
        pos = i * 3 / 2
        i0 = int(pos)
        i1 = min(i0 + 1, n_in - 1)
        frac = pos - i0
        v = int(samples[i0] * (1 - frac) + samples[i1] * frac)
        # clip
        if v > 32767:
            v = 32767
        elif v < -32768:
            v = -32768
        out[i] = v
    return out.tobytes()


import array  # noqa: E402  (placed after use to avoid early import noise)
