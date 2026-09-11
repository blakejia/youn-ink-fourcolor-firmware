//! Pairing policy. Pure: the C side owns HTTP, NVS and the display, this
//! decides what to do next.
//!
//! Port of the blocking `while (true)` in `main/common/server_pairing.cc`. That
//! loop mixed three things — HTTP, NVS, screen — with the protocol policy, and
//! the policy is the part that has actually broken before: the old loop could
//! only `return true`, so its error paths were unreachable and a device stuck
//! in PairStart showed no code, no error and no hint; and signing with an
//! unsynced clock produced a 1970 timestamp, which the server must reject, so
//! every failure spent one of the five per-IP pair-start slots until the device
//! was rate-limited into a loop. Both are runtime behaviour that could only be
//! observed on the device by waiting five minutes. Here they are assertions.

/// The code on screen is only good for this long; past it the device asks for
/// a new one. Server-side `expires_in` is the same 300.
pub const PAIRING_TIMEOUT_S: u32 = 300;
/// Claim polling while the user reads the code off the screen.
pub const CLAIM_POLL_MS: u32 = 2000;
/// Backoff after 401/429. The server limits pair-start to 5 per 300 s per IP, so
/// an immediate retry turns a rejected code into a rate-limit loop.
pub const CLAIM_ERROR_BACKOFF_MS: u32 = 5000;
/// Backoff after a failed pair-start, before asking again.
pub const PAIR_START_BACKOFF_MS: u32 = 5000;
/// How long to wait for SNTP before looking at the clock again.
pub const CLOCK_WAIT_MS: u32 = 5000;
/// Consecutive pair-start (or unsynced-clock) failures before giving up on this
/// attempt. Giving up is a real exit: the old loop never took it.
pub const MAX_PAIR_START_FAILURES: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing has been attempted yet.
    None,
    /// Waited for SNTP; still not plausible.
    ClockWaited,
    /// A code is on screen.
    PairStarted,
    PairStartFailed,
    /// 200 with `{"status":"pending"}` — the user has not confirmed yet.
    ClaimPending,
    /// 200 with a token.
    ClaimGranted,
    /// 401 (invalid/expired code) or 429 (too many failed attempts).
    ClaimRejected,
    /// Other status codes, or the connection failed.
    ClaimNetworkError,
    /// The token arrived but NVS would not take it.
    TokenWriteFailed,
}

