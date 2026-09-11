//! Xtensa-only C ABI surface.
//!
//! Deliberately thin: the C++ shim owns the hardware side (`esp_read_mac`,
//! `time`, `esp_fill_random`) and the public `device_sign_pair_start` symbol
//! declared in `include/device_signature.h`, then calls [`devsig_sign`] here for
//! the cryptography. Nothing in this file touches ESP-IDF.

use core::ffi::c_char;

use super::{Inputs, MAC_LEN, NONCE_LEN, sign_with_build_key};

fn c_str_len(s: *const c_char) -> usize {
    let mut n = 0;
    // SAFETY: the caller guarantees a NUL-terminated string.
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// Copy `src` into a caller buffer with a trailing NUL.
///
/// A buffer too small for the field becomes an empty string rather than a
/// truncated one: the server then rejects the request, which is far easier to
/// diagnose than a truncated timestamp that still produces a signature.
///
/// # Safety
/// `dst` must be valid for `cap` bytes.
unsafe fn copy_cstr(dst: *mut c_char, cap: usize, src: &[u8]) {
    if dst.is_null() {
        return;
    }
    if cap < src.len() + 1 {
        unsafe { *dst = 0 };
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.cast::<u8>(), src.len());
        *dst.add(src.len()) = 0;
    }
}

/// Sign a pair-start request from already-gathered inputs.
///
/// # Safety
/// `device_id` must be NUL-terminated; `mac_raw`/`nonce_raw` must be readable
/// for 6/16 bytes; every `*_out` must be valid for its matching `*_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn devsig_sign(
    device_id: *const c_char,
    mac_raw: *const u8,
    timestamp: i64,
    nonce_raw: *const u8,
    mac_hex_out: *mut c_char,
    mac_hex_len: usize,
    ts_out: *mut c_char,
    ts_len: usize,
    nonce_out: *mut c_char,
    nonce_len: usize,
    sig_out: *mut c_char,
    sig_len: usize,
) {
    if device_id.is_null() || mac_raw.is_null() || nonce_raw.is_null() {
        return;
    }

    let mut mac = [0u8; MAC_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    unsafe {
        core::ptr::copy_nonoverlapping(mac_raw, mac.as_mut_ptr(), MAC_LEN);
        core::ptr::copy_nonoverlapping(nonce_raw, nonce.as_mut_ptr(), NONCE_LEN);
    }

    let inputs = Inputs { mac, timestamp, nonce };
    let device_id =
        unsafe { core::slice::from_raw_parts(device_id.cast::<u8>(), c_str_len(device_id)) };
    let sig = sign_with_build_key(device_id, &inputs);

    unsafe {
        copy_cstr(mac_hex_out, mac_hex_len, sig.mac_hex_str());
        copy_cstr(ts_out, ts_len, sig.timestamp_str());
        copy_cstr(nonce_out, nonce_len, sig.nonce_b64_str());
        copy_cstr(sig_out, sig_len, sig.sig_b64_str());
    }
}
