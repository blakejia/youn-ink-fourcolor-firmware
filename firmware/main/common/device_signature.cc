#include "device_signature.h"
#include "boards/zectrix-s3-epaper-4.2/config.h"
#include <esp_mac.h>
#include <esp_timer.h>
#include <esp_random.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

// ─── SHA-256 (RFC 6234, no external deps) ───────────────────────────────────

#define SHA256_BLOCK_SIZE 64   // bytes
#define SHA256_DIGEST_SIZE 32   // bytes

typedef struct {
    uint32_t state[8];
    uint64_t bitlen;
    uint32_t buflen;
    uint8_t  buffer[64];
} sha256_ctx_t;

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t rotr(uint32_t x, uint8_t n) { return (x >> n) | (x << (32 - n)); }
static uint32_t ch(uint32_t x, uint32_t y, uint32_t z) { return (x & y) ^ (~x & z); }
static uint32_t maj(uint32_t x, uint32_t y, uint32_t z) { return (x & y) ^ (x & z) ^ (y & z); }
static uint32_t sigma0(uint32_t x) { return rotr(x, 2) ^ rotr(x, 13) ^ rotr(x, 22); }
static uint32_t sigma1(uint32_t x) { return rotr(x, 6) ^ rotr(x, 11) ^ rotr(x, 25); }
static uint32_t gamma0(uint32_t x) { return rotr(x, 7) ^ rotr(x, 18) ^ (x >> 3); }
static uint32_t gamma1(uint32_t x) { return rotr(x, 17) ^ rotr(x, 19) ^ (x >> 10); }

static void sha256_transform(sha256_ctx_t *ctx) {
    uint32_t W[64];
    for (int t = 0; t < 16; t++) {
        W[t] = (uint32_t)ctx->buffer[t * 4] << 24 |
               (uint32_t)ctx->buffer[t * 4 + 1] << 16 |
               (uint32_t)ctx->buffer[t * 4 + 2] << 8 |
               (uint32_t)ctx->buffer[t * 4 + 3];
    }
    for (int t = 16; t < 64; t++) {
        W[t] = gamma1(W[t-2]) + W[t-7] + gamma0(W[t-15]) + W[t-16];
    }
    uint32_t a = ctx->state[0], b = ctx->state[1], c = ctx->state[2],
             d = ctx->state[3], e = ctx->state[4], f = ctx->state[5],
             g = ctx->state[6], h = ctx->state[7];
    for (int t = 0; t < 64; t++) {
        uint32_t T1 = h + sigma1(e) + ch(e, f, g) + K[t] + W[t];
        uint32_t T2 = sigma0(a) + maj(a, b, c);
        h = g; g = f; f = e; e = d + T1;
        d = c; c = b; b = a; a = T1 + T2;
    }
    ctx->state[0] += a; ctx->state[1] += b; ctx->state[2] += c; ctx->state[3] += d;
    ctx->state[4] += e; ctx->state[5] += f; ctx->state[6] += g; ctx->state[7] += h;
}

static void sha256_init(sha256_ctx_t *ctx) {
    ctx->state[0] = 0x6a09e667; ctx->state[1] = 0xbb67ae85;
    ctx->state[2] = 0x3c6ef372; ctx->state[3] = 0xa54ff53a;
    ctx->state[4] = 0x510e527f; ctx->state[5] = 0x9b05688c;
    ctx->state[6] = 0x1f83d9ab; ctx->state[7] = 0x5be0cd19;
    ctx->bitlen = 0; ctx->buflen = 0;
}

static void sha256_update(sha256_ctx_t *ctx, const uint8_t *data, size_t len) {
    for (size_t i = 0; i < len; i++) {
        ctx->buffer[ctx->buflen++] = data[i];
        if (ctx->buflen == SHA256_BLOCK_SIZE) {
            sha256_transform(ctx);
            ctx->bitlen += 512;
            ctx->buflen = 0;
        }
    }
}

