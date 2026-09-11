"""Device pairing session management.

Three-step pairing protocol:
  1. Device → pair-start  → 6-digit code (public, rate-limited)
  2. Operator → pair-confirm  → code verified (operator auth)
  3. Device → pair-claim  → token issued (public, code one-time)

Pairing sessions are stored in SQLite (pairing_sessions table).
Rate limiting and claim lockout are in-memory sliding windows.
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import logging
import secrets
import sqlite3
import threading
import time
from pathlib import Path
from typing import Optional

from .config import settings

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

# Device signature authentication (HMAC pair-start)
TIMESTAMP_WINDOW_SEC = 30  # ±30s clock-skew tolerance
NONCE_CACHE_TTL_SEC = 300  # 5 minutes nonce replay window (in-memory)

# 限流/锁定表的 key 上限：pair-start/pair-claim 是公开端点，任意 IP 或
# device_id 都会建 key；超过该数量时清理窗口已完全过期的 key。
_MAX_LIMIT_KEYS = 4096


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
        # Device signature replay cache: "device_id:nonce" → first-seen time
        self._nonce_cache: dict[str, float] = {}

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

    def is_pending(
        self, device_id: str, code: str, *, now: Optional[int] = None
    ) -> bool:
        """True when session exists, code matches, not expired, not yet confirmed.

        Read-only: never records a claim failure. Lets the device poll
        pair-claim while the user reads the 6-digit code without tripping
        the claim lockout or dropping a valid code.
        """
        if now is None:
            now = int(time.time())
        with self._lock:
            row = self._conn.execute(
                "SELECT code, confirmed, expires_at FROM pairing_sessions WHERE device_id = ?",
                (device_id,),
            ).fetchone()
            if row is None:
                return False
            if now > row["expires_at"]:
                return False
            if row["confirmed"]:
                return False
            return secrets.compare_digest(code.encode(), row["code"].encode())

    def list_pending(self, *, now: Optional[int] = None) -> list[dict]:
        """Unconfirmed, unexpired sessions for the operator confirm UI."""
        if now is None:
            now = int(time.time())
        with self._lock:
            rows = self._conn.execute(
                "SELECT device_id, code, expires_at FROM pairing_sessions"
                " WHERE confirmed = 0 AND expires_at > ?",
                (now,),
            ).fetchall()
            return [
                {"device_id": r["device_id"], "code": r["code"],
                 "expires_in": r["expires_at"] - now}
                for r in rows
            ]

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
            self._prune_dead_keys(self._rate_limits, cutoff, now)
        return True

    @staticmethod
    def _prune_dead_keys(table: dict[str, list[float]], cutoff: float,
                          now: float) -> None:
        """Drop keys whose window has fully elapsed.

        pair-start / pair-claim are public endpoints, so any source IP or
        device_id can create a key; without pruning the dicts grow without
        bound for the process lifetime.
        """
        if len(table) <= _MAX_LIMIT_KEYS:
            return
        for k in [k for k, v in table.items() if not any(t > cutoff for t in v)]:
            del table[k]

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
            self._prune_dead_keys(self._claim_failures, cutoff, now)
            return len(failures) >= CONFIRM_LOCKOUT_THRESHOLD

    # ── device signature auth (HMAC pair-start) ──

    def verify_device_signature(self, device_id: str, mac: str,
                                 timestamp: int, nonce: str,
                                 signature_b64: str) -> bool:
        """Verify a pair-start device signature.

        Signed payload (fixed byte order): ``MAC(6 bytes) || timestamp(ASCII)
        || nonce(ASCII)`` under ``derived_key = HMAC-SHA256(MASTER_KEY,
        device_id)``, then base64-encoded. Enforces a ±30s timestamp window
        and a 5-minute in-memory nonce replay cache. Returns False on any
        failure; never raises.
        """
        master_key = settings.master_key
        if not master_key or len(master_key) < 32:
            log.error(
                "device signature rejected: MASTER_KEY not configured or <32 bytes"
            )
            return False

        # Timestamp window: reject both stale and far-future timestamps.
        now = int(time.time())
        if abs(now - timestamp) > TIMESTAMP_WINDOW_SEC:
            log.warning(
                "signature failed: timestamp out of window device_id=%s delta=%d",
                device_id, now - timestamp,
            )
            return False

        # Per-device derived key: HMAC-SHA256(MASTER_KEY, device_id).
        derived_key = hmac.new(
            master_key.encode(), device_id.encode(), hashlib.sha256
        ).digest()

        # Parse MAC: must be 12 hex chars decoding to exactly 6 bytes.
        try:
            mac_bytes = bytes.fromhex(mac)
        except (ValueError, TypeError):
            log.warning(
                "signature failed: bad MAC hex device_id=%s mac=%r", device_id, mac
            )
            return False
        if len(mac_bytes) != 6:
            log.warning(
                "signature failed: MAC is not 6 bytes device_id=%s mac=%r",
                device_id, mac,
            )
            return False

        # Reconstruct the exact payload the device signed.
        payload = mac_bytes + str(timestamp).encode() + nonce.encode()

        try:
            provided = base64.b64decode(signature_b64)
        except Exception:  # noqa: BLE001 — never raise at the auth boundary
            log.warning(
                "signature failed: undecodable base64 device_id=%s", device_id
            )
            return False
        expected = hmac.new(derived_key, payload, hashlib.sha256).digest()
        if not hmac.compare_digest(expected, provided):
            log.warning("signature failed: HMAC mismatch device_id=%s", device_id)
            return False

        # Nonce replay protection. Cache key is scoped per device so the same
        # nonce from two different devices does not collide.
        nonce_key = f"{device_id}:{nonce}"
        with self._lock:
            now_f = time.time()
            # Evict entries older than the replay window (bounded cache).
            self._nonce_cache = {
                k: t for k, t in self._nonce_cache.items()
                if now_f - t < NONCE_CACHE_TTL_SEC
            }
            if nonce_key in self._nonce_cache:
                log.warning("signature failed: nonce replay device_id=%s", device_id)
                return False
            self._nonce_cache[nonce_key] = now_f

        log.info("device signature verified device_id=%s", device_id)
        return True

    def check_whitelist(self, device_id: str) -> bool:
        """Return True if device_id is allowed to pair.

        ``settings.allowed_device_ids`` is a comma-separated string; an empty
        value means every device is accepted. Surrounding whitespace on
        entries is ignored.
        """
        raw = settings.allowed_device_ids
        if isinstance(raw, str):
            allowed = [d.strip() for d in raw.split(",") if d.strip()]
        else:  # tolerate a list/tuple if configured that way
            allowed = [str(d).strip() for d in raw if str(d).strip()]
        if not allowed:
            return True
        return device_id in allowed

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
