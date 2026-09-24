#include <stdio.h>
#include <driver/gpio.h>
#include <esp_timer.h>
#include <atomic>
#include "board_power_bsp.h"
#include "charge_status.h"
// led_policy.h must come after charge_status.h so ChargeStatus is fully defined.
#include "led_policy.h"

// PowerLedTask 判定纯函数：把 LED 语义逐条编码，任务侧只负责应用电平与等待。
// 与改前 PowerLedTask 分支（:18-:65）逐条对应（以改前代码为准：charging = 亮 200ms /
// 灭 2800ms；0=亮、1=灭——full 分支常置 0 表常亮，其余分支常置 1 表常灭）：
//   ovr && ovr_blink                     → 500ms 翻转一次，电平取反（原 :20-:27）
//   ovr && !ovr_blink                    → 常置 1（原 :29-:31，电平逐字）
//   !ovr && !charging && !full && 脉冲>0 → 亮 120ms / 灭 180ms，耗一个脉冲（原 :43-:50）
//   !ovr && full                         → 常置 0（原 :51-:54）
//   !ovr && charging                     → 先 0（亮）200ms，再 1（灭）2800ms（原 :55-:60 逐字顺序）
//   其余                                 → 常置 1（原 :61-:64）
// 结果形状（F4）：单一事实——各分支的 GPIO 电平只由 level/second_level 描述，
// 时长只描述等待；分派只看结构标志（consume_pulse / has_second），不看时长数值。
// 选"电平全显式"而不用"时长隐含"的原因：改任一行的电平必然生效（原来改 pulse/
// charging/blink 行的 level 是静默无操作），改时长只改节奏、永远串不了分支
// （原来给静态行加个时长会误入 charging 分支）。blink 行的 level 取输入旧相位：
// 新相位=!旧相位，GPIO=新相位?0:1 ⇒ GPIO0 ⇔ 新相位true ⇔ 旧相位false ⇔ level=false；
// 任务侧存入 !phase（=新相位），写 action.level，两者一致。
// 静态电平态（override 实色 / 充满 / 放电空闲）的有界等待 = 1000ms。
// 取 1000ms 而非 2000ms 的理由（F2）：LED 任务是 ChargeStatus::Tick 的唯一周期
// 调用者（其余调用都是按需的：zectrix-s3-epaper-4.2.cc:187/238），而 IsPowerPresent()
// 只读缓存快照，它喂给 ServicePowerPolicy 的 in.mains（→ power.rs:43 StayAwake{mains}）
// 与两处深睡充电唤醒守卫（application.cc:1163/1225）。1000ms 轮询下插入检出约 1.4s
//（poll + 400ms 稳定窗）、拔出均值约 1.4s（最坏 poll + 1s 上电保持 ≈ 2s）；
// 2000ms 会把两个方向推到约 2.4s/3.8s，影响面超出 LED 本身。另注意 sdkconfig 里
// tickless 是关的（:3230）且 HZ=100（:3176），500ms→1000ms 只是把该任务的每秒唤醒
// 从 2 次减到 1 次，省电收益本就有限——不要把这条报成大头。
// LED action struct — used by led_decide and PowerLedTask.
struct LedAction {
    bool level;
    uint32_t first_ms;
    bool has_second;
    bool second_level;
    uint32_t second_ms;
    bool consume_pulse;
    bool first_wait_notify;
    bool second_wait_notify;
};

// Calls Rust LED policy; C++ callers see the same LedAction shape as before.
static LedAction led_decide(bool ovr, bool ovr_blink, bool phase,
                            const ChargeStatus::Snapshot& s, uint32_t pulses) {
    rf_led_policy_inputs_t in{};
    in.override_enabled = ovr ? 1 : 0;
    in.override_blink   = ovr_blink ? 1 : 0;
    in.phase            = phase ? 1 : 0;
    in.charging         = s.charging ? 1 : 0;
    in.full             = s.full ? 1 : 0;
    in.pulses           = pulses;
    rf_led_policy_output_t out{};
    rf_led_policy_decide(&in, &out);
    return LedAction{
        /* level */             static_cast<bool>(out.level),
        /* first_ms */          out.first_ms,
        /* has_second */        static_cast<bool>(out.has_second),
        /* second_level */      static_cast<bool>(out.second_level),
        /* second_ms */         out.second_ms,
        /* consume_pulse */     static_cast<bool>(out.consume_pulse),
        /* first_wait_notify */ static_cast<bool>(out.first_wait_notify),
        /* second_wait_notify*/static_cast<bool>(out.second_wait_notify),
    };
}


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
        const bool phase = self->led_override_phase_.load(std::memory_order_relaxed);
        const uint32_t pulses =
            static_cast<uint32_t>(self->led_activity_pulses_.load(std::memory_order_relaxed));
        const LedAction action = led_decide(ovr, ovr_blink, phase, snap, pulses);
        if (ovr && ovr_blink) {
            self->led_override_phase_.store(!phase, std::memory_order_relaxed);
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, action.level ? 1 : 0);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            if (action.first_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.first_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.first_ms));
            }
            continue;
        }
        if (action.consume_pulse) {
            self->led_activity_pulses_.fetch_sub(1, std::memory_order_relaxed);
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, action.level ? 1 : 0);
            if (action.first_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.first_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.first_ms));
            }
            gpio_set_level(GPIO_NUM_3, action.second_level ? 1 : 0);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            if (action.second_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.second_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.second_ms));
            }
        } else if (action.has_second) {
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, action.level ? 1 : 0);
            if (action.first_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.first_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.first_ms));
            }
            gpio_set_level(GPIO_NUM_3, action.second_level ? 1 : 0);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            if (action.second_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.second_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.second_ms));
            }
        } else {
            gpio_hold_dis((gpio_num_t)GPIO_NUM_3);
            gpio_set_level(GPIO_NUM_3, action.level ? 1 : 0);
            gpio_hold_en((gpio_num_t)GPIO_NUM_3);
            if (action.first_wait_notify) {
                ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(action.first_ms));
            } else {
                vTaskDelay(pdMS_TO_TICKS(action.first_ms));
            }
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
