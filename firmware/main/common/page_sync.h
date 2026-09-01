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

/** 上/下翻页：手动切换 canvas 页（循环），重置当前页计时，进入 30s 手动保持（暂停自动轮换） */
void page_sync_next(void);
void page_sync_prev(void);

/** 画板当前是否在全屏显示（供按钮路由判断） */
bool page_sync_is_displaying(void);

/** 退出画板显示（恢复 UI 主导屏幕），画布内容仍缓存 */
void page_sync_stop_display(void);

/** 重新进入画板显示（显示当前页） */
void page_sync_resume_display(void);

#ifdef __cplusplus
}
#endif

#endif  // PAGE_SYNC_H
