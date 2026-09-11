//! Logging into the firmware's ESP-IDF log, without `alloc`.
//!
//! `core::fmt` is used just for the message text; the buffer is fixed and lives
//! on the caller's stack. Every message carries the same tag the C++ module
//! used, so existing logs stay greppable.

use core::ffi::c_char;
use core::fmt::{self, Write};

use crate::shim;

const LOG_BUF: usize = 192;
const TAG_BUF: usize = 16;

struct Buf {
    bytes: [u8; LOG_BUF],
    len: usize,
    overflowed: bool,
}

impl Buf {
    fn new() -> Self {
        Buf { bytes: [0; LOG_BUF], len: 0, overflowed: false }
    }

    /// NUL-terminate in place, replacing the tail with `...` if it did not fit.
    fn finish(&mut self) -> &[u8] {
        if self.len >= LOG_BUF - 1 {
            self.len = LOG_BUF - 1;
            self.overflowed = true;
        }
        if self.overflowed {
            let tail = b"...";
            let at = self.len - tail.len();
            self.bytes[at..self.len].copy_from_slice(tail);
        }
        self.bytes[self.len] = 0;
        &self.bytes[..=self.len]
    }
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        if self.len + bytes.len() > LOG_BUF - 1 {
            self.overflowed = true;
            return Err(fmt::Error);
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}

pub fn log(level: i32, tag: &str, args: fmt::Arguments<'_>) {
    let mut msg = Buf::new();
    // A full buffer is expected, not exceptional: `finish` marks the truncation.
    let _ = fmt::write(&mut msg, args);

    let mut ctag = shim::CBuf::<TAG_BUF>::new();
    ctag.push(tag);

    let msg = msg.finish();
    unsafe {
        shim::rf_log(
            level,
            ctag.as_ptr(),
            msg.as_ptr() as *const c_char,
        )
    };
}

#[macro_export]
macro_rules! log_e {
    ($tag:expr, $($t:tt)*) => {
        $crate::log::log($crate::shim::LOG_ERROR, $tag, format_args!($($t)*))
    };
}

#[macro_export]
macro_rules! log_w {
    ($tag:expr, $($t:tt)*) => {
        $crate::log::log($crate::shim::LOG_WARN, $tag, format_args!($($t)*))
    };
}

#[macro_export]
macro_rules! log_i {
    ($tag:expr, $($t:tt)*) => {
        $crate::log::log($crate::shim::LOG_INFO, $tag, format_args!($($t)*))
    };
}
