"""Page group storage for the Canvas Loop feature.

Each page is a (name, canvas_json, duration_minutes, order) tuple. The
canvas_json is rendered server-side to a 400x300 2bpp BWRY bitmap and stored
content-addressed by its MD5. The device pulls:

  GET /api/pages/schedule -> list of (md5, duration, order) + schedule_md5
  GET /api/pages/bitmap/{md5}.bin -> raw bitmap bytes

Multiple page sources can share the same content (same bitmap bytes), so each
bitmap tracks the list of sources referencing it. Refcount == len(sources).

Layout on disk (under settings.data_dir):

    pages/
      {name}.json          # page source: name, canvas_json, duration_minutes, order
      {md5}.bin            # rendered bitmap (30000 bytes)
      {md5}.bmp.json       # bitmap metadata: sources[], rendered_at, refcount
      schedule.json        # last good schedule: schedule_md5, server_time, pages[]
"""
from __future__ import annotations

import hashlib
import json
import logging
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

from .config import settings

log = logging.getLogger(__name__)

# Sentinel name substring that bitmap meta files contain. Used to disambiguate
# from page source files when scanning the pages directory.
_BITMAP_META_SUFFIX = ".bmp.json"


# ─── domain types ─────────────────────────────────────────────────────
@dataclass
class PageSource:
    name: str
    canvas_json: dict
    duration_minutes: int
    order: int

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> "PageSource":
        return cls(
            name=str(d["name"]),
            canvas_json=d["canvas_json"],
            duration_minutes=int(d["duration_minutes"]),
            order=int(d["order"]),
        )


@dataclass
class PageEntry:
    """One slot in the schedule returned to devices."""
    md5: str
    duration_minutes: int
    order: int
    name: str

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> "PageEntry":
        return cls(
            md5=str(d["md5"]),
            duration_minutes=int(d["duration_minutes"]),
            order=int(d["order"]),
            name=str(d["name"]),
        )


# ─── helpers ──────────────────────────────────────────────────────────
def _page_path(name: str) -> Path:
    safe = "".join(c for c in name if c.isalnum() or c in "._-")
    if not safe:
        raise ValueError(f"invalid page name: {name!r}")
    return settings.data_dir / "pages" / f"{safe}.json"


def _bitmap_path(md5: str) -> Path:
    return settings.data_dir / "pages" / f"{md5}.bin"


def _bitmap_meta_path(md5: str) -> Path:
    return settings.data_dir / "pages" / f"{md5}{_BITMAP_META_SUFFIX}"


def _schedule_path() -> Path:
    return settings.data_dir / "pages" / "schedule.json"


def _is_bitmap_meta(p: Path) -> bool:
    return p.name.endswith(_BITMAP_META_SUFFIX)


def _is_page_source(p: Path) -> bool:
    return (
        p.suffix == ".json"
        and not p.name.endswith(_BITMAP_META_SUFFIX)
        and p.name != "schedule.json"
    )


def _all_page_sources() -> list[PageSource]:
    out: list[PageSource] = []
    for p in (settings.data_dir / "pages").glob("*.json"):
        if not _is_page_source(p):
            continue
        try:
            d = json.loads(p.read_text())
            out.append(PageSource.from_dict(d))
        except (json.JSONDecodeError, KeyError, ValueError) as e:
            log.warning("bad page source %s: %s", p, e)
    out.sort(key=lambda s: (s.order, s.name))
    return out


def _all_bitmap_metas() -> list[dict]:
    out: list[dict] = []
    for p in (settings.data_dir / "pages").glob("*.json"):
        if not _is_bitmap_meta(p):
            continue
        try:
            out.append(json.loads(p.read_text()))
        except json.JSONDecodeError:
            continue
    return out


# ─── CRUD ─────────────────────────────────────────────────────────────
def upsert_page(
    name: str,
    canvas_json: dict,
    duration_minutes: int,
    order: int,
    bitmap_bytes: bytes,
) -> PageEntry:
    """Save the source + bitmap, update refcounts, return the entry."""
    if len(bitmap_bytes) != 30000:
        raise ValueError(f"bitmap must be 30000 bytes, got {len(bitmap_bytes)}")

    md5 = hashlib.md5(bitmap_bytes).hexdigest()
    src = PageSource(name=name, canvas_json=canvas_json,
                     duration_minutes=duration_minutes, order=order)

    src_path = _page_path(name)
    src_path.parent.mkdir(parents=True, exist_ok=True)
    src_path.write_text(json.dumps(src.to_dict(), ensure_ascii=False, indent=2))

    bpath = _bitmap_path(md5)
    bmpath_meta = _bitmap_meta_path(md5)
    if not bpath.exists():
        bpath.write_bytes(bitmap_bytes)
        bmpath_meta.write_text(json.dumps({
            "md5": md5,
            "sources": [name],
            "rendered_at": int(time.time()),
            "refcount": 0,
        }))
    # A page belongs to exactly one bitmap: drop the name from every other
    # bitmap meta before recording the new reference. Without this, a page
    # replaced twice within one rendered_at second leaves two bitmaps
    # claiming it, and the schedule tie-break (max rendered_at, then md5)
    # can point the device at the superseded bitmap.
    for meta in _all_bitmap_metas():
        if meta.get("md5") != md5 and name in meta.get("sources", []):
            _drop_source_reference(meta["md5"], source_name=name)
    _record_source_reference(md5, source_name=name)
    log.info("page upserted name=%s md5=%s duration=%dmin order=%d",
             name, md5, duration_minutes, order)
    return PageEntry(md5=md5, duration_minutes=duration_minutes, order=order, name=name)


