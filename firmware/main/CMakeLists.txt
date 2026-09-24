set(SOURCES
    "application.cc"
    "main.cc"
    "settings.cc"
    "system_info.cc"
    "audio/audio_codec.cc"
    "audio/audio_service.cc"
    "audio/codecs/es8311_audio_codec.cc"
    "audio/processors/audio_debugger.cc"
    "audio/processors/no_audio_processor.cc"
    "display/display.cc"
    "display/lcd_display.cc"
    "display/ui_page_registry.cc"
    "display/pages/factory_test_page_adapter.cc"
    # F5: LVGL display files kept - lcd_display.cc depends on LvglDisplay base class
    "display/lvgl_display/lvgl_display.cc"
    "display/lvgl_display/lvgl_theme.cc"
    "display/lvgl_display/lvgl_font.cc"
    "common/sleep_manager.cc"
    "boards/common/board.cc"
    "boards/common/button.cc"
    "boards/common/i2c_bus_lock.cc"
    "boards/common/i2c_device.cc"
    "boards/zectrix/zectrix_nfc.cc"
    "boards/zectrix-s3-epaper-4.2/board_power_bsp.cc"
    "boards/zectrix-s3-epaper-4.2/charge_status.cc"
    "boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc"
    "boards/zectrix-s3-epaper-4.2/i2c_power_hook.cc"
    "boards/zectrix-s3-epaper-4.2/rtc_pcf8563.cc"
    "boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc"
    "boards/zectrix-s3-epaper-4.2/FT/acoustic_selftest.cc"
    "boards/zectrix-s3-epaper-4.2/FT/factory_test_service.cc"
    "rawdraw/rawdraw.cc"
    "rawdraw/theme.cc"
    "rawdraw/layout_utils.cc"
    "rawdraw/framebuffer.cc"
    "rawdraw/layers.cc"
    "rawdraw/refresh.cc"
    "rawdraw/clock.cc"
    "rawdraw/components/bubble.cc"
    "rawdraw/components/button.cc"
    "rawdraw/components/scrollview.cc"
    "rawdraw/components/panel.cc"
    "rawdraw/components/progress_bar.cc"
    "rawdraw/components/toggle.cc"
    "rawdraw/components/slider.cc"
    "rawdraw/components/card.cc"
    "rawdraw/components/list_item.cc"
    "rawdraw/components/status_bar.cc"
    "rawdraw/components/footer_bar.cc"
    "rawdraw/components/modal.cc"
    "rawdraw/components/voice_wakeup.cc"
    "rawdraw/components/weather_card.cc"
    "rawdraw/components/calendar.cc"
    "common/weather_api.cc"
    "common/photo_storage.cc"
    "common/photo_downloader.cc"
    "common/storage_manager.cc"
    "common/bluetooth_manager.cc"
    "common/ble_gatt_service.cc"
    "common/ble_image_receiver.cc"
    "common/holiday_fetcher.cc"
    "common/server_pairing.cc"
    "rust/shim.cpp"
    "common/http_client_wrapper.cc"
    "ui/renderers/rawdraw/ebook_renderer.cc"
    "ui/renderers/rawdraw/chat_renderer.cc"
    "ui/renderers/rawdraw/settings_renderer.cc"
    "ui/renderers/rawdraw/wifi_renderer.cc"
    "ui/renderers/rawdraw/weather_renderer.cc"
    "ui/renderers/rawdraw/weather_detail_renderer.cc"
    "ui/renderers/rawdraw/lifebar_renderer.cc"
    "ui/renderers/rawdraw/almanac_renderer.cc"
    "ui/renderers/rawdraw/log_renderer.cc"
    "ui/renderers/rawdraw/pairing_renderer.cc"
    "ui/renderers/rawdraw/yearprogress_renderer.cc"
    "ui/renderers/rawdraw/calendar_renderer.cc"
    "ui/renderers/rawdraw/photo_gallery.cc"
    "ui/renderers/rawdraw/photo_detail_renderer.cc"
    "ui/renderers/rawdraw/news_renderer.cc"
    "ui/renderers/rawdraw/font_debug_renderer.cc"
    "ui/renderers/rawdraw/font_metrics_renderer.cc"

    "ui/rawdraw_ui_manager.cc"
    "streaming/text_chunker.cc"
    "streaming/tts_streamer.cc"
    "streaming/stream_pipeline.cc"
    # Task 3: Wi-Fi policy adapter (seam between 78__esp-wifi-connect and Rust policy)
    "wifi_policy_adapter.cc"
)

set(INCLUDE_DIRS
    "."
    "audio"
    "display"
    # F5: LVGL include directory kept for header compatibility, source files disabled
    "display/lvgl_display"
    "ui"
    "ui/renderers"
    "ui/renderers/rawdraw"
    "streaming"
    "protocols"
    "components/78__esp-ml307/include"
    "boards/common"
    "rawdraw"
    "rawdraw/components"
    "rust/include"
    "common"
)

