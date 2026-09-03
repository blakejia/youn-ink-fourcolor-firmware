/**
 * @file http_client_wrapper.cc
 * @brief 薄封装 esp_http_client 实现
 *
 * 参照 photo_downloader.cc 的 http_get 写法：
 * - esp_http_client_config_t + event_handler 收集响应
 * - esp_http_client_perform 同步执行
 * - token 非 NULL 时注入 Authorization: Bearer <token> 头
 */

#include "http_client_wrapper.h"

#include <cstring>
#include <cstdio>
#include <esp_log.h>
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
        if (evt->data_len > 0 && ctx->buf && ctx->len < ctx->buf_size) {
            int space = ctx->buf_size - ctx->len;
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
// 公共 API
// ============================================================

int http_wrapper_get(const char *url, const char *token,
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
    config.method = HTTP_METHOD_GET;
    config.event_handler = http_event_handler;
    config.user_data = &ctx;
    config.timeout_ms = timeout_ms;
    config.disable_auto_redirect = false;

    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (!client) {
        ESP_LOGE(kTag, "Failed to init HTTP client for %s", url);
        return -1;
    }

    set_bearer_header(client, token);

    esp_err_t err = esp_http_client_perform(client);
    if (err != ESP_OK) {
        ESP_LOGE(kTag, "HTTP GET %s failed: %s", url, esp_err_to_name(err));
        esp_http_client_cleanup(client);
        return -1;
    }

    int status = esp_http_client_get_status_code(client);
    out_buf[ctx.len] = '\0';
    *out_len = ctx.len;
    esp_http_client_cleanup(client);
    return status;
}

int http_wrapper_post_json(const char *url, const char *token,
                           const char *json_body,
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
    config.method = HTTP_METHOD_POST;
    config.event_handler = http_event_handler;
    config.user_data = &ctx;
    config.timeout_ms = timeout_ms;
    config.disable_auto_redirect = false;

    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (!client) {
        ESP_LOGE(kTag, "Failed to init HTTP client for %s", url);
        return -1;
    }

    set_bearer_header(client, token);
    esp_http_client_set_header(client, "Content-Type", "application/json");

    if (json_body) {
        esp_http_client_set_post_field(client, json_body, strlen(json_body));
    }

    esp_err_t err = esp_http_client_perform(client);
    if (err != ESP_OK) {
        ESP_LOGE(kTag, "HTTP POST %s failed: %s", url, esp_err_to_name(err));
        esp_http_client_cleanup(client);
        return -1;
    }

    int status = esp_http_client_get_status_code(client);
    out_buf[ctx.len] = '\0';
    *out_len = ctx.len;
    esp_http_client_cleanup(client);
    return status;
}

int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const http_header_t *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms)
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
    config.method = HTTP_METHOD_POST;
    config.event_handler = http_event_handler;
    config.user_data = &ctx;
    config.timeout_ms = timeout_ms;
    config.disable_auto_redirect = false;

    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (!client) {
        ESP_LOGE(kTag, "Failed to init HTTP client for %s", url);
        return -1;
    }

    set_bearer_header(client, token);
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

    esp_err_t err = esp_http_client_perform(client);
    if (err != ESP_OK) {
        ESP_LOGE(kTag, "HTTP POST %s failed: %s", url, esp_err_to_name(err));
        esp_http_client_cleanup(client);
        return -1;
    }

    int status = esp_http_client_get_status_code(client);
    out_buf[ctx.len] = '\0';
    *out_len = ctx.len;
    esp_http_client_cleanup(client);
    return status;
}
