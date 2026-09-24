/*
 * wifi_policy_adapter.cc — main-owned Wi-Fi policy adapter
 *
 * Calls wifi_policy_shim (component) for registration/invocation.
 * Policy decisions will land here in Task 4.
 */

#include "wifi_policy_adapter.h"

#include <esp_log.h>

#include "components/78__esp-wifi-connect/wifi_policy_shim.h"

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
 * In Task 4 this will call into Rust.  For now it is a pass-through stub
 * that accepts any input and returns a no-op action, so that the shim
 * wiring can be verified without changing Wi-Fi behaviour.
 *
 * input  — immutable, may not be retained.
 * output — caller-allocated, must be filled.
 */
static bool PolicyCallback(
    void*                         /* context */,
    const wifi_policy_input_v1_t*  input,
    wifi_policy_action_v1_t*       output) {

    if (input == nullptr || output == nullptr) {
        return false;
    }

    ++s_invoke_count;

    // TODO(Task 4): call Rust wifi_policy_decide() here.
    // For now, return a safe no-op action.
    (void)input;
    (void)output;  // zeroed by caller

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
