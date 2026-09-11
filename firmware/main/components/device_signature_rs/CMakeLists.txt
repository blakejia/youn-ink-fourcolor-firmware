# Pair-start device signature, implemented in Rust.
#
# The crate is plain Rust with no ESP-IDF bindings: cargo only produces
# `libdevice_signature.a` (no linker needed for a staticlib), and ESP-IDF's
# final link pulls the exported symbol in through the C shim next to this file.
#
# Upstream rustc has no Xtensa target, so this needs the esp-rs toolchain
# (`espup install -t esp32s3`). `+esp` selects it explicitly, so a change to
# the default rustup toolchain cannot silently break the firmware build.

idf_component_register(
    SRCS "shim.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_hw_support)

# IDF expands every component CMakeLists once in a scripting-only pass (to
# collect REQUIRES) before the real configure. That pass rejects build commands,
# so everything below the register() call has to stay behind this guard.
if(CMAKE_BUILD_EARLY_EXPANSION)
    return()
endif()

set(RUST_DIR "${CMAKE_CURRENT_LIST_DIR}/rust")
set(RUST_TARGET "xtensa-esp32s3-none-elf")
set(RUST_LIB "${RUST_DIR}/target/${RUST_TARGET}/release/libdevice_signature.a")

# Listed explicitly rather than globbed: the early-expansion pass rejects
# `CONFIGURE_DEPENDS`.
set(RUST_SOURCES
    "${RUST_DIR}/Cargo.toml"
    "${RUST_DIR}/Cargo.lock"
    "${RUST_DIR}/src/lib.rs"
    "${RUST_DIR}/src/ffi.rs")

# DEVICE_MASTER_KEY is baked into the archive, so it has to take part in
# dependency tracking: without this stamp, changing -DDEVICE_MASTER_KEY would
# reuse a stale .a and the device would sign with the previous key.
set(RUST_KEY_STAMP "${CMAKE_CURRENT_BINARY_DIR}/device_signature_master_key.txt")
file(WRITE "${RUST_KEY_STAMP}.tmp" "${DEVICE_MASTER_KEY}")
execute_process(COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${RUST_KEY_STAMP}.tmp" "${RUST_KEY_STAMP}")

# `-Zbuild-std=core`: the esp-rs toolchain ships no prebuilt core for the Xtensa
# targets (only `rust-src`), so core is compiled from source here.
add_custom_command(
    OUTPUT "${RUST_LIB}"
    COMMAND ${CMAKE_COMMAND} -E env "DEVICE_MASTER_KEY=${DEVICE_MASTER_KEY}"
            cargo +esp build --release --target ${RUST_TARGET}
                  -Zbuild-std=core --manifest-path "${RUST_DIR}/Cargo.toml"
    DEPENDS ${RUST_SOURCES} "${RUST_KEY_STAMP}"
    WORKING_DIRECTORY "${RUST_DIR}"
    COMMENT "Building Rust device signature (${RUST_TARGET})"
    VERBATIM)

add_custom_target(device_signature_rust DEPENDS "${RUST_LIB}")
add_dependencies(${COMPONENT_LIB} device_signature_rust)
target_link_libraries(${COMPONENT_LIB} INTERFACE "${RUST_LIB}")
