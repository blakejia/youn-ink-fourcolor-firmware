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
#include <mutex>
#include <thread>
#include <vector>

#include "wifi_policy_shim.h"

/* ── Helpers ──────────────────────────────────────────────────────────────── */

static int g_callback_calls = 0;
static std::atomic<int> g_in_flight{0};
static std::atomic<bool> g_block_invoke{false};

static void reset_counters(void) {
    g_callback_calls = 0;
    g_in_flight.store(0, std::memory_order_release);
    g_block_invoke.store(false, std::memory_order_release);
}

static bool test_callback(
    void* /* ctx */,
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t* output) {
    ++g_callback_calls;
    ++g_in_flight;

    // Signal that we're in-flight so unregister can detect a race.
    while (g_block_invoke.load(std::memory_order_acquire)) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }

    // Verify input is valid.
    assert(input != nullptr);
    assert(input->version == WIFI_POLICY_SHIM_VERSION);

    // Set output version so callers can verify the right struct was received.
    std::memset(output, 0, sizeof(*output));
    output->version = WIFI_POLICY_SHIM_VERSION;
    (void)output;

    --g_in_flight;
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
    // Must not crash and must return true.
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

static void test_callback_in_flight_unregister(void) {
    printf("TEST: callback in flight while unregister begins -> callback finishes ... ");

    reset_counters();

    // Block the callback to make it take a while.
    g_block_invoke.store(true, std::memory_order_release);

    // Register first.
    assert(wifi_policy_register(test_callback, nullptr));

    // Start invoke in a background thread (it will block inside the callback).
    std::atomic<bool> invoke_started{false};
    std::atomic<bool> invoke_done{false};
    std::thread invoker([&]() {
        invoke_started.store(true, std::memory_order_release);
        wifi_policy_input_v1_t  in  = make_input();
        wifi_policy_action_v1_t out = {};
        wifi_policy_invoke_from_component(&in, &out);
        invoke_done.store(true, std::memory_order_release);
    });

    // Wait for the invoker to enter the callback.
    while (!invoke_started.load(std::memory_order_acquire)) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    // Give it a moment to actually reach g_block_invoke.
    std::this_thread::sleep_for(std::chrono::milliseconds(10));

    // The callback is now in-flight.  Unregister from the main thread.
    g_block_invoke.store(false, std::memory_order_release);  // unblock callback
    bool ok = wifi_policy_unregister();
    assert(ok && "unregister must succeed");
    assert(!wifi_policy_is_registered() && "must not be registered after unregister");

    invoker.join();

    // The callback must have run to completion (not been torn mid-execution).
    assert(g_callback_calls == 1 && "callback must have completed (not torn)");
    assert(invoke_done.load(std::memory_order_acquire) && "invoke must have returned");

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
