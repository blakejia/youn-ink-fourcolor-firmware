/**
 * @file page_sync.h
 * @brief Canvas Loop page group sync (Rust implementation).
 *
 * Implemented in `rust/src/page_sync.rs`. The bitmaps live in PSRAM and the
 * panel is driven through the shared framebuffer. The device is duty-cycled:
 * there is no poll loop — each wake runs one `page_sync_sync_once`, then
 * repaints only when the RTC panel record says the glass is out of date.
 */
#ifndef PAGE_SYNC_H
#define PAGE_SYNC_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PAGE_SYNC_MAX_PAGES 5
#define PAGE_BITMAP_SIZE 30000

/** Inject the board display; also resolved through Board on demand. */
void page_sync_set_display(void* display);

/** Start the canvas. Duty-cycled: creates no task — each wake syncs once.
 *  Idempotent. */
void page_sync_start(void);

/** One sync: fetch the schedule and apply it; the page being painted is fetched by the paint path.
 *  Returns true on success. */
bool page_sync_sync_once(void);

/** Paint the server's current page unless the glass already shows it.
 *  Returns true when a refresh was actually spent. */
bool page_sync_paint_if_changed(void);

/** Seconds until the server's next page change; -1 = unknown/empty schedule. */
int32_t page_sync_next_wake_s(void);

/** Whether the last `page_sync_sync_once` succeeded. */
bool page_sync_sync_ok(void);

/** `policy.poll_interval_minutes * 60`. */
uint32_t page_sync_poll_s(void);

/** `policy.sleep_poll_interval_minutes * 60`. */
uint32_t page_sync_sleep_poll_s(void);

/** False while the server's sleep window is open. */
bool page_sync_screen_active(void);

/** Manual page step (wraps): a local override the next successful sync clears. */
void page_sync_next(void);
void page_sync_prev(void);

/** True while the canvas owns the panel. Lock-free: safe to call from a
 *  renderer that already holds the display mutex. */
bool page_sync_is_displaying(void);

/** True when the last schedule poll reached the server (status bar indicator). */
bool page_sync_server_reachable(void);

/**
 * Hand the panel to the UI/notification: the canvas stops drawing until
 * `page_sync_allow_display` or an internal resume.
 */
void page_sync_stop_display(void);

/** Let the canvas take the panel again (leaving Settings); repaints at once. */
void page_sync_allow_display(void);

#ifdef __cplusplus
}
#endif

#endif  // PAGE_SYNC_H
