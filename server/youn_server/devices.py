"""Device registry.

Persisted in SQLite (`devices.db`). Schema:

    CREATE TABLE devices (
        device_id     TEXT PRIMARY KEY,
        board_type    TEXT NOT NULL,
        first_seen    INTEGER NOT NULL,  -- unix seconds
        last_seen     INTEGER NOT NULL,
        ws_session_id TEXT,
        ip_address    TEXT,
        trust         INTEGER NOT NULL DEFAULT 0  -- 0=pending, 1=paired
    );

Pairing model:
- First hello from an unknown device_id: row inserted with trust=0.
- Operator must approve via the HTTP API (`POST /api/devices/{id}/approve`)
  OR the device passes a per-device shared secret obtained out-of-band
  (set in `device_secrets` table by operator).
- WS handshake enforces trust; image push / OTA also enforce it.

This module is process-local state mirrored to SQLite — sufficient for the
"中等规模" target. A multi-instance deployment would move state to Redis/Postgres.
"""
from __future__ import annotations

import logging
import sqlite3
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from .config import settings

log = logging.getLogger(__name__)


@dataclass
class Device:
    device_id: str
    board_type: str
    first_seen: int
    last_seen: int
    ws_session_id: Optional[str]
    ip_address: Optional[str]
    trust: bool

    @property
    def trusted(self) -> bool:
        return self.trust


_SCHEMA = """
CREATE TABLE IF NOT EXISTS devices (
    device_id     TEXT PRIMARY KEY,
    board_type    TEXT NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    ws_session_id TEXT,
    ip_address    TEXT,
    trust         INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS device_secrets (
    device_id TEXT PRIMARY KEY,
    secret    TEXT NOT NULL,
    created   INTEGER NOT NULL,
    token     TEXT
);
CREATE INDEX IF NOT EXISTS idx_devices_trust ON devices(trust);
"""


class DeviceRegistry:
    """Thread-safe SQLite-backed device registry.

    SQLite connections are serialized through a lock — this is fine for the
    tens-of-devices scale we target. Heavy concurrent writes would warrant WAL.
    """

    def __init__(self, db_path: Path) -> None:
        self._db_path = db_path
        self._lock = threading.RLock()
        # check_same_thread=False; we serialize via _lock.
        self._conn = sqlite3.connect(str(db_path), check_same_thread=False, isolation_level=None)
        self._conn.row_factory = sqlite3.Row
        self._conn.executescript(_SCHEMA)
        # Migrate existing device_secrets: add token column if missing.
        try:
            self._conn.execute("ALTER TABLE device_secrets ADD COLUMN token TEXT")
        except sqlite3.OperationalError:
            pass  # column already exists
        # Ensure unique index on token (idempotent).
        self._conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_device_secrets_token ON device_secrets(token)"
        )

    # ── write ──
    def upsert(
        self,
        device_id: str,
        board_type: str,
        *,
        ws_session_id: Optional[str] = None,
        ip_address: Optional[str] = None,
    ) -> Device:
        now = int(time.time())
        with self._lock:
            row = self._conn.execute(
                "SELECT * FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
            if row is None:
                self._conn.execute(
                    """
                    INSERT INTO devices(device_id, board_type, first_seen, last_seen,
                                        ws_session_id, ip_address, trust)
                    VALUES (?, ?, ?, ?, ?, ?, 0)
                    """,
                    (device_id, board_type, now, now, ws_session_id, ip_address),
                )
                log.info("device registered device_id=%s board=%s", device_id, board_type)
            else:
                self._conn.execute(
                    """
                    UPDATE devices
                       SET board_type    = ?,
                           last_seen     = ?,
                           ws_session_id = COALESCE(?, ws_session_id),
                           ip_address    = COALESCE(?, ip_address)
                     WHERE device_id = ?
                    """,
                    (board_type, now, ws_session_id, ip_address, device_id),
                )
            return self.get(device_id)  # type: ignore[return-value]

    def approve(self, device_id: str) -> bool:
        with self._lock:
            cur = self._conn.execute(
                "UPDATE devices SET trust = 1 WHERE device_id = ?", (device_id,)
            )
            return cur.rowcount > 0

    def revoke(self, device_id: str) -> bool:
        with self._lock:
            cur = self._conn.execute(
                "UPDATE devices SET trust = 0 WHERE device_id = ?", (device_id,)
            )
            return cur.rowcount > 0

    def set_secret(self, device_id: str, secret: str) -> None:
        with self._lock:
            self._conn.execute(
                """
                INSERT INTO device_secrets(device_id, secret, created) VALUES (?, ?, ?)
                ON CONFLICT(device_id) DO UPDATE SET secret = excluded.secret
                """,
                (device_id, secret, int(time.time())),
            )

    # ── read ──
    def get(self, device_id: str) -> Optional[Device]:
        with self._lock:
            row = self._conn.execute(
                "SELECT * FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
        if row is None:
            return None
        return Device(
            device_id=row["device_id"],
            board_type=row["board_type"],
            first_seen=row["first_seen"],
            last_seen=row["last_seen"],
            ws_session_id=row["ws_session_id"],
            ip_address=row["ip_address"],
            trust=bool(row["trust"]),
        )

    def get_secret(self, device_id: str) -> Optional[str]:
        with self._lock:
            row = self._conn.execute(
                "SELECT secret FROM device_secrets WHERE device_id = ?", (device_id,)
            ).fetchone()
        return None if row is None else row["secret"]

    def get_device_by_token(self, token: str) -> Optional[Device]:
        """Look up a device by its auth token. Returns None if not found."""
        with self._lock:
            row = self._conn.execute(
                "SELECT d.* FROM devices d JOIN device_secrets ds ON d.device_id = ds.device_id WHERE ds.token = ?",
                (token,),
            ).fetchone()
        if row is None:
            return None
        return Device(
            device_id=row["device_id"],
            board_type=row["board_type"],
            first_seen=row["first_seen"],
            last_seen=row["last_seen"],
            ws_session_id=row["ws_session_id"],
            ip_address=row["ip_address"],
            trust=bool(row["trust"]),
        )

    def set_token(self, device_id: str, token: str) -> None:
        """Store auth token for a device."""
        with self._lock:
            self._conn.execute(
                """
                INSERT INTO device_secrets(device_id, secret, created, token)
                VALUES (?, '', ?, ?)
                ON CONFLICT(device_id) DO UPDATE SET token = excluded.token
                """,
                (device_id, int(time.time()), token),
            )


    def list_all(self, only_trusted: bool = False) -> list[Device]:
        with self._lock:
            sql = "SELECT * FROM devices"
            if only_trusted:
                sql += " WHERE trust = 1"
            sql += " ORDER BY last_seen DESC"
            rows = self._conn.execute(sql).fetchall()
        return [
            Device(
                device_id=r["device_id"],
                board_type=r["board_type"],
                first_seen=r["first_seen"],
                last_seen=r["last_seen"],
                ws_session_id=r["ws_session_id"],
                ip_address=r["ip_address"],
                trust=bool(r["trust"]),
            )
            for r in rows
        ]

    def close(self) -> None:
        with self._lock:
            self._conn.close()


# Process-wide singleton.
registry = DeviceRegistry(settings.devices_db)
