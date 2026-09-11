//! Pair-start device signature: HMAC-SHA256 over `MAC(6) ‖ timestamp ‖ nonce`.
//!
//! Byte-for-byte replacement for the hand-rolled RFC-6234 SHA-256 + HMAC +
//! base64 that used to live in `main/common/device_signature.cc`. The wire
//! format is fixed by the server verifier
//! (`server/youn_server/pairing.py::verify_device_signature`):
//!
//! ```text
//! derived_key = HMAC-SHA256(MASTER_KEY, device_id)
//! payload     = mac_bytes(6) ‖ ascii(timestamp) ‖ ascii(base64(nonce))
//! signature   = base64(HMAC-SHA256(derived_key, payload))
//! ```
//!
//! `base64` here is *padded* standard base64 — the server rejects unpadded
//! input, and that mismatch is what produced "undecodable base64" 401s before.
//!
//! The whole algorithm is pure and host-testable ([`sign`]); the three values
//! that only the device can produce (Wi-Fi MAC, wall clock, entropy) are
//! gathered in [`ffi`], which is compiled only for the Xtensa target.

// `no_std` only for the device: on the host the crate is built as an rlib for
// `cargo test`, and a no_std host build cannot unwind (which the test harness
// needs). Nothing in here uses std either way.
#![cfg_attr(target_arch = "xtensa", no_std)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Master key, injected at build time: the component's `CMakeLists.txt` forwards
/// the CMake cache variable `DEVICE_MASTER_KEY` into cargo's environment.
///
/// Mirrors the `config.h` fallback — the placeholder means every signature is
/// rejected server-side, which is the intended failure mode for a build that
/// forgot to inject the key.
pub const MASTER_KEY: &str = match option_env!("DEVICE_MASTER_KEY") {
    Some(k) if !k.is_empty() => k,
    _ => "REPLACE_ME_AT_BUILD_TIME_WITH_32_BYTE_RANDOM",
};

/// Raw Wi-Fi station MAC length.
pub const MAC_LEN: usize = 6;
/// Raw nonce length.
pub const NONCE_LEN: usize = 16;
/// 16 raw bytes → 22 base64 chars + `==`.
pub const NONCE_B64_LEN: usize = 24;
/// 32 raw bytes → 43 base64 chars + `=`.
pub const SIGNATURE_B64_LEN: usize = 44;
/// A decimal `i64` (including the sign) never exceeds 20 characters.
pub const TIMESTAMP_LEN: usize = 20;
/// Fixed upper bound for the signed payload, so signing needs no allocator.
const PAYLOAD_MAX: usize = MAC_LEN + TIMESTAMP_LEN + NONCE_B64_LEN;

/// The inputs the host side cannot supply.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    /// Wi-Fi station MAC, raw bytes.
    pub mac: [u8; MAC_LEN],
    /// Unix seconds from the SNTP-synced wall clock (not boot uptime).
    pub timestamp: i64,
    /// 16 random bytes from the hardware RNG.
    pub nonce: [u8; NONCE_LEN],
}

/// A signed pair-start request. Every field is fixed width and NUL-terminated,
/// ready to be handed to `X-Device-*` headers as-is.
#[derive(Clone, Copy, Debug)]
pub struct Signature {
    pub mac_hex: [u8; MAC_LEN * 2 + 1],
    pub timestamp: [u8; TIMESTAMP_LEN + 1],
    pub nonce_b64: [u8; NONCE_B64_LEN + 1],
    pub sig_b64: [u8; SIGNATURE_B64_LEN + 1],
}

impl Signature {
    /// Field bytes without the NUL terminator.
    pub fn mac_hex_str(&self) -> &[u8] {
        strip_nul(&self.mac_hex)
    }
    pub fn timestamp_str(&self) -> &[u8] {
        strip_nul(&self.timestamp)
    }
    pub fn nonce_b64_str(&self) -> &[u8] {
        strip_nul(&self.nonce_b64)
    }
    pub fn sig_b64_str(&self) -> &[u8] {
        strip_nul(&self.sig_b64)
    }
}

