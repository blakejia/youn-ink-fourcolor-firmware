/**
 * @file server_pairing.cc
 * @brief 服务端配对鉴权实现
 *
 * 三步配对协议:
 *   1. POST pair-start  → {"code":"482913","expires_in":300}
 *   2. 用户在服务端确认
 *   3. POST pair-claim  → 200 {"token":"<64hex>"} / 401
 *
 * 成功后 token 写入 NVS namespace "server"。
 * 5 分钟超时重新 pair-start。
 */

#include "server_pairing.h"
#include "http_client_wrapper.h"
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include <esp_timer.h>
#include <esp_log.h>
#include <cJSON.h>
#include <esp_mac.h>
#include <nvs.h>
#include <nvs_flash.h>

static const char *kTag = "Pairing";

// NVS namespace "server" keys
static const char *kNvsNamespace = "server";
static const char *kNvsBaseUrl = "base_url";
static const char *kNvsToken = "token";
static const char *kNvsDeviceId = "device_id";

// Board type 常量（与 CMakeLists.txt BOARD_TYPE 一致）
static const char *kBoardType = "zectrix-s3-epaper-4.2";

// 配对超时：5 分钟（300 秒）
static const int kPairingTimeoutSec = 300;
// 轮询间隔：2 秒
static const int kClaimPollIntervalMs = 2000;
// HTTP 超时
static const int kHttpTimeoutMs = 5000;

// 显示回调
static server_pair_display_cb_t s_display_cb = nullptr;

// ============================================================
// NVS 辅助函数
// ============================================================

static bool nvs_read_str(const char *key, char *buf, int buf_len)
{
    nvs_handle_t handle;
    if (nvs_open(kNvsNamespace, NVS_READONLY, &handle) != ESP_OK) {
        return false;
    }
    size_t len = buf_len;
    esp_err_t err = nvs_get_str(handle, key, buf, &len);
    nvs_close(handle);
    return (err == ESP_OK && len > 1);  // len 包含 '\0'
}

static bool nvs_write_str(const char *key, const char *value)
{
    nvs_handle_t handle;
    if (nvs_open(kNvsNamespace, NVS_READWRITE, &handle) != ESP_OK) {
        ESP_LOGE(kTag, "NVS open failed for write");
        return false;
    }
    esp_err_t err = nvs_set_str(handle, key, value);
    if (err == ESP_OK) {
        nvs_commit(handle);
    }
    nvs_close(handle);
    return (err == ESP_OK);
}

// ============================================================
// 设备 ID 生成：efuse MAC 后 3 字节 → NOTE4C-XXXXXX
// ============================================================

static bool generate_device_id(char *buf, int buf_len)
{
    // 先尝试从 NVS 读取已有的
    if (nvs_read_str(kNvsDeviceId, buf, buf_len)) {
        return true;
    }

    // 从 efuse MAC 生成
    uint8_t mac[6];
    esp_err_t err = esp_read_mac(mac, ESP_MAC_WIFI_STA);
    if (err != ESP_OK) {
        ESP_LOGE(kTag, "Failed to read MAC: %s", esp_err_to_name(err));
        return false;
    }

    // 格式: NOTE4C-AABBCC（最后 3 字节）
    snprintf(buf, buf_len, "NOTE4C-%02X%02X%02X",
             mac[3], mac[4], mac[5]);

    // 写入 NVS 持久化
    nvs_write_str(kNvsDeviceId, buf);
    ESP_LOGI(kTag, "Generated device_id: %s", buf);
    return true;
}

// ============================================================
// 端点拼接
// ============================================================

bool server_pairing_build_endpoint(const char *path, char *buf, int buf_len)
{
    char base[128] = {0};
    if (!nvs_read_str(kNvsBaseUrl, base, sizeof(base))) {
        return false;
    }

    // base 已剥尾部斜杠，path 以 / 开头
    int ret = snprintf(buf, buf_len, "%s%s", base, path);
    if (ret < 0 || ret >= buf_len) {
        ESP_LOGE(kTag, "Endpoint buffer too small for %s%s", base, path);
        return false;
    }
    return true;
}

// ============================================================
// API: server_pairing_init
// ============================================================

ServerPairStatus server_pairing_init(void)
{
    char base_url[128] = {0};
    char token[80] = {0};

    bool has_base = nvs_read_str(kNvsBaseUrl, base_url, sizeof(base_url));
    bool has_token = nvs_read_str(kNvsToken, token, sizeof(token));

    if (!has_base) {
        ESP_LOGI(kTag, "No base_url in NVS → 需要配网");
        return SERVER_PAIR_NEEDS_PROVISION;
    }

    if (!has_token) {
        ESP_LOGI(kTag, "Has base_url but no token → 需要配对");
        return SERVER_PAIR_NEEDS_PAIRING;
    }

    ESP_LOGI(kTag, "base_url=%s, token已就绪", base_url);
    return SERVER_PAIR_OK;
}

// ============================================================
// API: server_pairing_set_display_cb
// ============================================================

void server_pairing_set_display_cb(server_pair_display_cb_t cb)
{
    s_display_cb = cb;
}

// ============================================================
// 配对流程内部实现
// ============================================================

/**
 * @brief 发起 pair-start 请求，返回配对码和过期时间
 * @return true=成功, false=网络错误
 */
