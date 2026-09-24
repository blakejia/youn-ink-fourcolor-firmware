/*
 * wifi_policy_shim.cc — Wi-Fi policy callback seam
 *
 * Component-owned storage and invocation guard.  Does NOT include any main
 * headers or Rust headers.
 *
 * Thread-safety model (single lock, simple invariant):
 *   - g_shim_mutex: the only lock.  Held by invoke() for the ENTIRE duration
 *     of the callback execution (from before reading g_callback through the
 *     return from cb()).  Held by unregister() from acquisition until it has
 *     cleared g_callback.
 *   - Because invoke() holds the lock for the full callback, any call to
 *     unregister() while a callback is in flight MUST block until that callback
 *     returns and releases the lock.
 *   - register() also acquires g_shim_mutex to make re-registration safe.
 */

#include "wifi_policy_shim.h"

#include <cstring>
#include <mutex>

extern "C" {
uint8_t rf_wifi_encode_rtc_cache(const uint8_t*, uint32_t, const uint8_t*, uint8_t, uint8_t*, uint32_t);
uint8_t rf_wifi_decode_rtc_cache(const uint8_t*, uint32_t, uint8_t*, uint32_t, uint8_t*, uint8_t*);
uint8_t rf_wifi_validate_rtc_cache(const uint8_t*, uint32_t);
uint8_t rf_wifi_parse_endpoint(const char*, uint8_t*, uint32_t, uint16_t*);
uint8_t rf_wifi_parse_url_authority(const char*, uint8_t*, uint32_t, uint16_t*);
}

namespace {

// Stored registration
static wifi_policy_callback_t  g_callback     = nullptr;
static void*                  g_context      = nullptr;
static uint32_t               g_invoke_count = 0;

// The only lock.  Held by invoke() for the full callback and by
// unregister() to drain in-flight callbacks before clearing.
static std::mutex             g_shim_mutex;

}  // namespace

extern "C" bool wifi_policy_register(wifi_policy_callback_t cb, void* ctx) {
    if (cb == nullptr) {
        return false;
    }
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    g_callback = cb;
    g_context  = ctx;
    return true;
}

extern "C" bool wifi_policy_unregister(void) {
    // Acquire the lock.  If a callback is currently executing inside
    // invoke(), we block here — cb() holds g_shim_mutex.  When cb() returns,
    // we acquire the lock and clear g_callback.
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    g_callback = nullptr;
    g_context  = nullptr;
    return true;
}

extern "C" bool wifi_policy_invoke_from_component(
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t*       output) {

    if (output == nullptr) {
        return false;
    }
    std::memset(output, 0, sizeof(*output));
    if (input == nullptr) {
        return false;
    }

    // unique_lock at function scope: the mutex is held until this function
    // returns — including during the cb() call below.  unregister() cannot
    // clear g_callback while cb() is running.
    std::unique_lock<std::mutex> lock(g_shim_mutex);

    wifi_policy_callback_t cb = g_callback;
    void* ctx = g_context;
    if (cb == nullptr) {
        return false;
    }
    ++g_invoke_count;

    // Copy input before the lock is released on the stack.
    wifi_policy_input_v1_t in_copy = *input;

    bool ok = cb(ctx, &in_copy, output);
    return ok;
    // lock destructor runs here — AFTER cb() returns.
}

extern "C" uint32_t wifi_policy_invoke_count(void) {
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    return g_invoke_count;
}

extern "C" bool wifi_policy_is_registered(void) {
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    return g_callback != nullptr;
}

extern "C" bool wifi_policy_encode_rtc_cache(const uint8_t* ssid, uint32_t ssid_len,
                                              const uint8_t* bssid, uint8_t channel,
                                              uint8_t* buf, uint32_t buf_len) {
    return rf_wifi_encode_rtc_cache(ssid, ssid_len, bssid, channel, buf, buf_len) != 0;
}

extern "C" bool wifi_policy_decode_rtc_cache(const uint8_t* buf, uint32_t buf_len,
                                              uint8_t* ssid_out, uint32_t ssid_out_cap,
                                              uint8_t* bssid_out, uint8_t* channel_out) {
    return rf_wifi_decode_rtc_cache(buf, buf_len, ssid_out, ssid_out_cap, bssid_out, channel_out) != 0;
}

extern "C" bool wifi_policy_validate_rtc_cache(const uint8_t* buf, uint32_t buf_len) {
    return rf_wifi_validate_rtc_cache(buf, buf_len) != 0;
}

extern "C" bool wifi_policy_parse_endpoint(const char* input, uint8_t* host_buf,
                                            uint32_t host_buf_len, uint16_t* port) {
    return rf_wifi_parse_endpoint(input, host_buf, host_buf_len, port) != 0;
}

extern "C" bool wifi_policy_parse_url_authority(const char* input, uint8_t* host_buf,
                                                 uint32_t host_buf_len, uint16_t* port) {
    return rf_wifi_parse_url_authority(input, host_buf, host_buf_len, port) != 0;
}
