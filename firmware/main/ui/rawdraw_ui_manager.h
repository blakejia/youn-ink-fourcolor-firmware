/**
 * @file rawdraw_ui_manager.h
 * @brief RawDraw-based UI manager (no LVGL dependency)
 *
 * Manages rawdraw page renderers, status bar drawing, page switching,
 * and button event routing. Renders directly to the 1bpp framebuffer.
 */

#ifndef RAWDRAW_UI_MANAGER_H
#define RAWDRAW_UI_MANAGER_H

// Include LVGL header FIRST so font_engine.h detects LVGL types and skips redefining them
#include "boards/zectrix-s3-epaper-4.2/custom_lcd_display.h"

#include "ui/renderers/rawdraw/page_renderer.h"
#include "ui/renderers/rawdraw/chat_renderer.h"
#include "ui/renderers/rawdraw/settings_renderer.h"
#include "ui/renderers/rawdraw/ebook_renderer.h"
#include "ui/renderers/rawdraw/wifi_renderer.h"
#include "ui/renderers/rawdraw/photo_gallery.h"
#include "ui/renderers/rawdraw/photo_detail_renderer.h"
#include "ui/renderers/rawdraw/weather_renderer.h"
#include "ui/renderers/rawdraw/weather_detail_renderer.h"
#include "ui/renderers/rawdraw/news_renderer.h"
#include "ui/renderers/rawdraw/lifebar_renderer.h"
#include "ui/renderers/rawdraw/almanac_renderer.h"
#include "ui/renderers/rawdraw/log_renderer.h"
#include "ui/renderers/rawdraw/pairing_renderer.h"
#include "ui/renderers/rawdraw/yearprogress_renderer.h"
#include "ui/renderers/rawdraw/font_debug_renderer.h"
#include "ui/renderers/rawdraw/font_metrics_renderer.h"
#include "ui/renderers/rawdraw/calendar_renderer.h"
#include "rawdraw/rawdraw.h"
#include "rawdraw/theme.h"
#include "rawdraw/style.h"
#include "rawdraw/framebuffer.h"
#include "rawdraw/clock.h"
#include "rawdraw/components/voice_wakeup.h"

#include <memory>
#include <string>
#include <functional>
#include <cstdint>
#include <array>
#include <atomic>
#include <vector>
#include <mutex>

// Forward declaration
class CustomLcdDisplay;

namespace ui {

/**
 * @brief Page identifiers for the rawdraw UI
 *
 * Only pages with rawdraw renderers are included here.
 * Separate from ui::PageId to avoid conflict with LVGL UiManager.
 */
enum class RawDrawPageId {
    Chat = 0,
    Ebook = 2,
    Wifi = 3,
    Settings = 4,
    Weather = 6,
    News = 7,
    WeatherDetail = 8,
    PhotoDetail = 9,
    LifeBar = 10,
    Almanac = 11,
    Log = 12,
    YearProgress = 13,
    Calendar = 14,
    FontDebug = 15,
    FontMetrics = 16,
    Pairing = 17,
    Count,
};

/**
 * @brief Status bar data for rawdraw rendering
 */
struct RawDrawStatusBarData {
    std::string page_title;
    std::string central_text;  // Overrides page_title when non-empty (e.g. "录音中...", "file.txt 1/3")
    bool wifi_connected = false;
    bool server_connected = false;
    bool bluetooth_enabled = false;
    int battery_level = -1;       // -1 = unknown
    bool battery_charging = false;
    bool battery_vertical = false;
    std::string date_format;      // "" = default (M月D日), "iso" = yyyy-mm-dd, "hidden" = hide date
    std::string server_date;      // date from WSS (yyyy-mm-dd), fallback for RTC
    std::string server_weekday;   // weekday from WSS (周一~周日)
};

/**
 * @brief Refresh callback type
 *
 * Called after RenderAll to trigger EPD update.
 * The caller (typically CustomLcdDisplay) provides this callback
 * to handle the actual EPD refresh timing.
 */
using RefreshCallback = std::function<void(const rawdraw::Rect& dirty_rect, bool urgent)>;
using PageSwitchCallback = std::function<void(RawDrawPageId page)>;

/**
 * @brief RawDraw UI Manager
 *
 * Central UI manager that:
 * - Owns all rawdraw page renderers
 * - Manages page switching with full framebuffer clear + re-render
 * - Routes button events to the active page renderer
 * - Draws the status bar + active page content
 * - Provides data update methods for each page
 *
 * Unlike the LVGL UiManager, this operates directly on the 1bpp framebuffer.
 * No LVGL objects, no tabview, no display driver — just raw pixels.
 */
class RawDrawUiManager {
public:
    RawDrawUiManager();
    ~RawDrawUiManager();

