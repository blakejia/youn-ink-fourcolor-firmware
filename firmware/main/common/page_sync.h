/**
 * @file page_sync.h
 * @brief Canvas Loop 页组同步（最小测试版：RAM 位图缓存 + 定时轮换显示）
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

typedef struct {
    char md5[33];
    uint32_t duration_seconds;
    int order;
    bool on_ram;
    uint8_t* bitmap;
} page_sync_entry_t;

void page_sync_set_display(void* display);
void page_sync_start(void);
const uint8_t* page_sync_current_bitmap(void);

#ifdef __cplusplus
}
#endif

#endif  // PAGE_SYNC_H
