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

// Set by the callback when it has entered and holds g_shim_mutex.
static std::atomic<bool> g_callback_started{false};

// When true the callback is still executing (has not returned yet).
static std::atomic<bool> g_callback_in_progress{false};

// Coordination: callback waits on this CV while holding g_shim_mutex.
static std::mutex g_callback_mutex;
static std::condition_variable g_callback_cv;

// Set by main: callback may now return (releases g_shim_mutex).
static std::atomic<bool> g_release_callback{false};

// Helper: wait on g_callback_cv with a timeout.  Returns true if the
// condition was met before the timeout, false on timeout.
static bool wait_with_timeout(std::unique_lock<std::mutex>& lock,
                             std::condition_variable& cv,
                             std::atomic<bool>& flag,
                             int timeout_ms) {
    auto deadline = std::chrono::steady_clock::now()
                  + std::chrono::milliseconds(timeout_ms);
    while (!flag.load(std::memory_order_acquire)) {
        auto status = cv.wait_until(lock, deadline);
        if (status == std::cv_status::timeout) {
            return false;  // timed out
        }
    }
    return true;
}

/*
 * test_callback: sets g_callback_started on entry (while holding g_shim_mutex),
 * waits on g_callback_cv until g_release_callback is set, then returns.
 */
static bool test_callback(
    void* /* ctx */,
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t* output) {
    ++g_callback_calls;
    g_callback_in_progress.store(true, std::memory_order_release);

    // Signal that we are now inside the callback, holding g_shim_mutex.
    // At this point the callback is definitely in-flight with the lock held.
    g_callback_started.store(true, std::memory_order_release);

    // Wait on the CV while holding the callback mutex.  g_callback_cv
    // is different from g_shim_mutex — this is just for coordination.
    {
        std::unique_lock<std::mutex> lock(g_callback_mutex);
        // Timed wait: 500 ms to detect if the protocol is wrong (e.g. deadlock).
        // The test also asserts unregister did not return within 300 ms.
        bool got_signal = wait_with_timeout(lock, g_callback_cv, g_release_callback, 500);
        if (!got_signal) {
            // Timeout: protocol violation.  Return false so the test fails.
            g_callback_in_progress.store(false, std::memory_order_release);
            return false;
        }
    }

    // Verify input is valid.
    assert(input != nullptr);
    assert(input->version == WIFI_POLICY_SHIM_VERSION);

    std::memset(output, 0, sizeof(*output));
    output->version = WIFI_POLICY_SHIM_VERSION;

    // Mark as finished just before returning.
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
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
    g_release_callback.store(false, std::memory_order_release);
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
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
    g_release_callback.store(true, std::memory_order_release);  // don't block
    bool ok = wifi_policy_register(test_callback, nullptr);
    assert(ok && "register must succeed");

    wifi_policy_input_v1_t  in  = make_input();
    wifi_policy_action_v1_t out = { .version = 0xFFFFFFFF };

    ok = wifi_policy_invoke_from_component(&in, &out);
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
 * Protocol:
 *   1. Callback thread: enters test_callback(), holds g_shim_mutex,
 *      sets g_callback_started, then waits on g_callback_cv until
 *      g_release_callback is set.
 *   2. Main thread: polls g_callback_started until true (callback is in-flight,
 *      holding g_shim_mutex), then calls wifi_policy_unregister() in a background
 *      thread — it will block on g_shim_mutex.
 *   3. Main thread: asserts unregister() does not return within 300 ms
 *      (it must be blocked on the mutex).
 *   4. Main thread: sets g_release_callback = true and notifies g_callback_cv.
 *   5. Callback thread: wait returns, callback returns, g_shim_mutex released.
 *   6. unregister() acquires g_shim_mutex, clears g_callback, returns.
 *   7. Main thread: joins unregister thread, asserts.
 *
 * Deadlock-free: the callback's wait() has a 500 ms timeout.  If the protocol
 * is wrong the test will timeout and the callback returns false.
 */
static void test_callback_in_flight_unregister(void) {
    printf("TEST: callback in flight while unregister begins -> callback finishes ... ");

    // Reset all coordination state.
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
    g_release_callback.store(false, std::memory_order_release);

    assert(wifi_policy_register(test_callback, nullptr));

    // Signalled when the background unregister thread has finished.
    std::mutex unregister_done_mutex;
    std::condition_variable unregister_done_cv;
    bool unregister_done = false;
    auto unregister_done_time = std::chrono::steady_clock::time_point{};

    // Start the invoker thread: it calls invoke(), which holds g_shim_mutex
    // through the full callback.  The callback sets g_callback_started
    // before waiting on g_callback_cv.
    std::thread invoker([&]() {
        wifi_policy_input_v1_t  in  = make_input();
        wifi_policy_action_v1_t out = {};
        wifi_policy_invoke_from_component(&in, &out);
    });

    // Wait for the callback to be in-flight (it holds g_shim_mutex now).
    while (!g_callback_started.load(std::memory_order_acquire)) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }

    // Callback is confirmed in-flight, holding g_shim_mutex.
    // Start the unregister thread — it will block on g_shim_mutex.
    std::thread unregistrar([&]() {
        bool ok = wifi_policy_unregister();
        assert(ok && "unregister must succeed");

        std::lock_guard<std::mutex> lock(unregister_done_mutex);
        unregister_done = true;
        unregister_done_time = std::chrono::steady_clock::now();
        unregister_done_cv.notify_one();
    });

    // Give unregister() 300 ms to confirm it has NOT returned yet
    // (it must be blocked on g_shim_mutex while the callback holds it).
    std::this_thread::sleep_for(std::chrono::milliseconds(300));
    {
        std::lock_guard<std::mutex> lock(unregister_done_mutex);
        assert(!unregister_done && "unregister must not return while callback is in-flight");
    }

    // Now allow the callback to return by setting g_release_callback
    // and notifying the callback's wait.
    g_release_callback.store(true, std::memory_order_release);
    g_callback_cv.notify_one();

    // Wait for unregister() to complete.
    {
        std::unique_lock<std::mutex> lock(unregister_done_mutex);
        unregister_done_cv.wait(lock, [&]() { return unregister_done; });
    }

    // unregister() returned only after the callback released g_shim_mutex.
    // Check both time order and state.
    assert(g_callback_calls == 1 &&
           "callback must have completed exactly once (not torn)");
    assert(!g_callback_in_progress.load(std::memory_order_acquire) &&
           "callback must have returned before unregister() returned");
    assert(!wifi_policy_is_registered() &&
           "must not be registered after unregister");

    invoker.join();
    unregistrar.join();

    printf("PASS\n");
}

static void test_invoke_count_increments(void) {
    printf("TEST: invoke_count increments ... ");
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
    g_release_callback.store(true, std::memory_order_release);  // don't block
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
    g_callback_calls = 0;
    g_callback_started.store(false, std::memory_order_release);
    g_callback_in_progress.store(false, std::memory_order_release);
    g_release_callback.store(true, std::memory_order_release);
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
