//! Golden vectors for the pair-start signature.
//!
//! The expected values were produced by the server's own test oracle
//! (`server/tests/device_sig.py`, which mirrors
//! `pairing.py::verify_device_signature`) — an independent implementation of
//! the wire format. If the Rust port drifts from the format the device and the
//! server agree on, these fail.
//!
//! `mac` is raw bytes here; on the device it comes from `esp_read_mac`.

use device_signature::{
    Inputs, MASTER_KEY, MAC_LEN, NONCE_B64_LEN, NONCE_LEN, SIGNATURE_B64_LEN, sign,
};

/// Same key the server test suite injects, so the vectors can be re-verified
/// against `pairing.py` without re-deriving anything.
const TEST_MASTER_KEY: &[u8] = b"test_master_key_at_least_32_bytes_long_xx";

fn inputs(mac: [u8; MAC_LEN], timestamp: i64, nonce: [u8; NONCE_LEN]) -> Inputs {
    Inputs { mac, timestamp, nonce }
}

fn sig_str(s: &device_signature::Signature) -> String {
    String::from_utf8(s.sig_b64_str().to_vec()).unwrap()
}

fn nonce_str(s: &device_signature::Signature) -> String {
    String::from_utf8(s.nonce_b64_str().to_vec()).unwrap()
}

fn ts_str(s: &device_signature::Signature) -> String {
    String::from_utf8(s.timestamp_str().to_vec()).unwrap()
}

#[test]
fn nonce_and_signature_match_the_server_oracle() {
    let cases: [(&str, [u8; MAC_LEN], i64, [u8; NONCE_LEN], &str, &str); 3] = [
        (
            "NOTE4C-3400FC",
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            1_789_099_606,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            "AAECAwQFBgcICQoLDA0ODw==",
            "28YTvlLlMvw6dCUswnizDl4c4ilXH0mU4iG+EybpXac=",
        ),
        (
            "NOTE4C-3400FC",
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            1_000_000_000,
            [0xFF; NONCE_LEN],
            "/////////////////////w==",
            "5jEv0gQw5j9emW9dJf57BHnDDfByDh9wnZdkc+wrrh4=",
        ),
        (
            "X",
            [0; MAC_LEN],
            1,
            [0; NONCE_LEN],
            "AAAAAAAAAAAAAAAAAAAAAA==",
            "UTQZkd7kPFlvG0wpMCFyUI1P4JBfg18pmzY8Foa1hjo=",
        ),
    ];

    for (device_id, mac, ts, nonce, want_nonce, want_sig) in cases {
        let out = sign(TEST_MASTER_KEY, device_id.as_bytes(), &inputs(mac, ts, nonce));
        assert_eq!(nonce_str(&out), want_nonce, "nonce for {device_id}@{ts}");
        assert_eq!(sig_str(&out), want_sig, "signature for {device_id}@{ts}");
        assert_eq!(ts_str(&out), ts.to_string(), "timestamp ascii");
        assert_eq!(
            String::from_utf8(out.mac_hex_str().to_vec()).unwrap(),
            mac.iter().map(|b| format!("{b:02X}")).collect::<String>(),
            "mac hex"
        );
    }
}

#[test]
fn base64_is_padded() {
    // The server base64-decodes strictly; the unpadded form used to 401.
    let out = sign(
        TEST_MASTER_KEY,
        b"NOTE4C-3400FC",
        &inputs([0; MAC_LEN], 1, [0; NONCE_LEN]),
    );
    assert!(nonce_str(&out).ends_with("=="), "16 bytes must pad with ==");
    assert!(sig_str(&out).ends_with('='), "32 bytes must pad with =");
    assert_eq!(nonce_str(&out).len(), NONCE_B64_LEN);
    assert_eq!(sig_str(&out).len(), SIGNATURE_B64_LEN);
}

#[test]
fn different_device_ids_produce_different_signatures() {
    let a = sign(TEST_MASTER_KEY, b"NOTE4C-3400FC", &inputs([1; MAC_LEN], 7, [2; NONCE_LEN]));
    let b = sign(TEST_MASTER_KEY, b"NOTE4C-3400FD", &inputs([1; MAC_LEN], 7, [2; NONCE_LEN]));
    assert_ne!(sig_str(&a), sig_str(&b));
}

#[test]
fn master_key_falls_back_to_the_placeholder() {
    // `cargo test` runs without DEVICE_MASTER_KEY; a real build passes it in.
    // Either way the constant must never be empty.
    assert!(!MASTER_KEY.is_empty());
}
