#include <stdio.h>
#include <driver/gpio.h>
#include <esp_timer.h>
#include <freertos/FreeRTOS.h>
#include "board_power_bsp.h"
#include "charge_status.h"

// PowerLedTask 判定纯函数：把 LED 语义表逐条编码，任务侧只负责应用电平与等待。
// 与改前 PowerLedTask 分支（:18-:65）逐条对应：
//   ovr && ovr_blink                     → 500ms 翻转一次，电平取反（原 :20-:27）
//   ovr && !ovr_blink                    → 常置 1（原 :29-:31，电平逐字）
//   !ovr && !charging && !full && 脉冲>0 → 亮 120ms / 灭 180ms，耗一个脉冲（原 :43-:50）
//   !ovr && full                         → 常置 0（原 :51-:54）
//   !ovr && charging                     → 先 0 保持 200ms，再 1 保持 2800ms（原 :55-:60 逐字顺序）
//   其余                                 → 常置 1（原 :61-:64）
// 注 1：brief 表把 charging 写成"灭 200ms / 亮 2800ms"，与代码实际顺序相反。
// 代码里 0=亮（full 分支常置 0 表常亮）、1=灭（其余分支常置 1 表常灭），charging 分支
// 实际是"先 0（亮）200ms，再 1（灭）2800ms"。此处以改前代码的实际观感为准，逐字保留。
// 注 2：brief 表第 2 行写"常亮(1)"——括号内是 GPIO 电平（原 :30 置 1），此处同样逐字保留电平。
struct LedAction {
    bool level;          // 单段电平；两段时为第一段电平（第 1 行闪烁不用此字段，任务侧取反相位）
    uint32_t on_ms;      // 第一段时长；0 = 无分段（第 1 行取 500，为翻转节奏）
    uint32_t off_ms;     // 第二段时长；0 = 无第二段
    bool consume_pulse;  // 是否消耗一个活动脉冲（仅第 3 行）
};
static LedAction led_decide(bool ovr, bool ovr_blink, const ChargeStatus::Snapshot& s,
                            uint32_t pulses) {
    if (ovr && ovr_blink) {
        return LedAction{false, 500, 0, false};
    }
    if (ovr) {
        return LedAction{true, 0, 0, false};
    }
    if (!s.charging && !s.full && pulses > 0) {
        return LedAction{false, 120, 180, true};
    }
    if (s.full) {
        return LedAction{false, 0, 0, false};
    }
    if (s.charging) {
        return LedAction{false, 200, 2800, false};
    }
    return LedAction{true, 0, 0, false};
}

// 静态电平状态（override 实色 / 充满 / 放电空闲）的有界等待：等事件（override 变化 /
// 活动脉冲 / 充电状态回调），附带慢 Tick 轮询。充电器插拔只能靠 Tick 检出（ChargeStatus
// 是 GPIO 轮询驱动，回调只在 Tick 后触发），无限期睡会让拔电检测无界滞后——属用户可见
// 回归，故保留 2s 慢轮询；原 500ms 无条件空转不再保留。
static constexpr uint32_t kLedStaticPollMs = 2000;

void BoardPowerBsp::PowerLedTask(void *arg) {
    auto* self = static_cast<BoardPowerBsp*>(arg);
    gpio_config_t gpio_conf = {};
    gpio_conf.intr_type     = GPIO_INTR_DISABLE;
    gpio_conf.mode          = GPIO_MODE_OUTPUT;
    gpio_conf.pin_bit_mask  = (0x1ULL << GPIO_NUM_3);
    gpio_conf.pull_down_en  = GPIO_PULLDOWN_DISABLE;
    gpio_conf.pull_up_en    = GPIO_PULLUP_ENABLE;
    ESP_ERROR_CHECK_WITHOUT_ABORT(gpio_config(&gpio_conf));
    for (;;) {
        ChargeStatus::Snapshot snap{};
        const bool has_status = self && self->charge_status_;
        if (has_status) {
            self->charge_status_->Tick(esp_timer_get_time() / 1000);
            snap = self->charge_status_->Get();
        }
        const bool ovr = self->led_override_enabled_.load(std::memory_order_relaxed);
        const bool ovr_blink = self->led_override_blink_.load(std::memory_order_relaxed);
        const uint32_t pulses =
            static_cast<uint32_t>(self->led_activity_pulses_.load(std::memory_order_relaxed));
        const LedAction action = led_decide(ovr, ovr_blink, snap, pulses);
        if (ovr && ovr_blink) {
            // 语义表第 1 行（原 :21-:27 逐字）：相位取反 → hold 包住写电平 → 500ms 节奏。
            // 等待改用 notify 等待：稳态节奏同为 500ms，事件到来提前醒一次重判（稳态无可见差异）。
            const bool phase = !self->led_override_phase_.load(std::memory_order_relaxed);
            self->led_override_phase_.store(phase, std::memory_order_relaxed);
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, phase ? 0 : 1);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.on_ms));
            continue;
        }
        if (action.consume_pulse) {
            // 语义表第 3 行（原 :43-:50 逐字）：亮 120ms / 灭 180ms，耗一个脉冲。
            // 脉冲形状不可抢占：两段都用 vTaskDelay，与改前逐字一致。
            self->led_activity_pulses_.fetch_sub(1, std::memory_order_relaxed);
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, 0);
            vTaskDelay(pdMS_TO_TICKS(action.on_ms));
            gpio_set_level(GPIO_NUM_3, 1);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            vTaskDelay(pdMS_TO_TICKS(action.off_ms));
        } else if (action.on_ms > 0 && action.off_ms > 0) {
            // 语义表第 5 行（原 :55-:60 逐字顺序）：先 0 保持 200ms，再 1 保持 2800ms。
            // 头段 200ms 用 vTaskDelay（逐字）；尾段 2800ms 用 notify 等待，稳态节奏不变。
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, 0);
            vTaskDelay(pdMS_TO_TICKS(action.on_ms));
            gpio_set_level(GPIO_NUM_3, 1);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.off_ms));
        } else {
            // 语义表第 2/4/6 行（原 :29-:31 / :51-:54 / :61-:64）：单电平 + hold 成对，电平逐字。
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, action.level ? 1 : 0);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(kLedStaticPollMs));
        }
    }
}

