/*
 * wifi_policy_shim_test.cc — host contract fixture
 *
 * Compile directly with the host compiler (no ESP-IDF headers).
 * Must not include ESP-IDF or any main headers.
 *
 * g++ -std=c++17 -pthread \
 *   -Ifirmware/main/components/78__esp-wifi-connect \
 *   firmware/main/tests/wifi_policy_shim_test.cc \
 *   firmware/main/components/78__esp-wifi-connect/wifi_policy_shim.cc \
 *   -o /tmp/wifi_policy_shim_test
 * /tmp/wifi_policy_shim_test
 */

#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <atomic>
#include <condition_variable>
#include <mutex>
#include <thread>
#include <vector>

#include "wifi_policy_shim.h"

/* ── Helpers ──────────────────────────────────────────────────────────────── */

static int g_callback_calls = 0;

// Set by the callback when it starts.  Used by the test to confirm the
// callback is in-flight before calling unregister().
static std::atomic<bool> g_callback_started{false};

// When true the callback is still executing (has not returned yet).
// Cleared by the callback just before it returns.
static std::atomic<bool> g_callback_in_progress{false};

static void reset_counters(void) {
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
}

/*
 * test_callback: sets g_callback_started on entry, keeps g_callback_in_progress
 * true for the duration of the call, and clears it on exit.
 * The test uses g_callback_started to confirm the callback is in-flight
 * before calling unregister().
 */
static bool test_callback(
    void* /* ctx */,
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t* output) {
    ++g_callback_calls;
    g_callback_in_progress.store(true, std::memory_order_release);
    g_callback_started.store(true, std::memory_order_release);

    // Verify input is valid.
    assert(input != nullptr);
    assert(input->version == WIFI_POLICY_SHIM_VERSION);

    std::memset(output, 0, sizeof(*output));
    output->version = WIFI_POLICY_SHIM_VERSION;

    // Mark as finished just before returning — while unregister() may already
    // be blocked waiting for the lock.
    g_callback_in_progress.store(false, std::memory_order_release);
    return true;
}

static wifi_policy_input_v1_t make_input(void) {
    wifi_policy_input_v1_t in = {};
    in.version = WIFI_POLICY_SHIM_VERSION;
    in.invoke_count = 0;
    in.wifi_connected = 1;
    in.rssi = -50;
    in.channel = 6;
    in.reconnect_count = 0;
    in.ip_fast_active = 0;
    in.ip_fast_ready = 0;
    in.ip_fast_cache_age_ms = 0;
    return in;
}

/* ── Tests ───────────────────────────────────────────────────────────────── */

static void test_null_callback(void) {
    printf("TEST: register(NULL) -> failure ... ");
    bool ok = wifi_policy_register(nullptr, nullptr);
    assert(!ok && "register(nullptr) must return false");
    printf("PASS\n");
}

static void test_unregister_noop(void) {
    printf("TEST: unregister before register -> success/no-op ... ");
    bool ok = wifi_policy_unregister();
    assert(ok && "unregister with no prior registration must return true");
    printf("PASS\n");
}

static void test_register_ok(void) {
    printf("TEST: register callback -> success ... ");
    reset_counters();
    bool ok = wifi_policy_register(test_callback, nullptr);
    assert(ok && "register must succeed");
    assert(wifi_policy_is_registered() && "must be registered after register()");
    printf("PASS\n");
}

static void test_unregister_twice(void) {
    printf("TEST: unregister twice -> success/no-op ... ");
    assert(wifi_policy_is_registered() && "must be registered at start");

    bool first = wifi_policy_unregister();
    assert(first && "first unregister must succeed");
    assert(!wifi_policy_is_registered() && "must not be registered after unregister");

    bool second = wifi_policy_unregister();
    assert(second && "second unregister must succeed (idempotent)");
    assert(!wifi_policy_is_registered() && "still not registered");

    printf("PASS\n");
}

static void test_invoke_no_callback(void) {
    printf("TEST: invoke without callback -> documented safe fallback ... ");
    assert(!wifi_policy_is_registered() && "must start unregistered");

    wifi_policy_action_v1_t out = { .version = 0xFFFFFFFF };  // sentinel
    wifi_policy_input_v1_t   in  = make_input();

    bool ok = wifi_policy_invoke_from_component(&in, &out);
    assert(!ok && "invoke without callback must return false");
    assert(out.version == 0 && "output must be zeroed on fallback");
    assert(out.action_kind == 0 && "action_kind must be zero on fallback");

    printf("PASS\n");
}

static void test_invoke_with_callback(void) {
    printf("TEST: invoke with registered callback -> callback called ... ");
    reset_counters();
    assert(wifi_policy_register(test_callback, nullptr));

    wifi_policy_input_v1_t  in  = make_input();
    wifi_policy_action_v1_t out = { .version = 0xFFFFFFFF };

    bool ok = wifi_policy_invoke_from_component(&in, &out);
    assert(ok && "invoke with callback must return true");
    assert(g_callback_calls == 1 && "callback must be called exactly once");
    assert(out.version == WIFI_POLICY_SHIM_VERSION && "output version must be set");

    wifi_policy_unregister();
    printf("PASS\n");
}