    /**
     * @brief Initialize the UI manager
     *
     * @param lcd Pointer to CustomLcdDisplay for framebuffer access
     * @param refresh_cb Optional callback to trigger EPD refresh after rendering
     */
    void Init(CustomLcdDisplay* lcd, RefreshCallback refresh_cb = nullptr);
    void SetPageSwitchCallback(PageSwitchCallback callback) { page_switch_cb_ = std::move(callback); }

    /**
     * @brief Switch to a different page
     *
     * Clears the entire framebuffer and re-renders the new page.
     *
     * @param page Target page ID
     */
    void SwitchPage(RawDrawPageId page);

    /**
     * @brief Set current page without rendering
     *
     * Updates page state (current_page_, title, renderer init) but
     * does NOT clear framebuffer or trigger refresh. Use when the
     * caller will handle rendering in a single pass.
     *
     * @param page Target page ID
     */
    void SetCurrentPageWithoutRender(RawDrawPageId page);

    /**
     * @brief Get the current active page
     */
    RawDrawPageId GetCurrentPage() const { return current_page_; }
    RawDrawPageId GetPreviousPage() const { return previous_page_; }
    bool IsDisplayRefreshPending() const;

    /**
     * @brief Get the active page renderer (may be null)
     */
    rawdraw::PageRenderer* GetActiveRenderer() const;

    /**
     * @brief Handle a button event
     *
     * Routes the event to the active page renderer's HandleInput().
     * If the renderer handles it, marks the framebuffer dirty and
     * requests a refresh.
     *
     * @param event Button event
     * @return true if the event was consumed
     */
    bool HandleInput(const rawdraw::ButtonEvent& event);

    /**
     * @brief Whether the global UP-double quick switch modal is open.
     *
     * LanMicApp checks this before page-local input handling so UP/DN/BOOT
     * are consumed by the modal and do not leak into Todo/Settings/etc.
     */
    bool IsQuickSwitchOpen() const { return quick_switch_open_; }

    /**
     * @brief LAN HTTP server (removed — Gallery/AP Transfer gone).
     * These are no-ops returning false.
     */
    bool StartLanHttpServer(const std::string& ip_address);
    void StopLanHttpServer();
    bool IsLanHttpServerRunning() const { return false; }
    bool IsHttpServerRunning() const { return false; }

    /**
     * @brief Render everything to the framebuffer
     *
     * Draws the status bar at the top, then calls the active page
     * renderer's Render() for the content area.
     *
     * @param fb Framebuffer pointer (1bpp)
     * @param width Framebuffer width
     * @param height Framebuffer height
     */
    void RenderAll(uint8_t* fb, int width, int height);

    /**
     * @brief Update the status bar data
     *
     * Does NOT trigger a render — call RenderAll() or HandleInput()
     * to display the changes.
     */
    void UpdateStatusBar(const RawDrawStatusBarData& data);

    /**
     * @brief Get current status bar data (non-const copy)
     */
    RawDrawStatusBarData GetStatusBarData() const {
        std::lock_guard<std::mutex> lock(ui_state_mutex_);
        return status_bar_data_;
    }

    // ============================================================
    // Page data update methods
    // ============================================================

    void AddChatMessage(const std::string& text, rawdraw::ChatRole role);
    void ClearChat();
    void BeginChatStream();
    bool AppendChatText(const char* chunk);
    void EndChatStream();
    void ShowChatStatus(const std::string& status, rawdraw::ChatRole role);
    void HideChatStatus();
    void SetChatListening(bool listening);
    void SetChatBottomStatus(const std::string& status);
    void SetSettingsItems(const std::vector<rawdraw::SettingsItemDef>& items);
    void UpdateSettingsItem(int index, const std::string& value);
    void UpdateSettingsChecked(int index, bool checked);
    void SetRawDrawTheme(rawdraw::ThemeId theme_id);
    rawdraw::ThemeId GetRawDrawTheme() const;
    void UpdateWifiStatus(const rawdraw::WifiStatus& status);
    rawdraw::WifiStatus GetWifiStatus() const;
    void SetWifiBlinking(bool blinking);
    void SetLifeBarVisible(bool visible);
    bool IsLifeBarVisible() const;

