/**
 * @file power.h
 * @brief Sleep/wake decision inputs and output (decided in Rust, `power.rs`).
 *
 * The C++ side gathers the inputs and calls `esp_sleep_*`; the policy itself
 * lives in Rust so it is covered by `cargo test`.
 */
#ifndef POWER_H
#define POWER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint8_t  mains;                 // 插着 USB/充电器
    uint8_t  notify_active;         // 通知在屏，或 /next 拉取在飞（均 hold 住不睡）
    uint8_t  busy;                  // SleepManager::CanSleepNow() == false
    uint8_t  sync_ok;               // 最近一次 schedule 拉取成功
    uint8_t  screen_active;         // 服务端 policy.screen_active
    uint8_t  on_canvas;             // 当前屏是画板
    uint8_t  _pad[2];
    uint64_t idle_ms;               // 距最后一次用户活动
    uint32_t grace_ms;              // NVS power:idle_grace_min * 60000
    uint32_t max_sleep_s;           // NVS power:max_sleep_min * 60
    uint32_t poll_s;                // policy.poll_interval_minutes * 60
    uint32_t sleep_poll_s;          // policy.sleep_poll_interval_minutes * 60
    uint32_t fail_streak;           // 连续同步失败次数
    int32_t  seconds_until_next_page;  // < 0 表示 null（空排期）
} rf_power_inputs_t;

typedef struct {
    uint8_t  sleep;                 // 0 = 保持清醒
    uint8_t  invalidate_panel;      // 睡前作废 RTC 面板记录
    uint8_t  _pad[2];
    uint32_t wake_s;                // sleep==1 时的定时唤醒秒数
    uint32_t stay_awake_ms;         // sleep==0 时下次再判定的间隔
} rf_power_decision_t;

void rf_power_decide(const rf_power_inputs_t* in, rf_power_decision_t* out);

#ifdef __cplusplus
}
#endif

#endif // POWER_H