fn strip_nul(buf: &[u8]) -> &[u8] {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    &buf[..end]
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn hex_upper(mac: &[u8; MAC_LEN]) -> [u8; MAC_LEN * 2 + 1] {
    let mut out = [0u8; MAC_LEN * 2 + 1];
    for (i, b) in mac.iter().enumerate() {
        out[i * 2] = HEX_UPPER[(b >> 4) as usize];
        out[i * 2 + 1] = HEX_UPPER[(b & 0x0f) as usize];
    }
    out
}

/// Decimal ASCII of `v` into `out`, returning the number of bytes written.
///
/// Hand-rolled rather than `core::fmt`, which would drag the whole formatting
/// machinery (and ~10 KB) into the staticlib for one integer.
fn write_decimal(v: i64, out: &mut [u8]) -> usize {
    let negative = v < 0;
    let mut n = v.unsigned_abs();
    let mut digits = [0u8; TIMESTAMP_LEN];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (n % 10) as u8;
        count += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let mut len = 0;
    if negative {
        out[len] = b'-';
        len += 1;
    }
    for i in (0..count).rev() {
        out[len] = digits[i];
        len += 1;
    }
    len
}

/// Padded base64 of `src` into `dst`; returns `dst.len()` on success.
fn base64_into(src: &[u8], dst: &mut [u8]) -> usize {
    B64.encode_slice(src, dst).unwrap_or(0)
}

/// Build the signed request. Pure: same inputs, same output, no syscalls.
pub fn sign(master_key: &[u8], device_id: &[u8], inputs: &Inputs) -> Signature {
    let mut out = Signature {
        mac_hex: hex_upper(&inputs.mac),
        timestamp: [0; TIMESTAMP_LEN + 1],
        nonce_b64: [0; NONCE_B64_LEN + 1],
        sig_b64: [0; SIGNATURE_B64_LEN + 1],
    };

    let mut ts_buf = [0u8; TIMESTAMP_LEN];
    let ts_len = write_decimal(inputs.timestamp, &mut ts_buf);
    out.timestamp[..ts_len].copy_from_slice(&ts_buf[..ts_len]);
    let nonce_len = base64_into(&inputs.nonce, &mut out.nonce_b64);
    if nonce_len != NONCE_B64_LEN {
        return out; // leave the signature empty rather than sign a wrong nonce
    }

    // derived_key = HMAC-SHA256(MASTER_KEY, device_id)
    let mut mac = match HmacSha256::new_from_slice(master_key) {
        Ok(m) => m,
        Err(_) => return out, // unreachable: HMAC accepts any key length
    };
    mac.update(device_id);
    let derived = mac.finalize().into_bytes();

    // payload = mac_bytes ‖ timestamp_ascii ‖ nonce_b64_ascii
    let mut payload = [0u8; PAYLOAD_MAX];
    payload[..MAC_LEN].copy_from_slice(&inputs.mac);
    payload[MAC_LEN..MAC_LEN + ts_len].copy_from_slice(&out.timestamp[..ts_len]);
    let nonce_at = MAC_LEN + ts_len;
    payload[nonce_at..nonce_at + nonce_len].copy_from_slice(&out.nonce_b64[..nonce_len]);
    let payload = &payload[..nonce_at + nonce_len];

    let mut hmac = match HmacSha256::new_from_slice(&derived) {
        Ok(m) => m,
        Err(_) => return out,
    };
    hmac.update(payload);
    let raw = hmac.finalize().into_bytes();
    let _ = base64_into(&raw, &mut out.sig_b64);

    out
}

/// Sign with the build-time [`MASTER_KEY`].
pub fn sign_with_build_key(device_id: &[u8], inputs: &Inputs) -> Signature {
    sign(MASTER_KEY.as_bytes(), device_id, inputs)
}

#[cfg(target_arch = "xtensa")]
mod ffi;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_covers_sign_and_extremes() {
        let mut buf = [0u8; TIMESTAMP_LEN];
        let cases: [(i64, &str); 6] = [
            (0, "0"),
            (1, "1"),
            (1789099606, "1789099606"),
            (-1, "-1"),
            (i64::MAX, "9223372036854775807"),
            (i64::MIN, "-9223372036854775808"),
        ];
        for (v, want) in cases {
            let n = write_decimal(v, &mut buf);
            assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), want, "v={v}");
        }
    }

    #[test]
    fn mac_is_uppercase_hex() {
        let out = hex_upper(&[0x00, 0x0a, 0xab, 0xcd, 0xef, 0xff]);
        assert_eq!(&out[..12], b"000AABCDEFFF");
        assert_eq!(out[12], 0, "must be NUL terminated");
    }

    #[test]
    fn field_widths_are_fixed() {
        let sig = sign(
            b"k",
            b"D",
            &Inputs { mac: [1, 2, 3, 4, 5, 6], timestamp: 1, nonce: [0; NONCE_LEN] },
        );
        assert_eq!(sig.mac_hex_str().len(), 12);
        assert_eq!(sig.timestamp_str(), b"1");
        assert_eq!(sig.nonce_b64_str().len(), NONCE_B64_LEN);
        assert_eq!(sig.sig_b64_str().len(), SIGNATURE_B64_LEN);
        assert_eq!(sig.nonce_b64_str().last(), Some(&b'='));
        assert_eq!(sig.sig_b64_str().last(), Some(&b'='));
    }

    #[test]
    fn timestamp_is_not_zero_padded() {
        // The payload uses the decimal digits verbatim, so a short timestamp
        // produces a different signature than a padded one.
        let mk = b"k";
        let base = |ts| sign(mk, b"D", &Inputs { mac: [0; 6], timestamp: ts, nonce: [0; 16] });
        assert_ne!(base(1).sig_b64_str(), base(10).sig_b64_str());
    }
}