impl Outcome {
    /// The C side passes this as a small integer; keep in step with
    /// `rf_pairing_outcome_t` in `rust/include/pairing.h`.
    pub fn from_code(code: u8) -> Outcome {
        match code {
            1 => Outcome::ClockWaited,
            2 => Outcome::PairStarted,
            3 => Outcome::PairStartFailed,
            4 => Outcome::ClaimPending,
            5 => Outcome::ClaimGranted,
            6 => Outcome::ClaimRejected,
            7 => Outcome::ClaimNetworkError,
            8 => Outcome::TokenWriteFailed,
            _ => Outcome::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    /// A code is on screen and its window has not been dropped.
    pub has_code: bool,
    /// Wall clock is plausible, i.e. SNTP has landed.
    pub clock_ok: bool,
    /// Seconds since the current window started.
    pub window_elapsed_s: u32,
    /// Consecutive pair-start (or clock) failures.
    pub pair_start_failures: u32,
    /// Result of the previous action.
    pub last: Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Ask the server for a code.
    PairStart,
    /// Ask again after a wait (the previous ask failed).
    PairStartAfter { ms: u32 },
    /// The window elapsed: drop the code and ask for a new one.
    ReissueCode,
    /// The code was rejected: drop it and back off first.
    ReissueCodeWithBackoff { ms: u32 },
    /// No usable clock: wait, do not spend a pair-start slot yet.
    WaitForClock { ms: u32 },
    /// Poll the claim now.
    Claim,
    /// Poll the claim after a wait.
    ClaimAfter { ms: u32 },
    /// Token obtained and stored.
    Paired,
    /// Abandon this attempt; the caller leaves the pairing page.
    GiveUp,
}

pub fn decide(i: &Inputs) -> Action {
    // Terminal outcomes first: a token that could not be stored, or one
    // already in hand, cannot be improved by another request.
    match i.last {
        Outcome::TokenWriteFailed => return Action::GiveUp,
        Outcome::ClaimGranted => return Action::Paired,
        _ => {}
    }

    // A code on the glass outlives its window: ask for a new one.
    if i.has_code && i.window_elapsed_s >= PAIRING_TIMEOUT_S {
        return Action::ReissueCode;
    }

    if i.has_code {
        return match i.last {
            // Rejected means the code is dead, and the next ask has to wait:
            // the server allows five pair-starts per 300 s per IP.
            Outcome::ClaimRejected => {
                Action::ReissueCodeWithBackoff { ms: CLAIM_ERROR_BACKOFF_MS }
            }
            Outcome::ClaimPending | Outcome::ClaimNetworkError => {
                Action::ClaimAfter { ms: CLAIM_POLL_MS }
            }
            // The first claim after a code follows immediately.
            _ => Action::Claim,
        };
    }

    // No code yet. Get one, but only once the clock can sign it — and not
    // forever: the attempt has a failure cap.
    if i.pair_start_failures >= MAX_PAIR_START_FAILURES {
        return Action::GiveUp;
    }
    if !i.clock_ok {
        return Action::WaitForClock { ms: CLOCK_WAIT_MS };
    }
    match i.last {
        Outcome::PairStartFailed => Action::PairStartAfter { ms: PAIR_START_BACKOFF_MS },
        _ => Action::PairStart,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            has_code: false,
            clock_ok: true,
            window_elapsed_s: 0,
            pair_start_failures: 0,
            last: Outcome::None,
        }
    }

    #[test]
    fn an_unpaired_device_asks_for_a_code() {
        assert_eq!(decide(&base()), Action::PairStart);
    }

    #[test]
    fn a_code_older_than_the_window_is_reissued() {
        let i = Inputs {
            has_code: true,
            window_elapsed_s: PAIRING_TIMEOUT_S,
            last: Outcome::ClaimPending,
            ..base()
        };
        assert_eq!(decide(&i), Action::ReissueCode);
    }

    #[test]
    fn a_code_inside_the_window_is_still_claimed() {
        let i = Inputs {
            has_code: true,
            window_elapsed_s: PAIRING_TIMEOUT_S - 1,
            last: Outcome::ClaimPending,
            ..base()
        };
        assert_eq!(decide(&i), Action::ClaimAfter { ms: CLAIM_POLL_MS });
    }

    #[test]
    fn the_first_claim_after_a_code_follows_without_delay() {
        let i = Inputs { has_code: true, last: Outcome::PairStarted, ..base() };
        assert_eq!(decide(&i), Action::Claim);
    }

    #[test]
    fn a_pending_claim_polls_on_the_two_second_cadence() {
        let i = Inputs { has_code: true, last: Outcome::ClaimPending, ..base() };
        assert_eq!(decide(&i), Action::ClaimAfter { ms: CLAIM_POLL_MS });
    }

    #[test]
    fn a_claim_network_error_polls_on_the_same_cadence() {
        let i = Inputs { has_code: true, last: Outcome::ClaimNetworkError, ..base() };
        assert_eq!(decide(&i), Action::ClaimAfter { ms: CLAIM_POLL_MS });
    }

    #[test]
    fn a_rejected_claim_drops_the_code_and_backs_off() {
        // 401/429 mean the code is dead. Asking for a new one immediately is
        // what turns a rejection into a rate-limit loop: the server allows
        // five pair-starts per 300 s per IP.
        let i = Inputs { has_code: true, last: Outcome::ClaimRejected, ..base() };
        assert_eq!(
            decide(&i),
            Action::ReissueCodeWithBackoff { ms: CLAIM_ERROR_BACKOFF_MS }
        );
    }

    #[test]
    fn a_failed_pair_start_waits_before_trying_again() {
        let i = Inputs { last: Outcome::PairStartFailed, ..base() };
        assert_eq!(decide(&i), Action::PairStartAfter { ms: PAIR_START_BACKOFF_MS });
    }

    #[test]
    fn a_granted_claim_finishes() {
        let i = Inputs { has_code: true, last: Outcome::ClaimGranted, ..base() };
        assert_eq!(decide(&i), Action::Paired);
    }

    #[test]
    fn a_token_that_cannot_be_stored_ends_the_attempt() {
        // The server has issued the token and marked the device trusted; the
        // device cannot store it. Carrying on would leave the two sides
        // disagreeing about who is paired.
        let i = Inputs { has_code: true, last: Outcome::TokenWriteFailed, ..base() };
        assert_eq!(decide(&i), Action::GiveUp);
    }

    #[test]
    fn an_unsynced_clock_waits_instead_of_spending_the_quota() {
        let i = Inputs { clock_ok: false, ..base() };
        assert_eq!(decide(&i), Action::WaitForClock { ms: CLOCK_WAIT_MS });
    }

    #[test]
    fn an_unsynced_clock_never_reaches_pair_start() {
        // The 1970 timestamp the server must reject is the reason this exists.
        for failures in 0..MAX_PAIR_START_FAILURES {
            let i = Inputs { clock_ok: false, pair_start_failures: failures, ..base() };
            assert_ne!(decide(&i), Action::PairStart);
            assert_now(&decide(&i), failures);
        }
    }

    fn assert_now(a: &Action, failures: u32) {
        match a {
            Action::WaitForClock { .. } => assert!(failures < MAX_PAIR_START_FAILURES),
            Action::GiveUp => assert!(failures >= MAX_PAIR_START_FAILURES),
            other => panic!("unsynced clock decided {other:?}"),
        }
    }

    #[test]
    fn pair_start_failures_give_up_at_the_cap() {
        let i = Inputs { pair_start_failures: MAX_PAIR_START_FAILURES, ..base() };
        assert_eq!(decide(&i), Action::GiveUp);
    }

    #[test]
    fn the_clock_wait_gives_up_at_the_same_cap() {
        let i = Inputs {
            clock_ok: false,
            pair_start_failures: MAX_PAIR_START_FAILURES,
            ..base()
        };
        assert_eq!(decide(&i), Action::GiveUp);
    }

    #[test]
    fn one_failure_below_the_cap_keeps_trying() {
        let i = Inputs { pair_start_failures: MAX_PAIR_START_FAILURES - 1, ..base() };
        assert_eq!(decide(&i), Action::PairStart);
    }

    #[test]
    fn every_outcome_code_the_c_side_sends_is_recognised() {
        // The C header mirrors these numbers; a silent shift would make a
        // granted claim look like something else.
        assert_eq!(Outcome::from_code(0), Outcome::None);
        assert_eq!(Outcome::from_code(1), Outcome::ClockWaited);
        assert_eq!(Outcome::from_code(2), Outcome::PairStarted);
        assert_eq!(Outcome::from_code(3), Outcome::PairStartFailed);
        assert_eq!(Outcome::from_code(4), Outcome::ClaimPending);
        assert_eq!(Outcome::from_code(5), Outcome::ClaimGranted);
        assert_eq!(Outcome::from_code(6), Outcome::ClaimRejected);
        assert_eq!(Outcome::from_code(7), Outcome::ClaimNetworkError);
        assert_eq!(Outcome::from_code(8), Outcome::TokenWriteFailed);
    }
}

/// `rf_pairing_action_t` in `rust/include/pairing.h`. Keep in step.
pub const RF_PAIR_ACTION_PAIR_START: u8 = 0;
pub const RF_PAIR_ACTION_CLAIM: u8 = 1;
pub const RF_PAIR_ACTION_WAIT: u8 = 2;
pub const RF_PAIR_ACTION_PAIRED: u8 = 3;
pub const RF_PAIR_ACTION_GIVE_UP: u8 = 4;

#[repr(C)]
pub struct CInputs {
    pub has_code: u8,
    pub clock_ok: u8,
    pub _pad: [u8; 2],
    pub window_elapsed_s: u32,
    pub pair_start_failures: u32,
    pub last: u8,
    pub _pad2: [u8; 3],
}

#[repr(C)]
pub struct CDecision {
    /// One of the RF_PAIR_ACTION_* codes.
    pub action: u8,
    /// Drop the code on screen and restart the window.
    pub drop_code: u8,
    pub _pad: [u8; 2],
    /// Wait this long before performing the action.
    pub delay_ms: u32,
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_pairing_decide(inp: *const CInputs, out: *mut CDecision) {
    let i = unsafe { &*inp };
    let a = decide(&Inputs {
        has_code: i.has_code != 0,
        clock_ok: i.clock_ok != 0,
        window_elapsed_s: i.window_elapsed_s,
        pair_start_failures: i.pair_start_failures,
        last: Outcome::from_code(i.last),
    });
    let (action, drop_code, delay_ms) = match a {
        Action::PairStart => (RF_PAIR_ACTION_PAIR_START, 0, 0),
        Action::PairStartAfter { ms } => (RF_PAIR_ACTION_PAIR_START, 0, ms),
        Action::ReissueCode => (RF_PAIR_ACTION_PAIR_START, 1, 0),
        Action::ReissueCodeWithBackoff { ms } => (RF_PAIR_ACTION_PAIR_START, 1, ms),
        Action::WaitForClock { ms } => (RF_PAIR_ACTION_WAIT, 0, ms),
        Action::Claim => (RF_PAIR_ACTION_CLAIM, 0, 0),
        Action::ClaimAfter { ms } => (RF_PAIR_ACTION_CLAIM, 0, ms),
        Action::Paired => (RF_PAIR_ACTION_PAIRED, 0, 0),
        Action::GiveUp => (RF_PAIR_ACTION_GIVE_UP, 0, 0),
    };
    let d = unsafe { &mut *out };
    d.action = action;
    d.drop_code = drop_code;
    d.delay_ms = delay_ms;
}

#[cfg(test)]
mod ffi_tests {
    use super::*;

