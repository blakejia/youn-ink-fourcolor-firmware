/**
 * @file settings_renderer.h
 * @brief Settings page renderer for rawdraw mode
 *
 * A two-pane page: a left nav of sections and, on the right, the rows of the
 * one the cursor is on. Both come from `rust/src/settings.rs`; this file draws
 * them and reports clicks, it does not decide what the menu holds or where the
 * cursor may rest.
 *
 * The C side supplies only what the device knows: a row's value, a toggle's
 * state, and what to run when a row is confirmed.
 */

#ifndef RAWDRAW_SETTINGS_RENDERER_H
#define RAWDRAW_SETTINGS_RENDERER_H

#include "page_renderer.h"
#include "rawdraw/style.h"
#include "rawdraw/theme.h"
#include "settings_menu.h"
#include <map>
#include <vector>
#include <string>
#include <functional>

namespace rawdraw {

/**
 * @brief Settings row kind. Mirrors rf_settings_kind_t's intent for drawing:
 * toggles get a switch, actions a highlighted value, read-outs a plain value.
 */
enum class SettingsItemType {
    Normal,    ///< Read-out: label left, value right
    Checkbox,  ///< Toggleable checkbox
    Action,    ///< Action button (highlighted style)
};

/**
 * @brief One row as the drawing code wants it
 */
struct SettingsItemDef {
    std::string label;
    std::string value;
    SettingsItemType type = SettingsItemType::Normal;
    bool checked = false;
    /// Wipes something: drawn in the danger colour, and the application asks
    /// twice before running it.
    bool danger = false;
};

/**
 * @brief Settings page renderer
 */
class SettingsRenderer : public PageRenderer {
public:
    SettingsRenderer();
    ~SettingsRenderer() override;

    // PageRenderer interface
    void Init(int width, int height) override;
    void Render(uint8_t* fb, int width, int height) override;
    bool HandleInput(const ButtonEvent& event) override;

    // Data interface
    /// Fill a row's value (SSID, IP, RSSI, reachability, action hints).
    void SetItemValue(uint8_t id, const std::string& value);
    /// Set a toggle's state.
    void SetItemChecked(uint8_t id, bool checked);
    /// Run what the menu asked for; the application owns the effects.
    ///
    /// `target` is the state to move a toggle row *to* — the model computes it
    /// from what the row drew — and `true` for an action row, which ignores it.
    /// It is not a "this was a toggle" flag: a handler that has to infer a
    /// direction from the connection cannot answer a press on an OFF row while
    /// the radio is down, and the row reads as a dead key.
    void SetItemHandler(std::function<void(uint8_t id, bool target)> handler) {
        item_handler_ = std::move(handler);
    }
    /// Which section the cursor is on.
    uint8_t GetFocusedSection() const { return section_; }

    /// Reveal (or re-mask) the Wi-Fi password row. The row's value holds the
    /// real key; this only decides whether the dots are drawn.
    void TogglePasswordReveal();

    /// How long a revealed password stays readable before it masks itself.
    static constexpr int64_t kPasswordRevealUs = 30LL * 1000 * 1000;

    void SetFirmwareVersion(const char* version) { firmware_version_ = version; }
    void SetDeviceInfo(const char* mac, const char* chip) {
        mac_address_ = mac;
        chip_model_ = chip;
    }

private:
    /// Ask the model what the current section holds and mirror it into
    /// `items_`, so the drawing below stays a pure function of that view.
    void SyncItemsFromModel();
    std::string ValueFor(uint8_t id) const;

    /// Render a single settings item as a card row
    void RenderItem(uint8_t* fb, int width, int y, int content_left,
                    int index, bool selected, int row_h);

    /// The current section's rows, rebuilt from the model on every render.
    std::vector<SettingsItemDef> items_;
    uint8_t section_ = 0;
    uint8_t focus_ = 0;
    uint8_t option_ = 0;
    std::map<uint8_t, std::string> values_;
    std::map<uint8_t, bool> checks_;
    std::function<void(uint8_t, bool)> item_handler_;

    /// Index of the password row in the current section, or -1 when the
    /// section has none. Recomputed with `items_`.
    int password_row_ = -1;
    bool password_revealed_ = false;
    int64_t password_revealed_at_us_ = 0;

    std::string firmware_version_;
    std::string mac_address_;
    std::string chip_model_;

    const lv_font_t* font_ = nullptr;
    const lv_font_t* title_font_ = nullptr;
    const lv_font_t* icon_font_ = nullptr;
    const lv_font_t* value_font_ = nullptr;

    // Right pane capacity: how many rows were once shown at a time. Kept as the
    // single scroll-window threshold so rows can be added without hunting.
    static constexpr int kVisibleOptionCount = 8;
};

}  // namespace rawdraw

#endif  // RAWDRAW_SETTINGS_RENDERER_H
