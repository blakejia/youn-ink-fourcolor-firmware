/*
 * wifi_policy_adapter.h — main-owned Wi-Fi policy adapter
 *
 * Thin seam between the component wifi_policy_shim and the policy decision layer.
 * Gathers component facts, invokes the registered callback, executes the returned
 * action.  Policy decisions themselves land here in Task 4 (Rust call).
 *
 * Registered once at startup; NOT unregistered on ordinary StopStation().
 * Unregistration is called only from WifiManager's final teardown hook.
 */

#ifndef WIFI_POLICY_ADAPTER_H
#define WIFI_POLICY_ADAPTER_H

#include <stdint.h>
#include <stdbool.h>

/*
 * Adapter registration — call once after WifiManager::Initialize() and
 * before any StartStation().  Safe to call twice (idempotent).
 */
void wifi_policy_adapter_register(void);

/*
 * Unregister and perform final teardown.  Idempotent.  Called only from
 * WifiManager::WifiPolicyShutdown(), i.e. from the WifiManager destructor.
 */
void wifi_policy_adapter_unregister(void);

/*
 * True if a callback is currently registered with the shim.
 */
bool wifi_policy_adapter_is_registered(void);

/*
 * Current invocation counter — for diagnostics only.
 */
uint32_t wifi_policy_adapter_invoke_count(void);

#endif /* WIFI_POLICY_ADAPTER_H */