    fn outcome_code(o: Outcome) -> u8 {
        match o {
            Outcome::None => 0,
            Outcome::ClockWaited => 1,
            Outcome::PairStarted => 2,
            Outcome::PairStartFailed => 3,
            Outcome::ClaimPending => 4,
            Outcome::ClaimGranted => 5,
            Outcome::ClaimRejected => 6,
            Outcome::ClaimNetworkError => 7,
            Outcome::TokenWriteFailed => 8,
        }
    }

    fn base() -> Inputs {
        Inputs {
            has_code: false,
            clock_ok: true,
            window_elapsed_s: 0,
            pair_start_failures: 0,
            last: Outcome::None,
        }
    }

    fn call(i: &Inputs) -> CDecision {
        let ci = CInputs {
            has_code: i.has_code as u8,
            clock_ok: i.clock_ok as u8,
            _pad: [0; 2],
            window_elapsed_s: i.window_elapsed_s,
            pair_start_failures: i.pair_start_failures,
            last: outcome_code(i.last),
            _pad2: [0; 3],
        };
        let mut d = CDecision { action: 255, drop_code: 255, _pad: [0; 2], delay_ms: 0 };
        // SAFETY: both pointers are to locals that outlive the call.
        unsafe { rf_pairing_decide(&ci, &mut d) };
        d
    }

