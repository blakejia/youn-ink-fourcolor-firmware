/**
 * @file pairing.h
 * @brief Pairing decision inputs and output (decided in Rust, `pairing.rs`).
 *
 * The C++ side owns HTTP, NVS and the screen; the protocol policy — when to ask
 * for a code, when to poll, when to back off, when to give up — lives in Rust so
 * `cargo test` covers it. Those rules have broken before: the old loop could
 * only `return true`, so its give-up path was unreachable, and signing with an
 * unsynced clock spent pair-start slots until the server rate-limited the
 * device into a loop.
 */
#ifndef PAIRING_H
#define PAIRING_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* What the previous action produced. Mirrors `pairing::Outcome`; the numbers
 * are asserted by a Rust test, so do not renumber without changing both. */
typedef enum {
    RF_PAIR_OUTCOME_NONE = 0,
    RF_PAIR_OUTCOME_CLOCK_WAITED = 1,
    RF_PAIR_OUTCOME_PAIR_STARTED = 2,
    RF_PAIR_OUTCOME_PAIR_START_FAILED = 3,
    RF_PAIR_OUTCOME_CLAIM_PENDING = 4,
    RF_PAIR_OUTCOME_CLAIM_GRANTED = 5,
    RF_PAIR_OUTCOME_CLAIM_REJECTED = 6,
    RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR = 7,
    RF_PAIR_OUTCOME_TOKEN_WRITE_FAILED = 8,
} rf_pairing_outcome_t;

typedef struct {
    uint8_t  has_code;              // 屏上有一个尚未作废的配对码
    uint8_t  clock_ok;              // 墙钟可信（SNTP 已同步）
    uint8_t  _pad[2];
    uint32_t window_elapsed_s;      // 当前 5 分钟窗口已过去多少秒
    uint32_t pair_start_failures;   // 连续 pair-start / 时钟失败次数
    uint8_t  last;                  // rf_pairing_outcome_t
    uint8_t  _pad2[3];
} rf_pairing_inputs_t;

/* What to do next. */
typedef enum {
    RF_PAIR_ACTION_PAIR_START = 0,  // POST pair-start
    RF_PAIR_ACTION_CLAIM = 1,       // POST pair-claim
    RF_PAIR_ACTION_WAIT = 2,        // 睡 delay_ms 后再判定（时钟不可信）
    RF_PAIR_ACTION_PAIRED = 3,      // 终止：token 已落盘
    RF_PAIR_ACTION_GIVE_UP = 4,     // 终止：离开配对页
} rf_pairing_action_t;

typedef struct {
    uint8_t  action;                // rf_pairing_action_t
    uint8_t  drop_code;             // 作废屏上的码并重置窗口
    uint8_t  _pad[2];
    uint32_t delay_ms;              // 动作前等待
} rf_pairing_decision_t;

void rf_pairing_decide(const rf_pairing_inputs_t* in, rf_pairing_decision_t* out);

#ifdef __cplusplus
}
#endif

#endif // PAIRING_H
