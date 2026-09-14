#pragma once

// The Settings menu's structure and navigation, from `rust/src/settings.rs`.
//
// The C++ renderer draws; it does not decide. Labels, section order, which rows
// exist and where the cursor may rest come from here — the renderer asks, then
// paints what it is told and highlights what `rf_settings_step` returns.
//
// Items are addressed by id. The renderer fills a row's value from the device
// (`SetItemValue`), and the application runs the effect the menu asks for
// (`SetItemHandler`) — the menu itself never restarts, sleeps or toggles.

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    RF_SETTINGS_KIND_ACTION = 0,  /* BOOT runs it */
    RF_SETTINGS_KIND_TOGGLE = 1,  /* BOOT flips it */
    RF_SETTINGS_KIND_INFO = 2,    /* read-out: shown, never selected */
} rf_settings_kind_t;

typedef enum {
    RF_SETTINGS_FOCUS_NAV = 0,      /* the cursor is on a section */
    RF_SETTINGS_FOCUS_OPTIONS = 1,  /* ... on a row of the current section */
} rf_settings_focus_t;

typedef enum {
    RF_SETTINGS_EFFECT_NONE = 0,
    RF_SETTINGS_EFFECT_ACTIVATE = 1, /* run item `effect_item` */
    RF_SETTINGS_EFFECT_TOGGLE = 2,   /* flip `effect_item` */
} rf_settings_effect_t;

typedef struct {
    const char* label;   /* NUL-terminated, static */
    uint8_t item_count;
    uint8_t _pad[7];
} rf_settings_section_t;

typedef struct {
    uint8_t id;
    uint8_t kind;        /* rf_settings_kind_t */
    uint8_t _pad[6];
    const char* label;
} rf_settings_item_t;

typedef struct {
    uint8_t section;     /* index into the sections */
    uint8_t focus;       /* rf_settings_focus_t */
    uint8_t option;      /* index into the section's items */
    uint8_t effect;      /* rf_settings_effect_t */
    uint8_t effect_item; /* item id for ACTIVATE / TOGGLE */
    uint8_t _pad[3];
} rf_settings_step_t;

uint8_t rf_settings_section_count(void);
void rf_settings_get_section(uint8_t index, rf_settings_section_t* out);
void rf_settings_get_item(uint8_t section, uint8_t index, rf_settings_item_t* out);

/* One click. `button` uses `rf_input_button_t`'s numbering (input.h): UP=0,
 * DOWN=1, BOOT=2. Long presses and the combo are routed elsewhere. */
void rf_settings_step(uint8_t section, uint8_t focus, uint8_t option, uint8_t button,
                      rf_settings_step_t* out);

/* Item ids. The C side switches on these to fill a value or run an effect. */
enum {
    RF_SETTINGS_ITEM_RESTART = 0,
    RF_SETTINGS_ITEM_RESET_NETWORK = 1,
    /* Retired as a row while 系统 hides 省电模式; the number stays reserved so
     * the ids below it never move. The application still handles it. */
    RF_SETTINGS_ITEM_SLEEP = 2,
    RF_SETTINGS_ITEM_WIFI_TOGGLE = 3,
    RF_SETTINGS_ITEM_WIFI_STATE = 4,
    RF_SETTINGS_ITEM_WIFI_IP = 5,
    RF_SETTINGS_ITEM_WIFI_SIGNAL = 6,
    RF_SETTINGS_ITEM_SERVER = 7,
    RF_SETTINGS_ITEM_WIFI_SSID = 8,
    /* An Action row: BOOT on it reveals (or re-masks) the password. */
    RF_SETTINGS_ITEM_WIFI_PASSWORD = 9,
    RF_SETTINGS_ITEM_WIFI_ERROR = 10,
    /* Wi-Fi credentials and the pairing: set up again from the provisioning
     * page, code and all. */
    RF_SETTINGS_ITEM_RESET_DEVICE = 11,
};

/* How many masking dots to draw for a password of `len` bytes; the renderer
 * picks the glyph. */
size_t rf_settings_masked_len(size_t len);

/* Does this id wipe something? The renderer colours the row with this. */
uint8_t rf_settings_is_destructive(uint8_t id);

/* Nothing is armed. */
#define RF_SETTINGS_CONFIRM_NONE 0xFF

typedef struct {
    uint8_t armed_id;     /* RF_SETTINGS_ITEM_* or RF_SETTINGS_CONFIRM_NONE */
    uint8_t act;          /* 1 = run the effect now */
    uint8_t _pad[6];
    uint64_t armed_at_ms; /* keep between calls */
} rf_settings_confirm_t;

/* One press on a destructive row: the first arms it, a second one inside the
 * window acts (`act` = 1). Pressing another row, or pressing after the window,
 * arms that row instead. The caller owns the two state fields. */
void rf_settings_confirm(uint8_t armed_id, uint64_t armed_at_ms, uint8_t pressed_id,
                         uint64_t now_ms, rf_settings_confirm_t* out);

#ifdef __cplusplus
}
#endif
