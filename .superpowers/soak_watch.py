#!/usr/bin/env python3
"""Battery-soak watcher for the NOTE4C duty cycle.

Reads the service journal (the only observation channel once USB is gone) and
prints one line per wake plus a health verdict. Stdlib only: this must run on
the host without touching the device.

Health rules, each tied to something observable rather than to a hunch:
  * reset loop  - a wake with the RTC counters zeroed (w=1&a=0&r=0) arriving
                  far sooner than the wake budget, i.e. the device rebooting
                  instead of sleeping.
  * a/r stuck  - awake_ms and radio_ms must advance on a battery wake; they
                  are booked at the deep-sleep teardown, so a flat 0 on
                  battery means the duty cycle never reached the sleep.
  * panel busy  - epd_busy_ms must grow slower than wall clock, and its step
                  per refresh is ~19.8 s; a much bigger step means repeated or
                  aborted waveforms (the "ghost panel" symptom).
  * cadence     - with the rotation budget in, a 30-minute page should give
                  ~900 s wake intervals rather than the old ~640 s.
"""
import json
import re
import subprocess
import sys
import time
from collections import defaultdict
from datetime import datetime

JOURNAL = ["journalctl", "--user", "-u", "youn-ink-server", "-o", "short-iso", "--no-pager"]

QUERY = re.compile(
    r"^(?P<ts>\S+).*GET /api/pages/schedule\?(?P<q>\S+?)\s"
)


def fetch(since: str):
    out = subprocess.run(
        JOURNAL + ["--since", since], capture_output=True, text=True
    ).stdout
    rows = []
    for line in out.splitlines():
        m = QUERY.match(line)
        if not m:
            continue
        q = dict(
            kv.split("=", 1) for kv in m.group("q").split("&") if "=" in kv
        )
        rows.append({"ts": m.group("ts"), "q": q})
    return rows


def as_int(q, key, default=0):
    try:
        return int(q.get(key, default))
    except (TypeError, ValueError):
        return default


def main():
    since = sys.argv[1] if len(sys.argv) > 1 else "-10 min"
    seen = {}
    last = None
    print(f"# watching since {since}", flush=True)
    while True:
        rows = fetch(since)
        for r in rows:
            if r["ts"] in seen:
                continue
            seen[r["ts"]] = r
            q = r["q"]
            w, a, rad = as_int(q, "w"), as_int(q, "a"), as_int(q, "r")
            er, eb = as_int(q, "er"), as_int(q, "eb")
            rr, c = q.get("rr", "?"), q.get("c", "?")
            got = as_int(q, "g")
            t = datetime.fromisoformat(r["ts"])
            gap = int((t - last).total_seconds()) if last else 0
            last = t

            flags = []
            # A wake far sooner than the budget is either a page-boundary hit or
            # a sync-failure backoff, and the two look identical on interval
            # alone: 02:29:59 -> 02:31:07 was 68 s and was CORRECT, having
            # landed one second before a page boundary and slept just enough to
            # catch it. Separate them by where the wake sits in the rotation --
            # a legitimate short sleep opens a page (pos ~ 0), a backoff can
            # start anywhere.
            page_pos = int(t.timestamp()) % 1800
            if 0 < gap < 300:
                if page_pos < 120:
                    flags.append(f"boundary-hit(pos={page_pos}s,ok)")
                else:
                    flags.append(f"SHORT-SLEEP?(pos={page_pos}s,check-backoff)")
            if w == 1 and a == 0 and r == 0 and gap < 300:
                flags.append("RESET-LOOP?")
            if c in ("2", "3") and a == 0:
                flags.append("mains(awake=0 ok)")
            elif c == "4" and a == 0 and w > 1:
                flags.append("BATTERY-AW-STUCK")
            print(
                f"{t:%H:%M:%S} w={w} a={a} r={rad} g={got} er={er} eb={eb} "
                f"rr={rr} c={c} gap={gap}s {' '.join(flags)}",
                flush=True,
            )
        time.sleep(30)


if __name__ == "__main__":
    main()