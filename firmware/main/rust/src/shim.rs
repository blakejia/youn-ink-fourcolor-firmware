//! Raw FFI to the shims in `../shim.cpp`.
//!
//! Every unsafe call the crate makes to ESP-IDF goes through here, so the rest
//! of the crate is ordinary safe Rust. The shims keep their C signatures plain
//! (`uint8_t *`, `int`, `size_t`) rather than mirroring IDF typedefs.

use core::ffi::{c_char, c_int, c_void};

pub const LOG_ERROR: c_int = 1;
pub const LOG_WARN: c_int = 2;
pub const LOG_INFO: c_int = 3;
pub const LOG_DEBUG: c_int = 4;

unsafe extern "C" {
    // ── runtime ──
    pub fn rf_abort() -> !;
    pub fn rf_log(level: c_int, tag: *const c_char, msg: *const c_char);
    /// PSRAM allocation; null on failure.
    pub fn rf_alloc(bytes: usize) -> *mut u8;
    pub fn rf_free(p: *mut u8);
    /// One FreeRTOS mutex guarding the Rust-side module state. It has priority
    /// inheritance, which a spinlock would not.
    ///
    /// Lock order is `state -> display`; the display mutex is never held while
    /// taking this one (`page_sync_is_displaying` is a lock-free atomic read
    /// precisely so renderers can call it under the display mutex).
    pub fn rf_state_lock();
    pub fn rf_state_unlock();
    /// `stack_bytes` follows ESP-IDF's byte-counted stacks. Returns 0 on success.
    pub fn rf_task_create(
        entry: extern "C" fn(*mut c_void),
        name: *const c_char,
        stack_bytes: u32,
        priority: u8,
        arg: *mut c_void,
    ) -> c_int;
    pub fn rf_task_exit() -> !;
    pub fn rf_delay_ms(ms: u32);
    /// `esp_timer_get_time()`, i.e. microseconds since boot.
    pub fn rf_now_us() -> u64;
    /// One-shot FreeRTOS timer; the callback runs in the timer daemon task.
    pub fn rf_timer_create_once(name: *const c_char, period_ms: u32, cb: extern "C" fn()) -> c_int;
    pub fn rf_timer_start();
    pub fn rf_timer_stop();
    pub fn rf_timer_delete();

    // ── http (http_client_wrapper.h) ──
    pub fn rf_http_get(
        url: *const c_char,
        token: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn rf_http_post_json(
        url: *const c_char,
        token: *const c_char,
        body: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        timeout_ms: c_int,
    ) -> c_int;

    // ── pairing helpers (server_pairing.h) ──
    pub fn rf_build_endpoint(path: *const c_char, out: *mut c_char, out_len: c_int) -> c_int;
    pub fn rf_get_token(out: *mut c_char, out_len: c_int) -> c_int;
    pub fn rf_get_device_id(out: *mut c_char, out_len: c_int) -> c_int;

    // ── display (CustomLcdDisplay) ──
    /// Injects the board display (mirrors the old `page_sync_set_display`).
    /// A null pointer falls back to `Board::GetInstance().GetDisplay()`.
    pub fn rf_set_display(display: *mut c_void);
    /// Takes the display mutex and returns the shared framebuffer, or null.
    /// Must be paired with [`rf_fb_end`].
    pub fn rf_fb_begin() -> *mut u8;
    /// Expected framebuffer size in bytes for this panel.
    pub fn rf_fb_len() -> c_int;
    pub fn rf_fb_end();
    pub fn rf_request_full_refresh();
    /// Paints the "no pages configured" screen from the C++ side (it needs
    /// `std::vector<TextItem>`, which is a display concern, not logic).
    pub fn rf_draw_empty_hint();
}

/// `abort()`, used by the panic handler.
///
/// # Safety
/// Never returns.
pub unsafe fn abort() -> ! {
    unsafe { rf_abort() }
}

/// A fixed-capacity, NUL-terminated byte buffer for values handed to C.
pub struct CBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> CBuf<N> {
    pub const fn new() -> Self {
        CBuf { bytes: [0; N], len: 0, overflowed: false }
    }

    pub fn push(&mut self, text: &str) {
        self.push_bytes(text.as_bytes());
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        // Reserve one byte for the terminator.
        if self.len + bytes.len() >= N {
            self.overflowed = true;
            return;
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when something did not fit, i.e. the caller must not use the result.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Pointer to the NUL-terminated contents.
    pub fn as_ptr(&self) -> *const c_char {
        self.bytes.as_ptr() as *const c_char
    }

    /// Mutable pointer for C functions that write into the buffer.
    pub fn as_mut_ptr(&mut self) -> *mut c_char {
        self.bytes.as_mut_ptr() as *mut c_char
    }
}

impl<const N: usize> core::fmt::Write for CBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.overflowed {
            return Err(core::fmt::Error);
        }
        self.push(s);
        if self.overflowed { Err(core::fmt::Error) } else { Ok(()) }
    }
}

/// Host-only stand-ins for the `shim.cpp` symbols, so `cargo test` can link.
///
/// They are inert on purpose: the modules' *logic* is covered by the pure
/// parsers, and anything that needs real I/O (HTTP, PSRAM, the panel, RTOS
/// tasks) can only be verified on hardware. Keeping them here — rather than
/// behind a feature flag — means the device build cannot accidentally use them.
#[cfg(all(test, not(target_arch = "xtensa")))]
mod host_stubs {
    use core::ffi::{c_char, c_int, c_void};

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_abort() -> ! {
        panic!("rf_abort")
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_log(_level: c_int, _tag: *const c_char, _msg: *const c_char) {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_alloc(_bytes: usize) -> *mut u8 {
        core::ptr::null_mut()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_free(_p: *mut u8) {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_state_lock() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_state_unlock() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_task_create(
        _entry: extern "C" fn(*mut c_void),
        _name: *const c_char,
        _stack_bytes: u32,
        _priority: u8,
        _arg: *mut c_void,
    ) -> c_int {
        -1
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_task_exit() -> ! {
        panic!("rf_task_exit")
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_delay_ms(_ms: u32) {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_now_us() -> u64 {
        0
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_create_once(
        _name: *const c_char,
        _period_ms: u32,
        _cb: extern "C" fn(),
    ) -> c_int {
        -1
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_start() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_stop() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_delete() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_http_get(
        _url: *const c_char,
        _token: *const c_char,
        _buf: *mut c_char,
        _len: *mut c_int,
        _timeout_ms: c_int,
    ) -> c_int {
        -1
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_http_post_json(
        _url: *const c_char,
        _token: *const c_char,
        _body: *const c_char,
        _buf: *mut c_char,
        _len: *mut c_int,
        _timeout_ms: c_int,
    ) -> c_int {
        -1
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_build_endpoint(
        _path: *const c_char,
        _out: *mut c_char,
        _out_len: c_int,
    ) -> c_int {
        0
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_get_token(_out: *mut c_char, _out_len: c_int) -> c_int {
        0
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_get_device_id(_out: *mut c_char, _out_len: c_int) -> c_int {
        0
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_set_display(_display: *mut c_void) {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_len() -> c_int {
        0
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_begin() -> *mut u8 {
        core::ptr::null_mut()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_end() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_request_full_refresh() {}
    #[unsafe(no_mangle)]
    pub extern "C" fn rf_draw_empty_hint() {}
}
