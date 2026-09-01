/**
 * @file server_pairing.h
 * @brief 服务端配对鉴权模块
 *
 * 管理设备接入的三步配对协议：
 *   1. POST pair-start → 屏显 6 位配对码
 *   2. 用户在服务端 operator 界面输码确认
 *   3. POST pair-claim（轮询）→ 获取 token，写 NVS
 *
 * NVS namespace "server" keys:
 *   base_url  (≤128B str)  — 服务端地址，尾部斜杠已剥
 *   token     (64hex str)  — 设备永久鉴权 token
 *   device_id (≤32B str)   — MAC 派生，格式 NOTE4C-XXXXXX
 *
 * BuildEndpoint(path) 供 page_sync / OTA / pairing 共用。
 */

#ifndef SERVER_PAIRING_H
#define SERVER_PAIRING_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief 配对状态返回码
 */
typedef enum {
    SERVER_PAIR_OK = 0,          ///< 已就绪（base_url + token 均有效）
    SERVER_PAIR_NEEDS_PROVISION, ///< 无 base_url，需进配网
    SERVER_PAIR_NEEDS_PAIRING,   ///< 有 base_url 但无 token，需配对
    SERVER_PAIR_ERROR            ///< 内部错误
} ServerPairStatus;

/**
 * @brief 配对显示回调
 *
 * 当配对码生成时调用，由 UI 层接线到屏显。
 * code: 6 位数字字符串（如 "482913"）
 * expires_in: 剩余秒数
 */
typedef void (*server_pair_display_cb_t)(const char *code, int expires_in);

/**
 * @brief 初始化配对模块，读取 NVS 配置
 *
 * @return SERVER_PAIR_OK / NEEDS_PROVISION / NEEDS_PAIRING / ERROR
 */
ServerPairStatus server_pairing_init(void);

/**
 * @brief 注册配对码屏显回调（在主会话接线）
 */
void server_pairing_set_display_cb(server_pair_display_cb_t cb);

/**
 * @brief 启动配对流程（pair-start + 轮询 pair-claim）
 *
 * 阻塞当前 task，内部按 spec 每 2 秒轮询。
 * 5 分钟超时自动重新 pair-start。
 * 成功后将 token 写入 NVS。
 *
 * @return true=配对成功, false=超时或网络不可达
 */
bool server_pairing_run(void);

/**
 * @brief 读取设备 base_url（供外部使用）
 *
 * @param buf     输出缓冲区
 * @param buf_len 缓冲区大小
 * @return true=读取成功
 */
bool server_pairing_get_base_url(char *buf, int buf_len);

/**
 * @brief 读取设备 token（供外部使用）
 *
 * @param buf     输出缓冲区（至少 65 字节）
 * @param buf_len 缓冲区大小
 * @return true=读取成功
 */
bool server_pairing_get_token(char *buf, int buf_len);

/**
 * @brief 读取设备 ID
 *
 * @param buf     输出缓冲区（至少 32 字节）
 * @param buf_len 缓冲区大小
 * @return true=读取成功
 */
bool server_pairing_get_device_id(char *buf, int buf_len);

/**
 * @brief 端点拼接工具函数
 *
 * base_url 保存时已剥尾部斜杠，path 以 / 开头。
 * 例: BuildEndpoint("/api/health") → "https://youn.example.com/api/health"
 *
 * @param path    API 路径（以 / 开头）
 * @param buf     输出缓冲区
 * @param buf_len 缓冲区大小
 * @return true=拼接成功, false=base_url 未设置或缓冲区不足
 */
bool server_pairing_build_endpoint(const char *path, char *buf, int buf_len);

/**
 * @brief 清除 server NVS 数据（长按 BOOT 重配）
 */
void server_pairing_clear(void);

#ifdef __cplusplus
}
#endif

#endif  // SERVER_PAIRING_H
