#ifndef __BOARD_POWER_BSP_H__
#define __BOARD_POWER_BSP_H__

#include <atomic>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>

class ChargeStatus;

class BoardPowerBsp {
private:
    const int epdPowerPin_;
    const int audioPowerPin_;
    const int audioAmpPin_;
    const int vbatPowerPin_;
    ChargeStatus* charge_status_ = nullptr;
    std::atomic<bool> led_override_enabled_{false};
    std::atomic<bool> led_override_blink_{false};
    std::atomic<bool> led_override_phase_{false};
    std::atomic<int> led_activity_pulses_{0};
    TaskHandle_t led_task_{nullptr};

    static void PowerLedTask(void *arg);

public:
    BoardPowerBsp(int epdPowerPin, int audioPowerPin, int audioAmpPin, int vbatPowerPin,
                  ChargeStatus* charge_status);
    ~BoardPowerBsp();
    void PowerEpdOn();
    void PowerEpdOff();
    void PowerAudioOn();
    void PowerAudioOff();
    void PowerAmpOn();
    void PowerAmpOff();
    void VbatPowerOn();
    void VbatPowerOff();
    void SetFactoryLedOverride(bool enabled, bool blink);
    void FlashActivityLed();
};

#endif
