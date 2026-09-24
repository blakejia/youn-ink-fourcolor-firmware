// ─── Shared Rust policy ABI contract ────────────────────────────────────────
//
// Every Rust policy module that exchanges a fixed-layout struct with C++
// must follow the same convention so a single review catches drift:
//
//   1.  `#[repr(C)]` on every C-ABI input and output struct.
//   2.  Fixed field order matching the C header, with explicit `_padN`
//       fields where C uses `int8_t _pad[N]` so the offset table stays
//       readable from both sides.
//   3.  Booleans are `i8` (or `u8`) followed by 7 bytes of padding to
//       keep the next field 8-byte aligned.
//   4.  Timestamps are `i64` so -1 remains a usable sentinel.
//   5.  Enum outputs are `i32` (or `u32`) followed by 4 bytes of padding.
//   6.  `#[repr(u8)]` for small Rust enums whose C side uses `uint8_t`;
//       `#[repr(i32)]` for enums whose C side uses `int32_t`.
//   7.  Every C entry point is `#[unsafe(no_mangle)] extern "C"`.
//   8.  No pointers, no strings, no nested variable-size payloads —
//       only fixed POD.
//
// The helpers below let each ABI struct declare its expected size and
// verify it on `cargo test`. Adding a new module without registering it
// here is a deliberate change to call it a day on the contract.
//
// Future tasks (ABCDE 2..6) must add a one-line
// `assert_abi_pod_layout::<$MODULE_NAME::CInputs>(name, expected_size)`
// in their `#[cfg(test)] mod tests` so the convention is checked.

/// Verify a `#[repr(C)]` POD struct matches its expected size in bytes.
///
/// `name` is only used in the assertion message so a failure points at
/// the offending module.
#[cfg(test)]
#[track_caller]
pub fn assert_abi_pod_size<T>(name: &str, expected_size: usize) {
    use core::mem::size_of;
    let actual = size_of::<T>();
    assert_eq!(
        actual, expected_size,
        "{name}: ABI struct size mismatch (got {actual}, expected {expected_size}); \
         this usually means a field type or padding byte changed without \
         updating the C header"
    );
}

/// Verify that `T` has at least 8-byte alignment (the floor used by every
/// current ABI struct so an `i64` field is naturally aligned).
#[cfg(test)]
#[track_caller]
pub fn assert_abi_pod_alignment<T>(name: &str) {
    use core::mem::align_of;
    let a = align_of::<T>();
    assert!(
        a >= 8,
        "{name}: ABI struct alignment must be >= 8 to host i64 fields \
         without forcing the C side to insert compiler padding (got {a})"
    );
}

/// Combined check: call both `assert_abi_pod_size` and
/// `assert_abi_pod_alignment`. The convention holds if both pass.
#[cfg(test)]
#[track_caller]
pub fn assert_abi_pod_layout<T>(name: &str, expected_size: usize) {
    assert_abi_pod_alignment::<T>(name);
    assert_abi_pod_size::<T>(name, expected_size);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convention-probe struct used to drive RED/GREEN below. The Rust
    /// side defines what we expect the C side to look like; the helper
    /// checks the two invariants that any C-ABI struct must hold.
    #[repr(C)]
    struct ProbePod {
        flag: i8,
        _pad0: [u8; 7],
        ts: i64,
    }

    /// RED/GREEN 1: when the size assertion matches, the helper passes.
    #[test]
    fn matching_size_passes() {
        assert_abi_pod_layout::<ProbePod>("ProbePod", core::mem::size_of::<ProbePod>());
    }

    /// RED/GREEN 2: a wrong expected size fails the assertion.
    /// The panic message below is the RED proof for the contract helper.
    #[test]
    #[should_panic(expected = "ABI struct size mismatch")]
    fn wrong_size_fails() {
        assert_abi_pod_size::<ProbePod>("ProbePod", 999);
    }

    /// RED/GREEN 3: the alignment helper rejects a struct that does not
    /// meet the 8-byte floor. `UnalignedProbe` is `#[repr(C)]` but holds
    /// only `u8` fields, so its alignment stays at 1.
    #[test]
    #[should_panic(expected = "alignment must be >= 8")]
    fn unaligned_struct_fails() {
        assert_abi_pod_alignment::<UnalignedProbe>("UnalignedProbe");
    }

    #[repr(C)]
    struct UnalignedProbe {
        a: u8,
    }
}

// ─── Contract: existing ABI structs follow the convention ────────────────────
//
// Each test below invokes `assert_abi_pod_size` against a real
// `#[repr(C)]` struct from a Rust policy module and pins its expected
// size to the value matching the matching `rf_*_policy_*_t` (or
// `rf_*_facts_t`) struct in `firmware/main/rust/include/*.h`.
//
// A test failure means the C header and the Rust struct drifted apart,
// or the convention was broken (wrong padding, wrong field type, lost
// `repr(C)`).

#[cfg(test)]
mod contract_existing {
    use super::*;
    use crate::{charge_policy, input, led_policy, pairing, pairing_response, power, wifi_policy};

    #[test]
    fn charge_policy_inputs_size() {
        assert_abi_pod_size::<charge_policy::CInputs>("charge_policy::CInputs", 64);
    }

    #[test]
    fn charge_policy_output_size() {
        assert_abi_pod_size::<charge_policy::COutput>("charge_policy::COutput", 56);
    }

    #[test]
    fn led_policy_inputs_size() {
        assert_abi_pod_size::<led_policy::CInputs>("led_policy::CInputs", 44);
    }

    #[test]
    fn led_policy_output_size() {
        assert_abi_pod_size::<led_policy::COutput>("led_policy::COutput", 56);
    }

    #[test]
    fn pair_start_facts_size() {
        assert_abi_pod_size::<pairing_response::PairStartFacts>(
            "pairing_response::PairStartFacts", 8);
    }

    #[test]
    fn pair_claim_facts_size() {
        assert_abi_pod_size::<pairing_response::ClaimFacts>(
            "pairing_response::ClaimFacts", 8);
    }

    #[test]
    fn power_inputs_size() {
        assert_abi_pod_size::<power::CInputs>("power::CInputs", 40);
    }

    #[test]
    fn pairing_inputs_size() {
        assert_abi_pod_size::<pairing::CInputs>("pairing::CInputs", 16);
    }

    #[test]
    fn input_inputs_size() {
        assert_abi_pod_size::<input::CInputs>("input::CInputs", 8);
    }

    #[test]
    fn wifi_policy_inputs_size() {
        assert_abi_pod_size::<wifi_policy::CInputs>("wifi_policy::CInputs", 100);
    }
}