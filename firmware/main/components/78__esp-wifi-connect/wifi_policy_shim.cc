/*
 * wifi_policy_shim.cc — Wi-Fi policy callback seam
 *
 * Component-owned storage and invocation guard.  Does NOT include any main
 * headers or Rust headers.
 *
 * Thread-safety model:
 *   - registration_mutex_: held during register/unregister.
 *   - invoke_mutex_: short-lived; blocks unregister for the duration of a
 *     callback invocation so the in-flight call always completes.
 *   - unregister() acquires registration_mutex_, then invoke_mutex_ (in that
 *     order) and clears the callback under both locks.
 *   - invoke() acquires invoke_mutex_ first, then checks the callback.
 */

#include "wifi_policy_shim.h"

#include <cstring>
#include <mutex>

namespace {

// Stored registration
static wifi_policy_callback_t   g_callback   = nullptr;
static void*                   g_context    = nullptr;
static uint32_t                g_invoke_count = 0;

// Per-call guard: acquired by invoke() before reading g_callback so that
// unregister() can block invoke() until every in-flight call finishes.
static std::mutex              g_invoke_mutex;

// Registration mutex: acquired by register/unregister so that a concurrent
// invoke() cannot simultaneously unregister the callback.
static std::mutex              g_registration_mutex;

}  // namespace

extern "C" bool wifi_policy_register(wifi_policy_callback_t cb, void* ctx) {
    if (cb == nullptr) {
        return false;
    }
    std::lock_guard<std::mutex> reg_lock(g_registration_mutex);

    // Also acquire invoke lock so that a callback currently being invoked sees
    // either the old or the new registration, never a torn state.
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
    g_callback = cb;
    g_context  = ctx;
    return true;
}

extern "C" bool wifi_policy_unregister(void) {
    // 1. Block new invoke() calls.
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
    // 2. Clear under the same lock so invoke() cannot observe a partially-
    //    cleared or nullptr-after-unregister state.
    g_callback = nullptr;
    g_context  = nullptr;
    // registration_mutex_ is not needed here: once invoke_mutex_ is held
    // no invoke() can read g_callback, and no new invoke() can start.
    return true;
}

extern "C" bool wifi_policy_invoke_from_component(
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t*       output) {

    if (output == nullptr) {
        return false;
    }

    // Zero-initialise the output so callers can safely inspect it on false.
    std::memset(output, 0, sizeof(*output));
    if (input == nullptr) {
        return false;
    }

    wifi_policy_callback_t cb   = nullptr;
    void*                 ctx  = nullptr;

    // Acquire invoke lock before reading g_callback so that unregister() must
    // wait for every in-flight invocation to finish.
    {
        std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
        cb  = g_callback;
        ctx = g_context;
    }

    if (cb == nullptr) {
        // Safe fallback: all output is zero (no-op).
        return false;
    }

    wifi_policy_input_v1_t in_copy = *input;
    ++g_invoke_count;

    bool ok = cb(ctx, &in_copy, output);
    return ok;
}

extern "C" uint32_t wifi_policy_invoke_count(void) {
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
    return g_invoke_count;
}

extern "C" bool wifi_policy_is_registered(void) {
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
    return g_callback != nullptr;
}