static bool do_pair_start(const char *device_id, char *code_out, int code_out_len,
                          int *expires_out)
{
    char url[256];
    if (!server_pairing_build_endpoint("/api/devices/pair-start", url, sizeof(url))) {
        return false;
    }

    // 构造 JSON body
    char body[256];
    snprintf(body, sizeof(body),
             "{\"device_id\":\"%s\",\"board_type\":\"%s\"}",
             device_id, kBoardType);

    char resp[512];
    int resp_len = sizeof(resp);

    int status = http_wrapper_post_json(url, NULL, body, resp, &resp_len, kHttpTimeoutMs);
    if (status < 0) {
        ESP_LOGE(kTag, "pair-start 网络错误");
        return false;
    }
    if (status != 200) {
        ESP_LOGE(kTag, "pair-start HTTP %d", status);
        return false;
    }

    // 解析响应
    cJSON *json = cJSON_Parse(resp);
    if (!json) {
        ESP_LOGE(kTag, "pair-start 响应 JSON 解析失败");
        return false;
    }

    cJSON *code_item = cJSON_GetObjectItemCaseSensitive(json, "code");
    cJSON *expires_item = cJSON_GetObjectItemCaseSensitive(json, "expires_in");

    bool ok = false;
    if (cJSON_IsString(code_item) && cJSON_IsNumber(expires_item)) {
        snprintf(code_out, code_out_len, "%s", code_item->valuestring);
        *expires_out = expires_item->valueint;
        ok = true;
    }
    cJSON_Delete(json);
    return ok;
}

/**
 * @brief 发起 pair-claim 请求
 * @return HTTP 状态码（200=成功, 401=未确认/过期）
 */
static int do_pair_claim(const char *device_id, const char *code,
                         char *token_out, int token_out_len)
{
    char url[256];
    if (!server_pairing_build_endpoint("/api/devices/pair-claim", url, sizeof(url))) {
        return -1;
    }

    char body[256];
    snprintf(body, sizeof(body),
             "{\"device_id\":\"%s\",\"code\":\"%s\"}",
             device_id, code);

    char resp[512];
    int resp_len = sizeof(resp);

    int status = http_wrapper_post_json(url, NULL, body, resp, &resp_len, kHttpTimeoutMs);
    if (status != 200 && status != 401) {
        ESP_LOGW(kTag, "pair-claim HTTP %d（可能网络错误）", status);
        return status;
    }

    if (status == 200) {
        cJSON *json = cJSON_Parse(resp);
        if (json) {
            cJSON *token_item = cJSON_GetObjectItemCaseSensitive(json, "token");
            if (cJSON_IsString(token_item)) {
                snprintf(token_out, token_out_len, "%s", token_item->valuestring);
            }
            cJSON_Delete(json);
        }
    }

    return status;
}

// ============================================================
// API: server_pairing_run
// ============================================================

bool server_pairing_run(void)
{
    char device_id[32] = {0};
    if (!generate_device_id(device_id, sizeof(device_id))) {
        ESP_LOGE(kTag, "无法生成 device_id");
        return false;
    }

    ESP_LOGI(kTag, "开始配对, device_id=%s", device_id);

    // 配对循环（5 分钟超时后重新 pair-start）
    int64_t start_time = esp_timer_get_time();
    char code[16] = {0};
    int expires_in = 0;

    while (true) {
        int64_t elapsed_us = esp_timer_get_time() - start_time;
        int elapsed_sec = (int)(elapsed_us / 1000000);

        if (elapsed_sec >= kPairingTimeoutSec) {
            ESP_LOGW(kTag, "配对超时 (%ds)，重新发起 pair-start", kPairingTimeoutSec);
            start_time = esp_timer_get_time();
            code[0] = '\0';
        }

        // 如果没有有效 code，发起 pair-start
        if (code[0] == '\0') {
            ESP_LOGI(kTag, "发起 pair-start...");
            if (!do_pair_start(device_id, code, sizeof(code), &expires_in)) {
                ESP_LOGE(kTag, "pair-start 失败，5 秒后重试");
                vTaskDelay(pdMS_TO_TICKS(5000));
                continue;
            }
            ESP_LOGI(kTag, "配对码: %s, 有效期: %ds", code, expires_in);

            // 通知 UI 显示配对码
            if (s_display_cb) {
                s_display_cb(code, expires_in);
            }
        }

        // 轮询 pair-claim
        char token[80] = {0};
        int status = do_pair_claim(device_id, code, token, sizeof(token));

        if (status == 200 && token[0] != '\0') {
            // 配对成功，写入 NVS
            ESP_LOGI(kTag, "配对成功！写入 token");
            nvs_write_str(kNvsToken, token);

            // 清除显示
            if (s_display_cb) {
                s_display_cb(nullptr, 0);
            }
            return true;
        }

        if (status == 401) {
            ESP_LOGW(kTag, "pair-claim 401: code 过期或未确认，重新 pair-start");
            start_time = esp_timer_get_time();
            code[0] = '\0';
            continue;
        }

        // 网络错误 → 退避重试
        ESP_LOGW(kTag, "pair-claim 网络错误 (status=%d)，退避重试", status);
        vTaskDelay(pdMS_TO_TICKS(kClaimPollIntervalMs));
    }
}

// ============================================================
// API: server_pairing_get_base_url / get_token / get_device_id
// ============================================================

bool server_pairing_get_base_url(char *buf, int buf_len)
{
    return nvs_read_str(kNvsBaseUrl, buf, buf_len);
}

bool server_pairing_get_token(char *buf, int buf_len)
{
    return nvs_read_str(kNvsToken, buf, buf_len);
}

bool server_pairing_get_device_id(char *buf, int buf_len)
{
    return generate_device_id(buf, buf_len);
}

// ============================================================
// API: server_pairing_clear
// ============================================================

void server_pairing_clear(void)
{
    nvs_handle_t handle;
    if (nvs_open(kNvsNamespace, NVS_READWRITE, &handle) == ESP_OK) {
        nvs_erase_all(handle);
        nvs_commit(handle);
        nvs_close(handle);
        ESP_LOGI(kTag, "已清除 server NVS 数据");
    }
}
