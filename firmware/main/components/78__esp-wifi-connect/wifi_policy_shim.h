/*
 * wifi_policy_shim.h — Wi-Fi policy callback seam
 *
 * Component-owned narrow ABI between the independent esp-wifi-connect component
 * and the main-owned wifi_policy_adapter.  The component (and its event-handler
 * task) never includes main headers or Rust headers.
 *
 * No allocation, no ownership transfer, no callback back into the component,
 * no blocking operations.  The callback may not be invoked from an ISR.
 *
 * ABI version 1 — structs are fixed-size POD; the callback pointer is opaque.
 */

#ifndef WIFI_POLICY_SHIM_H
#define WIFI_POLICY_SHIM_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Version ─────────────────────────────────────────────────────────────── */

#define WIFI_POLICY_SHIM_VERSION 1

/* ── Input POD ─────────────────────────────────────────────────────────────
 *
 * All fields are copied in; the callee owns nothing.  Fields not recognised
 * by a version-1 adapter are zero-initialised by the caller.
 */
typedef struct wifi_policy_input_v1 {
    uint32_t version;               /**< Must be WIFI_POLICY_SHIM_VERSION   */
    uint32_t invoke_count;          /**< Monotonic call counter              */
    int      wifi_connected;        /**< 0 = disconnected, 1 = connected    */
    int      rssi;                  /**< dBm, valid only when connected > 0 */
    int      channel;               /**< 0 when disconnected               */
    uint32_t reconnect_count;        /**< Current reconnect attempt number  */
    int      ip_fast_active;        /* 0 = not running, 1 = running       */
    int      ip_fast_ready;         /* 0 = not done, 1 = probe succeeded */
    uint32_t ip_fast_cache_age_ms;  /* 0 when ip_fast_active == 0         */
    uint8_t  have_wifi_cache;
    uint8_t  cache_bssid_valid;
    uint8_t  cache_channel;
    uint8_t  cache_ssid[32];
    uint8_t  cache_ssid_len;
    uint8_t  cache_bssid[6];
    int32_t  wifi_cache_age_ms;
    uint8_t  have_ip_cache;
    int32_t  ip_cache_age_ms;
    int32_t  fast_fail_count;
    uint8_t  fast_enabled;
    uint8_t  endpoint_present;
    uint8_t  probe_target;
    uint8_t  host_is_ip_literal;
} wifi_policy_input_v1_t;


/* ── Action POD ────────────────────────────────────────────────────────────
 *
 * Zero-initialised struct means "take no action" (documented safe fallback
 * when no callback is registered).
 */
typedef struct wifi_policy_action_v1 {
    uint32_t version;               /**< Must be WIFI_POLICY_SHIM_VERSION   */
    /* Action kind.  Values 0–2 are reserved for the shim layer itself:
     *   0 = no-op / safe fallback
     *   1 = invoke registered callback
     *  ≥3 = reserved for future shim extensions
     * Adapter-returned values must be ≥ 10 so they never collide. */
    uint32_t action_kind;
    /* Retry suppression: when non-zero the component may skip the next
     * automatic reconnect attempt for this many ms.  0 = use default. */
    uint32_t retry_delay_ms;
    /* Cache directives */
    uint32_t clear_wifi_cache  : 1;
    uint32_t clear_ip_cache    : 1;
    uint32_t retain_ip         : 1;
    uint32_t : 29;  /* padding — do not rely on other bits */
} wifi_policy_action_v1_t;

/* ── Callback type ─────────────────────────────────────────────────────────
 *
 * input  — pointer to immutable caller's memory; the callback must not retain it.
 * output — pointer to caller-allocated output struct; the callback fills it.
 *
 * The callback returns true on success, false on error (shim logs but does not
 * abort).  A false return is treated as a safe no-op.
 */
typedef bool (*wifi_policy_callback_t)(
    void*                    context,   /* opaque, set at registration */
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t*        output);

/* ── Public C API ─────────────────────────────────────────────────────────── */

/**
 * Register a policy callback.
 *
 * @param cb     The callback function.  NULL is rejected (returns false).
 * @param ctx    Opaque context forwarded to every callback invocation.
 * @return true  Registration succeeded and is live for the next invocation.
 * @return false NULL callback, or registration failed.
 *
 * Thread-safety: safe to call from any task after esp_wifi_init().
 * Re-registration replaces the previous callback without requiring unregister.
 * Ordinary StartStation/StopStation cycles do NOT unregister the callback.
 */
bool wifi_policy_register(wifi_policy_callback_t cb, void* ctx);

/**
 * Unregister the current policy callback.
 *
 * Idempotent: calling with no prior registration is a no-op (returns true).
 *
 * Guarantees that any callback already in flight (on any task) completes
 * before this function returns.  After unregister returns, the shim will
 * use the safe no-op action on every subsequent policy decision until a
 * new registration is made.
 *
 * Thread-safety: safe to call from any task.  Must not be called while
 * wifi_policy_invoke_from_component() is holding the invocation guard.
 */
bool wifi_policy_unregister(void);

/**
 * Invoke the registered policy callback with the given input facts.
 *
 * Documented safe fallback when no callback is registered: returns false,
 * output is zeroed, caller uses its default path.
 *
 * @return true  Callback was invoked and returned true.
 * @return false No callback registered, or callback returned false.
 *              Output has been zeroed; caller must fall back safely.
 *
 * Thread-safety: guarded by a short-lived mutex.  A concurrent unregister
 * will block until this function returns.
 */
bool wifi_policy_invoke_from_component(
    const wifi_policy_input_v1_t* input,
    wifi_policy_action_v1_t*       output);

/**
 * Returns the current invocation counter — for diagnostics only.
 */
uint32_t wifi_policy_invoke_count(void);

/**
 * Returns true if a callback is currently registered.
 */
bool wifi_policy_is_registered(void);

bool wifi_policy_encode_rtc_cache(const uint8_t* ssid, uint32_t ssid_len,
                                  const uint8_t* bssid, uint8_t channel,
                                  uint8_t* buf, uint32_t buf_len);
bool wifi_policy_decode_rtc_cache(const uint8_t* buf, uint32_t buf_len,
                                  uint8_t* ssid_out, uint32_t ssid_out_cap,
                                  uint8_t* bssid_out, uint8_t* channel_out);
bool wifi_policy_validate_rtc_cache(const uint8_t* buf, uint32_t buf_len);
bool wifi_policy_parse_endpoint(const char* input, uint8_t* host_buf,
                                uint32_t host_buf_len, uint16_t* port);
bool wifi_policy_parse_url_authority(const char* input, uint8_t* host_buf,
                                     uint32_t host_buf_len, uint16_t* port);

/* ── Access-point health (Rust: wifi_policy.rs) ─────────────────────────── */

/* WIFI_REASON_BEACON_TIMEOUT from esp_wifi_types_generic.h. */
#define WIFI_POLICY_REASON_BEACON_TIMEOUT 200

/* Advance the BEACON_TIMEOUT streak for the disconnect just observed.
 * `connected_ms` is how long the ended connection lasted; any reason other
 * than BEACON_TIMEOUT clears the streak. */
uint32_t rf_wifi_beacon_timeout_streak(uint32_t streak, int32_t reason,
                                       uint64_t connected_ms);

/* Whether modem sleep must be off for this association (repeated
 * BEACON_TIMEOUT disconnects only). */
uint8_t rf_wifi_suppress_modem_sleep(uint32_t streak, int32_t reason);

#ifdef __cplusplus
}
#endif

#endif /* WIFI_POLICY_SHIM_H */
