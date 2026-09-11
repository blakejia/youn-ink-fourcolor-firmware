/**
 * @file page_sync.h
 * @brief Canvas Loop page group sync (Rust implementation).
 *
 * Implemented in `rust/src/page_sync.rs`. The bitmaps live in PSRAM, the page
 * table is polling-based and the panel is driven through the shared framebuffer.
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

/** Start the polling task. Idempotent. */
void page_sync_start(void);

/** Manual page step (wraps); resets the current page timer. */
void page_sync_next(void);
void page_sync_prev(void);

/** True while the canvas owns the panel. Lock-free: safe to call from a
 *  renderer that already holds the display mutex. */
bool page_sync_is_displaying(void);

/** True when the last schedule poll reached the server (status bar indicator). */
bool page_sync_server_reachable(void);

/**
 * Hand the panel to the UI/notification: the canvas stops drawing (but keeps
 * polling) until `page_sync_allow_display` or an internal resume.
 */
void page_sync_stop_display(void);

/** Let the canvas take the panel again (leaving Settings); repaints at once. */
void page_sync_allow_display(void);

#ifdef __cplusplus
}
#endif

#endif  // PAGE_SYNC_H
