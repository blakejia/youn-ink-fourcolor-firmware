# rr=4 panic diagnosis evidence — 2026-09-24 (Task 0)

Task: `docs/superpowers/plans/2026-09-24-rust-policy-migration.md` Task 0.
Device: NOTE4C-3400FC (`zectrix-s3-epaper-4.2`). No firmware/server change in this task.
Read-only sources only: source inspection, `journalctl --user -u youn-ink-server`,
`SELECT` on `server/data/devices.db`. No serial port opened, no USB reset.

## 1. Source facts (reset path, verified by reading)

```text
C++ assert / ESP_ERROR_CHECK / Rust panic
→ abort/panic
→ esp_reset_reason() == ESP_RST_PANIC (4)
→ shim::rf_last_reset_reason()
→ page_sync::fetch_schedule()
→ ?rr=4
→ server journal and devices.db
```

- `firmware/main/main.cc:38-102` — boot logs `reset reason=%d`; only `ESP_RST_SW`
  gets the 500 ms deep-sleep bounce (`:51-58`); NVS init retries once then
  `ESP_ERROR_CHECK(ret)` (`:64-70`, abort on repeated NVS failure); PM configure
  failure falls back to DFS-only with `ESP_LOGW`, no abort (`:93-99`).
- `firmware/main/rust/src/lib.rs:31-38` — `#[panic_handler]` calls
  `shim::abort()`; any Rust panic becomes `abort()`, i.e. `rr=4` next boot.
- `firmware/main/rust/shim.cpp:102-103` — `rf_abort()` → `abort()`.
- `firmware/main/rust/shim.cpp:419-430` — `rf_last_reset_reason()` samples
  `esp_reset_reason()` once (first-call-wins) into `RTC_DATA_ATTR`
  `g_last_reset_reason`; retained across deep sleep / soft reset, cleared on
  power loss. Stale-`rr` caveat: after the first sample, later boots in the same
  power cycle keep reporting the old value.
- `firmware/main/rust/src/page_sync.rs:167-178` — `fetch_schedule()` builds
  `GET /api/pages/schedule?w=&a=&r=&g=&f=&er=&eb=&rr=` from RTC counters
  (`w`=wakes, `a`=awake_ms, `r`=radio_ms, `g`=http_gets, `f`=refresh-submit ms,
  `er`=panel refreshes, `eb`=panel busy ms, `rr`=cached reset reason).
- `firmware/main/boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc` fatal sites
  (all abort → `rr=4` candidates, none attributable from server evidence):
  `assert(buffer)` `:215`, `assert(prev_buffer)` `:219`, `assert(tx_buf)` `:224`,
  `assert(buffer_1)` `:233`, `assert(dirty_mutex)` `:251`,
  `assert(ret == ESP_OK)` on SPI polling transmit `:882`, `:894`, `:934`, `:946`,
  `ESP_ERROR_CHECK(spi_bus_initialize / spi_bus_add_device)` `:811-812`,
  `:847-848`, `ESP_ERROR_CHECK(free_ret)` `:792`, `:828`.

## 2. Observation window (journal, exact text)

Capture: `journalctl --user -u youn-ink-server --since '2026-09-24 12:00:00'
--no-pager -q` → `/tmp/note4c-rr4/server-journal.txt` (254 lines, 37357 bytes;
dated copies `server-journal-2026-09-24.txt`, `sqlite-2026-09-24.txt` in the
same dir; outside the repo, uncommitted). Times below are CST as logged
(`date`: CST; journal `Sep 24 HH:MM:SS` == CST).

Schedule + bitmap lines, verbatim (query strings ARE logged by this service):

