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
 *     returns and releases the lock.  There is no way for unregister() to
 *     clear g_callback while cb() is executing.
 *   - register() also acquires g_shim_mutex to make re-registration safe against
 *     concurrent unregister()/invoke().
 */

#include "wifi_policy_shim.h"

#include <cstring>
#include <mutex>

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

    wifi_policy_callback_t cb   = nullptr;
    void*                 ctx  = nullptr;

    // Acquire the lock and hold it for the ENTIRE callback execution.
    // This is the key invariant: unregister() cannot clear g_callback
    // while cb() is running, because cb() holds g_shim_mutex.
    {
        std::lock_guard<std::mutex> lock(g_shim_mutex);
        cb  = g_callback;
        ctx = g_context;
        if (cb == nullptr) {
            return false;
        }
        ++g_invoke_count;
        // Keep the lock for the cb() call below.
    }

    // g_shim_mutex is NOT held here.
    // cb is non-null and stable because:
    //   - unregister() would have blocked on g_shim_mutex above, waiting for us.
    //   - register() would also have blocked on g_shim_mutex.
    // Copy input before the call.
    wifi_policy_input_v1_t in_copy = *input;

    bool ok = cb(ctx, &in_copy, output);
    return ok;
}

extern "C" uint32_t wifi_policy_invoke_count(void) {
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    return g_invoke_count;
}

extern "C" bool wifi_policy_is_registered(void) {
    std::lock_guard<std::mutex> lock(g_shim_mutex);
    return g_callback != nullptr;
}
