/**
 * @file settings_renderer.cc
 * @brief Modernized settings page renderer implementation - v3.8.0 visual fix
 *
 * F1 FIXES:
 * - 文字和横线不重叠：横线 y = item_y + item_h - 1，文字在 item 中央偏上
 * - 选中效果：黑色背景矩形 + 白色文字
 * - 版本号从 PROJECT_VER 宏获取，不硬编码
 * - WiFi 图标使用正确码位 "\xee\xa4\x80"/"\xee\xa4\x81"
 *
 * Design for 400x300 1bpp ePaper:
 * - Title bar with subtle border
 * - Card-based rows with borders and padding
 * - Icon (left) + Label (middle) + Value/Indicator (right)
 * - Checkbox for toggleable items
 * - Selected items have inverted colors
 * - No section headers in visual layout
 */

#include "settings_renderer.h"
#include "input.h"
#include "rawdraw/layout_utils.h"
#include "rawdraw/rawdraw.h"
#include "rawdraw/style.h"
#include "rawdraw/theme.h"
#include "rawdraw/font_engine.h"
#include <esp_timer.h>
#include <algorithm>
#include <cstdio>
#include <ctime>
#include <string>

// Version from CMakeLists.txt PROJECT_VER
#ifndef PROJECT_VER
#define PROJECT_VER "3.8.0"
#endif
#ifndef IDF_VER
#define IDF_VER "unknown"
#endif

// External font references
extern const lv_font_t SourceHanSansSC_Regular_slim;
extern const lv_font_t SourceHanSansSC_Medium_slim;
extern const lv_font_t fa_settings_16;