```text
Sep 24 12:01:13 ... "GET /api/pages/schedule?w=3&a=0&r=0&g=3&f=1&er=3&eb=73017&rr=11 HTTP/1.1" 200 OK
Sep 24 12:01:16 ... "GET /api/pages/bitmap/4d51e115e0883a53a0aeb086dd0c7f2e.bin HTTP/1.1" 200 OK
Sep 24 12:17:14 ... "GET /api/pages/schedule?w=4&a=366220&r=366220&g=5&f=1&er=4&eb=97356&rr=11 HTTP/1.1" 200 OK
Sep 24 12:27:18 ... "GET /api/pages/schedule?w=5&a=369864&r=369864&g=6&f=1&er=4&eb=97356&rr=11 HTTP/1.1" 200 OK
Sep 24 12:30:07 ... "GET /api/pages/schedule?w=6&a=376818&r=376818&g=7&f=1&er=4&eb=97356&rr=11 HTTP/1.1" 200 OK
Sep 24 12:30:10 ... "GET /api/pages/bitmap/750b3e1feaf134c02c2f36adeaf413db.bin HTTP/1.1" 200 OK
Sep 24 12:41:05 ... "GET /api/pages/schedule?w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4&v=4125&p=98&c=4 HTTP/1.1" 200 OK
Sep 24 12:41:08 ... "GET /api/pages/bitmap/30cb9f0a1849bbca4e7a4d4ea43f72af.bin HTTP/1.1" 200 OK
Sep 24 12:56:26 ... "GET /api/pages/schedule?w=3&a=204752&r=204752&g=4&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 13:00:04 ... "GET /api/pages/schedule?w=4&a=209806&r=209806&g=5&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 13:00:07 ... "GET /api/pages/bitmap/c774b0d36322591d491c9d942a7d1bb9.bin HTTP/1.1" 200 OK
Sep 24 13:10:59 ... "GET /api/pages/schedule?w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4&v=4125&p=98&c=4 HTTP/1.1" 200 OK
Sep 24 13:11:02 ... "GET /api/pages/bitmap/077a663b34d756e5beafabf3e1c6b6f7.bin HTTP/1.1" 200 OK
Sep 24 13:24:07 ... "GET /api/pages/schedule?w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4&v=4118&p=98&c=4 HTTP/1.1" 200 OK
Sep 24 13:24:10 ... "GET /api/pages/bitmap/077a663b34d756e5beafabf3e1c6b6f7.bin HTTP/1.1" 200 OK
Sep 24 13:33:01 ... "GET /api/pages/schedule?w=2&a=184213&r=184213&g=2&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 13:33:04 ... "GET /api/pages/bitmap/ed0dde94d562fd5aa850a0c1d2626d66.bin HTTP/1.1" 200 OK
Sep 24 13:43:52 ... "GET /api/pages/schedule?w=3&a=235631&r=235631&g=4&f=0&er=3&eb=73017&rr=4 HTTP/1.1" 200 OK
Sep 24 13:53:53 ... "GET /api/pages/schedule?w=4&a=239555&r=239555&g=5&f=0&er=3&eb=73017&rr=4 HTTP/1.1" 200 OK
Sep 24 14:00:09 ... "GET /api/pages/schedule?w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4&v=4116&p=97&c=4 HTTP/1.1" 200 OK
Sep 24 14:00:15 ... "GET /api/pages/bitmap/25e5a2a6173ffa1758bbae68c401b4f0.bin HTTP/1.1" 200 OK
Sep 24 14:13:16 ... "GET /api/pages/schedule?w=2&a=192177&r=192177&g=2&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 14:23:22 ... "GET /api/pages/schedule?w=3&a=196831&r=196831&g=3&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 14:30:02 ... "GET /api/pages/schedule?w=4&a=205896&r=205896&g=4&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
Sep 24 14:30:07 ... "GET /api/pages/bitmap/05fb9affe30d8fdfc27dc55bf814d6e5.bin HTTP/1.1" 200 OK
Sep 24 14:40:48 ... "GET /api/pages/schedule?w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4&v=4116&p=97&c=4 HTTP/1.1" 200 OK
Sep 24 14:40:51 ... "GET /api/pages/bitmap/99c5e639285405548a76e2f59aaa144a.bin HTTP/1.1" 200 OK
Sep 24 14:53:47 ... "GET /api/pages/schedule?w=2&a=185873&r=185873&g=2&f=0&er=2&eb=48678&rr=4 HTTP/1.1" 200 OK
```

Pattern, per (w, er, eb, rr) tuple:

