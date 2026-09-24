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
#include <ctime>
#include "device_signature.h"
#include "pairing.h"
#include "pairing_response.h"

static const char *kTag = "Pairing";

// NVS namespace "server" keys
static const char *kNvsNamespace = "server";
static const char *kNvsBaseUrl = "base_url";
static const char *kNvsToken = "token";
static const char *kNvsDeviceId = "device_id";

// Board type 常量（与 CMakeLists.txt BOARD_TYPE 一致）
static const char *kBoardType = "zectrix-s3-epaper-4.2";

// HTTP 超时
static const int kHttpTimeoutMs = 5000;
// 「时间可信」阈值（2020-09-13）。时间未同步时签名时间戳会是 1970，服务端必然
// 401 且每次失败都消耗一次 per-IP 配额 —— 所以 pairing.rs 决定先等时钟再发。
static const int64_t kPlausibleUnixSeconds = 1600000000LL;

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

/** 系统时间是否已由 SNTP 调好（未同步时 time(NULL) 返回 1970）。 */
static bool wall_clock_ok(void)
{
    return static_cast<int64_t>(time(NULL)) >= kPlausibleUnixSeconds;
}

/**
 * @brief 发起 pair-start 请求，返回配对码和过期时间
 * @return true=成功, false=网络错误
 *
 * 何时算成功（200 + JSON 有效 + code 为字符串 + expires_in 为数字）由
 * pairing_response.rs 分类，cargo test 覆盖；这里仍负责 HTTP 与 cJSON。
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

    // Sign the request
    char mac_hex[16], ts_str[16], nonce_b64[32], sig_b64[64];
    device_sign_pair_start(device_id,
                           mac_hex, sizeof(mac_hex),
                           ts_str, sizeof(ts_str),
                           nonce_b64, sizeof(nonce_b64),
                           sig_b64, sizeof(sig_b64));
    ESP_LOGI(kTag, "pair-start signing: mac=%s ts=%s nonce=%s sig_len=%d",
             mac_hex, ts_str, nonce_b64, (int)strlen(sig_b64));

    http_header_t extra[] = {
        {"X-Device-Mac", mac_hex},
        {"X-Device-Timestamp", ts_str},
        {"X-Device-Nonce", nonce_b64},
        {"X-Device-Signature", sig_b64},
    };

    char resp[512];
    int resp_len = sizeof(resp);
    int status = http_wrapper_post_json(
        url, NULL, body, extra, 4, resp, &resp_len, kHttpTimeoutMs);

    // 解析响应。旧实现只在 200 时解析；非 200 / 传输错误直接判失败，
    // 不去读可能未初始化的响应缓冲。
    rf_pair_start_facts_t facts = {};
    facts.status = status;
    if (status == 200) {
        cJSON *json = cJSON_Parse(resp);
        facts.json_valid = (json != nullptr) ? 1 : 0;
        if (json) {
            cJSON *code_item = cJSON_GetObjectItemCaseSensitive(json, "code");
            cJSON *expires_item = cJSON_GetObjectItemCaseSensitive(json, "expires_in");
            facts.code_is_string = cJSON_IsString(code_item) ? 1 : 0;
            facts.expires_is_number = cJSON_IsNumber(expires_item) ? 1 : 0;
            if (facts.code_is_string && facts.expires_is_number) {
                snprintf(code_out, code_out_len, "%s", code_item->valuestring);
                *expires_out = expires_item->valueint;
            }
            cJSON_Delete(json);
        }
    }

    if (rf_pairing_classify_pair_start(&facts) == RF_PAIR_OUTCOME_PAIR_STARTED) {
        return true;
    }
    if (status < 0) {
        ESP_LOGE(kTag, "pair-start 网络错误");
    } else if (status != 200) {
        ESP_LOGE(kTag, "pair-start HTTP %d", status);
    } else if (!facts.json_valid) {
        ESP_LOGE(kTag, "pair-start 响应 JSON 解析失败");
    } else {
        ESP_LOGE(kTag, "pair-start 响应字段类型不匹配");
    }
    return false;
}

/**
 * @brief 发起 pair-claim 请求
 * @return HTTP 状态码（200=成功, 401=未确认/过期）
 * @param outcome 接收分类结果（rf_pairing_outcome_t）：GRANTED=拿到 token，
 *        PENDING=等待用户确认，REJECTED=401/429 换新码，NETWORK_ERROR=其他。
 *
 * 分类规则（200+任意非空 token=granted；200 无 token 或 JSON 无效=pending；
 * 401/429=rejected；其余=network error）在 pairing_response.rs 里，cargo test
 * 覆盖；这里仍负责 HTTP 与 cJSON。
 */
