/**
 * @file device_signature.h
 * @brief Device signature generation for pair-start authentication
 */
#ifndef DEVICE_SIGNATURE_H
#define DEVICE_SIGNATURE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Sign a pair-start request.
 *
 * @param device_id  Device ID string (e.g. "NOTE4C-3400FC")
 * @param mac_out    Buffer for hex MAC (12 chars + null)
 * @param mac_len    sizeof(mac_out)
 * @param ts_out     Buffer for timestamp string (10 digits + null)
 * @param ts_len     sizeof(ts_out)
 * @param nonce_out  Buffer for base64 nonce (24 chars + null)
 * @param nonce_len  sizeof(nonce_out)
 * @param sig_out    Buffer for base64 HMAC-SHA256 (44 chars + null)
 * @param sig_len    sizeof(sig_out)
 */
void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len);

#ifdef __cplusplus
}
#endif

#endif
