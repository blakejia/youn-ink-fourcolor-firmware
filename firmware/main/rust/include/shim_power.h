/**
 * @file shim_power.h
 * @brief Duty-cycle plumbing: what is on the glass, how we woke, audio rail.
 *
 * The panel record lives in RTC memory because every deep-sleep wake is a cold
 * boot: without it the device cannot tell whether the frame it is about to
 * draw is already on the glass, and would pay a >= 15 s full refresh per wake.
 */
#ifndef SHIM_POWER_H
#define SHIM_POWER_H

#include <stdint.h>

#define RF_PANEL_MAGIC 0x50414E31u

#ifdef __cplusplus
extern "C" {
#endif

/* Layout is relied on by the host stub and the Rust reader: valid at offset
 * 4, md5 at offset 8, index at 44, sizeof == 48. Keep them in sync. */
typedef struct {
    uint32_t magic;              /* 0  */
    uint8_t  valid;              /* 4  */
    uint8_t  _pad[3];            /* 5  */
    char     displayed_md5[33];  /* 8  */
    int32_t  displayed_index;    /* 44 */
} rf_panel_record_t;             /* sizeof == 48 */

void rf_panel_record_get(rf_panel_record_t* out);
void rf_panel_mark_pending(const char* md5, int index);
void rf_panel_record_invalidate(void);

/* 0 = 其它, 1 = timer 唤醒, 2 = ext0(BOOT), 3 = ext1(充电插入) */
int  rf_wakeup_cause(void);

void rf_rails_audio(int on);   /* 音频 + 功放 */

#ifdef __cplusplus
}
#endif

#endif // SHIM_POWER_H