- 5 cold starts in-window, all `w=1&a=0&r=0&g=0&f=0&er=0&eb=0&rr=4` with battery
  riding (`v≈4116-4125&p=97-98&c=4`): 12:41:05, 13:10:59, 13:24:07, 14:00:09,
  14:40:48 CST.
- EVERY `w=1` is followed within 3–6 s by a successful bitmap `200 OK`.
- Between cold starts, `er` climbs 0→2 (eb 0→48678) or 0→3 (eb 0→73017):
  completed panel waveforms are booked AFTER each `w=1` and BEFORE the next
  reset. The device survives multiple full EPD refresh cycles per power cycle.
- Before the first `rr=4`, four schedule GETs carry `rr=11` (12:01–12:30);
  `rr=11` is recorded as observed, not mapped (classic `esp_reset_reason_t`
  tops at 10; mapping needs an IDF header grep — open item, §5).
- `battery_history` corroborates each in-window `w=1` with a
  `(wakes=1, awake_ms=0, radio_ms=0, epd_refreshes=0, epd_busy_ms=0)` row at the
  matching epoch (1790224865, 1790226659, 1790227447, 1790229609, 1790232048),
  mv 4116–4125, pct 97–98, charge 4. One more all-zero cold-start row sits just
  outside the journal window at 11:24:13 CST (ts 1790220253, v=4114/p=97/c=4).
- `devices.power_counters` latest snapshot
  `{"wakes": 2, "awake_ms": 185873, "radio_ms": 185873, "http_gets": 2,
  "refresh_submit_ms": 0, "epd_refreshes": 2, "epd_busy_ms": 48678,
  "reset_reason": 4}` exactly matches the final journal line (14:53:47
  `w=2...er=2&eb=48678&rr=4`). It is a single latest snapshot:
  `battery_history` has no `reset_reason` column, so no per-request history can
  be reconstructed from SQLite alone.

## 3. Reproduction checklist (bounded, no code change, serial unopened)

1. Battery-only operation; USB disconnected — NOT performed in this task
   (device is remote; no physical access claimed).
2. No Web Serial or `/dev/ttyACM0` reader opened — held for the whole task.
3. Journal watched for schedule → bitmap → EPD activity — done, §2.
4. Next cold start `w=1` + `rr` — five recorded in-window, all `rr=4`.
5. Specific EPD line isolated — NO. No source line can be isolated from server
   evidence: the server never receives panic text, only HTTP 200s.

## 4. Conclusion (brief format)

```text
unresolved: ruled out per-request counter-history reconstruction from SQLite
(single latest snapshot, no reset_reason column in battery_history), ruled out
panic-site inference from rr=4 alone (no backtrace reaches the server; every
w=1 is followed by a successful bitmap 200 and er/eb increment 0->2..3 before
the next reset, so completed EPD waveforms bracket each reset); next probe is
a fixed 2-hour battery-only journal watch recording, for the next w=1, the
schedule->bitmap latency, whether er stays 0 (reset before the first refresh
completes) or increments (reset after successful EPD work), and the bitmap md5
vs the displayed page — if er still increments before every reset, the EPD
refresh path stays a non-isolable suspect and Task 6 remains blocked pending
an explicit user waiver.
```

Task 6 stays blocked: no positive attribution (`custom_lcd_display`, other C++,
Rust, or otherwise) is supported by the evidence. No new panic metadata ABI
was created in this task.

## 5. Open items / limitations

- `rr=11` (12:01–12:30 window) unmapped; needs IDF `esp_system.h` enum grep.
- `shim.cpp:420-423` comment cites raw RTC cause codes (`0xf=BROWNOUT`,
  `0x8=RTCWDT`) as if they were `esp_reset_reason_t` values; observed `rr`
  values (4, 11) are consistent with `esp_reset_reason()` returns, but the
  comment conflates the two numbering spaces — flag for a future docs touch.
- `devices` row shows `ip_address='testclient'` while `last_seen` equals the
  last live schedule line and counters match it exactly: snapshot treated as
  live-device state, IP field treated as untrusted.
- Journal starts 12:00:00 CST per the brief command; the 11:24:13 cold start is
  battery_history-only (no journal text).