    // ============================================================
    // Page renderer access (for advanced usage)
    // ============================================================

    rawdraw::ChatRenderer* GetChatRenderer() { return chat_renderer_.get(); }
    rawdraw::EbookRenderer* GetEbookRenderer() { return ebook_renderer_.get(); }
    rawdraw::WifiRenderer* GetWifiRenderer() { return wifi_renderer_.get(); }
    rawdraw::SettingsRenderer* GetSettingsRenderer() { return settings_renderer_.get(); }
    rawdraw::PhotoGalleryRenderer* GetPhotoGalleryRenderer() { return photo_gallery_renderer_.get(); }
    rawdraw::PhotoDetailRenderer* GetPhotoDetailRenderer() { return photo_detail_renderer_.get(); }
    rawdraw::WeatherRenderer* GetWeatherRenderer() { return weather_renderer_.get(); }
    rawdraw::WeatherDetailRenderer* GetWeatherDetailRenderer() { return weather_detail_renderer_.get(); }
    rawdraw::NewsRenderer* GetNewsRenderer() { return news_renderer_.get(); }
    rawdraw::LifeBarRenderer* GetLifeBarRenderer() { return lifebar_renderer_.get(); }
    rawdraw::AlmanacRenderer* GetAlmanacRenderer() { return almanac_renderer_.get(); }
    rawdraw::LogRenderer* GetLogRenderer() { return log_renderer_.get(); }
    rawdraw::PairingRenderer* GetPairingRenderer() { return pairing_renderer_.get(); }
    rawdraw::YearProgressRenderer* GetYearProgressRenderer() { return yearprogress_renderer_.get(); }
    rawdraw::CalendarRenderer* GetCalendarRenderer() { return calendar_renderer_.get(); }
    rawdraw::FontDebugRenderer* GetFontDebugRenderer() { return font_debug_renderer_.get(); }
    rawdraw::FontMetricsRenderer* GetFontMetricsRenderer() { return font_metrics_renderer_.get(); }

    int GetWidth() const { return width_; }
    int GetHeight() const { return height_; }

    /**
     * @brief Request a full EPD refresh (for clearing ghosting)
     * Only sets a flag — does NOT actually push fb to EPD.
     * Use TriggerRefresh() for immediate EPD update.
     */
    void RequestFullRefresh();

    /**
     * @brief Queue a re-render of the current page on the main/UI loop.
     *
     * Use this from callbacks that may run outside LanMicApp::Run().
     */
    void RequestActivePageRefresh();

    // Gallery removed — these are no-ops.
    void SetGallerySlideshowIntervalMinutes(int minutes);
    bool ShowPhotoById(const std::string& photo_id);

    /**
     * @brief Show WiFi config page
     */
    void ShowWifiConfigPage(const std::string& ssid,
                            const std::string& password,
                            const std::string& url);
    /**
     * @brief Show pairing code page (Lifecycle PairWaitCode binding)
     */
    void ShowPairingCodePage(const std::string& code, int expires_in);

    /**
     * @brief Marks entire framebuffer dirty and calls refresh_cb_ to push
     * fb content to the EPD. This is the method that actually makes
     * pixels appear on the screen.
     *
     * @param urgent If true, forces immediate full refresh (no throttling)
     */
    void TriggerRefresh(bool urgent = false);
    bool TryDisplayCurrentPhotoRaw4Color();

    /**
     * @brief Get page title for a given RawDrawPageId
     */
    static const char* GetPageTitle(RawDrawPageId page);

    // ============================================================
    // Clock and voice wakeup integration
    // ============================================================

    void VoiceWakeupTick();
    void VoiceWakeupTrigger(bool network_available);
    void VoiceWakeupDone();
    bool VoiceWakeupIsActive() const;
    void PumpClockRefresh();

private:
    struct QuickSwitchItem {
        RawDrawPageId page;
        const char* label;
        const char* icon;  // UTF-8 icon string (FontAwesome codepoint)
    };

    // Display state
    CustomLcdDisplay* lcd_ = nullptr;
    int width_ = 400;
    int height_ = 300;

