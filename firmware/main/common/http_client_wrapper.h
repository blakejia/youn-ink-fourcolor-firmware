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
 * @brief HTTP GET 请求
 *
 * @param url       完整 URL
 * @param token     Bearer token（NULL 表示公开端点）
 * @param out_buf   响应体缓冲区
 * @param out_len   [in] 缓冲区大小 / [out] 实际响应长度
 * @param timeout_ms 超时毫秒
 * @return HTTP 状态码，-1 表示网络/连接错误
 */
int http_wrapper_get(const char *url, const char *token,
                     char *out_buf, int *out_len, int timeout_ms);

/**
 * @brief HTTP POST (JSON body)
 *
 * @param url        完整 URL
 * @param token      Bearer token（NULL 表示公开端点）
 * @param json_body  JSON 请求体字符串
 * @param out_buf    响应体缓冲区
 * @param out_len    [in] 缓冲区大小 / [out] 实际响应长度
 * @param timeout_ms 超时毫秒
 * @return HTTP 状态码，-1 表示网络/连接错误
 */
int http_wrapper_post_json(const char *url, const char *token,
                           const char *json_body,
                           char *out_buf, int *out_len, int timeout_ms);

#ifdef __cplusplus
}
#endif

#endif  // HTTP_CLIENT_WRAPPER_H
