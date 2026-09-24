//! Pairing response classification. Pure: the C side owns HTTP, the response
//! buffer, cJSON parsing, the NVS token write and the screen; this module only
//! turns already-parsed facts into the outcome `pairing.rs` consumes.
//!
//! The rules are a compatibility port, not a tightening. `do_pair_start` used
//! to succeed only on 200 + valid JSON + string `code` + number `expires_in`;
//! the claim path granted on any non-empty string token, stayed pending on 200
//! without one (including invalid JSON), rejected on 401/429, and called
//! everything else a network error. Each rule below has a test named after the
//! behaviour it pins.

/// Facts the C side extracted from a pair-start response (HTTP + cJSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairStartFacts {
    /// HTTP status, or a negative transport status.
    pub status: i32,
    pub json_valid: bool,
    pub code_is_string: bool,
    pub expires_is_number: bool,
}

/// Facts the C side extracted from a pair-claim response (HTTP + cJSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimFacts {
    /// HTTP status, or a negative transport status.
    pub status: i32,
    pub json_valid: bool,
    pub token_is_string: bool,
    /// `token` is a string with a first byte. The C side must pass this
    /// separately: `cJSON_IsString` alone does not tell an empty string from a
    /// non-empty one.
    pub token_nonempty: bool,
}

/// What a pair-start response means. Maps onto `rf_pairing_outcome_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStartResult {
    /// Code and expiry in hand; show the code.
    Started,
    /// Anything else: count a failure and retry later.
    Failed,
}

/// What a pair-claim response means. Maps onto `rf_pairing_outcome_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimResult {
    /// Store the token.
    Granted,
    /// 200 without a token — the user has not confirmed yet.
    Pending,
    /// 401/429 — the code is dead; drop it and back off.
    Rejected,
    /// Anything else, including transport failures.
    NetworkError,
}

/// The old `do_pair_start` succeeded only on 200 + parseable JSON + string
/// `code` + number `expires_in`. Anything else was "网络错误" → one failure.
pub fn classify_pair_start(f: &PairStartFacts) -> PairStartResult {
    if f.status == 200 && f.json_valid && f.code_is_string && f.expires_is_number {
        PairStartResult::Started
    } else {
        PairStartResult::Failed
    }
}

/// The old claim outer check, in order: 200 + token → paired; 200 alone →
/// pending; 401/429 → drop the code; anything else → network error.
pub fn classify_claim(f: &ClaimFacts) -> ClaimResult {
    if f.status == 200 {
        // `token_is_string && token_nonempty` is exactly the old
        // `cJSON_IsString(token_item)` + `token[0] != '\0'` pair.
        if f.token_is_string && f.token_nonempty {
            ClaimResult::Granted
        } else {
            ClaimResult::Pending
        }
    } else if f.status == 401 || f.status == 429 {
        ClaimResult::Rejected
    } else {
        ClaimResult::NetworkError
    }
}

/// `rf_pairing_outcome_t` in `rust/include/pairing.h`. Keep in step.
pub const RF_PAIR_OUTCOME_PAIR_STARTED: u8 = 2;
pub const RF_PAIR_OUTCOME_PAIR_START_FAILED: u8 = 3;
pub const RF_PAIR_OUTCOME_CLAIM_PENDING: u8 = 4;
pub const RF_PAIR_OUTCOME_CLAIM_GRANTED: u8 = 5;
pub const RF_PAIR_OUTCOME_CLAIM_REJECTED: u8 = 6;
pub const RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR: u8 = 7;

impl PairStartResult {
    /// The `rf_pairing_outcome_t` the C loop feeds back into `rf_pairing_decide`.
    pub fn outcome_code(self) -> u8 {
        match self {
            PairStartResult::Started => RF_PAIR_OUTCOME_PAIR_STARTED,
            PairStartResult::Failed => RF_PAIR_OUTCOME_PAIR_START_FAILED,
        }
    }
}

impl ClaimResult {
    /// The `rf_pairing_outcome_t` the C loop feeds back into `rf_pairing_decide`.
    pub fn outcome_code(self) -> u8 {
        match self {
            ClaimResult::Granted => RF_PAIR_OUTCOME_CLAIM_GRANTED,
            ClaimResult::Pending => RF_PAIR_OUTCOME_CLAIM_PENDING,
            ClaimResult::Rejected => RF_PAIR_OUTCOME_CLAIM_REJECTED,
            ClaimResult::NetworkError => RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR,
        }
    }
}

/// # Safety
/// `f` must point to a valid `PairStartFacts`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_pairing_classify_pair_start(f: *const PairStartFacts) -> u8 {
    classify_pair_start(unsafe { &*f }).outcome_code()
}

