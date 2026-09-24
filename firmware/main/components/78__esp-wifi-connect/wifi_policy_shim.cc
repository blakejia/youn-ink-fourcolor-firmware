/*
 * wifi_policy_shim.cc — Wi-Fi policy callback seam
 *
 * Component-owned storage and invocation guard.  Does NOT include any main
 * headers or Rust headers.
 *
 * Thread-safety model:
 *   - g_invoke_mutex: held by invoke() for the ENTIRE duration of the callback
 *     execution (not just the pointer read).  This is the key invariant:
 *     unregister() blocks on this mutex and therefore cannot return until the
 *     in-flight callback has fully completed.
 *   - g_registration_mutex: serialises register() and unregister() so that
 *     a re-registration cannot race with a concurrent unregister().
 *   - unregister() acquires g_invoke_mutex first, then clears the callback.
 *     Because invoke() holds g_invoke_mutex through the full callback, the
 *     clear in unregister() cannot happen until every in-flight callback
 *     has returned.
 *   - register() acquires g_registration_mutex, then g_invoke_mutex so that
 *     it also blocks any in-flight unregister() or invoke().
 */

#include "wifi_policy_shim.h"

#include <cstring>
#include <mutex>

namespace {

// Stored registration
static wifi_policy_callback_t   g_callback      = nullptr;
static void*                  g_context       = nullptr;
static uint32_t                g_invoke_count  = 0;

// Invocation guard: held by invoke() for the ENTIRE callback execution,
// and by unregister() to drain in-flight calls before clearing.
// This is the only lock needed for the core invariant.
static std::mutex             g_invoke_mutex;

// Serialises register() calls against unregister() so a re-registration
// cannot race with an unregister-in-progress.
static std::mutex              g_registration_mutex;

}  // namespace

extern "C" bool wifi_policy_register(wifi_policy_callback_t cb, void* ctx) {
    if (cb == nullptr) {
        return false;
    }
    // Serialise against any concurrent unregister().
    std::lock_guard<std::mutex> reg_lock(g_registration_mutex);
    // Block any in-flight unregister() or invoke() so the update is atomic.
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
    g_callback = cb;
    g_context  = ctx;
    return true;
}

extern "C" bool wifi_policy_unregister(void) {
    // Acquire g_invoke_mutex FIRST — this is what makes unregister() wait
    // for every in-flight callback to finish before it clears g_callback.
    // If invoke() is currently executing a callback, we block here until
    // that callback returns and releases the lock.
    std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);

    // Now clear under the lock.  No new invoke() call can enter the callback
    // (it would block on g_invoke_mutex), and no in-flight invoke() exists
    // (we hold the lock it was holding).  The clear is therefore safe.
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

    // Zero-initialise the output so callers can safely inspect it on false.
    std::memset(output, 0, sizeof(*output));
    if (input == nullptr) {
        return false;
    }

    wifi_policy_callback_t cb   = nullptr;
    void*                 ctx  = nullptr;

    // Acquire the invocation guard BEFORE reading g_callback.  We hold this
    // lock for the ENTIRE callback execution below, so that unregister()
    // cannot return until the callback has fully returned.
    {
        std::lock_guard<std::mutex> inv_lock(g_invoke_mutex);
        cb  = g_callback;
        ctx = g_context;
        if (cb == nullptr) {
            // No callback registered — safe fallback (output already zeroed).
            return false;
        }
        ++g_invoke_count;
    }

    // cb is non-null and will remain stable while g_invoke_mutex is not held:
    // unregister() cannot clear it (it would block on g_invoke_mutex),
    // and register() cannot replace it (it also acquires g_invoke_mutex).
    // Copy the input so the callback can inspect it without lifetime issues.
    wifi_policy_input_v1_t in_copy = *input;

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
