/**
 * @file http_client_wrapper.cc
 * @brief 薄封装 esp_http_client 实现
 *
 * 参照 photo_downloader.cc 的 http_get 写法：
 * - esp_http_client_config_t + event_handler 收集响应
 * - esp_http_client_perform 同步执行
 * - token 非 NULL 时注入 Authorization: Bearer <token> 头
 *
 * GET 与 POST 只差一个 method、一个可选 body 与可选附加头，所以两者走同一个
 * do_request()：响应收集与缓冲边界（下面那个 -1）只有一份。
 */

#include "http_client_wrapper.h"

#include <cstdio>
#include <cstring>
#include <esp_log.h>
#include <esp_crt_bundle.h>
#include <esp_http_client.h>

static const char *kTag = "HttpWrap";

// ============================================================
// HTTP event handler（收集响应数据）
// ============================================================

typedef struct {
    char *buf;
    int buf_size;
    int len;
} HttpRespCtx;

static esp_err_t http_event_handler(esp_http_client_event_t *evt)
{
    auto *ctx = static_cast<HttpRespCtx *>(evt->user_data);
    switch (evt->event_id) {
    case HTTP_EVENT_ERROR:
        ESP_LOGE(kTag, "HTTP event error");
        break;
    case HTTP_EVENT_ON_DATA:
        // -1 reserves the terminator: the payload may fill at most 容量-1 bytes,
        // so the NUL written below can never land past the caller's buffer.
        if (evt->data_len > 0 && ctx->buf && ctx->len < ctx->buf_size - 1) {
            int space = ctx->buf_size - 1 - ctx->len;
            int copy_len = evt->data_len < space ? evt->data_len : space;
            memcpy(ctx->buf + ctx->len, evt->data, copy_len);
            ctx->len += copy_len;
        }
        break;
    default:
        break;
    }
    return ESP_OK;
}

// ============================================================
// 内部辅助：设置 Bearer 鉴权头
// ============================================================

static void set_bearer_header(esp_http_client_handle_t client, const char *token)
{
    if (token && token[0] != '\0') {
        char bearer[80];
        snprintf(bearer, sizeof(bearer), "Bearer %s", token);
        esp_http_client_set_header(client, "Authorization", bearer);
    }
}

// ============================================================
// 内部辅助：唯一一处请求实现
// ============================================================

static int do_request(esp_http_client_method_t method, const char *url,
                      const char *token, const char *json_body,
                      const http_header_t *extra_headers, int extra_count,
                      char *out_buf, int *out_len, int timeout_ms)
{
    if (!url || !out_buf || !out_len || *out_len <= 0) {
        return -1;
    }

    HttpRespCtx ctx = {};
    ctx.buf = out_buf;
    ctx.buf_size = *out_len;
    ctx.len = 0;

    esp_http_client_config_t config = {};
    config.url = url;
    config.method = method;
    config.event_handler = http_event_handler;
    config.user_data = &ctx;
    config.timeout_ms = timeout_ms;
    config.disable_auto_redirect = false;
    // HTTPS 必须显式指定服务器校验方式，否则 esp-tls/mbedTLS 直接拒绝建连：
    //   "No server verification option set in esp_tls_cfg_t structure"
    //   → ESP_ERR_MBEDTLS_SSL_SETUP_FAILED（一个字节都不会发出去）
    // 线上域名是 Let's Encrypt 签发的 *.1024.center，公共 CA 即可验过；
    // 证书包由 CONFIG_MBEDTLS_CERTIFICATE_BUNDLE=y（DEFAULT_FULL）提供。
    // 写法与 main/components/78__esp-ml307/src/esp/esp_ssl.cc:71 一致。
    config.crt_bundle_attach = esp_crt_bundle_attach;

    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (!client) {
        ESP_LOGE(kTag, "Failed to init HTTP client for %s", url);
        return -1;
    }

    set_bearer_header(client, token);

    if (method == HTTP_METHOD_POST) {
        esp_http_client_set_header(client, "Content-Type", "application/json");
        if (extra_headers && extra_count > 0) {
            for (int i = 0; i < extra_count; i++) {
                esp_http_client_set_header(client,
                                           extra_headers[i].key,
                                           extra_headers[i].value);
            }
        }
        if (json_body) {
            esp_http_client_set_post_field(client, json_body, strlen(json_body));
        }
    }

    esp_err_t err = esp_http_client_perform(client);
    if (err != ESP_OK) {
        ESP_LOGE(kTag, "HTTP %s %s failed: %s",
                 method == HTTP_METHOD_POST ? "POST" : "GET", url,
                 esp_err_to_name(err));
        esp_http_client_cleanup(client);
        return -1;
    }

    int status = esp_http_client_get_status_code(client);
    // Always safe: ctx.len <= buf_size - 1 (see HTTP_EVENT_ON_DATA).
    out_buf[ctx.len] = '\0';
    *out_len = ctx.len;
    esp_http_client_cleanup(client);
    return status;
}

// ============================================================
// 公共 API
// ============================================================

int http_wrapper_get(const char *url, const char *token,
                     char *out_buf, int *out_len, int timeout_ms)
{
    return do_request(HTTP_METHOD_GET, url, token, nullptr, nullptr, 0,
                      out_buf, out_len, timeout_ms);
}

int http_wrapper_post_json(const char *url, const char *token,
                           const char *json_body,
                           const http_header_t *extra_headers,
                           int extra_count,
                           char *out_buf, int *out_len,
                           int timeout_ms)
{
    return do_request(HTTP_METHOD_POST, url, token, json_body,
                      extra_headers, extra_count, out_buf, out_len, timeout_ms);
}
