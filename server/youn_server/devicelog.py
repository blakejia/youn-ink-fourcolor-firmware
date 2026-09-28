"""Device-side log mirror: append-only plain text, one file per device.

Layout on disk::

    data/devicelogs/
        NOTE4C-3400FC.log          current
        NOTE4C-3400FC.log.1        previous (rotation)

Each line is ``<server receive time> <device log line>``. The leading stamp is
the *server's* clock; the ``(27930)`` inside the body is the device's uptime in
milliseconds, not a wall clock. They are different quantities and the UI says so.

Rotation is done here rather than with ``RotatingFileHandler``: appends are the
only operation, so a rename-and-reopen is simpler than the handler's rollover
machinery, and it keeps the ``dropped`` marker on the same write path.
"""
from __future__ import annotations

import os
from typing import List, Tuple

from .config import settings

#: Refuse a device id that is not a safe single filename component. The id
#: reaches us from the device token, but a traversal here would write outside
#: device_log_dir, so it is checked rather than trusted.
_UNSAFE = ("/", "\\", "..", "\x00")

MAX_LINES_PER_REQUEST = 1000
_ROTATE_BYTES = 5 * 1024 * 1024
_ROTATE_KEEP = 3


def is_safe_device_id(device_id: str) -> bool:
    """Whether ``device_id`` is a single safe log-filename component.

    The HTTP layer (`_require_safe_device_id` in app.py) calls this *before*
    touching the path so an unsafe id is a 4xx, not a 500 from `_path_for`.
    Both must consult this one predicate — a second, almost-but-not-identical
    check in app.py is how a `..`-containing id once slipped through the guard
    and still 500ed in `_path_for`.
    """
    return bool(device_id) and not any(bad in device_id for bad in _UNSAFE)


def _rotate_if_needed(path) -> None:
    try:
        if path.stat().st_size < _ROTATE_BYTES:
            return
    except FileNotFoundError:
        return
    # Shift .N -> .N+1 from the top so the newest previous stays at .1.
    for n in range(_ROTATE_KEEP - 1, 0, -1):
        older = path.with_suffix(path.suffix + f".{n}")
        newer = path.with_suffix(path.suffix + f".{n + 1}")
        if older.exists():
            os.replace(older, newer)
    os.replace(path, path.with_suffix(path.suffix + ".1"))


def _path_for(device_id: str) -> "os.PathLike[str] | str":
    if not is_safe_device_id(device_id):
        raise ValueError("unsafe device id")
    return settings.device_log_dir / f"{device_id}.log"


def append_lines(device_id: str, received_iso: str, text: str, dropped: int) -> int:
    """Append ``text`` (one log line per \\n) stamped with ``received_iso``.

    Returns the number of non-empty device lines written. ``dropped`` > 0 first
    writes an explicit marker so a gap in the sequence is visible on disk
    instead of silent.
    """
    path = _path_for(device_id)
    settings.device_log_dir.mkdir(parents=True, exist_ok=True)
    _rotate_if_needed(path)

    lines = [ln for ln in text.split("\n") if ln.strip()]
    with open(path, "a", encoding="utf-8") as f:
        if dropped > 0:
            f.write(f"{received_iso} ... [dropped {dropped} lines]\n")
        for ln in lines:
            f.write(f"{received_iso} {ln}\n")
    return len(lines)


def tail_lines(device_id: str, n: int) -> Tuple[List[str], bool]:
    """Return the last ``n`` lines and whether older lines were omitted."""
    path = _path_for(device_id)
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            all_lines = f.read().splitlines()
    except FileNotFoundError:
        return [], False
    if n <= 0:
        return [], bool(all_lines)
    if len(all_lines) <= n:
        return all_lines, False
    return all_lines[-n:], True