/// # Safety
/// `f` must point to a valid `ClaimFacts`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_pairing_classify_claim(f: *const ClaimFacts) -> u8 {
    classify_claim(unsafe { &*f }).outcome_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_start(
        status: i32,
        json_valid: bool,
        code_is_string: bool,
        expires_is_number: bool,
    ) -> PairStartFacts {
        PairStartFacts { status, json_valid, code_is_string, expires_is_number }
    }

    fn claim(status: i32, json_valid: bool, token_is_string: bool, token_nonempty: bool) -> ClaimFacts {
        ClaimFacts { status, json_valid, token_is_string, token_nonempty }
    }

    #[test]
    fn pair_start_requires_200_valid_string_code_and_number_expiry() {
        assert_eq!(
            classify_pair_start(&pair_start(200, true, true, true)),
            PairStartResult::Started
        );
    }

    #[test]
    fn pair_start_type_mismatch_is_failure() {
        // A 200 with valid JSON still fails when either field has the wrong
        // type — that was the old cJSON_IsString/IsNumber gate.
        assert_eq!(
            classify_pair_start(&pair_start(200, true, false, true)),
            PairStartResult::Failed
        );
        assert_eq!(
            classify_pair_start(&pair_start(200, true, true, false)),
            PairStartResult::Failed
        );
        assert_eq!(
            classify_pair_start(&pair_start(200, false, true, true)),
            PairStartResult::Failed
        );
    }

    #[test]
    fn pair_start_non_200_or_transport_error_is_failure() {
        assert_eq!(
            classify_pair_start(&pair_start(500, true, true, true)),
            PairStartResult::Failed
        );
        assert_eq!(
            classify_pair_start(&pair_start(-1, false, false, false)),
            PairStartResult::Failed
        );
    }

    #[test]
    fn claim_200_with_any_nonempty_token_is_granted() {
        assert_eq!(
            classify_claim(&claim(200, true, true, true)),
            ClaimResult::Granted
        );
        // No length or charset validation: any non-empty string grants.
    }

    #[test]
    fn claim_200_without_token_or_with_invalid_json_is_pending() {
        // 200 + {"status":"pending"} — or a body that failed to parse at all.
        assert_eq!(
            classify_claim(&claim(200, true, false, false)),
            ClaimResult::Pending
        );
        assert_eq!(
            classify_claim(&claim(200, false, false, false)),
            ClaimResult::Pending
        );
        // An empty-string token is "no token" in the old outer check too.
        assert_eq!(
            classify_claim(&claim(200, true, true, false)),
            ClaimResult::Pending
        );
    }

    #[test]
    fn claim_401_or_429_is_rejected() {
        assert_eq!(
            classify_claim(&claim(401, false, false, false)),
            ClaimResult::Rejected
        );
        assert_eq!(
            classify_claim(&claim(429, false, false, false)),
            ClaimResult::Rejected
        );
    }

    #[test]
    fn other_statuses_and_transport_errors_are_network_errors() {
        assert_eq!(
            classify_claim(&claim(500, false, false, false)),
            ClaimResult::NetworkError
        );
        assert_eq!(
            classify_claim(&claim(-1, false, false, false)),
            ClaimResult::NetworkError
        );
    }

    #[test]
    fn outcome_codes_match_pairing_h() {
        assert_eq!(PairStartResult::Started.outcome_code(), RF_PAIR_OUTCOME_PAIR_STARTED);
        assert_eq!(PairStartResult::Failed.outcome_code(), RF_PAIR_OUTCOME_PAIR_START_FAILED);
        assert_eq!(ClaimResult::Pending.outcome_code(), RF_PAIR_OUTCOME_CLAIM_PENDING);
        assert_eq!(ClaimResult::Granted.outcome_code(), RF_PAIR_OUTCOME_CLAIM_GRANTED);
        assert_eq!(ClaimResult::Rejected.outcome_code(), RF_PAIR_OUTCOME_CLAIM_REJECTED);
        assert_eq!(ClaimResult::NetworkError.outcome_code(), RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR);
    }

    #[test]
    fn the_c_abi_returns_the_same_codes() {
        let ps = pair_start(200, true, true, true);
        let ps_bad = pair_start(200, true, true, false);
        // SAFETY: both pointers are to locals that outlive the calls.
        assert_eq!(unsafe { rf_pairing_classify_pair_start(&ps) }, 2);
        assert_eq!(
            unsafe { rf_pairing_classify_pair_start(&ps_bad) },
            RF_PAIR_OUTCOME_PAIR_START_FAILED
        );

        let granted = claim(200, true, true, true);
        let pending = claim(200, false, false, false);
        let rejected = claim(429, false, false, false);
        let neterr = claim(-1, false, false, false);
        // SAFETY: all pointers are to locals that outlive the calls.
        assert_eq!(unsafe { rf_pairing_classify_claim(&granted) }, RF_PAIR_OUTCOME_CLAIM_GRANTED);
        assert_eq!(unsafe { rf_pairing_classify_claim(&pending) }, RF_PAIR_OUTCOME_CLAIM_PENDING);
        assert_eq!(unsafe { rf_pairing_classify_claim(&rejected) }, RF_PAIR_OUTCOME_CLAIM_REJECTED);
        assert_eq!(
            unsafe { rf_pairing_classify_claim(&neterr) },
            RF_PAIR_OUTCOME_CLAIM_NETWORK_ERROR
        );
    }
}
