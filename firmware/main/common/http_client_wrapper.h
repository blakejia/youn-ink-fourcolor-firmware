/**
 * @file http_client_wrapper.h
 * @brief 薄封装 esp_http_client，统一注入 Bearer 鉴权头
 *
 * page_sync / OTA / pairing 共用。
 * token 可为 NULL（公开端点不带鉴权）。
 * 采用 select()-based timeout（参照 photo_downloader.cc，不用 setsockopt）。
 */

#ifndef HTTP_CLIENT_WRAPPER_H
#define HTTP_CLIENT_WRAPPER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Key-value pair for custom HTTP headers.
 */
typedef struct {
    const char *key;
    const char *value;
} http_header_t;

/**
 * @brief HTTP GET request
 *
 * @param url       Full URL
 * @param token     Bearer token (NULL = public endpoint)
 * @param out_buf   Response body buffer (must hold *out_len bytes)
 * @param out_len   [in] buffer capacity including the NUL / [out] payload length
 * @param timeout_ms timeout in milliseconds
 * @return HTTP status code, -1 for network/connection error
 */
int http_wrapper_get(const char *url, const char *token,
                     char *out_buf, int *out_len, int timeout_ms);

/**
 * @brief HTTP POST (JSON body)
 *
 * @param url        Full URL
 * @param token      Bearer token (NULL = public endpoint)
 * @param json_body  JSON request body string
 * @param out_buf    Response body buffer (must hold *out_len bytes)
 * @param out_len    [in] buffer capacity including the NUL / [out] payload length
 * @param timeout_ms timeout in milliseconds
 * @return HTTP status code, -1 for network/connection error
 */
int http_wrapper_post_json(const char *url, const char *token,
                           const char *json_body,
                           char *out_buf, int *out_len, int timeout_ms);

/**
 * @brief HTTP POST (JSON body) with extra custom headers
 *
 * @param url           Full URL
 * @param token         Bearer token (NULL = public endpoint)
 * @param json_body     JSON request body string
 * @param extra_headers Array of extra header key-value pairs (may be NULL)
 * @param extra_count   Number of extra headers (0 if extra_headers is NULL)
 * @param out_buf       Response body buffer
 * @param out_len       [in] buffer capacity including the NUL / [out] payload length
 * @param timeout_ms    timeout in milliseconds
 * @return HTTP status code, -1 for network/connection error
 */
int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const http_header_t *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms);

#ifdef __cplusplus
}
#endif

#endif  // HTTP_CLIENT_WRAPPER_H