def delete_page(name: str) -> bool:
    """Remove the source page and decrement its bitmap refcount."""
    src_path = _page_path(name)
    if not src_path.exists():
        return False
    try:
        src = PageSource.from_dict(json.loads(src_path.read_text()))
    except (json.JSONDecodeError, KeyError, ValueError):
        src_path.unlink()
        return True
    md5_to_drop: Optional[str] = None
    for meta in _all_bitmap_metas():
        if name in meta.get("sources", []):
            md5_to_drop = meta["md5"]
            break
    src_path.unlink()
    if md5_to_drop:
        _drop_source_reference(md5_to_drop, source_name=name)
    log.info("page deleted name=%s bitmap=%s", name, md5_to_drop)
    return True


def list_pages() -> list[PageSource]:
    return _all_page_sources()


def get_bitmap(md5: str) -> Optional[bytes]:
    """Return raw 2bpp BWRY bytes for a content-addressed md5, or None."""
    p = _bitmap_path(md5)
    if not p.exists() or not p.is_file():
        return None
    if (settings.data_dir / "pages").resolve() not in p.resolve().parents:
        return None
    return p.read_bytes()


# ─── refcounting ──────────────────────────────────────────────────────
def _record_source_reference(md5: str, source_name: str) -> None:
    """Add `source_name` to the bitmap's source list and recompute refcount."""
    meta_path = _bitmap_meta_path(md5)
    if not meta_path.exists():
        return
    try:
        meta = json.loads(meta_path.read_text())
    except json.JSONDecodeError:
        return
    sources = list(meta.get("sources", []))
    if source_name not in sources:
        sources.append(source_name)
    meta["sources"] = sources
    meta["refcount"] = len(sources)
    meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2))


def _drop_source_reference(md5: str, source_name: str) -> None:
    """Remove `source_name` from the bitmap's source list; drop bitmap if empty."""
    meta_path = _bitmap_meta_path(md5)
    if not meta_path.exists():
        return
    try:
        meta = json.loads(meta_path.read_text())
    except json.JSONDecodeError:
        return
    sources = [s for s in meta.get("sources", []) if s != source_name]
    meta["sources"] = sources
    meta["refcount"] = len(sources)
    meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2))
    if not sources:
        try:
            _bitmap_path(md5).unlink()
        except FileNotFoundError:
            pass
        try:
            meta_path.unlink()
        except FileNotFoundError:
            pass
        log.info("bitmap dropped (refcount=0) md5=%s source=%s", md5, source_name)


# ─── schedule ─────────────────────────────────────────────────────────
def compute_schedule_md(entries: list[PageEntry]) -> str:
    """Hash of the (md5, duration, order) tuples. Stable across shuffles."""
    payload = sorted([(e.md5, e.duration_minutes, e.order) for e in entries])
    return hashlib.md5(json.dumps(payload, separators=(",", ":")).encode()).hexdigest()


def save_schedule(entries: list[PageEntry], schedule_md5: str) -> None:
    """Persist the last good schedule (idempotent; file overwritten)."""
    p = _schedule_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps({
        "schedule_md5": schedule_md5,
        "server_time": int(time.time()),
        "pages": [e.to_dict() for e in entries],
    }, ensure_ascii=False, indent=2))


def load_schedule() -> Optional[dict]:
    """Return last persisted schedule, or None."""
    p = _schedule_path()
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except json.JSONDecodeError:
        return None


def build_schedule_from_disk() -> list[PageEntry]:
    """Snapshot the current source pages into schedule entries."""
    out: list[PageEntry] = []
    for src in _all_page_sources():
        candidates: list[tuple[float, str]] = []
        for meta in _all_bitmap_metas():
            if src.name in meta.get("sources", []):
                candidates.append((meta.get("rendered_at", 0), meta["md5"]))
        if not candidates:
            continue
        candidates.sort(reverse=True)
        md5 = candidates[0][1]
        out.append(PageEntry(
            md5=md5, duration_minutes=src.duration_minutes,
            order=src.order, name=src.name,
        ))
    out.sort(key=lambda e: e.order)
    return out


def schedule_position(entries: list[PageEntry], now_ts: float) -> tuple[int, Optional[int]]:
    """Which page should be showing at `now_ts`, and how long it has left.

    The cycle repeats from the Unix epoch, so both the server and the device can
    derive the same answer from any wall clock without storing state. Durations
    are clamped to >= 1 minute: a zero-minute page would make the cycle zero.
    """
    durations = [max(e.duration_minutes, 1) * 60 for e in entries]
    cycle_s = sum(durations)
    if cycle_s <= 0:
        return 0, None
    pos_s = int(now_ts) % cycle_s
    acc = 0
    for i, d in enumerate(durations):
        if pos_s < acc + d:
            return i, acc + d - pos_s
        acc += d
    return len(durations) - 1, 60  # 不可达；兜底避免返回 None 之外的东西


# Run once at import to heal any drift from earlier crashes.
(settings.data_dir / "pages").mkdir(parents=True, exist_ok=True)