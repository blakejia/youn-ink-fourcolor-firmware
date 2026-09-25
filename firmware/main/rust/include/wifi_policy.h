/*
 * wifi_policy.h — Wi-Fi policy C ABI
 *
 * Rust owns the codec, endpoint parser, and decision table.
 * C++ gathers facts, calls `rf_wifi_policy_decide`, and executes the action.
 *
 * ABI version 1 — structs are fixed-size POD; no pointers, no strings.
 */

#ifndef WIFI_POLICY_H
#define WIFI_POLICY_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Version ─────────────────────────────────────────────────────────────── */

#define RF_WIFI_POLICY_VERSION 1

/* ── Input POD ────────────────────────────────────────────────────────────
 *
 * All fields are copied in; the callee owns nothing.
 */
typedef struct rf_wifi_policy_inputs {
    uint32_t version;

    /* ── Wi-Fi state ─────────────────────────────────────────── */
    uint32_t invoke_count;
    int32_t  wifi_connected;          /* 0 = disconnected, 1 = connected */
    int32_t  rssi;                   /* dBm, valid only when connected > 0 */
    uint8_t  channel;                 /* 0 when disconnected              */
    uint8_t  _pad0[3];
    uint32_t reconnect_count;

    /* ── IP fast state ───────────────────────────────────────── */
    int32_t  ip_fast_active;         /* 0 = not running, 1 = running      */
    int32_t  ip_fast_ready;          /* 0 = not done, 1 = probe succeeded */
    uint32_t ip_fast_cache_age_ms;   /* valid when ip_fast_active == 1    */

    /* ── Wi-Fi fast cache ────────────────────────────────────── */
    uint8_t  have_wifi_cache;
    uint8_t  cache_bssid_valid;
    uint8_t  cache_channel;
    uint8_t  _pad1;
    uint8_t  cache_ssid[32];         /* NOT NUL-terminated                */
    uint8_t  cache_ssid_len;
    uint8_t  cache_bssid[6];
    int32_t  wifi_cache_age_ms;

    /* ── IP fast cache ───────────────────────────────────────── */
    uint8_t  have_ip_cache;
    uint8_t  _pad2[3];
    int32_t  ip_cache_age_ms;

    /* ── Policy knobs ─────────────────────────────────────────── */
    int32_t  fast_fail_count;        /* consecutive fast-connect failures */
    uint8_t  fast_enabled;            /* 0 = disabled, 1 = enabled        */
    uint8_t  endpoint_present;        /* 0 = missing, 1 = present        */
    uint8_t  probe_target;           /* NetworkProbeTarget: 0=HttpOta, 1=WebSocket, 2=Mqtt */
    uint8_t  host_is_ip_literal;      /* 0 = DNS name, 1 = IPv4 literal  */
} rf_wifi_policy_inputs_t;

/* ── Output POD ──────────────────────────────────────────────────────────── */

typedef struct rf_wifi_policy_output {
    uint32_t version;

    /* Action kind (matches wifi_policy_shim.h::wifi_policy_action_v1_t::action_kind):
     *   0  = no-op
     *   10 = DirectConnect
     *   11 = Probe
     *   12 = Scan
     *   13 = Retry
     *   14 = Stop
     *   20 = DeferProbe
     */
    uint8_t  action_kind;
    uint8_t  clear_wifi_cache;
    uint8_t  clear_ip_cache;
    uint32_t retry_delay_ms;          /* 0 = use component default       */
    uint8_t  retain_ip;               /* for DeferProbe: keep IP cache    */
} rf_wifi_policy_output_t;

/* ── Decision entry point ───────────────────────────────────────────────── */

/**
 * Decide the Wi-Fi policy action from the given inputs.
 *
 * @param inp  Immutable inputs gathered by C++.
 * @param out  Caller-allocated output; all fields written on return.
 *
 * # Safety
 * Both pointers must be non-null and point to valid, aligned structs.
 */
void rf_wifi_policy_decide(
    const rf_wifi_policy_inputs_t* inp,
    rf_wifi_policy_output_t*       out);

/* ── RTC cache codec ───────────────────────────────────────────────────── */
uint8_t rf_wifi_encode_rtc_cache(const uint8_t* ssid, uint32_t ssid_len,
                                 const uint8_t* bssid, uint8_t channel,
                                 uint8_t* buf, uint32_t buf_len);
uint8_t rf_wifi_decode_rtc_cache(const uint8_t* buf, uint32_t buf_len,
                                 uint8_t* ssid_out, uint32_t ssid_out_cap,
                                 uint8_t* bssid_out, uint8_t* channel_out);
uint8_t rf_wifi_validate_rtc_cache(const uint8_t* buf, uint32_t buf_len);

/* ── Endpoint parsing helpers ────────────────────────────────────────────── */

/**
 * Parse an MQTT endpoint (plain host[:port]) and write the host to a buffer.
 *
 * @param input       NUL-terminated endpoint string.
 * @param host_buf    Output buffer for the parsed host (min 256 bytes).
 * @param host_buf_len Length of host_buf.
 * @param port        Output port (set to 8883 if no port in string).
 * @return true on success, false on parse failure.
 *
 * # Safety
 * `input` must be a valid NUL-terminated string.
 * `host_buf` must point to at least 256 writable bytes.
 * `port` must not be null.
 */
uint8_t rf_wifi_parse_endpoint(
    const char* input,
    uint8_t*   host_buf,
    uint32_t   host_buf_len,
    uint16_t*  port);

/**
 * Parse a URL authority (scheme://host[:port]/...) and write the host to a buffer.
 *
 * Requires http://, https://, ws://, or wss://.
 * Default ports: http/ws → 80, https/wss → 443.
 *
 * @param input       NUL-terminated URL string.
 * @param host_buf    Output buffer for the parsed host (min 256 bytes).
 * @param host_buf_len Length of host_buf.
 * @param port        Output port.
 * @return true on success, false on parse failure.
 *
 * # Safety
 * `input` must be a valid NUL-terminated string.
 * `host_buf` must point to at least 256 writable bytes.
 * `port` must not be null.
 */
uint8_t rf_wifi_parse_url_authority(
    const char* input,
    uint8_t*   host_buf,
    uint32_t   host_buf_len,
    uint16_t*  port);

/* ── Access-point health (modem-sleep suppression) ──────────────────────── */

/* WIFI_REASON_BEACON_TIMEOUT from esp_wifi_types_generic.h. */
#define RF_WIFI_REASON_BEACON_TIMEOUT 200
/* Consecutive BEACON_TIMEOUT disconnects that mean the AP cannot hold a
 * modem-sleep schedule; see wifi_policy.rs for the evidence. */
#define RF_WIFI_MODEM_SLEEP_SUPPRESS_AFTER 3
/* A connection that lasted at least this long counts as healthy: its
 * BEACON_TIMEOUT starts a fresh run instead of extending the previous one. */
#define RF_WIFI_HEALTHY_CONNECTION_MS 60000ULL

/**
 * Advance the BEACON_TIMEOUT streak for the disconnect just observed.
 * `connected_ms` is how long the ended connection lasted; any reason other
 * than BEACON_TIMEOUT clears the streak.
 */
uint32_t rf_wifi_beacon_timeout_streak(
    uint32_t streak,
    int32_t  reason,
    uint64_t connected_ms);

/**
 * Whether modem sleep must be off for this association: true only after
 * repeated BEACON_TIMEOUT disconnects.
 */
uint8_t rf_wifi_suppress_modem_sleep(uint32_t streak, int32_t reason);

#ifdef __cplusplus
}
#endif

#endif /* WIFI_POLICY_H */
