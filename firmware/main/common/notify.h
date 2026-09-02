/**
 * @file notify.h
 * @brief 待确认通知模块：BOOT 拉取，上/下键 ack，5min 自动关闭
 *
 * 状态机：IDLE -> FETCHING (GET in flight) -> NOTIFYING (展示位图)
 *          -> IDLE (ack / dismiss / 5min timeout)
 */
#ifndef NOTIFY_H
#define NOTIFY_H

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief 初始化通知模块（需在 HTTP client 初始化后调用）
 */
void notify_init(void);

/**
 * @brief 反初始化
 */
void notify_deinit(void);

/**
 * @brief 请求拉取下一条待确认通知（BOOT 短按触发）
 *
 * 非阻塞：内部发起异步 HTTP GET /api/notifications/next。
 * 有通知时切换到 NOTIFYING 并显示位图。
 */
void notify_request_next(void);

/**
 * @brief 是否正在展示通知（NOTIFYING 状态）
 */
bool notify_is_active(void);

/**
 * @brief 提交 ack 并关闭展示（上/下键触发）
 *
 * @param decision  "agree" 或 "reject"
 */
void notify_post_ack(const char *decision);

/**
 * @brief 关闭通知展示（BOOT 短按或 5min 超时触发），不发 ack
 */
void notify_dismiss(void);

#ifdef __cplusplus
}
#endif

#endif // NOTIFY_H
