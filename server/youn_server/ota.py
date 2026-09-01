"""OTA firmware management.

Storage layout (under settings.firmware_dir):

    firmware/
      v6.5.9-note4c.bin     # raw binary uploaded by operator
      v6.5.9-note4c.bin.sha256
      v6.5.9-note4c.bin.sig # HMAC-SHA256(discover_secret, sha256)
      latest.json            # {"version", "size", "sha256", "filename", "channel"}
    archive/                 # previous versions kept here

Public surface:
    save_firmware(file, version, channel) → metadata
    latest(channel) → metadata or None
    get_file_path(version) → Path
    verify_against_signature(file_path, sig_hex) → bool
"""
from __future__ import annotations

import hashlib
import hmac
import json
import logging
import shutil
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

from .config import settings

log = logging.getLogger(__name__)


@dataclass
class FirmwareMeta:
    version: str
    filename: str
    size: int
    sha256: str
    channel: str
    uploaded_at: int
    notes: str = ""

    def to_json(self) -> dict:
        return asdict(self)

    @classmethod
    def from_json(cls, d: dict) -> "FirmwareMeta":
        return cls(
            version=str(d["version"]),
            filename=str(d["filename"]),
            size=int(d["size"]),
            sha256=str(d["sha256"]),
            channel=str(d.get("channel", "stable")),
            uploaded_at=int(d.get("uploaded_at", 0)),
            notes=str(d.get("notes", "")),
        )


def _safe_filename(version: str) -> str:
    return "".join(c for c in version if c.isalnum() or c in "._-").strip() or "firmware"


def save_firmware(
    data: bytes, *, version: str, channel: str = "stable", notes: str = ""
) -> FirmwareMeta:
    """Persist a firmware blob and return its metadata.

    Symlinks the previous `latest.json` into archive/ before overwriting.
    """
    target_dir: Path = settings.firmware_dir
    target_dir.mkdir(parents=True, exist_ok=True)
    archive_dir = target_dir / "archive"
    archive_dir.mkdir(parents=True, exist_ok=True)

    sha = hashlib.sha256(data).hexdigest()
    fname = f"{_safe_filename(version)}.bin"
    fpath = target_dir / fname
    fpath.write_bytes(data)
    (target_dir / f"{fname}.sha256").write_text(sha + "\n")

    sig = hmac.new(
        settings.discovery_shared_secret.encode("utf-8"),
        sha.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    (target_dir / f"{fname}.sig").write_text(sig + "\n")

    meta = FirmwareMeta(
        version=version,
        filename=fname,
        size=len(data),
        sha256=sha,
        channel=channel,
        uploaded_at=int(time.time()),
        notes=notes,
    )

    latest_path = target_dir / "latest.json"
    prev = latest_path.read_bytes() if latest_path.exists() else None
    latest_path.write_text(json.dumps(meta.to_json(), ensure_ascii=False, indent=2))
    if prev is not None:
        # Snapshot previous latest.json as <prev_version>.json in archive/.
        try:
            prev_meta = FirmwareMeta.from_json(json.loads(prev.decode("utf-8")))
        except Exception:  # noqa: BLE001
            prev_meta = None
        if prev_meta is not None and prev_meta.version != version:
            shutil.copy2(
                latest_path,
                archive_dir / f"{_safe_filename(prev_meta.version)}.json",
            )
            shutil.copy2(
                target_dir / prev_meta.filename,
                archive_dir / prev_meta.filename,
            )

    log.info("firmware saved version=%s size=%d sha=%s", version, len(data), sha)
    return meta


def latest(channel: str = "stable") -> Optional[FirmwareMeta]:
    p = settings.firmware_dir / "latest.json"
    if not p.exists():
        return None
    try:
        meta = FirmwareMeta.from_json(json.loads(p.read_text()))
    except (json.JSONDecodeError, KeyError, ValueError) as e:
        log.warning("latest.json malformed: %s", e)
        return None
    if meta.channel != channel:
        return None
    return meta


def get_file_path(filename: str) -> Optional[Path]:
    p = settings.firmware_dir / filename
    if not p.exists() or not p.is_file():
        return None
    # Path-traversal hardening.
    if settings.firmware_dir.resolve() not in p.resolve().parents:
        return None
    return p


def verify_signature(file_path: Path) -> bool:
    """Recompute signature and compare. Used in DEPLOY guide troubleshooting."""
    sig_path = file_path.with_suffix(file_path.suffix + ".sig")
    if not sig_path.exists():
        return False
    sha = hashlib.sha256(file_path.read_bytes()).hexdigest()
    expected = hmac.new(
        settings.discovery_shared_secret.encode("utf-8"),
        sha.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    actual = sig_path.read_text().strip()
    return hmac.compare_digest(expected, actual)