idf_component_register(
    SRCS ${SOURCES}
    INCLUDE_DIRS ${INCLUDE_DIRS}
    REQUIRES
        spiffs
        mbedtls
        spi_flash
        efuse
        driver
        esp_wifi
        bt
        nvs_flash
        78__esp-opus
        78__esp-wifi-connect
        78__xiaozhi-fonts
        esp_app_format
        app_update
        esp_partition
        fatfs
        esp_driver_i2s
        esp_http_client
        esp_https_ota
        json
        esp_timer
        esp_http_server
        mqtt
        esp_psram
        esp_lcd
        esp_netif
        esp_event
        log
)

target_compile_definitions(
    ${COMPONENT_LIB}
    PRIVATE
    BOARD_TYPE=\"zectrix-s3-epaper-4.2\"
    BOARD_NAME=\"zectrix-s3-epaper-4.2\"
    BUILTIN_TEXT_FONT=SourceHanSansSC_Regular_slim
    BUILTIN_ICON_FONT=font_zectrix_16_1
    PROJECT_VER=\"6.5.9\"  # F5: Make version available to renderer code
)

# Device signature master key is injected at build time (CMake cache -> cargo
# env -> `option_env!`) into the Rust implementation under `main/rust/`, which is
# its only consumer: see the cargo wiring at the bottom of this file.
# ── Rust modules (main/rust) ───────────────────────────────────────────────
#
# `rust/` holds the Rust implementations of the pair-start signature, page_sync
# and notify, plus `shim.cpp`, the only file that touches ESP-IDF on their
# behalf. They live inside `main` rather than in a component because the shim
# needs main's headers (`board.h`, `custom_lcd_display.h`, ...) and components
# cannot depend on main.
#
# Upstream rustc has no Xtensa target, so this needs the esp-rs toolchain
# (`espup install -t esp32s3`); `+esp` selects it explicitly. `-Zbuild-std=core`
# is required because that toolchain ships `rust-src` but no prebuilt core for
# the Xtensa targets.
set(RUST_DIR "${CMAKE_CURRENT_LIST_DIR}/rust")
set(RUST_TARGET "xtensa-esp32s3-none-elf")
set(RUST_LIB "${RUST_DIR}/target/${RUST_TARGET}/release/librust_firmware.a")

# Listed explicitly (no glob): IDF does not expand this file early, but an
# explicit list keeps the rebuild rules obvious.
set(RUST_SOURCES
    "${RUST_DIR}/Cargo.toml"
    "${RUST_DIR}/Cargo.lock"
    "${RUST_DIR}/src/lib.rs"
    "${RUST_DIR}/src/device_signature.rs"
    "${RUST_DIR}/src/device_signature/ffi.rs"
    "${RUST_DIR}/src/page_sync.rs"
    "${RUST_DIR}/src/notify.rs"
    "${RUST_DIR}/src/json.rs"
    "${RUST_DIR}/src/log.rs"
    "${RUST_DIR}/src/shim.rs"
    # The modules below were added after this list was written and were never
    # registered, so editing them did not rebuild the library: an incremental
    # build quietly linked the previous Rust code.
    "${RUST_DIR}/src/input.rs"
    "${RUST_DIR}/src/lifecycle.rs"
    "${RUST_DIR}/src/pairing.rs"
    "${RUST_DIR}/src/pairing_response.rs"
    "${RUST_DIR}/src/power.rs"
    "${RUST_DIR}/src/settings.rs"
    "${RUST_DIR}/src/charge_policy.rs"
    "${RUST_DIR}/src/battery_activity_policy.rs"
    "${RUST_DIR}/src/time_gate_policy.rs"
    "${RUST_DIR}/src/notify_policy.rs"
    "${RUST_DIR}/src/led_policy.rs"
    "${RUST_DIR}/src/wifi_policy.rs")

# DEVICE_MASTER_KEY is baked into the archive, so it has to take part in
# dependency tracking: without this stamp, changing -DDEVICE_MASTER_KEY would
# reuse a stale .a and the device would sign with the previous key.
set(RUST_KEY_STAMP "${CMAKE_CURRENT_BINARY_DIR}/rust_devsig_master_key.txt")
file(WRITE "${RUST_KEY_STAMP}.tmp" "${DEVICE_MASTER_KEY}")
execute_process(COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${RUST_KEY_STAMP}.tmp" "${RUST_KEY_STAMP}")

add_custom_command(
    OUTPUT "${RUST_LIB}"
    COMMAND ${CMAKE_COMMAND} -E env "DEVICE_MASTER_KEY=${DEVICE_MASTER_KEY}"
            cargo +esp build --release --target ${RUST_TARGET}
                  -Zbuild-std=core --manifest-path "${RUST_DIR}/Cargo.toml"
    DEPENDS ${RUST_SOURCES} "${RUST_KEY_STAMP}"
    WORKING_DIRECTORY "${RUST_DIR}"
    COMMENT "Building Rust firmware modules (${RUST_TARGET})"
    VERBATIM)

add_custom_target(rust_firmware DEPENDS "${RUST_LIB}")
add_dependencies(${COMPONENT_LIB} rust_firmware)
target_link_libraries(${COMPONENT_LIB} INTERFACE "${RUST_LIB}")
