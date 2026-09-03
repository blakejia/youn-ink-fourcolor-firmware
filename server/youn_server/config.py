"""Server-wide configuration loaded from .env / environment.

Single source of truth. All other modules import from here, never read os.environ directly.
"""
from __future__ import annotations

import logging
import os
import sys
from logging.handlers import RotatingFileHandler
from pathlib import Path

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
        extra="ignore",
    )

    # ── OpenAI-compatible stack ──
    openai_api_key: str = Field(default="")
    openai_base_url: str = Field(default="https://api.openai.com/v1")
    openai_asr_model: str = Field(default="whisper-1")
    openai_llm_model: str = Field(default="gpt-4o-mini")
    openai_tts_model: str = Field(default="tts-1")
    openai_tts_voice: str = Field(default="alloy")
    openai_tts_response_format: str = Field(default="pcm")

    # ── Network ──
    listen_host: str = Field(default="0.0.0.0")
    listen_port: int = Field(default=9001)
    push_image_host: str = Field(default="0.0.0.0")
    push_image_port: int = Field(default=8766)
    public_ws_url: str = Field(default="ws://127.0.0.1:9001/ws")
    public_http_base: str = Field(default="http://127.0.0.1:8766")

    # ── Discovery / auth ──
    discovery_shared_secret: str = Field(default="change-me-32-bytes-of-entropy-min")
    discovery_service_tag: str = Field(default="youn-ink-v1")

    # ── Storage (resolved to absolute paths relative to package) ──
    data_dir: Path = Field(default=Path("./data"))
    devices_db: Path = Field(default=Path("./data/devices.db"))
    images_dir: Path = Field(default=Path("./data/images"))
    firmware_dir: Path = Field(default=Path("./data/firmware"))
    uploads_dir: Path = Field(default=Path("./data/uploads"))

    # ── Logging ──
    log_level: str = Field(default="INFO")
    log_dir: Path = Field(default=Path("./data"))
    log_file: str = Field(default="server.log")

    # ── Audio ──
    device_sample_rate: int = Field(default=16000)
    device_sample_width: int = Field(default=2)
    device_channels: int = Field(default=1)
    asr_chunk_bytes: int = Field(default=3200)
    operator_token: str = Field(default="")
    notify_default_ttl: int = Field(default=300)

    # ── Device signature authentication (HMAC pair-start) ──
    # Symmetric MASTER_KEY shared with firmware; >=32 random bytes recommended.
    # Empty = device auth disabled at runtime: server starts but rejects every
    # pair-start (logged at startup). Read from .env, never hardcoded.
    master_key: str = Field(default="")
    # Comma-separated device_id whitelist; empty string = all devices accepted.
    allowed_device_ids: str = Field(default="")

    # ── Canvas Loop policy ──
    canvas_sleep_start: str = Field(default="00:00")
    canvas_sleep_end: str = Field(default="06:00")
    canvas_timezone: str = Field(default="Asia/Shanghai")
    canvas_poll_interval_minutes: int = Field(default=10)
    canvas_sleep_poll_interval_minutes: int = Field(default=60)
    canvas_min_page_duration_minutes: int = Field(default=10)

    def resolve_paths(self, base: Path) -> None:
        for field in ("data_dir", "devices_db", "images_dir", "firmware_dir", "uploads_dir", "log_dir"):
            p = getattr(self, field)
            if not p.is_absolute():
                setattr(self, field, (base / p).resolve())


def _load_settings() -> Settings:
    s = Settings()
    # Resolve relative paths against the directory of the entry-point script,
    # so a systemd unit launching `python llmserve.py` writes into server/data.
    base = Path(os.environ.get("YOUN_SERVER_BASE", Path.cwd())).resolve()
    s.resolve_paths(base)
    for d in (s.data_dir, s.images_dir, s.firmware_dir, s.uploads_dir, s.log_dir):
        d.mkdir(parents=True, exist_ok=True)
    return s


settings = _load_settings()


def setup_logging() -> None:
    """Configure root logger with rotating file + stdout.

    Idempotent — safe to call from both llmserve.py and push_image.py.
    """
    root = logging.getLogger()
    if getattr(root, "_youn_configured", False):
        return
    root.setLevel(getattr(logging, settings.log_level.upper(), logging.INFO))

    fmt = logging.Formatter(
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )

    sh = logging.StreamHandler(sys.stdout)
    sh.setFormatter(fmt)
    root.addHandler(sh)

    fh = RotatingFileHandler(
        settings.log_dir / settings.log_file,
        maxBytes=10 * 1024 * 1024,
        backupCount=5,
        encoding="utf-8",
    )
    fh.setFormatter(fmt)
    root.addHandler(fh)

    # Quiet noisy third-party libs.
    for noisy in ("httpx", "httpcore", "websockets"):
        logging.getLogger(noisy).setLevel(logging.WARNING)

    root._youn_configured = True  # type: ignore[attr-defined]
