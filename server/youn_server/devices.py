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
    trust         INTEGER NOT NULL DEFAULT 0,
    power_counters TEXT
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
        # Prove writability first, because SQLite will not: it opens a file it
        # cannot write in read-only mode *without raising*, and every statement
        # below is a no-op on an existing database, so the server starts looking
        # healthy and only fails when a device tries to pair. BEGIN IMMEDIATE
        # alone is not enough — that only takes a lock, which a read-only
        # database grants. Creating a table forces a page write and is rolled
        # straight back, leaving the database exactly as it was found.
        try:
            self._conn.execute("BEGIN IMMEDIATE")
            self._conn.execute("CREATE TABLE IF NOT EXISTS __write_probe(x INTEGER)")
            self._conn.execute("ROLLBACK")
        except sqlite3.OperationalError as exc:
            self._conn.close()
            raise RuntimeError(
                f"devices database is not writable: {db_path} ({exc}). "
                "Check the file and directory permissions for the user running "
                "the server — SQLite opens a database read-only, silently, when "
                "it cannot write it, so without this check the failure surfaces "
                "later as a 500 on a device request."
            ) from exc
        self._conn.executescript(_SCHEMA)
        # Migrate existing device_secrets: add token column if missing. Only that
        # error is expected here; swallowing OperationalError generally is how a
        # read-only database stayed hidden (the probe above now catches it, but
        # narrowing this keeps the next one from hiding the same way).
        try:
            self._conn.execute("ALTER TABLE device_secrets ADD COLUMN token TEXT")
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
        # Task 1 duration ledger: per-device power-counter snapshot (JSON text).
        # ALTER is idempotent via the duplicate-column guard, like the token
        # migration above — existing databases gain the column on next boot.
        try:
            self._conn.execute("ALTER TABLE devices ADD COLUMN power_counters TEXT")
        except sqlite3.OperationalError as exc:
            if "duplicate column name" not in str(exc):
                raise
        self._conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_device_secrets_token ON device_secrets(token)"
        )
        # Battery telemetry history (spec 2026-09-22): one row per wake that
        # carried ?v=&p=&c= on the schedule GET, plus that GET's power-counter
        # snapshot so adjacent-row deltas explain the discharge rate.
        self._conn.execute(
            """CREATE TABLE IF NOT EXISTS battery_history (
                   device_id TEXT NOT NULL,
                   ts INTEGER NOT NULL,
                   mv INTEGER NOT NULL,
                   pct INTEGER NOT NULL,
                   charge INTEGER NOT NULL,
                   wakes INTEGER NOT NULL DEFAULT 0,
                   awake_ms INTEGER NOT NULL DEFAULT 0,
                   radio_ms INTEGER NOT NULL DEFAULT 0,
                   http_gets INTEGER NOT NULL DEFAULT 0,
                   refresh_submit_ms INTEGER NOT NULL DEFAULT 0,
                   PRIMARY KEY (device_id, ts)
               )""")

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

    def touch(self, device_id: str, ip_address: Optional[str] = None) -> None:
        """Stamp `last_seen` for an authenticated device request.

        The registry was previously written only by the WebSocket hello path;
        devices that poll over plain HTTP (the normal steady state) kept the
        timestamp of their last WS handshake, so the admin UI showed days-old
        liveness for a device checking in every ten seconds.

        Callers MUST have authenticated the request first — this is a heartbeat,
        not a validation step, so an unauthenticated call would let anyone forge
        liveness. `ip_address` is refreshed only when supplied: many polls arrive
        through a proxy and would otherwise overwrite the device's LAN address.
        """
        now = int(time.time())
        with self._lock:
            if ip_address is None:
                self._conn.execute(
                    "UPDATE devices SET last_seen = ? WHERE device_id = ?",
                    (now, device_id),
                )
            else:
                self._conn.execute(
                    "UPDATE devices SET last_seen = ?, ip_address = ? WHERE device_id = ?",
                    (now, ip_address, device_id),
                )

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

    def set_power_counters(self, device_id: str, json_text: str) -> None:
        """Store the device's latest power-counter snapshot (JSON text)."""
        with self._lock:
            self._conn.execute(
                "UPDATE devices SET power_counters = ? WHERE device_id = ?",
                (json_text, device_id),
            )

    def get_power_counters(self, device_id: str) -> Optional[str]:
        """Return the stored power-counter snapshot, or None when unset."""
        with self._lock:
            row = self._conn.execute(
                "SELECT power_counters FROM devices WHERE device_id = ?", (device_id,)
            ).fetchone()
        if row is None:
            return None
        try:
            return row["power_counters"]
        except (IndexError, KeyError):
            return None

    def add_battery_sample(self, device_id: str, ts: int, mv: int, pct: int,
                           charge: int, counters: dict) -> None:
        """Append one battery sample; drop out-of-range values silently."""
        for name, lo, hi in (("mv", 2500, 5000), ("pct", 0, 100), ("charge", 0, 4)):
            v = {"mv": mv, "pct": pct, "charge": charge}[name]
            if not (lo <= v <= hi):
                return
        c = counters or {}
        with self._lock:
            self._conn.execute(
                "INSERT OR REPLACE INTO battery_history VALUES (?,?,?,?,?,?,?,?,?,?)",
                (device_id, ts, mv, pct, charge, c.get("wakes", 0),
                 c.get("awake_ms", 0), c.get("radio_ms", 0), c.get("http_gets", 0),
                 c.get("refresh_submit_ms", 0)))
            self._conn.execute(
                "DELETE FROM battery_history WHERE device_id = ? AND ts < ?",
                (device_id, ts - 90 * 86400))

    def battery_history(self, device_id: str, since_ts: int) -> list:
        with self._lock:
            cur = self._conn.execute(
                "SELECT ts, mv, pct, charge, wakes, awake_ms, radio_ms, http_gets,"
                " refresh_submit_ms FROM battery_history"
                " WHERE device_id = ? AND ts >= ? ORDER BY ts", (device_id, since_ts))
            return [dict(zip([d[0] for d in cur.description], row)) for row in cur.fetchall()]


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
