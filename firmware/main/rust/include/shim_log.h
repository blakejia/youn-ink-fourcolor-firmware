/* Device log ring buffer ABI.
 *
 * The buffer lives in `.rtc_noinit`, NOT `.rtc.data`. This is the whole point:
 * `cpu_start.c` memsets `.rtc_bss` on every non-deep-sleep reset (which is why
 * `w=1` followed the rr=4 panics), while `.rtc_noinit` is NOLOAD and nothing in
 * IDF clears it. So pre-crash lines survive a panic without any panic-path code.
 *
 * Buffer magic guards the other direction: after a power cycle `.rtc_noinit`
 * holds garbage, so a mismatched magic declares the buffer empty.
 */
#ifndef SHIM_LOG_H
#define SHIM_LOG_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Line framing inside the ring: [seq:u32][len:u16][text:len].
 * `text` is the fully formatted log line (level, tag and uptime included),
 * produced by one vsnprintf in the vprintf hook. No tag/level fields: LOG V1
 * bakes them into the format string (see esp_log_format.h). */

/* Copy the oldest un-acked contiguous run into `out` (NUL-terminated).
 * Does NOT advance the tail: only rf_logbuf_ack does, so a failed upload
 * leaves the lines for the next attempt. */
void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines);

/* Advance the tail past every line with seq <= seq_hi. Call only after the
 * server accepted the payload. */
void rf_logbuf_ack(uint32_t seq_hi);

/* `dropped`: lines lost to ring overwrite since the last power cycle.
 * `used`: bytes currently held. */
void rf_logbuf_stats(uint32_t* dropped, uint32_t* used);

/* Install the esp_log_set_vprintf hook (idempotent). Keeps forwarding to the
 * previous hook so the serial console still prints. */
void rf_logbuf_install_hook(void);

#ifdef __cplusplus
}
#endif

#endif  /* SHIM_LOG_H */
