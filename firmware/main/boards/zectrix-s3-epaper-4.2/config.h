#ifndef _BOARD_CONFIG_H_
#define _BOARD_CONFIG_H_

#include <driver/gpio.h>

#define AUDIO_INPUT_SAMPLE_RATE  16000
#define AUDIO_OUTPUT_SAMPLE_RATE 16000

#define AUDIO_I2S_GPIO_MCLK  GPIO_NUM_14
#define AUDIO_I2S_GPIO_WS    GPIO_NUM_38
#define AUDIO_I2S_GPIO_BCLK  GPIO_NUM_15
#define AUDIO_I2S_GPIO_DIN   GPIO_NUM_16
#define AUDIO_I2S_GPIO_DOUT  GPIO_NUM_45

#define AUDIO_CODEC_PA_PIN       GPIO_NUM_46
#define AUDIO_CODEC_I2C_SDA_PIN  GPIO_NUM_47
#define AUDIO_CODEC_I2C_SCL_PIN  GPIO_NUM_48
#define AUDIO_CODEC_ES8311_ADDR  ES8311_CODEC_DEFAULT_ADDR

#define BOOT_BUTTON_GPIO        GPIO_NUM_0


#define UP_BUTTON_GPIO          GPIO_NUM_39
#define DOWN_BUTTON_GPIO        GPIO_NUM_18
//开机电源键与下键复用
#define VBAT_PWR_GPIO           GPIO_NUM_18
#define CONFIRM_BUTTON_GPIO     GPIO_NUM_0
#define CHARGE_DETECT_GPIO      GPIO_NUM_2
#define CHARGE_FULL_GPIO        GPIO_NUM_1
// CHARGE_DETECT charging level definition: 0 means low=charging, 1 means high=charging.
#define CHARGE_DETECT_CHARGING_LEVEL 0
// Deep-sleep charger-insert wake (Stage 1 power policy, see application.cc):
// the level follows charge_status.cc, which is the authority — Tick compares
// the pin against CHARGE_DETECT_CHARGING_LEVEL (0 = low means charging), so
// attach = LOW and unplugged = HIGH, and ext1 wakes on ANY_LOW. (Arming the
// level that already holds while sleeping would fire the wake source
// immediately: ANY_HIGH on battery = instant boot-loop, not duty cycling.)
// The sleep log prints the raw pin (pin2=%d) precisely so the first matrix
// run confirms it: expect pin2=1 + mains=0 on battery, pin2=0 + mains=1 on
// USB. No internal pull is added deliberately: the level belongs to the
// charger IC's own network and a pull could fight it.
// (Replaces the dead CHARGE_GPIO_AFFECT_SLEEP macro: mains-never-sleeps plus
// USB-wakes-via-ext1 is the behaviour it described.)
// 1 = plug-in pulls GPIO2 LOW (unplugged HIGH); 0 = plug-in drives HIGH.
// Drives the ext1 wake mode registered before each deep sleep. Derived from
// CHARGE_DETECT_CHARGING_LEVEL above (plug pulls the pin toward the charging
// level) so the two cannot drift: a mismatch would silently break plug-to-wake
// and only surface on hardware.
#define CHARGE_DETECT_PLUG_PULLS_LOW (1 - CHARGE_DETECT_CHARGING_LEVEL)

// RTC (PCF8563T/5)
#define RTC_INT_GPIO            GPIO_NUM_5
#define RTC_I2C_ADDR            0x51

// NFC (GT23SC6699)
#define NFC_I2C_ADDR            0x55
#define NFC_FD_GPIO             GPIO_NUM_7
#define NFC_PWR_GPIO            GPIO_NUM_21
#define NFC_FD_ACTIVE_LEVEL     0
/*EPD port Init*/
#define EPD_SPI_NUM        SPI3_HOST

#define EPD_DC_PIN    GPIO_NUM_10
#define EPD_CS_PIN    GPIO_NUM_11
#define EPD_SCK_PIN   GPIO_NUM_12
#define EPD_MOSI_PIN  GPIO_NUM_13
#define EPD_RST_PIN   GPIO_NUM_9
#define EPD_BUSY_PIN  GPIO_NUM_8

#define EXAMPLE_LCD_WIDTH   400
#define EXAMPLE_LCD_HEIGHT  300

/*DEV POWER init*/
#define EPD_PWR_PIN     GPIO_NUM_6
#define Audio_PWR_PIN   GPIO_NUM_42
#define AUDIO_PWR_FORCE_LEVEL 1
#define Audio_AMP_PIN   GPIO_NUM_46
#define VBAT_PWR_PIN    GPIO_NUM_17
 
#define DISPLAY_MIRROR_X false
#define DISPLAY_MIRROR_Y false
#define DISPLAY_SWAP_XY  false

#define DISPLAY_OFFSET_X  0
#define DISPLAY_OFFSET_Y  0



// Device signature master key now lives in the Rust component
// (main/components/device_signature_rs), injected at build time via
// `idf.py -DDEVICE_MASTER_KEY=... build`. Value NEVER committed.
#endif // _BOARD_CONFIG_H_