static int do_pair_claim(const char *device_id, const char *code,
                         char *token_out, int token_out_len, uint8_t *outcome)
{
    char url[256];
    if (!server_pairing_build_endpoint("/api/devices/pair-claim", url, sizeof(url))) {
        rf_pair_claim_facts_t facts = {};
        facts.status = -1;
        *outcome = rf_pairing_classify_claim(&facts);
        return -1;
    }

    char body[256];
    snprintf(body, sizeof(body),
             "{\"device_id\":\"%s\",\"code\":\"%s\"}",
             device_id, code);

    char resp[512];
    int resp_len = sizeof(resp);

    int status = http_wrapper_post_json(url, NULL, body, nullptr, 0, resp, &resp_len, kHttpTimeoutMs);
    if (status != 200 && status != 401) {
        ESP_LOGW(kTag, "pair-claim HTTP %d（可能网络错误）", status);
    }

    rf_pair_claim_facts_t facts = {};
    facts.status = status;
    // 旧实现只在 200 时解析；401/429 的响应体从未被读过，保持如此。
    if (status == 200) {
        cJSON *json = cJSON_Parse(resp);
        facts.json_valid = (json != nullptr) ? 1 : 0;
        if (json) {
            cJSON *token_item = cJSON_GetObjectItemCaseSensitive(json, "token");
            facts.token_is_string = cJSON_IsString(token_item) ? 1 : 0;
            // cJSON_IsString 不足以区分空串与非空串，首字节检查单独传递。
            facts.token_nonempty =
                (cJSON_IsString(token_item) && token_item->valuestring[0] != '\0') ? 1 : 0;
            if (facts.token_is_string && facts.token_nonempty) {
                snprintf(token_out, token_out_len, "%s", token_item->valuestring);
            }
            cJSON_Delete(json);
        }
    }

    *outcome = rf_pairing_classify_claim(&facts);
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

    // 协议策略（何时要码、何时轮询、何时退避、何时放弃、失败上限）在
    // pairing.rs 里，由 cargo test 覆盖；这里只做机制。每轮把上一个动作的
    // 结果喂给决策，再执行它给出的动作与等待时长 —— 常量因此也只有一个来源。
    int64_t window_start_us = esp_timer_get_time();
    char code[16] = {0};
    int expires_in = 0;
    uint32_t failures = 0;
    rf_pairing_outcome_t last = RF_PAIR_OUTCOME_NONE;

    while (true) {
        rf_pairing_inputs_t in = {};
        in.has_code = (code[0] != '\0') ? 1 : 0;
        in.clock_ok = wall_clock_ok() ? 1 : 0;
        in.window_elapsed_s =
            (uint32_t)((esp_timer_get_time() - window_start_us) / 1000000);
        in.pair_start_failures = failures;
        in.last = (uint8_t)last;

        rf_pairing_decision_t d = {};
        rf_pairing_decide(&in, &d);

        if (d.drop_code) {
            if (code[0] != '\0') {
                ESP_LOGW(kTag, "配对码作废（超时或已失效），重新发起 pair-start");
            }
            code[0] = '\0';
            window_start_us = esp_timer_get_time();
        }
        if (d.delay_ms > 0) {
            vTaskDelay(pdMS_TO_TICKS(d.delay_ms));
        }

        switch (d.action) {
        case RF_PAIR_ACTION_WAIT:
            // 时钟仍不可信。这一次等待算一次失败，否则一台永远同步不上的
            // 设备会无限等下去；上限由决策判定，这里只记账。
            if (!wall_clock_ok()) {
                failures++;
            }
            last = RF_PAIR_OUTCOME_CLOCK_WAITED;
            continue;

        case RF_PAIR_ACTION_PAIR_START:
            if (last == RF_PAIR_OUTCOME_PAIR_START_FAILED) {
                ESP_LOGE(kTag, "pair-start 失败，%u ms 后重试（已失败 %u 次）",
                         (unsigned)d.delay_ms, (unsigned)failures);
            }
            ESP_LOGI(kTag, "发起 pair-start...");
            if (!do_pair_start(device_id, code, sizeof(code), &expires_in)) {
                failures++;
                last = RF_PAIR_OUTCOME_PAIR_START_FAILED;
                continue;
            }
            failures = 0;
            window_start_us = esp_timer_get_time();
            ESP_LOGI(kTag, "配对码: %s, 有效期: %ds", code, expires_in);
            if (s_display_cb) {
                s_display_cb(code, expires_in);
            }
            last = RF_PAIR_OUTCOME_PAIR_STARTED;
            continue;

        case RF_PAIR_ACTION_CLAIM: {
            char token[80] = {0};
            uint8_t outcome = 0;
            int status = do_pair_claim(device_id, code, token, sizeof(token), &outcome);

            switch ((rf_pairing_outcome_t)outcome) {
            case RF_PAIR_OUTCOME_CLAIM_GRANTED:
                ESP_LOGI(kTag, "配对成功！写入 token");
                if (!nvs_write_str(kNvsToken, token)) {
                    // 落盘失败不能当成功：服务端已签发 token 并置 trust，
                    // 设备却拿不到 → 之后 page_sync/notify 全 401，两侧状态分叉。
                    last = RF_PAIR_OUTCOME_TOKEN_WRITE_FAILED;
                } else {
                    last = RF_PAIR_OUTCOME_CLAIM_GRANTED;
                }
                continue;

            case RF_PAIR_OUTCOME_CLAIM_PENDING:
                // 200 且无 token = {"status":"pending"}，正常等待用户确认。
                ESP_LOGD(kTag, "pair-claim pending：等待用户在服务端确认");
                last = RF_PAIR_OUTCOME_CLAIM_PENDING;
                continue;

            case RF_PAIR_OUTCOME_CLAIM_REJECTED:
                ESP_LOGW(kTag, "pair-claim %d: 换新码并退避重试", status);
                last = RF_PAIR_OUTCOME_CLAIM_REJECTED;
                continue;

            case RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR:
            default:
                ESP_LOGW(kTag, "pair-claim 网络错误 (status=%d)，退避重试", status);
                last = RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR;
                continue;
            }
        }

        case RF_PAIR_ACTION_PAIRED:
            if (s_display_cb) {
                s_display_cb(nullptr, 0);
            }
            return true;

        case RF_PAIR_ACTION_GIVE_UP:
        default:
            // 必须真实退出：旧实现 while(true) 只有 return true，Error 分支
            // 永远不可达，设备永远停在 PairStart（无码、无错误、无提示）。
            if (last == RF_PAIR_OUTCOME_TOKEN_WRITE_FAILED) {
                ESP_LOGE(kTag, "token 写入 NVS 失败，本轮配对作废");
            } else if (!wall_clock_ok()) {
                ESP_LOGE(kTag, "SNTP 长时间未同步，放弃本轮配对");
            } else {
                ESP_LOGE(kTag, "pair-start 连续失败 %u 次，放弃本轮配对",
                         (unsigned)failures);
            }
            if (s_display_cb) {
                s_display_cb(nullptr, -1);
            }
            return false;
        }
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
