/**
 * @file wifi_renderer.h
 * @brief Modernized WiFi status page renderer for rawdraw mode
 *
 * Features: visual connection status with large icon, signal strength
 * bars visualization, server status card, connection progress.
 */

#ifndef RAWDRAW_WIFI_RENDERER_H
#define RAWDRAW_WIFI_RENDERER_H

#include "page_renderer.h"
#include <string>

namespace rawdraw {

/**
 * @brief WiFi connection state
 */
enum class WifiState {
    Disconnected,        ///< Initial state / WiFi lost
    ApStarted,           ///< Config AP started (e.g. ZecTrix-XXXX visible)
    ApClientConnected,   ///< Phone joined the config AP
    Provisioning,        ///< Form submitted, attempting WiFi connection
    Connecting,          ///< Legacy alias (renders as Provisioning)
    Connected,           ///< WiFi connected (got IP)
    Error,               ///< Last attempt failed (see error_msg)
};

/**
 * @brief WiFi status data
 */
struct WifiStatus {
    WifiState state = WifiState::Disconnected;
    std::string ssid;
    int signal_strength = 0;    ///< dBm (typically -30 to -90)
    int progress = 0;           ///< Connection progress (0-100)
    bool server_connected = false;
    std::string server_uri;
    // Provisioning state additions
    std::string ap_ssid;       ///< Config AP SSID (e.g. ZecTrix-00FD)
    std::string ap_password;   ///< Config AP password
    std::string ap_url;        ///< Config web URL (e.g. http://192.168.4.1)
    std::string error_msg;     ///< Error text (state==Error)
    int error_code = 0;        ///< Raw WiFi reason code
    int provisioning_step = 0; ///< 0-100 (Provisioning only)
};
/**
 * @brief Modernized WiFi status page renderer
 *
 * Design for 400x300 1bpp ePaper:
 * - Large central icon representing connection state
 * - Signal strength bars (5 bars, like phone UI)
 * - Server status card with icon + text
 * - Connection progress with progress bar
 * - Clean card-based layout
 */
class WifiRenderer : public PageRenderer {
public:
    WifiRenderer();
    ~WifiRenderer() override;

    // PageRenderer interface
    void Init(int width, int height) override;
    void Render(uint8_t* fb, int width, int height) override;
    bool HandleInput(const ButtonEvent& event) override;

    // Data interface
    void Update(const WifiStatus& status);
    WifiStatus GetStatus() const { return status_; }

    // Animation control
    void SetBlinking(bool blinking);
    bool IsBlinking() const { return is_blinking_; }

private:
    // Render each state
    void RenderConnecting(uint8_t* fb, int width, int height);
    void RenderConnected(uint8_t* fb, int width, int height);
    void RenderDisconnected(uint8_t* fb, int width, int height);
    void RenderApStarted(uint8_t* fb, int width, int height);
    void RenderApClientConnected(uint8_t* fb, int width, int height);
    void RenderProvisioning(uint8_t* fb, int width, int height);
    void RenderError(uint8_t* fb, int width, int height);

    // Map WiFi reason code (from WIFI_EVENT_STA_DISCONNECTED) to Chinese message
    static const char* ReasonToMessage(int reason);

    // Draw signal strength bars (5-bar visualization)
    void DrawSignalBars(uint8_t* fb, int width, int x, int y,
                         int bar_count, int signal_pct);

    // Draw WiFi icon at given position with size
    void DrawWifiIcon(uint8_t* fb, int width, int x, int y, int size,
                       Color color);

    // Draw server status card
    void DrawServerCard(uint8_t* fb, int width, int x, int y, int w,
                         bool connected, const std::string& uri);

    // Get WiFi icon code for signal level
    const char* GetWifiIcon(int signal_dbm) const;

    // Convert dBm to percentage
    int SignalToPercent(int dbm) const;

    WifiStatus status_;
    bool is_blinking_ = false;
    int blink_frame_ = 0;

    const lv_font_t* font_ = nullptr;
    const lv_font_t* title_font_ = nullptr;
    const lv_font_t* icon_font_ = nullptr;
    const lv_font_t* large_icon_font_ = nullptr;
};

}  // namespace rawdraw

#endif  // RAWDRAW_WIFI_RENDERER_H
