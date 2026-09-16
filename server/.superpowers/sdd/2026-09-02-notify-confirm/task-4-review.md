# Task 4 Review: 固件 — BSP 改名 + notify 模块 + 接线

- **Reviewer**: Task4Reviewer
- **Date**: 2026-09-02
- **Commit under review**: `163bbcd` (`feat(firmware): notification module + BSP GPIO wiring`)
- **Review method**: brief (plan lines 615–865) vs. report vs. diff, cross-checked against the actual firmware sources (`page_sync.{h,cc}`, `server_pairing.h`, `http_client_wrapper.{h,cc}`, `rawdraw_ui_manager`, board files). No build was re-run (project-wide validation is the main agent's job; the report's build log shows a clean link).

## Spec compliance: ✅

| Requirement (plan Task 4) | Status | Evidence |
|---|---|---|
| `firmware/main/common/notify.h` created, exact 6-function public API | ✅ | notify.h:14–55 matches plan Step 2 verbatim, plus the required `#include <stdbool.h>` fix the plan fragment omitted |
| `firmware/main/common/notify.cc` created, IDLE/FETCHING/NOTIFYING state machine | ✅ | notify.cc:43–46, transitions in `notify_request_next` / `fetch_task` / `dismiss_locked_state` |
| `notify_request_next` 异步，不阻塞按钮回调（plan 的 `// TODO: run in background task` 必须落地） | ✅ | notify.cc:230–240 — caller only flips state and `xTaskCreate(fetch_task, 8KB, prio 3)`; the HTTP GET runs in the background task; task-create failure falls back to IDLE |
| cJSON parse + base64 decode → 30000-byte framebuffer, safe path (mutex + memcpy + `RequestUrgentFullRefresh`) | ✅ | notify.cc:107–148 (`parse_next_response`, mbedtls_base64_decode with `olen == PAGE_BITMAP_SIZE` validation), notify.cc:49–72 (`show_bitmap`) — byte-for-byte the same safe path as `page_sync.cc::show_page` (page_sync.cc:196–199), incl. fb-size guard |
| BSP rename TODO_* → `UP_BUTTON_GPIO/DOWN_BUTTON_GPIO/CONFIRM_BUTTON_GPIO` = 39/18/0 | ✅ | config.h; zectrix-s3-epaper-4.2.cc `kBoardConfirmButtonGpio` now `CONFIRM_BUTTON_GPIO` (same value GPIO_NUM_0 as before — no behavior change) |
| application.cc 三分支 OnBootClick/OnUpClick/OnDownClick | ✅ | application.cc:412–434, 466–480 — precedence: notify active > page_sync displaying > rawdraw; `notify_init()` wired after `page_sync_start()` in `ServerPairingTaskTrampoline` |
| 5min FreeRTOS timer 对齐 ttl 300s | ✅ | notify.cc:39, 225–228 — one-shot timer created in `notify_init`, started on entering NOTIFYING, stopped on dismiss/timeout |
| CMakeLists.txt `common/notify.cc` | ✅ | firmware/main/CMakeLists.txt, right after `common/page_sync.cc` |
| Reuses `http_wrapper_get/post_json`, `server_pairing_get_token` — no new HTTP stack | ✅ | notify.cc:25–27 includes; token via `server_pairing_get_token`, URL via `server_pairing_build_endpoint` |
| Server contract alignment (Task 2) | ✅ | GET carries `device_id` query param (required by server, `app.py:458` per report — and the plan fragment omitted it); 204-no-content handled; POST body `{"decision":...}`; id is uuid.hex(32), fits `s_notification_id[40]` |

## Strengths

1. **The plan's placeholder was actually implemented, and better than sketched.** Plan Step 3 shipped `fetch_next()` synchronously in the button callback with a `// TODO: run in background task`. The implementation does the GET in a dedicated task (8KB stack, priority 3 — same profile as `page_sync`'s task, page_sync.cc:284) and also makes `notify_post_ack` asynchronous with a heap-passed body, so neither button callback ever blocks on HTTP.
2. **Buffer hygiene is right.** Response buffer and bitmap staging are PSRAM-allocated (45KB + 30KB off task stack), freed on every exit path including the alloc-failure early return; every `vTaskDelete` path is paired. Null-termination of the response is redundant-but-safe: `http_client_wrapper` already writes `out_buf[ctx.len]='\0'` *inside* the caller's capacity (http_client_wrapper.cc:36–45 caps at `buf_size`, and `ctx.len` can only reach `buf_size-1` because the cap check runs before the write), and notify.cc's clamped terminator lands on that same byte.
3. **EPD concurrency discipline followed.** `show_bitmap` never calls `DisplayRaw4ColorImage`; it takes `GetMutex()`, memcpys 30000B, releases, and calls `RequestUrgentFullRefresh()` — identical to `page_sync::show_page`, avoiding the known NOTE4C concurrent-refresh hang (documented in both files).
4. **Sensible contract discoveries baked in.** The `device_id` query param (mandatory per server, absent from the plan fragment) and 204-vs-200 handling show the implementer read the actual server code instead of coding to the plan's sketch.
5. **Honest report.** The "Concerns" section (no-snapshot restore, non-atomic state, no on-device verification) flags exactly the right residual risks rather than claiming more than was verified.

