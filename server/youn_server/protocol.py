"""Wire protocol between ESP32 device and server.

Derived from `server/mock_client.py` and `firmware/main/protocols/protocol.h`.

Two message channels share the same WebSocket:

1. Text frames (UTF-8 JSON) — control + AI pipeline messages.
2. Binary frames (server → device) — TTS audio. Layout:

       ┌──────────────┬─────────────────────┬─────────────────────┐
       │ header_len   │  JSON header        │   raw PCM payload   │
       │  uint16 BE   │  (UTF-8 JSON)       │   (16-bit LE mono)  │
       └──────────────┴─────────────────────┴─────────────────────┘

   The JSON header MUST contain `"type": "tts_audio"` and MAY carry
   `sample_rate`, `sample_width`, `channels`, `seq` for client sanity.

Text frame types (all JSON objects, server-side authoritative list):

    Inbound (device → server):
      hello                  handshake; server replies with hello_ack + server_ready
      ptt_start              begin push-to-talk recording
      ptt_audio (binary)     raw PCM16 16 kHz mono (sent as binary frames interleaved with text)
      ptt_stop               stop recording, server starts ASR → LLM → TTS
      pong                   heartbeat reply
      list_images            optional; refresh device-side gallery index
      ota_check              optional; query available firmware

    Outbound (server → device):
      hello_ack              handshake accepted; includes session_id, server_version
      server_ready           audio channel ready
      status                 {"status": "recording" | "processing"}
      asr_interim            partial ASR (best-effort, optional)
      transcript_final       final ASR text
      asr_final              alias of transcript_final (both names used by firmware/mock)
      cli_summary            streaming LLM token; {"latestAssistantText": "...", "done": bool}
      llm_done               final LLM full text
      tts_start / tts_end    boundary markers around binary audio
      image_push_meta        {"id": ..., "width": 400, "height": 300, "format": "bwry2bpp",
                              "size": 30000, "title": "..."}
      image_push_done        transfer complete
      ota_available          {"version": "...", "size": ..., "sha256": "...", "url": "..."}
      error                  {"message": "..."}
      pong                   heartbeat reply

TTS binary frame header keys (informational, not enforced server-side):
    type           "tts_audio"
    sample_rate    16000
    sample_width   2
    channels       1
    seq            int, monotonic
"""
from __future__ import annotations

import json
import struct
from dataclasses import dataclass
from typing import Any, Literal


# ── Inbound message types ─────────────────────────────────────────────
class MsgType:
    HELLO = "hello"
    PTT_START = "ptt_start"
    PTT_STOP = "ptt_stop"
    PONG = "pong"
    OTA_CHECK = "ota_check"


# ── Outbound message types ────────────────────────────────────────────
class OutMsg:
    HELLO_ACK = "hello_ack"
    SERVER_READY = "server_ready"
    STATUS = "status"
    ASR_INTERIM = "asr_interim"
    TRANSCRIPT_FINAL = "transcript_final"
    ASR_FINAL = "asr_final"
    CLI_SUMMARY = "cli_summary"
    LLM_DONE = "llm_done"
    TTS_START = "tts_start"
    TTS_END = "tts_end"
    OTA_AVAILABLE = "ota_available"
    ERROR = "error"
    PONG = "pong"


StatusValue = Literal["idle", "recording", "processing", "speaking"]


def ok(msg_type: str, **fields: Any) -> dict[str, Any]:
    """Build a JSON-friendly outbound message dict."""
    return {"type": msg_type, **fields}


# ── TTS binary frame codec ─────────────────────────────────────────────
TTS_HEADER_TYPE = "tts_audio"


@dataclass(frozen=True)
class TtsFrameHeader:
    sample_rate: int = 16000
    sample_width: int = 2
    channels: int = 1
    seq: int = 0
    text: str = ""

    def to_json_bytes(self) -> bytes:
        d = {
            "type": TTS_HEADER_TYPE,
            "sample_rate": self.sample_rate,
            "sample_width": self.sample_width,
            "channels": self.channels,
            "seq": self.seq,
        }
        if self.text:
            d["text"] = self.text
        return json.dumps(d, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def pack_tts_frame(pcm: bytes, header: TtsFrameHeader) -> bytes:
    """Encode a TTS audio frame: uint16 BE header_len + JSON header + PCM payload."""
    json_bytes = header.to_json_bytes()
    return struct.pack(">H", len(json_bytes)) + json_bytes + pcm


def unpack_tts_frame(frame: bytes) -> tuple[TtsFrameHeader, bytes]:
    """Decode a TTS audio frame. Raises ValueError on malformed input.

    Returned PCM is the bytes after the JSON header; sample_rate/sample_width/channels
    in the header describe those bytes for the device's I2S driver.
    """
    if len(frame) < 2:
        raise ValueError("frame shorter than header_len")
    header_len = struct.unpack(">H", frame[:2])[0]
    if len(frame) < 2 + header_len:
        raise ValueError("frame shorter than declared header length")
    try:
        d = json.loads(frame[2 : 2 + header_len].decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as e:
        raise ValueError(f"invalid json header: {e}") from e
    header = TtsFrameHeader(
        sample_rate=int(d.get("sample_rate", 16000)),
        sample_width=int(d.get("sample_width", 2)),
        channels=int(d.get("channels", 1)),
        seq=int(d.get("seq", 0)),
        text=str(d.get("text", "")),
    )
    return header, frame[2 + header_len :]


def is_tts_audio_json_header(d: dict[str, Any]) -> bool:
    return d.get("type") == TTS_HEADER_TYPE
