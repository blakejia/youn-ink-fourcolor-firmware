/**
 * @file lifecycle.h
 * @brief Lifecycle transition legality (decided in Rust, `lifecycle.rs`).
 *
 * TransitionLifecycle is a publish channel: fifteen call sites announce the
 * phase their module entered, and none of them consults the current state. What
 * was missing is a contract on which announcements are legal, so a wrong one is
 * visible instead of turning into a device that behaves oddly for reasons nobody
 * can see. This says nothing about a lifecycle that never moves — a missing
 * transition is a liveness question and needs a watchdog, not a verdict.
 */
#ifndef LIFECYCLE_H
#define LIFECYCLE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Same numbering as enum LifecycleState in device_state.h. */
typedef enum {
    RF_LIFECYCLE_NO_CHANGE = 0,
    RF_LIFECYCLE_OK = 1,
    RF_LIFECYCLE_SUSPICIOUS = 2,
} rf_lifecycle_verdict_kind_t;

typedef struct {
    uint8_t kind;               /* rf_lifecycle_verdict_kind_t */
    uint8_t _pad[7];
    const char *message;        /* SUSPICIOUS only, NUL-terminated; else NULL */
} rf_lifecycle_verdict_t;

/** `from`/`to` are LifecycleState values. */
void rf_lifecycle_verdict(uint8_t from, uint8_t to, rf_lifecycle_verdict_t *out);

#ifdef __cplusplus
}
#endif

#endif // LIFECYCLE_H