## Issues

### Critical
None.

### Important

1. **`OnBootClick` swallows the canvas-exit gesture** — application.cc:470–480. Previously, BOOT short-click with the canvas displayed propagated to rawdraw, where `photo_gallery` uses it to leave the canvas (`photo_gallery.cc:231` `kMemoryCardMode` → `page_sync_stop_display()`). Now every BOOT click while `page_sync_is_displaying()` fires `notify_request_next()` and returns, so **BOOT can never exit the canvas anymore** (long-press paths are unaffected). This matches the plan fragment literally (`if (!page_sync_is_displaying()) return; notify_request_next();` — the plan itself dropped the propagation), but it's a real UX regression introduced by this task. Fix suggestion: only fetch on BOOT when a notification is plausible, or keep canvas-exit on BOOT and move fetch to another gesture — needs a product call, hence "important, needs decision" rather than a code nit.

2. **204 branch never resets state** — notify.cc:162–164. `if (status == 204) { ESP_LOGI(...); }` falls through without `s_state = NotifyState::IDLE`, leaving the module stuck in FETCHING forever: all subsequent `notify_request_next()` calls early-return, and since NOTIFYING is never entered, the 5-min timer never fires to unstick it. One 204 (the common "nothing pending" case!) permanently bricks the feature until reboot. **The fix is one line** (`s_state = NotifyState::IDLE;` in the 204 branch). This would have been caught by the most basic on-device smoke test. (Confirmed present in the file on disk, not a diff-rendering artifact.)

3. **Cross-context races on `s_state` / `s_notification_id` / `s_lcd->GetMutex()`:**
   - `timeout_timer_cb` runs on the timer-service task and calls `dismiss_locked_state()` → `page_sync_prev()/next()` → `show_page()` → `xSemaphoreTake(lcd->GetMutex())` *from the timer daemon task*. FreeRTOS docs explicitly forbid blocking in timer callbacks; an 8-word-config-dependent stall in the daemon task would freeze every software timer in the system (sleep timer, etc.). Worst case is bounded (the mutex holder never sleeps), but the pattern is wrong: the timer callback should set a flag / notify a task instead of doing display work.
   - `ack_task` copies `s_notification_id` *after* `notify_dismiss()` cleared it (notify.cc:270 dismiss runs before the task is scheduled). First read wins only by scheduling luck; in practice the ack POST can go out with an empty id → `/api/notifications//ack` → 404. Pass the id in the task arg alongside the body instead of reading the global.
   - `s_state` is a plain enum read/written from button task, fetch task, and timer task. On Xtensa this is a single aligned word so torn reads aren't the issue — it's the TOCTOU sequences (e.g. timer callback's `if (s_state == NOTIFYING) dismiss` racing a fetch task's IDLE→NOTIFYING transition). Same risk class as page_sync's existing `s_displaying` (acknowledged in the report), but notify has *three* writers where page_sync has two.

### Minor

4. `notify_deinit()` (notify.cc:232–239) deletes the timer with block-time 0 and doesn't guard against a concurrent in-flight `fetch_task`/`ack_task` still touching module state afterward. Never called in practice (no caller wired), so latent only.
5. `s_lcd` is lazily bound via `Board::GetInstance().GetDisplay()` with an unchecked `static_cast<CustomLcdDisplay*>` (notify.cc:226–228). page_sync does the same cast, but page_sync receives the pointer through an explicit `page_sync_set_display()` injection point; notify reaches around to the Board singleton, coupling itself to that board's display type. A `notify_set_display()` mirroring page_sync would be cleaner and would have matched the "Consumes" list in the plan.
6. Response buffer math: 40000B base64 + metadata sits in a 45056B buffer — fine today, silently truncation-fails (decode length mismatch → drop notification) if the server ever pads the JSON. A comment noting the hard cap exists; acceptable.
7. `kFetchTaskStack` (8192) is reused for `ack_task` — fine, but the name no longer describes both uses. Cosmetic.

## Verdict: **request changes**

The module is well-built and exceeds the plan's sketch in most respects, but issue #2 (204 leaves FETCHING forever) is a one-line defect that disables the entire feature after the first empty poll, and issue #3's timer-callback display work is a real FreeRTOS misuse. Both are small, targeted fixes:

1. Add `s_state = NotifyState::IDLE;` to the 204 branch in `fetch_task`.
2. Make `timeout_timer_cb` non-blocking (defer dismiss to a task/flag).
3. Pass the notification id to `ack_task` as a task argument instead of reading the cleared global.
4. Decide the BOOT-vs-canvas-exit routing question (issue #1) — even if the answer is "plan said so, keep it", it should be a recorded decision, since it removes the only short-click exit from the canvas.

Build status (per report): clean link, 31% partition free. On-device end-to-end behavior (BOOT fetch → bitmap → ack → restore) remains unverified and should be part of the fix-round smoke test.
