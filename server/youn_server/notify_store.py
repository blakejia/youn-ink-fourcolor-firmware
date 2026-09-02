"""FIFO notification queue persisted as JSONL (append-only)."""
from __future__ import annotations

import json
import threading
import time
import uuid
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Optional

from .config import settings


@dataclass
class Notification:
    id: str
    device_id: str
    title: str
    body: str
    created_at: float
    ttl_sec: int
    status: str              # pending | shown | acked | expired | error
    decision: Optional[str]  # agree | reject | None
    acked_at: Optional[float]

    def is_expired(self, now: float | None = None) -> bool:
        now = now or time.time()
        return now >= self.created_at + self.ttl_sec


class NotifyStore:
    def __init__(self, path: Path | None = None):
        self._path = path or (settings.data_dir / "notifications.jsonl")
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self._items: dict[str, Notification] = {}
        self._load()

    def _load(self) -> None:
        if not self._path.exists():
            return
        for line in self._path.read_text().splitlines():
            try:
                d = json.loads(line)
                self._items[d["id"]] = Notification(**d)
            except (json.JSONDecodeError, KeyError):
                continue

    def _append(self, n: Notification) -> None:
        with self._lock:
            with self._path.open("a") as f:
                f.write(json.dumps(asdict(n), ensure_ascii=False) + "\n")

    def enqueue(self, device_id: str, title: str, body: str,
                ttl_sec: int = 300) -> Notification:
        n = Notification(
            id=uuid.uuid4().hex,
            device_id=device_id,
            title=title,
            body=body,
            created_at=time.time(),
            ttl_sec=ttl_sec,
            status="pending",
            decision=None,
            acked_at=None,
        )
        with self._lock:
            self._items[n.id] = n
        self._append(n)
        return n

    def next_for(self, device_id: str) -> Optional[Notification]:
        """Return earliest pending, non-expired notification; mark shown atomically."""
        now = time.time()
        with self._lock:
            for n in sorted(self._items.values(), key=lambda x: x.created_at):
                if n.device_id != device_id or n.status != "pending":
                    continue
                if n.is_expired(now):
                    n.status = "expired"
                    continue
                n.status = "shown"
                self._append(n)
                return n
        return None

    def ack(self, notification_id: str, decision: str) -> Optional[Notification]:
        with self._lock:
            n = self._items.get(notification_id)
            if n is None:
                return None
            if n.status == "acked":
                return n  # idempotent
            n.status = "acked"
            n.decision = decision
            n.acked_at = time.time()
            self._append(n)
            return n

    def recent(self, device_id: str = "", limit: int = 20) -> list[Notification]:
        items = self._items.values()
        if device_id:
            items = (n for n in items if n.device_id == device_id)
        return sorted(items, key=lambda x: x.created_at, reverse=True)[:limit]


_store: NotifyStore | None = None


def get_store() -> NotifyStore:
    global _store
    if _store is None:
        _store = NotifyStore()
    return _store