static void sha256_final(sha256_ctx_t *ctx, uint8_t *out) {
    uint32_t i = ctx->buflen;
    ctx->buffer[i++] = 0x80;
    if (ctx->buflen < 56) {
        memset(ctx->buffer + i, 0, 56 - i);
    } else {
        memset(ctx->buffer + i, 0, SHA256_BLOCK_SIZE - i);
        sha256_transform(ctx);
        memset(ctx->buffer, 0, 56);
    }
    ctx->bitlen += ctx->buflen * 8;
    ctx->buffer[63] = (uint8_t)(ctx->bitlen);
    ctx->buffer[62] = (uint8_t)(ctx->bitlen >> 8);
    ctx->buffer[61] = (uint8_t)(ctx->bitlen >> 16);
    ctx->buffer[60] = (uint8_t)(ctx->bitlen >> 24);
    ctx->buffer[59] = (uint8_t)(ctx->bitlen >> 32);
    ctx->buffer[58] = (uint8_t)(ctx->bitlen >> 40);
    ctx->buffer[57] = (uint8_t)(ctx->bitlen >> 48);
    ctx->buffer[56] = (uint8_t)(ctx->bitlen >> 56);
    sha256_transform(ctx);
    for (i = 0; i < 8; i++) {
        out[i * 4]     = (uint8_t)(ctx->state[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(ctx->state[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(ctx->state[i] >> 8);
        out[i * 4 + 3] = (uint8_t)ctx->state[i];
    }
}

static void sha256(const uint8_t *data, size_t len, uint8_t *out) {
    sha256_ctx_t ctx;
    sha256_init(&ctx);
    sha256_update(&ctx, data, len);
    sha256_final(&ctx, out);
}

// ─── HMAC-SHA256 ─────────────────────────────────────────────────────────────

static void hmac_sha256(const uint8_t *key, size_t key_len,
                         const uint8_t *msg, size_t msg_len,
                         uint8_t out[32]) {
    uint8_t k_pad[64] = {0};
    if (key_len > 64) {
        sha256(key, key_len, k_pad);
    } else {
        memcpy(k_pad, key, key_len);
    }
    uint8_t ipad[64], opad[64];
    for (int i = 0; i < 64; i++) {
        ipad[i] = k_pad[i] ^ 0x36;
        opad[i] = k_pad[i] ^ 0x5C;
    }
    uint8_t inner[32];
    sha256_ctx_t tctx = {0};
    sha256_init(&tctx);
    sha256_update(&tctx, ipad, 64);
    sha256_update(&tctx, msg, msg_len);
    sha256_final(&tctx, inner);
    sha256_ctx_t tctx2 = {0};
    sha256_init(&tctx2);
    sha256_update(&tctx2, opad, 64);
    sha256_update(&tctx2, inner, 32);
    sha256_final(&tctx2, out);
}

// ─── Base64 (no padding for 16/32-byte inputs) ───────────────────────────────

static const char kB64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static void base64_encode_nopad(const uint8_t *in, size_t in_len, char *out) {
    size_t o = 0;
    for (size_t i = 0; i < in_len; i += 3) {
        uint32_t triple = (uint32_t)in[i] << 16;
        if (i + 1 < in_len) triple |= (uint32_t)in[i+1] << 8;
        if (i + 2 < in_len) triple |= (uint32_t)in[i+2];
        out[o++] = kB64[(triple >> 18) & 0x3F];
        out[o++] = kB64[(triple >> 12) & 0x3F];
        if (i + 1 < in_len) out[o++] = kB64[(triple >> 6) & 0x3F];
        if (i + 2 < in_len) out[o++] = kB64[triple & 0x3F];
    }
    out[o] = '\0';
}

// ─── Public API ──────────────────────────────────────────────────────────────

void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len) {
    // 1. MAC (hex, 12 chars)
    uint8_t mac[6];
    esp_read_mac(mac, ESP_MAC_WIFI_STA);
    snprintf(mac_out, mac_len, "%02X%02X%02X%02X%02X%02X",
             mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

    // 2. Timestamp (unix seconds)
    int64_t now_us = esp_timer_get_time();
    int64_t now_s = now_us / 1000000;
    snprintf(ts_out, ts_len, "%lld", (long long)now_s);

    // 3. Nonce (16 random bytes → 24-char base64)
    uint8_t nonce_raw[16];
    esp_fill_random(nonce_raw, 16);
    base64_encode_nopad(nonce_raw, 16, nonce_out);

    // 4. Derive key: derived_key = HMAC-SHA256(DEVICE_MASTER_KEY, device_id)
    uint8_t derived_key[32];
    hmac_sha256(
        (const uint8_t *)DEVICE_MASTER_KEY, strlen(DEVICE_MASTER_KEY),
        (const uint8_t *)device_id, strlen(device_id),
        derived_key
    );

    // 5. Build payload: MAC(6) || timestamp_str || nonce_b64_str
    size_t ts_str_len  = strlen(ts_out);
    size_t nonce_str_len = strlen(nonce_out);
    size_t payload_len = 6 + ts_str_len + nonce_str_len;
    uint8_t *payload = (uint8_t *)malloc(payload_len);
    if (!payload) return;
    memcpy(payload, mac, 6);
    memcpy(payload + 6, ts_out, ts_str_len);
    memcpy(payload + 6 + ts_str_len, nonce_out, nonce_str_len);

    // 6. Sign: sig = base64(HMAC-SHA256(derived_key, payload))
    uint8_t sig_raw[32];
    hmac_sha256(derived_key, 32, payload, payload_len, sig_raw);
    base64_encode_nopad(sig_raw, 32, sig_out);

    free(payload);
}
