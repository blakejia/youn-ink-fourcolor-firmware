/**
 * @file shim.c
 * @brief Public pair-start signing entry point, gathering the device inputs.
 *
 * `device_sign_pair_start` keeps the exact signature declared in
 * `main/common/device_signature.h`, so no caller changed when the cryptography
 * moved to Rust (`rust/src/ffi.rs::devsig_sign`). Splitting it this way also
 * keeps the link order trivial: this object lives in the component's archive
 * and its only unresolved symbol (`devsig_sign`) is resolved by the Rust
 * staticlib that CMake attaches immediately after that archive.
 */
#include <stddef.h>
#include <stdint.h>
#include <time.h>

#include <esp_mac.h>
#include <esp_random.h>

#include "device_signature.h"

/* Implemented in Rust. Writes mac_hex / timestamp / nonce_b64 / sig_b64, each
 * NUL-terminated; a buffer too small for its field is left as an empty string. */
extern void devsig_sign(const char *device_id,
                        const uint8_t *mac_raw,
                        int64_t timestamp,
                        const uint8_t *nonce_raw,
                        char *mac_hex_out, size_t mac_hex_len,
                        char *ts_out, size_t ts_len,
                        char *nonce_out, size_t nonce_len,
                        char *sig_out, size_t sig_len);

#define DEVSIG_MAC_LEN 6
#define DEVSIG_NONCE_LEN 16

void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len)
{
    if (device_id == NULL) {
        return;
    }

    uint8_t mac[DEVSIG_MAC_LEN];
    if (esp_read_mac(mac, ESP_MAC_WIFI_STA) != ESP_OK) {
        return; /* leave every field untouched, as before */
    }

    uint8_t nonce[DEVSIG_NONCE_LEN];
    esp_fill_random(nonce, sizeof(nonce));

    /* Unix seconds from the SNTP-synced wall clock (not boot uptime). */
    devsig_sign(device_id, mac, (int64_t)time(NULL), nonce,
                mac_out, mac_len, ts_out, ts_len,
                nonce_out, nonce_len, sig_out, sig_len);
}