    #[test]
    fn the_c_side_sees_a_rejection_as_a_delayed_pair_start_that_drops_the_code() {
        // This triple is what the C loop acts on: ask again, drop the code,
        // wait 5 s. Getting any part wrong is the rate-limit loop.
        let d = call(&Inputs { has_code: true, last: Outcome::ClaimRejected, ..base() });
        assert_eq!(d.action, RF_PAIR_ACTION_PAIR_START);
        assert_eq!(d.drop_code, 1);
        assert_eq!(d.delay_ms, CLAIM_ERROR_BACKOFF_MS);
    }

    #[test]
    fn the_c_side_sees_a_pending_claim_as_a_delayed_claim() {
        let d = call(&Inputs { has_code: true, last: Outcome::ClaimPending, ..base() });
        assert_eq!(d.action, RF_PAIR_ACTION_CLAIM);
        assert_eq!(d.drop_code, 0);
        assert_eq!(d.delay_ms, CLAIM_POLL_MS);
    }

    #[test]
    fn the_c_side_is_told_to_wait_when_the_clock_cannot_sign() {
        let d = call(&Inputs { clock_ok: false, ..base() });
        assert_eq!(d.action, RF_PAIR_ACTION_WAIT);
        assert_eq!(d.delay_ms, CLOCK_WAIT_MS);
    }

    #[test]
    fn the_two_terminal_actions_are_distinguishable() {
        assert_eq!(
            call(&Inputs { has_code: true, last: Outcome::ClaimGranted, ..base() }).action,
            RF_PAIR_ACTION_PAIRED
        );
        assert_eq!(
            call(&Inputs { has_code: true, last: Outcome::TokenWriteFailed, ..base() }).action,
            RF_PAIR_ACTION_GIVE_UP
        );
    }

    #[test]
    fn a_code_within_its_window_is_claimed_without_delay() {
        let d = call(&Inputs { has_code: true, last: Outcome::PairStarted, ..base() });
        assert_eq!(d.action, RF_PAIR_ACTION_CLAIM);
        assert_eq!(d.delay_ms, 0);
    }
}
