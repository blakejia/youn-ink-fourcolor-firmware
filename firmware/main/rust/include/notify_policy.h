/**
 * @file notify_policy.h
 * @brief Notification pull/rate/consume policy inputs and outputs
 *         (decisions live in Rust, `notify_policy.rs`).
 *
 * C++ owns HTTP transport, response buffers, `.bin`/JSON file writes, the
 * fetch/ack task lifecycle and the panel. This header defines the ABI by
 * which C++ asks Rust whether to pull now, how to classify a response and
 * what to do with the parsed body.
 *
 * The decision table is a compatibility port of the existing `notify.rs`
 * behaviour — not a redesign:
 *
 *   - Pull gate mirrors `request_next` (`state != IDLE -> return`) plus the
 *     RunPowerCycle skips, with EPD-busy deferral instead of pulling into a
 *     refresh wave.
 *   - Response classes mirror `fetch_once`'s `match status` on the `.bin`
 *     path (200 binary / 204 empty / 404 -> JSON fallback / other error) and
 *     `handle_next`'s split on the JSON path; negative status is a
 *     transport failure in both.
 *   - Consume mirrors the show/skip logic: a parseable body with a fresh id
 *     shows, a re-offered id/etag skips (never re-shown), an unparseable
 *     body retries with backoff.
 *
 * New gates (rate limit + failure backoff) are additive: defaults keep the
 * current unconditional-fetch behaviour, and an unset clock (`now_s < 0`)
 * skips the time gates so cold boot still fetches.
 */
#ifndef NOTIFY_POLICY_H
#define NOTIFY_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Notify states, mirroring `notify.rs` (IDLE / FETCHING / NOTIFYING). */
#define RF_NOTIFY_STATE_IDLE      0
#define RF_NOTIFY_STATE_FETCHING  1
#define RF_NOTIFY_STATE_NOTIFYING 2

/** Pull gate outcomes (`rf_notify_pull_output_t.action`). */
#define RF_NOTIFY_PULL_ACTION_PULL          0
#define RF_NOTIFY_PULL_ACTION_DEFER_BUSY    1
#define RF_NOTIFY_PULL_ACTION_SKIP_NOT_IDLE 2
#define RF_NOTIFY_PULL_ACTION_BACKOFF       3

/** Response classes from `rf_notify_classify_response`. */
#define RF_NOTIFY_RESPONSE_BINARY_NOTIFY    0
#define RF_NOTIFY_RESPONSE_JSON_NOTIFY      1
#define RF_NOTIFY_RESPONSE_EMPTY            2
#define RF_NOTIFY_RESPONSE_JSON_FALLBACK    3
#define RF_NOTIFY_RESPONSE_TRANSPORT_ERROR  4
#define RF_NOTIFY_RESPONSE_STATUS_ERROR     5

/** Consume outcomes from `rf_notify_decide_consume`. */
#define RF_NOTIFY_CONSUME_SHOW                0
#define RF_NOTIFY_CONSUME_SKIP_DUPLICATE      1
#define RF_NOTIFY_CONSUME_RETRY_BACKOFF       2
#define RF_NOTIFY_CONSUME_NONE_EMPTY          3
#define RF_NOTIFY_CONSUME_FETCH_JSON_FALLBACK 4

/**
 * Facts for the pull gate. C++ fills this from the notify module state, the
 * SleepManager busy vote and its own persisted stamps; Rust is pure: no
 * cross-call state, no reads of globals.
 *
 * Layout contract with `notify_policy.rs` (`#[repr(C)]` + a Rust test
 * asserting offsets/size): `uint8_t` state at 0, `uint8_t` epd_busy at 1,
 * 6 pad bytes, then four 8-byte slots (now/last_pull/last_failure as
 * int64_t, fail_streak + three uint32_t settings), 48 bytes total. Keep
 * field order, types and count in step with the Rust struct.
 */
typedef struct {
    /** RF_NOTIFY_STATE_*: only IDLE ever pulls. */
    uint8_t  state;
    /** Panel refresh in flight (SleepManager Display busy vote): defer. */
    uint8_t  epd_busy;
    /** Padding to align `now_s` on an 8-byte boundary. */
    uint8_t  _pad[6];
    /** Current wall clock, seconds; negative = unset (skip time gates). */
    int64_t  now_s;
    /** Last pull attempt, seconds; negative = none yet. */
    int64_t  last_pull_s;
    /** Last failed pull, seconds; negative = none yet. */
    int64_t  last_failure_s;
    /** Consecutive pull failures (RTC-persisted like the sync streak). */
    uint32_t fail_streak;
    /** Minimum seconds between pulls (0 = disabled). */
    uint32_t min_interval_s;
    /** Backoff base seconds (streak 1 -> base, then doubles). */
    uint32_t base_backoff_s;
    /** Backoff ceiling seconds. */
    uint32_t max_backoff_s;
} rf_notify_pull_inputs_t;

/**
 * Pull gate result. `wait_s` is the seconds to wait before retrying when
 * `action == RF_NOTIFY_PULL_ACTION_BACKOFF`; 0 otherwise.
 */
typedef struct {
    uint8_t  action;   /**< RF_NOTIFY_PULL_ACTION_*. */
    uint8_t  _pad[3];
    uint32_t wait_s;
} rf_notify_pull_output_t;

/**
 * Facts for response classification: the HTTP status C++ already has plus
 * which endpoint produced it. 8 bytes; layout asserted by the Rust test.
 */
typedef struct {
    int32_t  status;       /**< HTTP status, or a negative transport status. */
    uint8_t  binary_path;  /**< Response came from the `.bin` endpoint. */
    uint8_t  _pad[3];
} rf_notify_response_facts_t;

/**
 * Decide whether to pull now (and how long to wait otherwise).
 *
 * Pure function: outputs depend only on `inp`.
 */
void rf_notify_pull_decide(const rf_notify_pull_inputs_t* inp,
                           rf_notify_pull_output_t* out);

/** Classify a /next response: RF_NOTIFY_RESPONSE_*. */
uint8_t rf_notify_classify_response(const rf_notify_response_facts_t* f);

/**
 * Consume decision for a classified response: RF_NOTIFY_CONSUME_*.
 * @param parsed    1 = C++ decoded a usable bitmap + id.
 * @param duplicate 1 = id/etag matches the already-consumed notification.
 */
uint8_t rf_notify_decide_consume(uint8_t response_class, uint8_t parsed,
                                 uint8_t duplicate);

/**
 * Backoff window in seconds for a failure streak:
 * `min(base * 2^(streak-1), max)`; streak 0 means no failure outstanding.
 */
uint32_t rf_notify_backoff_delay_s(uint32_t streak, uint32_t base_s,
                                   uint32_t max_s);

/** Next streak after one pull outcome (success resets, failure increments). */
uint32_t rf_notify_record_result(uint8_t ok, uint32_t streak);

/**
 * Read the RTC-backed pull-gate stamps: last attempt (s), last failure (s)
 * and failure streak. `-1` stamps mean "never". C++ (shim.cpp) owns the
 * storage; null pointers are ignored.
 */
void rf_notify_gate_stats(int64_t* last_pull_s, int64_t* last_failure_s,
                          uint32_t* streak);

/**
 * Persist one pull outcome: stamp the attempt, and — when `failed` is
 * nonzero — the failure stamp. `streak` is the step already computed by
 * `rf_notify_record_result` (Rust owns the ladder, C++ owns the bytes).
 */
void rf_notify_gate_record(int64_t now_s, uint8_t failed, uint32_t streak);

#ifdef __cplusplus
}
#endif

#endif  /* NOTIFY_POLICY_H */
