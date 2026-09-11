/**
 * @file input.h
 * @brief Button routing inputs and output (decided in Rust, `input.rs`).
 *
 * Which of the popup, the canvas or the UI owns a gesture is a precedence
 * table, and it lives in Rust so `cargo test` covers it. The C++ side gathers
 * the facts (what is on the panel, which page the UI is on, whether the config
 * AP is up) and performs the answer; it owns no part of the ordering.
 */
#ifndef INPUT_H
#define INPUT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    RF_INPUT_UP = 0,
    RF_INPUT_DOWN = 1,
    RF_INPUT_BOOT = 2,
} rf_input_button_t;

typedef enum {
    RF_INPUT_CLICK = 0,
    RF_INPUT_DOUBLE_CLICK = 1,
    RF_INPUT_LONG_PRESS = 2,
    RF_INPUT_COMBO_LONG_PRESS = 3, /* UP and DOWN held together */
} rf_input_gesture_t;

typedef struct {
    uint8_t button;                 /* rf_input_button_t */
    uint8_t gesture;                /* rf_input_gesture_t */
    uint8_t notify_active;          /* a notification is on the panel */
    uint8_t canvas_displaying;      /* the canvas is what is drawn */
    uint8_t on_settings;            /* the UI's current page is Settings */
    uint8_t previous_is_settings;   /* leaving Settings would go nowhere */
    uint8_t config_mode;            /* the provisioning AP is up */
    uint8_t provisioning;           /* lifecycle is ApProvision */
} rf_input_inputs_t;

/* Keep in step with the RF_INPUT_ACTION_* constants in input.rs; a Rust test
 * asserts the numbering. */
typedef enum {
    RF_INPUT_ACTION_IGNORE = 0,
    RF_INPUT_ACTION_UI_INPUT = 1,
    RF_INPUT_ACTION_NOTIFY_ACK = 2,
    RF_INPUT_ACTION_NOTIFY_DISMISS = 3,
    RF_INPUT_ACTION_NOTIFY_FETCH_NEXT = 4,
    RF_INPUT_ACTION_CANVAS_PREV = 5,
    RF_INPUT_ACTION_CANVAS_NEXT = 6,
    RF_INPUT_ACTION_ENTER_SETTINGS = 7,
    RF_INPUT_ACTION_LEAVE_SETTINGS = 8,
    RF_INPUT_ACTION_ENTER_WIFI_CONFIG = 9,
    RF_INPUT_ACTION_EXIT_WIFI_CONFIG = 10,
    RF_INPUT_ACTION_STOP_CANVAS = 11,
} rf_input_action_t;

typedef struct {
    uint8_t action;                     /* rf_input_action_t */
    uint8_t agree;                      /* NOTIFY_ACK: 1 = agree */
    uint8_t drop_orphan_notification;   /* a popup has to come down on the way in */
    uint8_t enter_settings;             /* ENTER/EXIT/STOP: the entry is allowed */
    uint8_t switch_to_previous;         /* LEAVE_SETTINGS: the page switch is allowed
                                           (0 = hand the panel back and stay put) */
} rf_input_decision_t;

void rf_input_decide(const rf_input_inputs_t* in, rf_input_decision_t* out);

#ifdef __cplusplus
}
#endif

#endif // INPUT_H
