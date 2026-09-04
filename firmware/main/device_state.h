#ifndef _DEVICE_STATE_H_
#define _DEVICE_STATE_H_

enum DeviceState {
    kDeviceStateUnknown,
    kDeviceStateStarting,
    kDeviceStateWifiConfiguring,
    kDeviceStateIdle,
    kDeviceStateConnecting,
    kDeviceStateListening,
    kDeviceStateSpeaking,
    kDeviceStateUpgrading,
    kDeviceStateActivating,
    kDeviceStateAudioTesting,
    kDeviceStateFatalError
};

/**
 * 设备生命周期状态（配网→配对→同步→休眠）。
 *
 * 与 DeviceState（音频对话轴）正交：Lifecycle 回答"设备在哪个阶段"，
 * DeviceState 回答"当前在做什么"。所有跃迁只走
 * Application::TransitionLifecycle，逐条打 LOG。
 */
enum LifecycleState {
    kLifecycleUnknown,
    kLifecycleBoot,            ///< 上电初始化
    kLifecycleWifiConnecting,  ///< STA 连接中（有已存凭据）
    kLifecycleApProvision,     ///< 配网 AP 已起（含手机接入/提交等子步，见 reason）
    kLifecyclePairStart,       ///< pair-start 请求中
    kLifecyclePairWaitCode,    ///< 已拿码，轮询 pair-claim 等用户确认
    kLifecycleSyncIdle,        ///< 已配对，page_sync 运行
    kLifecycleSleep,           ///< 休眠
    kLifecycleError            ///< 错误（见 reason）
};

#endif // _DEVICE_STATE_H_ 