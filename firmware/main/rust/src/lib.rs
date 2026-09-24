//! Rust implementations of the NOTE4C firmware's leaf modules.
//!
//! One crate, one staticlib (`librust_firmware.a`). Modules are independent
//! ports of C++ files that used to live in `main/common/`:
//!
//! - [`device_signature`] — pair-start HMAC signature.
//! - [`page_sync`] — Canvas Loop schedule polling, bitmap cache and rotation.
//! - [`notify`] — pending-notification pull, display, ack and timeout.
//!
//! Everything that touches ESP-IDF lives behind `shim.rs` declarations and the
//! C/C++ shims next to this crate; the rest is pure and runs under `cargo test`
//! on the host.

// `no_std` only for the device: on the host the crate is built as an rlib for
// `cargo test`, and a no_std host build cannot unwind (which the test harness
// needs). Nothing here uses std either way.
#![cfg_attr(target_arch = "xtensa", no_std)]
pub mod abi_contract;

pub mod charge_policy;
pub mod led_policy;
pub mod wifi_policy;
pub mod device_signature;
pub mod input;
pub mod json;
pub mod lifecycle;
pub mod log;
pub mod notify;
pub mod page_sync;
pub mod pairing;
pub mod pairing_response;
pub mod power;
pub mod settings;
pub mod shim;

/// A panic aborts the whole firmware instead of unwinding into C++, which has
/// exceptions enabled (`CONFIG_COMPILER_CXX_EXCEPTIONS=y`) and would not
/// survive a foreign frame.
#[cfg(target_arch = "xtensa")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { shim::abort() }
}
