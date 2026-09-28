/* Log-upload decision ABI.
 *
 * Rust owns the whole table (switch / pending / network / backoff); C++ only
 * fills the facts. Layout is pinned by a contract test in
 * rust/tests/log_upload_policy.rs — keep the explicit padding.
 */
#ifndef LOG_UPLOAD_POLICY_H
#define LOG_UPLOAD_POLICY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint8_t  local_set;      /* rf_log_upload_opinion_t: device settings menu */
    uint8_t  server_set;     /* rf_log_upload_opinion_t: schedule policy */
    uint8_t  has_pending;
    uint8_t  wifi_ready;
    uint32_t pending_bytes;
    uint32_t pending_lines;
    uint32_t fail_streak;
    uint32_t _pad;
    int64_t  last_fail_s;    /* negative = none yet */
    int64_t  now_s;          /* negative = wall clock unset */
} rf_log_upload_inputs_t;    /* sizeof == 40 */

typedef struct {
    uint8_t  action;         /* rf_log_upload_action_t */
    uint8_t  _pad[3];
    uint32_t max_bytes;
} rf_log_upload_decision_t;  /* sizeof == 8 */

/* Both switches are three-state. "No opinion" is not "off": it is what lets
 * the other side decide. See rf_log_upload_resolve below. */
typedef enum {
    RF_LOG_OPINION_NONE = 0,
    RF_LOG_OPINION_OFF = 1,
    RF_LOG_OPINION_ON = 2,
} rf_log_upload_opinion_t;

typedef enum {
    RF_LOG_UPLOAD = 0,
    RF_LOG_SKIP_DISABLED = 1,
    RF_LOG_SKIP_EMPTY = 2,
    RF_LOG_SKIP_NO_NET = 3,
    RF_LOG_SKIP_BACKOFF = 4,
} rf_log_upload_action_t;

/* The conflict rule: an explicit service opinion wins, otherwise an explicit
 * local one, otherwise off. Exposed so C++ can render the same answer the
 * decision used. */
uint8_t rf_log_upload_resolve(uint8_t local_set, uint8_t server_set);
rf_log_upload_decision_t rf_log_upload_decide(const rf_log_upload_inputs_t* in);
uint32_t rf_log_upload_backoff_s(uint32_t streak, uint32_t base_s, uint32_t max_s);

#ifdef __cplusplus
}
#endif

#endif  /* LOG_UPLOAD_POLICY_H */