/*
 * test_callback_in_flight_unregister
 *
 * Tests the core invariant: unregister() must not return until the in-flight
 * callback has fully completed and released g_shim_mutex.
 *
 * Correct protocol (no deadlock, no sleep-polling):
 *
 *   1. invoker thread: calls invoke(); inside invoke(), g_shim_mutex is held
 *      for the full callback execution; callback sets g_callback_started, then returns.
 *      After cb() returns, invoke() releases g_shim_mutex.
 *   2. main thread: waits for g_callback_started (callback confirmed in-flight,
 *      holding g_shim_mutex), then calls wifi_policy_unregister() — which blocks
 *      waiting for g_shim_mutex.
 *   3. callback finishes and releases g_shim_mutex; unregister() acquires it,
 *      clears g_callback, returns.
 *   4. main thread (still in wifi_policy_unregister()): returns, asserts.
 *
 * Deadlock-free because the callback always releases g_shim_mutex when it returns,
 * and unregister() waits for that release.  g_callback_in_progress tracks whether
 * the callback has returned yet; a condition variable lets the main thread wait
 * for the invoker to finish without polling.
 */
static void test_callback_in_flight_unregister(void) {
    printf("TEST: callback in flight while unregister begins -> callback finishes ... ");

    reset_counters();
    assert(wifi_policy_register(test_callback, nullptr));

    // Signalled by the invoker thread after the callback has returned and
    // g_shim_mutex has been released.
    std::mutex finish_mutex;
    std::condition_variable finish_cv;
    bool invoker_finished = false;

    // Launch invoker: calls invoke(), which holds g_shim_mutex through the
    // full callback execution.  The callback sets g_callback_started on entry.
    std::thread invoker([&]() {
        wifi_policy_input_v1_t  in  = make_input();
        wifi_policy_action_v1_t out = {};
        wifi_policy_invoke_from_component(&in, &out);

        // Callback has returned and g_shim_mutex is released.  Signal the main thread.
        {
            std::lock_guard<std::mutex> lock(finish_mutex);
            invoker_finished = true;
        }
        finish_cv.notify_one();
    });

    // Wait for the callback to be in-flight (it holds g_shim_mutex).
    while (!g_callback_started.load(std::memory_order_acquire)) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }

    // g_callback_started is true: the callback is confirmed executing inside
    // invoke(), holding g_shim_mutex.  Calling wifi_policy_unregister() here
    // will block waiting for g_shim_mutex.
    bool ok = wifi_policy_unregister();
    assert(ok && "unregister must succeed");
    assert(!wifi_policy_is_registered() && "must not be registered after unregister");

    // At this point the callback has returned and unregister() has cleared
    // g_callback.  Wait for the invoker thread to confirm it has finished.
    {
        std::unique_lock<std::mutex> lock(finish_mutex);
        finish_cv.wait(lock, [&]() { return invoker_finished; });
    }

    invoker.join();

    // The callback completed exactly once (not torn) and unregister() returned
    // only after the callback released g_shim_mutex.
    assert(g_callback_calls == 1 &&
           "callback must have completed exactly once (not torn)");
    assert(!g_callback_in_progress.load(std::memory_order_acquire) &&
           "callback must have returned before unregister() returned");

    printf("PASS\n");
}

static void test_invoke_count_increments(void) {
    printf("TEST: invoke_count increments ... ");
    reset_counters();
    assert(wifi_policy_register(test_callback, nullptr));

    wifi_policy_input_v1_t  in  = make_input();
    wifi_policy_action_v1_t out = {};

    uint32_t before = wifi_policy_invoke_count();
    wifi_policy_invoke_from_component(&in, &out);
    wifi_policy_invoke_from_component(&in, &out);
    uint32_t after = wifi_policy_invoke_count();

    assert(after > before && "invoke count must increase");
    assert(g_callback_calls == 2 && "two invocations must have called the callback twice");

    wifi_policy_unregister();
    printf("PASS\n");
}

static void test_re_register_replaces(void) {
    printf("TEST: re-register replaces previous callback ... ");
    reset_counters();
    assert(wifi_policy_register(test_callback, nullptr));

    int second_calls = 0;
    auto second_cb = [](void* arg, const wifi_policy_input_v1_t*, wifi_policy_action_v1_t* out) -> bool {
        auto* counter = static_cast<int*>(arg);
        ++(*counter);
        std::memset(out, 0, sizeof(*out));
        return true;
    };

    bool ok = wifi_policy_register(second_cb, &second_calls);
    assert(ok && "re-register must succeed");

    wifi_policy_input_v1_t  in  = make_input();
    wifi_policy_action_v1_t out = {};
    wifi_policy_invoke_from_component(&in, &out);

    assert(second_calls == 1 && "second callback must be called");
    assert(g_callback_calls == 0 && "first callback must not be called after re-register");

    wifi_policy_unregister();
    printf("PASS\n");
}

static void test_null_inputs(void) {
    printf("TEST: invoke with null input/output -> false ... ");
    wifi_policy_action_v1_t out = {};
    assert(!wifi_policy_invoke_from_component(nullptr, &out));
    wifi_policy_input_v1_t in = make_input();
    assert(!wifi_policy_invoke_from_component(&in, nullptr));
    printf("PASS\n");
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void) {
    printf("\n=== wifi_policy_shim host fixture ===\n\n");

    test_null_callback();
    test_unregister_noop();
    test_register_ok();
    test_unregister_twice();
    test_invoke_no_callback();
    test_invoke_with_callback();
    test_callback_in_flight_unregister();
    test_invoke_count_increments();
    test_re_register_replaces();
    test_null_inputs();

    printf("\nAll tests passed.\n");
    return 0;
}