namespace rawdraw {

namespace {

constexpr const char* kIconBluetooth = "\xef\x8a\x93";  // U+F293 Bluetooth-b

// ---------------------------------------------------------------------------
// Settings page layout tuning (single source of truth)
// Adjust these values first when fine-tuning visual density/alignment.
// ---------------------------------------------------------------------------      // Smaller cards: subtract from slot height        // Content safe inset from card top     // Content safe inset from card bottom
constexpr int kTextOpticalNudgeY = 0;       // Extra text nudge after ink-box centering       // Extra icon nudge after baseline compensation
constexpr int kValueOpticalNudgeY = 0;      // Extra value nudge after baseline compensation    // Clear mask tightly follows dialog border         // About dialog info row height             // About dialog row spacing          // Local clear pad around modal dialogs
constexpr int kDialogClearRadiusBoost = 0;  // Clear radius equals dialog radius
constexpr int kSettingsNavDividerX = 90;
constexpr int kSettingsNavItemH = 44;
constexpr int kSettingsTableRowH = 34;
// Settings content starts immediately below the global status/menu bar.
// Keep this small so the 400x300 screen can show five compact rows without
// visually drifting into the bottom shell area.
constexpr int kSettingsContentTopGap = 8;
constexpr int kSettingsTableTop = Style::kStatusBarHeight + kSettingsContentTopGap;
// Diagnostic overlay for real-device layout calibration.
// Set to false, or comment out the DrawSettingsLayoutDebugOverlay() call near
// the end of SettingsRenderer::Render(), to remove this extra top layer.

Color TokenInkOnPaper(ThemeToken token) {
    const PaintStyle style = ThemeManager::Get().Style(token);
    // Accent/Danger/Progress tokens are full paint styles: fg is intended for
    // text drawn on their colored bg. On white paper, the visible semantic ink
    // is the colored bg/border instead.
    if (style.bg != WHITE) return style.bg;
    if (style.border != WHITE) return style.border;
    return style.fg;
}

std::string FitTextToWidth(const std::string& text, const lv_font_t* font, int max_width) {
    if (!font || max_width <= 0 || text.empty()) return "";
    if (MeasureTextWidth(text.c_str(), font) <= max_width) return text;

    static const std::string kEllipsis = "...";
    const int ellipsis_w = MeasureTextWidth(kEllipsis.c_str(), font);
    if (ellipsis_w >= max_width) return "";

    std::string fitted;
    const char* p = text.c_str();
    while (*p) {
        const char* start = p;
        uint32_t ch = utf8_next(&p);
        if (ch == 0) break;

        std::string next = fitted;
        next.append(start, p - start);
        next += kEllipsis;
        if (MeasureTextWidth(next.c_str(), font) > max_width) break;
        fitted.append(start, p - start);
    }

    if (fitted.empty()) return "";
    fitted += kEllipsis;
    return fitted;
}







[[maybe_unused]] void DrawSettingsVectorIcon(uint8_t* fb, int width, const std::string& label,
                                             int x, int center_y, Color color) {
    // FontAwesome 16px icons from fa_settings_16 font
    // Map label to FA Unicode character
    const char* icon_char = nullptr;
    
    if (label == "系统") {
        icon_char = "\xef\x80\x93";  // U+F013 gear
    } else if (label == "网络" || label == "Wi-Fi") {
        icon_char = "\xef\x87\xab";  // U+F1EB wifi
    } else if (label == "功能" || label == "语音唤醒") {
        icon_char = "\xef\x82\xad";  // U+F0AD wrench
    } else if (label == "时钟显示") {
        icon_char = "\xef\x80\x97";  // U+F017 clock
    } else if (label == "存储" || label == "存储空间") {
        icon_char = "\xef\x87\x80";  // U+F1C0 database
    } else if (label == "关于") {
        icon_char = "\xef\x81\x9a";  // U+F05A info-circle
    } else if (label == "音量") {
        icon_char = "\xef\x80\xa8";  // U+F028 volume-up
    } else if (label == "电池方向") {
        icon_char = "\xef\x89\x80";  // U+F240 battery-full
    } else if (label == "重启") {
        icon_char = "\xef\x80\xa1";  // U+F021 sync/refresh (旋转)
    } else if (label == "关机") {
        icon_char = "\xef\x80\x91";  // U+F011 power-off
    } else if (label == "日期格式") {
        icon_char = "\xef\x81\xb3";  // U+F073 calendar
    } else if (label == "AI对话长度") {
        icon_char = "\xef\x81\xb5";  // U+F075 comment
    } else if (label == "同步间隔" || label == "同步记录") {
        icon_char = "\xef\x80\xa1";  // U+F021 sync/refresh
    } else if (label == "服务") {
        icon_char = "\xef\x88\xb3";  // U+F233 server
    } else if (label == "服务地址") {
        icon_char = "\xef\x81\x81";  // U+F041 map-marker (定位)
    } else if (label == "蓝牙") {
        icon_char = kIconBluetooth;
    } else {
        icon_char = "\xef\x81\x9a";  // fallback: info-circle
    }
    
    // Use ink-box centering (baseline math) for icon positioning
    // Same as text - icon glyphs have box_h/ofs_y that differ from line_height
    const int top_y = InkCenteredTextTopY(&fa_settings_16, icon_char, center_y, 0);
    DrawText(fb, width, x, top_y, icon_char, &fa_settings_16, color);
}







}  // namespace

SettingsRenderer::SettingsRenderer()
    : font_(&SourceHanSansSC_Regular_slim)
    , title_font_(&SourceHanSansSC_Medium_slim)
    , icon_font_(&fa_settings_16)
    , value_font_(&SourceHanSansSC_Regular_slim) {
}

SettingsRenderer::~SettingsRenderer() {}

void SettingsRenderer::Init(int width, int height) {
    width_ = width;
    height_ = height;
    needs_full_refresh_ = true;
    section_ = 0;
    focus_ = RF_SETTINGS_FOCUS_NAV;
    option_ = 0;
    // F1: Use PROJECT_VER macro, not hardcoded version
    if (firmware_version_.empty()) {
        firmware_version_ = "v" PROJECT_VER;
    }
}


















void SettingsRenderer::Render(uint8_t* fb, int width, int height) {
    if (!fb) return;

    SyncItemsFromModel();
    const auto& theme = ThemeManager::Get();
    const PaintStyle bg_style = theme.Style(ThemeToken::BackgroundPrimary);
    const PaintStyle text_style = theme.Style(ThemeToken::TextPrimary);
    const PaintStyle border_style = theme.Style(ThemeToken::Border);
    const PaintStyle selected_style = theme.Component(ComponentRole::SettingsSelected);
    const int body_top = Style::kStatusBarHeight;
    const int body_bottom = height - 3;
    const int content_x = kSettingsNavDividerX + 16;
    const int content_right = width - 20;
    const int row_h = kSettingsTableRowH;

    DrawStyledRect(fb, width, {0, body_top, width, body_bottom - body_top}, bg_style);
    DrawVLine(fb, width, kSettingsNavDividerX,
              body_top + kSettingsContentTopGap, body_bottom - 1, border_style.border);

    const uint8_t section_count = rf_settings_section_count();
    if (section_ >= section_count) section_ = 0;
    rf_settings_section_t current_sec = {};
    rf_settings_get_section(section_, &current_sec);

    const int nav_top = body_top + kSettingsContentTopGap;
    for (uint8_t i = 0; i < section_count; ++i) {
        const int sy = nav_top + i * kSettingsNavItemH;
        // The pill marks the section the cursor is on. With the cursor in the
        // rows below, the row rail is the only highlight, so which pane has
        // the focus is never ambiguous.
        const bool selected = (i == section_);
        rf_settings_section_t sec = {};
        rf_settings_get_section(i, &sec);
        const char* label = sec.label ? sec.label : "";
        const int nav_pill_x = 16;
        const int nav_pill_w = kSettingsNavDividerX - 26;
        const int nav_pill_h = 28;
        const int nav_pill_y = sy + (kSettingsNavItemH - nav_pill_h) / 2;
        const int icon_x = nav_pill_x + 7;
        const int icon_center_y = sy + kSettingsNavItemH / 2;  // Same center as label
        const int label_x = nav_pill_x + 27;
        const int label_y = InkCenteredTextTopY(font_, label, icon_center_y, kTextOpticalNudgeY);
        if (selected) {
            DrawStyledRoundRect(fb, width, height, {nav_pill_x, nav_pill_y, nav_pill_w, nav_pill_h},
                                Style::kBorderRadiusMD, selected_style);
        }
        const Color nav_fg = selected ? selected_style.fg : text_style.fg;
        DrawSettingsVectorIcon(fb, width, label, icon_x, icon_center_y, nav_fg);
        DrawText(fb, width, std::max(2, label_x), std::max(body_top + 2, label_y),
                 label, font_, nav_fg);
    }

    // A section with no rows of its own (关于) draws the about panel instead.
    const bool about_section = (current_sec.item_count == 0);
    if (about_section) {
        struct InfoRow {
            const char* label;
            std::string value;
        };
        const std::string version = firmware_version_.empty() ? "未知" : firmware_version_;
        const std::string serial = mac_address_.empty() ? "未读取" : mac_address_;
        const std::vector<InfoRow> rows = {
            {"设备名称", "notellm"},
            {"型号", "Youn-Beta1.0"},
            {"固件版本", version},
            {"硬件版本", chip_model_.empty() ? "ESP32-S3" : chip_model_},
            {"序列号", serial},
            {"官方网站", "blog.lazyyoun.xyz"},
        };
        int y = kSettingsTableTop;
        for (const auto& row : rows) {
            const int center_y = y + row_h / 2;
            const int label_x = content_x;
            DrawStyledText(fb, width, label_x, InkCenteredTextTopY(font_, row.label, center_y, kTextOpticalNudgeY),
                           row.label, font_, text_style);
            const std::string value = FitTextToWidth(row.value, value_font_,
                std::max(0, content_right - (label_x + 112)));
            const int value_w = MeasureTextWidth(value.c_str(), value_font_);
            DrawStyledText(fb, width, content_right - value_w,
                           InkCenteredTextTopY(value_font_, value.c_str(), center_y, kValueOpticalNudgeY),
                           value.c_str(), value_font_, text_style);
            y += row_h;
        }
    } else {
        const int total = static_cast<int>(items_.size());
        const int selected_pos = (total > 0) ? std::min<int>(option_, total - 1) : 0;
        const int visible_count = std::min(kVisibleOptionCount, total);
        int window_start = std::max(0, selected_pos - visible_count / 2);
        if (window_start + visible_count > total) {
            window_start = std::max(0, total - visible_count);
        }
        int y = kSettingsTableTop;
        const int available_h = std::max(row_h, body_bottom - kSettingsTableTop - 2);
        // Right-side settings rows are intentionally denser than the historical
        // five-row layout. 8 rows fit by sharing the available pane height, and
        // scrolling only starts once a section has more than eight options.
        const int option_row_h = std::min(row_h, std::max(28, available_h / std::max(1, visible_count)));
        for (int i = 0; i < visible_count; ++i) {
            const int item_index = window_start + i;
            RenderItem(fb, width, y, content_x, item_index,
                       focus_ == RF_SETTINGS_FOCUS_OPTIONS && item_index == selected_pos,
                       option_row_h);
            y += option_row_h;
        }

        if (total > kVisibleOptionCount) {
            const int track_x = width - 10;
            const int track_y = kSettingsTableTop + 2;
            const int track_h = std::max(24, option_row_h * visible_count - 4);
            DrawVLine(fb, width, track_x, track_y, track_y + track_h, border_style.border);
            const int thumb_h = std::max(10, track_h * visible_count / total);
            const int max_start = std::max(1, total - visible_count);
            const int thumb_y = track_y + (track_h - thumb_h) * window_start / max_start;
            DrawRect(fb, width, {track_x - 2, thumb_y, 4, thumb_h}, selected_style.border);
        }
    }

    needs_full_refresh_ = false;
}

void SettingsRenderer::RenderItem(uint8_t* fb, int width, int y,
                                   int content_left, int index, bool selected,
                                   int row_h) {
    const SettingsItemDef& item = items_[index];
    const int content_right = width - 20;
    const auto& theme = ThemeManager::Get();
    const PaintStyle text_style = theme.Style(ThemeToken::TextPrimary);
    const PaintStyle selected_style = theme.Component(ComponentRole::SettingsSelected);
    const Color action_color = TokenInkOnPaper(item.label == "关机" ? ThemeToken::Danger : ThemeToken::Accent);
    // The selected setting row uses a compact left rail instead of a full
    // filled background, so row content must stay readable on white paper.
    const Color fg_color = text_style.fg;
    const int row_center_y = y + row_h / 2;
    const int right_margin = 0;
    const int icon_x = content_left;
    const int label_x = icon_x + 16 + Style::kSpacingSM;  // 16 is icon width
    const int label_y = InkCenteredTextTopY(font_, item.label.c_str(), row_center_y, kTextOpticalNudgeY);

    if (selected) {
        // 1bpp fallback turns selected surfaces into light paper, so keep the
        // compact focus rail as solid ink for a clear cursor.
        DrawRect(fb, width, {content_left - 8, row_center_y - 8, 3, 16}, selected_style.border);
    }

    DrawSettingsVectorIcon(fb, width, item.label, icon_x, row_center_y, fg_color);

    int label_right = content_right - right_margin;

    if (item.type == SettingsItemType::Checkbox) {
        const int track_w = 52;
        const int track_h = 20;
        const int knob = 16;
        const int track_x = content_right - track_w;
        const int track_y = row_center_y - track_h / 2;
        label_right = track_x - Style::kSpacingLG;
        const PaintStyle switch_style = item.checked ? theme.Style(ThemeToken::Accent)
                                                     : theme.Style(ThemeToken::Disabled);
        DrawStyledRoundRect(fb, width, 300, {track_x, track_y, track_w, track_h},
                            Style::kBorderRadiusPill, switch_style);
        const char* switch_text = item.checked ? "ON" : "OFF";
        const int text_w = MeasureTextWidth(switch_text, value_font_);
        const int text_x = item.checked ? (track_x + 7) : (track_x + track_w - text_w - 6);
        DrawText(fb, width, text_x,
                 InkCenteredTextTopY(value_font_, switch_text, row_center_y, kValueOpticalNudgeY),
                 switch_text, value_font_, switch_style.fg);
        const int knob_x = item.checked ? (track_x + track_w - knob - 2) : (track_x + 2);
        const int knob_cx = knob_x + knob / 2;
        // Use a true circle rather than a rounded square. Hardware screenshots
        // showed square border pixels on the previous RectBorder-based knob.
        DrawCircle(fb, width, {knob_cx, row_center_y - 1}, knob / 2, theme.Style(ThemeToken::BackgroundPrimary).bg);
        DrawCircleBorder(fb, width, {knob_cx, row_center_y - 1}, knob / 2, 1, text_style.fg);
    } else if (!item.value.empty()) {
        const int value_right = content_right - right_margin;
        const int max_val_w = std::max(0, value_right - (content_left + 88));
        std::string display_value = FitTextToWidth(item.value, value_font_, max_val_w);
        const int value_w = MeasureTextWidth(display_value.c_str(), value_font_);
        const int val_x = value_right - value_w;
        label_right = val_x - Style::kSpacingLG;

        if (!display_value.empty()) {
            DrawText(fb, width, val_x, InkCenteredTextTopY(value_font_, display_value.c_str(), row_center_y, kValueOpticalNudgeY), display_value.c_str(),
                     value_font_, text_style.fg);
        }
    } else if (item.type == SettingsItemType::Action) {
        const char* action_text = item.value.empty() ? "执行" : item.value.c_str();
        const int action_w = MeasureTextWidth(action_text, value_font_);
        const int act_x = content_right - right_margin - action_w;
        label_right = act_x - Style::kSpacingLG;

        DrawText(fb, width, act_x, InkCenteredTextTopY(value_font_, action_text, row_center_y, kValueOpticalNudgeY),
                 action_text, value_font_, item.type == SettingsItemType::Action ? action_color : fg_color);
    }

    int label_max_w = std::max(0, label_right - label_x);
    std::string display_label = FitTextToWidth(item.label, font_, label_max_w);
    if (!display_label.empty()) {
        DrawText(fb, width, label_x, label_y, display_label.c_str(), font_, fg_color);
    }
    DrawHLine(fb, width, y + row_h - 1, content_left, content_right, theme.Style(ThemeToken::Border).border);
}





bool SettingsRenderer::HandleInput(const ButtonEvent& event) {
    if (items_.empty()) return false;

    // Clicks only: long presses and the combo are routed by the application
    // (leaving the page, entering the config AP), and never reach the menu.
    uint8_t button = 0;
    switch (event.type) {
        case ButtonEvent::kUpClick:
            button = RF_INPUT_UP;
            break;
        case ButtonEvent::kDownClick:
            button = RF_INPUT_DOWN;
            break;
        case ButtonEvent::kBootClick:
            button = RF_INPUT_BOOT;
            break;
        default:
            return false;
    }

    rf_settings_step_t st = {};
    rf_settings_step(section_, focus_, option_, button, &st);
    const bool moved = (st.section != section_ || st.focus != focus_ || st.option != option_);
    section_ = st.section;
    focus_ = st.focus;
    option_ = st.option;

    bool acted = false;
    if (st.effect == RF_SETTINGS_EFFECT_ACTIVATE || st.effect == RF_SETTINGS_EFFECT_TOGGLE) {
        acted = true;
        if (item_handler_) item_handler_(st.effect_item, st.effect == RF_SETTINGS_EFFECT_TOGGLE);
    }

    // Only a visible change is worth a full refresh: on this panel one costs
    // 10-25 s. A confirm that acts repaints through its own path.
    if (!moved && !acted) return false;
    needs_full_refresh_ = true;
    return true;
}

void SettingsRenderer::SetItemValue(uint8_t id, const std::string& value) {
    if (values_[id] == value) return;
    values_[id] = value;
    needs_full_refresh_ = true;
}

void SettingsRenderer::SetItemChecked(uint8_t id, bool checked) {
    if (checks_[id] == checked) return;
    checks_[id] = checked;
    needs_full_refresh_ = true;
}

std::string SettingsRenderer::ValueFor(uint8_t id) const {
    auto it = values_.find(id);
    return (it == values_.end()) ? std::string() : it->second;
}

// The model owns the menu; this mirrors the current section into `items_` so
// the drawing below stays a pure function of one view.
void SettingsRenderer::SyncItemsFromModel() {
    if (section_ >= rf_settings_section_count()) section_ = 0;
    rf_settings_section_t sec = {};
    rf_settings_get_section(section_, &sec);

    items_.clear();
    items_.reserve(sec.item_count);
    for (uint8_t i = 0; i < sec.item_count; ++i) {
        rf_settings_item_t it = {};
        rf_settings_get_item(section_, i, &it);
        SettingsItemDef def;
        def.label = it.label ? it.label : "";
        def.value = ValueFor(it.id);
        def.checked = checks_[it.id];
        switch (it.kind) {
            case RF_SETTINGS_KIND_ACTION:
                def.type = SettingsItemType::Action;
                break;
            case RF_SETTINGS_KIND_TOGGLE:
                def.type = SettingsItemType::Checkbox;
                break;
            default:
                // Read-outs use the plain row: label left, value right, and no
                // affordance that confirming them would do anything.
                def.type = SettingsItemType::Normal;
                break;
        }
        items_.push_back(std::move(def));
    }
    if (option_ >= items_.size()) option_ = 0;
}

























}  // namespace rawdraw
