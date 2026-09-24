/**
 * @file protocol_parse.h
 * @brief Protocol field classification inputs and outputs (decisions live
 *         in Rust, `protocol_parse.rs`).
 *
 * Rust receives C++-extracted facts / short strings and returns fixed
 * parsed structs; it never touches a JSON DOM, HTTP/TLS, or files.
 *
 * Scope is the genuinely remaining seams (see the Task 6 report): schedule
 * entry field classification (`md5` length + `duration_minutes`
 * presence/value), policy `*_minutes` scaling, and HTTP status mapping.
 * Already Rust-owned and deliberately untouched here: wifi endpoint parsing
 * (`wifi_policy.rs`), pairing classification (`pairing_response.rs`),
 * notify response classification + body parsing (`notify_policy.rs` /
 * `notify.rs`), battery activity policy (`battery_activity_policy.rs`).
 * OTA manifest: no JSON manifest parsing exists anywhere in the firmware
 * (only a URL string passthrough), so there is nothing to move.
 *
 * Compatibility: entries without a usable md5/duration are skipped, never
 * fatal; minutes fall back instead of stopping polling; negative status is
 * a transport error, never a server answer.
 */
#ifndef PROTOCOL_PARSE_H
#define PROTOCOL_PARSE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** md5 length a schedule entry must carry (32 hex chars). */
#define RF_PROTOCOL_SCHEDULE_MD5_LEN 32

/** Upper bound for policy minutes (mirrors page_sync MAX_POLL_MINUTES). */
#define RF_PROTOCOL_MAX_POLL_MINUTES 1440

/** HTTP status business classes. */
#define RF_PROTOCOL_HTTP_OK               0 /**< 200: usable body */
#define RF_PROTOCOL_HTTP_EMPTY            1 /**< 204: no content */
#define RF_PROTOCOL_HTTP_NOT_FOUND        2 /**< 404: old-server fallback signal */
#define RF_PROTOCOL_HTTP_REJECTED         3 /**< 401/429: auth/rate rejected */
#define RF_PROTOCOL_HTTP_TRANSPORT_ERROR  4 /**< negative: network failure, never a server answer */
#define RF_PROTOCOL_HTTP_ERROR            5 /**< everything else */

/**
 * Facts for one schedule `pages[]` entry.
 *
 * Layout contract with `protocol_parse.rs` (`#[repr(C)]` + a Rust test
 * asserting offsets/size): `uint32_t` md5_len at 0, `uint8_t`
 * has_duration at 4, 3 pad bytes, `int64_t` duration_minutes at 8,
 * 16 bytes total. Keep field order, types and count in step.
 */
typedef struct {
    /** Byte length of the entry's md5 value (32 = usable). */
    uint32_t md5_len;
    /** 1 = `duration_minutes` was present with an integer value. */
    uint8_t  has_duration;
    uint8_t  _pad[3];
    /** The `duration_minutes` value (if `has_duration`). */
    int64_t  duration_minutes;
} rf_protocol_schedule_entry_facts_t;

/**
 * Entry decision (`usable == 0` = skip this entry, never fail the whole
 * response).
 *
 * Layout contract: `uint8_t` usable at 0, 3 pad bytes, `uint32_t`
 * duration_s at 4, 8 bytes total. Rust test asserts the offsets.
 */
typedef struct {
    /** 1 = usable (md5 well-formed AND duration present). */
    uint8_t  usable;
    uint8_t  _pad[3];
    /** `duration_minutes.max(0) * 60` (if `usable`). */
    uint32_t duration_s;
} rf_protocol_schedule_entry_decision_t;

/** Classify one schedule entry (Rust, pure). */
rf_protocol_schedule_entry_decision_t rf_protocol_schedule_entry(
    const rf_protocol_schedule_entry_facts_t* facts);

/** Scale a policy `*_minutes` value to seconds (Rust, pure). */
uint32_t rf_protocol_policy_minutes_to_s(uint8_t present, int64_t minutes,
                                         uint32_t fallback_s);

/** Map an HTTP status to a business class (Rust, pure). */
uint8_t rf_protocol_http_class(int32_t status);

#ifdef __cplusplus
}
#endif

#endif  /* PROTOCOL_PARSE_H */
