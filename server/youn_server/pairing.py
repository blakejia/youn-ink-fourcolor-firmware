"""Device pairing session management.

Three-step pairing protocol:
  1. Device → pair-start  → 6-digit code (public, rate-limited)
  2. Operator → pair-confirm  → code verified (operator auth)
  3. Device → pair-claim  → token issued (public, code one-time)

Pairing sessions are stored in SQLite (pairing_sessions table).
Rate limiting and claim lockout are in-memory sliding windows.
"""
from __future__ import annotations

import logging
import secrets
import sqlite3
import threading
import time
from pathlib import Path
from typing import Optional

log = logging.getLogger(__name__)

PAIRING_SCHEMA = """
CREATE TABLE IF NOT EXISTS pairing_sessions (
    device_id  TEXT PRIMARY KEY,
    code       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    confirmed  INTEGER NOT NULL DEFAULT 0
);
"""

CODE_LENGTH = 6
CODE_TTL = 300  # 5 minutes
CONFIRM_LOCKOUT_SECONDS = 600  # 10 minutes
CONFIRM_LOCKOUT_THRESHOLD = 5  # wrong claims before lockout
RATE_LIMIT_WINDOW = 300  # 5 minutes
RATE_LIMIT_MAX = 5


class PairingStore:
    """Thread-safe SQLite-backed pairing session store.

    Also manages in-memory rate limiting (per-IP) and claim lockout (per-device).
    """

    def __init__(self, db_path: Path) -> None:
        self._db_path = db_path
        self._lock = threading.RLock()
        self._conn = sqlite3.connect(
            str(db_path), check_same_thread=False, isolation_level=None
        )
        self._conn.row_factory = sqlite3.Row
        self._conn.executescript(PAIRING_SCHEMA)

        # Rate limiting: ip → list of timestamps
        self._rate_limits: dict[str, list[float]] = {}
        # Claim lockout: device_id → list of failed attempt timestamps
        self._claim_failures: dict[str, list[float]] = {}

    # ── session CRUD ──

    def create_session(self, device_id: str) -> tuple[str, int]:
        """Create a new pairing session. Returns (code, expires_in_seconds).

        Cleans expired sessions for this device first.
        """
        now = int(time.time())
        expires_at = now + CODE_TTL
        code = "".join(secrets.choice("0123456789") for _ in range(CODE_LENGTH))
        with self._lock:
            # Clean expired sessions
            self._conn.execute(
                "DELETE FROM pairing_sessions WHERE device_id = ? AND expires_at < ?",
                (device_id, now),
            )
            # Upsert new session
            self._conn.execute(
                """
                INSERT INTO pairing_sessions(device_id, code, created_at, expires_at, confirmed)
                VALUES (?, ?, ?, ?, 0)
                ON CONFLICT(device_id) DO UPDATE SET
                    code = excluded.code,
                    created_at = excluded.created_at,
                    expires_at = excluded.expires_at,
                    confirmed = 0
                """,
                (device_id, code, now, expires_at),
            )
        log.info("pairing session created device_id=%s", device_id)
        return code, CODE_TTL

    def confirm_session(self, device_id: str, code: str) -> bool:
        """Confirm a pairing session (operator step). Returns True on success.

        Validates code matches and session is not expired.
        """
        now = int(time.time())
        with self._lock:
            row = self._conn.execute(
                "SELECT code, expires_at FROM pairing_sessions WHERE device_id = ?",
                (device_id,),
            ).fetchone()
            if row is None:
                log.warning("confirm failed: no session for device_id=%s", device_id)
                return False
            if now > row["expires_at"]:
                log.warning("confirm failed: expired session device_id=%s", device_id)
                return False
            if not secrets.compare_digest(code.encode(), row["code"].encode()):
                log.warning("confirm failed: wrong code device_id=%s", device_id)
                return False
            self._conn.execute(
                "UPDATE pairing_sessions SET confirmed = 1 WHERE device_id = ?",
                (device_id,),
            )
        log.info("pairing session confirmed device_id=%s", device_id)
        return True

    def claim_session(
        self, device_id: str, code: str, *, now: Optional[int] = None
    ) -> Optional[str]:
        """Claim a pairing session (device step). Returns token on success, None on failure.

        Validates code matches, session is confirmed and not expired.
        After successful claim the session is deleted (one-time use).
        """
        if now is None:
            now = int(time.time())
        with self._lock:
            row = self._conn.execute(
                "SELECT code, confirmed, expires_at FROM pairing_sessions WHERE device_id = ?",
                (device_id,),
            ).fetchone()
            if row is None:
                log.warning("claim failed: no session device_id=%s", device_id)
                return None
            if now > row["expires_at"]:
                log.warning("claim failed: expired session device_id=%s", device_id)
                return None
            if not row["confirmed"]:
                log.warning("claim failed: not confirmed device_id=%s", device_id)
                return None
            if not secrets.compare_digest(code.encode(), row["code"].encode()):
                log.warning("claim failed: wrong code device_id=%s", device_id)
                return None
            # Success — delete session and return token
            self._conn.execute(
                "DELETE FROM pairing_sessions WHERE device_id = ?",
                (device_id,),
            )
        token = secrets.token_hex(32)
        log.info("pairing claimed device_id=%s", device_id)
        return token

    # ── rate limiting (in-memory sliding window) ──

    def check_rate_limit(self, ip: str) -> bool:
        """Returns True if request is allowed, False if rate-limited."""
        now = time.time()
        cutoff = now - RATE_LIMIT_WINDOW
        with self._lock:
            timestamps = self._rate_limits.get(ip, [])
            # Prune old entries
            timestamps = [t for t in timestamps if t > cutoff]
            if len(timestamps) >= RATE_LIMIT_MAX:
                self._rate_limits[ip] = timestamps
                return False
            timestamps.append(now)
            self._rate_limits[ip] = timestamps
        return True

    # ── claim lockout (in-memory sliding window) ──

    def record_claim_failure(self, device_id: str) -> None:
        """Record a failed claim attempt."""
        now = time.time()
        cutoff = now - CONFIRM_LOCKOUT_SECONDS
        with self._lock:
            failures = self._claim_failures.get(device_id, [])
            failures = [t for t in failures if t > cutoff]
            failures.append(now)
            self._claim_failures[device_id] = failures

    def is_claim_locked(self, device_id: str) -> bool:
        """Returns True if device is locked out from claiming."""
        now = time.time()
        cutoff = now - CONFIRM_LOCKOUT_SECONDS
        with self._lock:
            failures = self._claim_failures.get(device_id, [])
            failures = [t for t in failures if t > cutoff]
            self._claim_failures[device_id] = failures
            return len(failures) >= CONFIRM_LOCKOUT_THRESHOLD

    def cleanup_expired(self) -> int:
        """Delete all expired sessions. Returns count deleted."""
        now = int(time.time())
        with self._lock:
            cur = self._conn.execute(
                "DELETE FROM pairing_sessions WHERE expires_at < ?",
                (now,),
            )
            return cur.rowcount

    def close(self) -> None:
        with self._lock:
            self._conn.close()