BoardPowerBsp::BoardPowerBsp(int epdPowerPin, int audioPowerPin, int audioAmpPin, int vbatPowerPin,
                             ChargeStatus* charge_status)
    : epdPowerPin_(epdPowerPin),
      audioPowerPin_(audioPowerPin),
      audioAmpPin_(audioAmpPin),
      vbatPowerPin_(vbatPowerPin),
      charge_status_(charge_status) {
    gpio_config_t gpio_conf = {};
    gpio_conf.intr_type     = GPIO_INTR_DISABLE;
    gpio_conf.mode          = GPIO_MODE_OUTPUT;
    gpio_conf.pin_bit_mask  = (0x1ULL << epdPowerPin_) | (0x1ULL << audioPowerPin_) | (0x1ULL << audioAmpPin_) | (0x1ULL << vbatPowerPin_);
    gpio_conf.pull_down_en  = GPIO_PULLDOWN_DISABLE;
    gpio_conf.pull_up_en    = GPIO_PULLUP_ENABLE;
    ESP_ERROR_CHECK_WITHOUT_ABORT(gpio_config(&gpio_conf));
    // 先注册充电状态回调再起任务：避免任务先跑 Tick、回调后赋值的数据竞争。
    // 此时 led_task_ 仍为 nullptr，回调内判空即空操作，安全。
    if (charge_status_ != nullptr) {
        charge_status_->OnStateChanged([this](const ChargeStatus::Snapshot&) {
            TaskHandle_t h = led_task_;
            if (h != nullptr) {
                xTaskNotifyGive(h);
            }
        });
    }
    xTaskCreatePinnedToCore(PowerLedTask, "PowerLedTask", 3 * 1024, this, 2, &led_task_, 0);
}

BoardPowerBsp::~BoardPowerBsp() {
}

void BoardPowerBsp::PowerEpdOn() {
    gpio_hold_dis((gpio_num_t) epdPowerPin_);
    gpio_set_level((gpio_num_t) epdPowerPin_, 1);
    gpio_hold_en((gpio_num_t)epdPowerPin_);
}

void BoardPowerBsp::PowerEpdOff() {
    gpio_hold_dis((gpio_num_t) epdPowerPin_);
    gpio_set_level((gpio_num_t) epdPowerPin_, 0);
    gpio_hold_en((gpio_num_t)epdPowerPin_);
}

void BoardPowerBsp::PowerAmpOn() {
    gpio_hold_dis((gpio_num_t)audioAmpPin_);
    gpio_set_level((gpio_num_t) audioAmpPin_, 1);
    gpio_hold_en((gpio_num_t)audioAmpPin_);
}

void BoardPowerBsp::PowerAmpOff() {
    gpio_hold_dis((gpio_num_t)audioAmpPin_);
    gpio_set_level((gpio_num_t) audioAmpPin_, 0);
    gpio_hold_en((gpio_num_t)audioAmpPin_);
}

void BoardPowerBsp::PowerAudioOn() {
    gpio_hold_dis((gpio_num_t)audioPowerPin_);
    gpio_set_level((gpio_num_t) audioPowerPin_, 1);
    gpio_hold_en((gpio_num_t)audioPowerPin_);
}

void BoardPowerBsp::PowerAudioOff() {
    gpio_hold_dis((gpio_num_t)audioPowerPin_);
    gpio_set_level((gpio_num_t) audioPowerPin_, 0);
    gpio_hold_en((gpio_num_t)audioPowerPin_);
}

void BoardPowerBsp::VbatPowerOn() {
    gpio_hold_dis((gpio_num_t)vbatPowerPin_);
    gpio_set_level((gpio_num_t) vbatPowerPin_, 1);
    gpio_hold_en((gpio_num_t)vbatPowerPin_);
}

void BoardPowerBsp::VbatPowerOff() {
    gpio_hold_dis((gpio_num_t)vbatPowerPin_);
    gpio_set_level((gpio_num_t) vbatPowerPin_, 0);
    gpio_hold_en((gpio_num_t)vbatPowerPin_);
}

void BoardPowerBsp::SetFactoryLedOverride(bool enabled, bool blink) {
    led_override_enabled_.store(enabled, std::memory_order_relaxed);
    led_override_blink_.store(blink, std::memory_order_relaxed);
    led_override_phase_.store(false, std::memory_order_relaxed);
    TaskHandle_t h = led_task_;
    if (h != nullptr) {
        xTaskNotifyGive(h);
    }
}

void BoardPowerBsp::FlashActivityLed() {
    led_activity_pulses_.store(1, std::memory_order_relaxed);
    TaskHandle_t h = led_task_;
    if (h != nullptr) {
        xTaskNotifyGive(h);
    }
}
