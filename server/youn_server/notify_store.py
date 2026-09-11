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

# 内存中保留的通知上限（超出时淘汰最旧的终态条目；pending 永不淘汰）
_MAX_ITEMS = 500


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

    def _sweep(self, now: float) -> None:
        """过期标记 + 有界内存（调用方须持有 self._lock）。

        旧实现只在 next_for 里对 pending 做过期判定，置 shown 之后的项
        既不会变 expired 也没有任何清理路径 → _items 与 notifications.jsonl
        单调增长（dismiss 不发 ack 的项尤其明显）。
        """
        for n in self._items.values():
            if n.status in ("pending", "shown", "error") and n.is_expired(now):
                n.status = "expired"
        if len(self._items) <= _MAX_ITEMS:
            return
        # 只淘汰终态的最旧条目，pending（待确认）永不淘汰。
        removable = sorted(
            (n for n in self._items.values() if n.status != "pending"),
            key=lambda x: x.created_at,
        )
        for n in removable[: len(self._items) - _MAX_ITEMS]:
            self._items.pop(n.id, None)
        self._compact()

    def _compact(self) -> None:
        """按内存状态重写 JSONL，丢弃已淘汰条目（调用方须持有 self._lock）。"""
        tmp = self._path.with_suffix(".jsonl.tmp")
        with tmp.open("w") as f:
            for n in sorted(self._items.values(), key=lambda x: x.created_at):
                f.write(json.dumps(asdict(n), ensure_ascii=False) + "\n")
        tmp.replace(self._path)

    def get(self, notification_id: str) -> Optional[Notification]:
        with self._lock:
            return self._items.get(notification_id)

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
            self._sweep(n.created_at)
        self._append(n)
        return n

    def next_for(self, device_id: str) -> Optional[Notification]:
        """Return earliest pending, non-expired notification; mark shown atomically."""
        now = time.time()
        with self._lock:
            self._sweep(now)
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
            self._sweep(time.time())
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

    def mark_error(self, notification_id: str) -> Optional[Notification]:
        """Mark a notification as 'error' (e.g. render failed) and persist.

        Public counterpart to the private ``_items[id].status = "error"``
        pattern callers used to write inline: this one takes the store lock
        and writes through ``_append`` so the status change survives a
        process restart instead of being lost in memory.
        """
        with self._lock:
            n = self._items.get(notification_id)
            if n is None:
                return None
            if n.status == "acked":
                return n  # terminal; don't overwrite acked state
            n.status = "error"
            self._append(n)
            return n

    def recent(self, device_id: str = "", limit: int = 20) -> list[Notification]:
        with self._lock:
            self._sweep(time.time())
            items = list(self._items.values())
        if device_id:
            items = [n for n in items if n.device_id == device_id]
        return sorted(items, key=lambda x: x.created_at, reverse=True)[:limit]


_store: NotifyStore | None = None


def get_store() -> NotifyStore:
    global _store
    if _store is None:
        _store = NotifyStore()
    return _store
