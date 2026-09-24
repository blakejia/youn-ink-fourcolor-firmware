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
#include <stdint.h>

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
 * @brief 是否有 /next 拉取在飞行中（FETCHING 状态）
 *
 * 断射频前的守卫信号：服务端交出通知即置 shown，飞行中拆射频会让响应
 * stranded、通知永久丢失。见实现注释。
 */
bool notify_is_fetching(void);

/**
 * @brief 当前通知模块状态（0=IDLE 1=FETCHING 2=NOTIFYING，RF_NOTIFY_STATE_*）
 *
 * 供 C++ 拉取门（notify_policy.h）读取事实：只有 IDLE 才允许发起拉取。
 */
uint8_t notify_state(void);

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

/**
 * @brief 关闭通知展示但不动屏幕所有权（调用方随后自行接管屏幕）
 *
 * 用于「离开当前屏」路径：notify_dismiss 会把画板画回屏幕上，
 * 与紧随其后的 SwitchPage 抢屏，导致 UI 页面被画板盖掉。
 */
void notify_dismiss_quiet(void);

#ifdef __cplusplus
}
#endif

#endif // NOTIFY_H
