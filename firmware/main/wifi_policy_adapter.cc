/*
 * wifi_policy_adapter.cc — main-owned Wi-Fi policy adapter
 *
 * Calls wifi_policy_shim (component) for registration/invocation.
 * PolicyCallback maps the narrow component facts to the rich Rust POD,
 * calls rf_wifi_policy_decide, and maps the result back to the shim action.
 */

#include "wifi_policy_adapter.h"

#include <cstdint>
#include <cstring>

#include <esp_log.h>

#include "components/78__esp-wifi-connect/wifi_policy_shim.h"
#include "wifi_policy.h"
#define TAG "WifiPolicyAdapter"

namespace {
// Tracks whether the adapter is currently registered with the shim.
// No internal policy state: this adapter holds no cross-invocation state.
static bool s_registered = false;

// Counts invocations for diagnostics.
static uint32_t s_invoke_count = 0;

/*
 * Policy callback bound at registration.
 *
 * input  — immutable, may not be retained.
 * output — caller-allocated, must be filled.
 */
static bool PolicyCallback(
    void*                         /* context */,
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t*       output) {
    if (!s_registered || input == nullptr || output == nullptr) {
        return false;
    }

    ++s_invoke_count;

    rf_wifi_policy_inputs_t rust_input{};
    rust_input.version = RF_WIFI_POLICY_VERSION;
    rust_input.invoke_count = input->invoke_count;
    rust_input.wifi_connected = input->wifi_connected;
    rust_input.rssi = input->rssi;
    rust_input.channel = static_cast<uint8_t>(input->channel);
    rust_input.reconnect_count = input->reconnect_count;
    rust_input.ip_fast_active = input->ip_fast_active;
    rust_input.ip_fast_ready = input->ip_fast_ready;
    rust_input.ip_fast_cache_age_ms = input->ip_fast_cache_age_ms;
    rust_input.have_wifi_cache = input->have_wifi_cache;
    rust_input.cache_bssid_valid = input->cache_bssid_valid;
    rust_input.cache_channel = input->cache_channel;
    std::memcpy(rust_input.cache_ssid, input->cache_ssid, sizeof(rust_input.cache_ssid));
    rust_input.cache_ssid_len = input->cache_ssid_len;
    std::memcpy(rust_input.cache_bssid, input->cache_bssid, sizeof(rust_input.cache_bssid));
    rust_input.wifi_cache_age_ms = input->wifi_cache_age_ms;
    rust_input.have_ip_cache = input->have_ip_cache;
    rust_input.ip_cache_age_ms = input->ip_cache_age_ms;
    rust_input.fast_fail_count = input->fast_fail_count;
    rust_input.fast_enabled = input->fast_enabled;
    rust_input.endpoint_present = input->endpoint_present;
    rust_input.probe_target = input->probe_target;
    rust_input.host_is_ip_literal = input->host_is_ip_literal;
    rf_wifi_policy_output_t rust_output{};
    rf_wifi_policy_decide(&rust_input, &rust_output);

    output->version = WIFI_POLICY_SHIM_VERSION;
    output->action_kind = rust_output.action_kind;
    output->retry_delay_ms = rust_output.retry_delay_ms;
    output->clear_wifi_cache = rust_output.clear_wifi_cache != 0;
    output->clear_ip_cache = rust_output.clear_ip_cache != 0;
    return true;
}

}  // namespace

void wifi_policy_adapter_register(void) {
    if (s_registered) {
        ESP_LOGI(TAG, "Adapter already registered");
        return;
    }

    bool ok = wifi_policy_register(&PolicyCallback, nullptr);
    if (!ok) {
        ESP_LOGE(TAG, "wifi_policy_register failed");
        return;
    }

    s_registered = true;
    ESP_LOGI(TAG, "Wi-Fi policy adapter registered");
}

void wifi_policy_adapter_unregister(void) {
    if (!s_registered) {
        return;  // idempotent
    }

    wifi_policy_unregister();
    s_registered = false;
    ESP_LOGI(TAG, "Wi-Fi policy adapter unregistered");
}

bool wifi_policy_adapter_is_registered(void) {
    return s_registered;
}

uint32_t wifi_policy_adapter_invoke_count(void) {
    return s_invoke_count;
}