    // Current page
    RawDrawPageId current_page_ = RawDrawPageId::Chat;
    RawDrawPageId previous_page_ = RawDrawPageId::Chat;

    // Status bar
    RawDrawStatusBarData status_bar_data_;

    // UI state mutex (protects status_bar_data_ + shared state)
    mutable std::mutex ui_state_mutex_;

    // Page renderers (owned)
    std::unique_ptr<rawdraw::ChatRenderer> chat_renderer_;
    std::unique_ptr<rawdraw::EbookRenderer> ebook_renderer_;
    std::unique_ptr<rawdraw::WifiRenderer> wifi_renderer_;
    std::unique_ptr<rawdraw::SettingsRenderer> settings_renderer_;
    std::unique_ptr<rawdraw::PhotoGalleryRenderer> photo_gallery_renderer_;
    std::unique_ptr<rawdraw::PhotoDetailRenderer> photo_detail_renderer_;
    std::unique_ptr<rawdraw::WeatherRenderer> weather_renderer_;
    std::unique_ptr<rawdraw::WeatherDetailRenderer> weather_detail_renderer_;
    std::unique_ptr<rawdraw::NewsRenderer> news_renderer_;
    std::unique_ptr<rawdraw::LifeBarRenderer> lifebar_renderer_;
    std::unique_ptr<rawdraw::AlmanacRenderer> almanac_renderer_;
    std::unique_ptr<rawdraw::LogRenderer> log_renderer_;
    std::unique_ptr<rawdraw::PairingRenderer> pairing_renderer_;
    std::unique_ptr<rawdraw::YearProgressRenderer> yearprogress_renderer_;
    std::unique_ptr<rawdraw::CalendarRenderer> calendar_renderer_;
    std::unique_ptr<rawdraw::FontDebugRenderer> font_debug_renderer_;
    std::unique_ptr<rawdraw::FontMetricsRenderer> font_metrics_renderer_;

    // Full refresh flag
    bool full_refresh_pending_ = false;

    PageSwitchCallback page_switch_cb_;

    // Refresh callback
    RefreshCallback refresh_cb_;

    // Clock component (persistent, drawn on every RenderAll)
    rawdraw::Clock clock_;
    esp_timer_handle_t clock_refresh_timer_ = nullptr;
    esp_timer_handle_t transient_refresh_timer_ = nullptr;

    std::atomic<bool> input_refresh_locked_{false};
    int last_clock_minute_key_ = -1;
    std::atomic<bool> active_page_refresh_pending_{false};
    std::atomic<bool> transient_refresh_pending_{false};
    std::atomic<bool> clock_refresh_pending_{false};

    // Voice wakeup overlay state
    rawdraw::VoiceWakeupState voice_wakeup_state_;

    // Global quick switch overlay
    bool quick_switch_open_ = false;
    int quick_switch_index_ = 0;
    int quick_switch_first_visible_ = 0;
    std::vector<uint8_t> quick_switch_backing_;

    // Internal helpers
    rawdraw::PageRenderer* GetRendererForPage(RawDrawPageId page) const;
    void InitRenderer(RawDrawPageId page);
    void RefreshActivePage(bool urgent = false);
    void RefreshActivePageRect(const rawdraw::Rect& rect, bool urgent = false);
    void DrawStatusBar(uint8_t* fb, int width, int height);
    void ArmClockRefreshTimer();
    void ArmTransientRefreshTimer(int delay_ms = 2000);
    static void OnClockRefreshTimer(void* arg);
    static void OnTransientRefreshTimer(void* arg);
    void DrawGlobalPageFrame(uint8_t* fb, int width, int height);
    void SnapshotQuickSwitchBacking(uint8_t* fb);
    void RestoreQuickSwitchBacking(uint8_t* fb);
    void RedrawQuickSwitchOnly(uint8_t* fb);
    void RefreshRect(const rawdraw::Rect& rect, bool urgent = false);
    bool HandleQuickSwitchInput(const rawdraw::ButtonEvent& event);
    void DrawQuickSwitchOverlay(uint8_t* fb, int width, int height);
    rawdraw::Rect GetQuickSwitchBounds() const;
    static const std::array<QuickSwitchItem, 1>& GetQuickSwitchItems();
    void MarkAllRenderersFullRefresh();
};

}  // namespace ui

#endif  // RAWDRAW_UI_MANAGER_H
