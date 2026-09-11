//! Lifecycle transition legality.
//!
//! `Application::TransitionLifecycle` is a publish channel, not a controller:
//! fifteen call sites each announce the phase their module just entered, and no
//! call site consults the current state before doing so. There is therefore no
//! transition *policy* to move — what was missing is a contract on what counts
//! as a legal announcement, so a wrong one is visible instead of becoming a
//! device that behaves oddly for reasons nobody can see.
//!
//! What this deliberately does NOT catch: a lifecycle that never moves. The bug
//! this codebase actually had — a device stuck in WifiConnecting after pairing
//! was skipped, so it never slept — was a *missing* transition, and no legality
//! table can see one that was never written. That is a liveness question and
//! needs a watchdog, not a verdict.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Unknown = 0,
    Boot = 1,
    WifiConnecting = 2,
    ApProvision = 3,
    PairStart = 4,
    PairWaitCode = 5,
    SyncIdle = 6,
    Sleep = 7,
    Error = 8,
}

impl Lifecycle {
    pub fn from_code(code: u8) -> Lifecycle {
        match code {
            1 => Lifecycle::Boot,
            2 => Lifecycle::WifiConnecting,
            3 => Lifecycle::ApProvision,
            4 => Lifecycle::PairStart,
            5 => Lifecycle::PairWaitCode,
            6 => Lifecycle::SyncIdle,
            7 => Lifecycle::Sleep,
            8 => Lifecycle::Error,
            _ => Lifecycle::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same state. The caller already drops these; nothing to report.
    NoChange,
    Ok,
    /// Legal by the table but telling: the code did something the device did not
    /// intend. Reported once, in the log, and execution continues — a lifecycle
    /// announcement is not worth halting a device over.
    Suspicious(&'static core::ffi::CStr),
}

pub fn verdict(from: Lifecycle, to: Lifecycle) -> Verdict {
    if from == to {
        return Verdict::NoChange;
    }
    // An error is worth reporting wherever it comes from.
    if to == Lifecycle::Error {
        return Verdict::Ok;
    }
    if from == Lifecycle::Unknown {
        return if to == Lifecycle::Boot {
            Verdict::Ok
        } else {
            Verdict::Suspicious(c"a phase was announced before init; every reporter runs after Boot")
        };
    }
    if to == Lifecycle::Boot {
        return Verdict::Suspicious(c"Boot is the init transition; it happens once");
    }
    if from == Lifecycle::Sleep {
        return Verdict::Suspicious(c"nothing transitions out of Sleep: after that announcement the device is off, so something still running means the sleep did not take");
    }
    Verdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_transition_is_boot() {
        assert_eq!(verdict(Lifecycle::Unknown, Lifecycle::Boot), Verdict::Ok);
    }

    #[test]
    fn a_transition_before_init_is_suspicious() {
        // Everything that reports runs after `Initialize()` announced Boot, so
        // an earlier announcement means a module is describing a phase the
        // device never entered.
        assert!(matches!(
            verdict(Lifecycle::Unknown, Lifecycle::WifiConnecting),
            Verdict::Suspicious(_)
        ));
    }

    #[test]
    fn nothing_leaves_sleep() {
        // The only thing after announcing Sleep is esp_deep_sleep_start(). If
        // something still runs, the sleep did not take.
        assert!(matches!(
            verdict(Lifecycle::Sleep, Lifecycle::SyncIdle),
            Verdict::Suspicious(_)
        ));
    }

    #[test]
    fn boot_happens_once() {
        assert!(matches!(
            verdict(Lifecycle::WifiConnecting, Lifecycle::Boot),
            Verdict::Suspicious(_)
        ));
    }

    #[test]
    fn repeating_a_state_is_not_a_change() {
        assert_eq!(verdict(Lifecycle::SyncIdle, Lifecycle::SyncIdle), Verdict::NoChange);
    }

    #[test]
    fn the_plain_boot_path_is_clean() {
        let path = [
            Lifecycle::Unknown,
            Lifecycle::Boot,
            Lifecycle::WifiConnecting,
            Lifecycle::SyncIdle,
            Lifecycle::Sleep,
        ];
        for pair in path.windows(2) {
            assert_eq!(verdict(pair[0], pair[1]), Verdict::Ok);
        }
    }

    #[test]
    fn the_pairing_path_is_clean() {
        let path = [
            Lifecycle::WifiConnecting,
            Lifecycle::PairStart,
            Lifecycle::PairWaitCode,
            Lifecycle::SyncIdle,
        ];
        for pair in path.windows(2) {
            assert_eq!(verdict(pair[0], pair[1]), Verdict::Ok);
        }
    }

    #[test]
    fn the_provisioning_path_is_clean() {
        let path = [
            Lifecycle::WifiConnecting,
            Lifecycle::ApProvision,
            Lifecycle::WifiConnecting,
            Lifecycle::PairStart,
        ];
        for pair in path.windows(2) {
            assert_eq!(verdict(pair[0], pair[1]), Verdict::Ok);
        }
    }

    #[test]
    fn the_failed_pairing_retry_is_clean() {
        // Error -> PairStart is deliberately legal: after the three attempts
        // fail, the dedup latch is cleared so a later WiFi reconnect may try
        // again, and that retry announces PairStart from Error.
        assert_eq!(verdict(Lifecycle::PairWaitCode, Lifecycle::Error), Verdict::Ok);
        assert_eq!(verdict(Lifecycle::Error, Lifecycle::PairStart), Verdict::Ok);
    }

    #[test]
    fn an_error_can_arrive_from_anywhere() {
        for from in [Lifecycle::Boot, Lifecycle::WifiConnecting, Lifecycle::PairStart, Lifecycle::SyncIdle] {
            assert_eq!(verdict(from, Lifecycle::Error), Verdict::Ok);
        }
    }

    #[test]
    fn every_lifecycle_code_the_c_side_sends_is_recognised() {
        assert_eq!(Lifecycle::from_code(0), Lifecycle::Unknown);
        assert_eq!(Lifecycle::from_code(1), Lifecycle::Boot);
        assert_eq!(Lifecycle::from_code(2), Lifecycle::WifiConnecting);
        assert_eq!(Lifecycle::from_code(3), Lifecycle::ApProvision);
        assert_eq!(Lifecycle::from_code(4), Lifecycle::PairStart);
        assert_eq!(Lifecycle::from_code(5), Lifecycle::PairWaitCode);
        assert_eq!(Lifecycle::from_code(6), Lifecycle::SyncIdle);
        assert_eq!(Lifecycle::from_code(7), Lifecycle::Sleep);
        assert_eq!(Lifecycle::from_code(8), Lifecycle::Error);
    }
}

// ─────────────────────────── C boundary ───────────────────────────

/// `rf_lifecycle_verdict_t.kind` in `rust/include/lifecycle.h`.
pub const RF_LIFECYCLE_NO_CHANGE: u8 = 0;
pub const RF_LIFECYCLE_OK: u8 = 1;
pub const RF_LIFECYCLE_SUSPICIOUS: u8 = 2;

#[repr(C)]
pub struct CVerdict {
    pub kind: u8,
    pub _pad: [u8; 7],
    /// Suspicious only: a NUL-terminated static string, null otherwise.
    pub message: *const core::ffi::c_char,
}

/// # Safety
/// `out` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_lifecycle_verdict(from: u8, to: u8, out: *mut CVerdict) {
    let v = verdict(Lifecycle::from_code(from), Lifecycle::from_code(to));
    let d = unsafe { &mut *out };
    match v {
        Verdict::NoChange => {
            d.kind = RF_LIFECYCLE_NO_CHANGE;
            d.message = core::ptr::null();
        }
        Verdict::Ok => {
            d.kind = RF_LIFECYCLE_OK;
            d.message = core::ptr::null();
        }
        Verdict::Suspicious(msg) => {
            d.kind = RF_LIFECYCLE_SUSPICIOUS;
            d.message = msg.as_ptr();
        }
    }
}

#[cfg(test)]
mod ffi_tests {
    use super::*;

    fn call(from: u8, to: u8) -> CVerdict {
        let mut d = CVerdict { kind: 255, _pad: [0; 7], message: 1 as *const _ };
        // SAFETY: `d` outlives the call.
        unsafe { rf_lifecycle_verdict(from, to, &mut d) };
        d
    }

    #[test]
    fn the_c_side_gets_a_message_exactly_when_it_is_suspicious() {
        let d = call(7, 6); // Sleep -> SyncIdle
        assert_eq!(d.kind, RF_LIFECYCLE_SUSPICIOUS);
        assert!(!d.message.is_null());
        let msg = unsafe { core::ffi::CStr::from_ptr(d.message) };
        assert!(msg.to_str().unwrap().contains("off"));

        let d = call(1, 2); // Boot -> WifiConnecting
        assert_eq!(d.kind, RF_LIFECYCLE_OK);
        assert!(d.message.is_null());
    }

    #[test]
    fn the_c_side_can_tell_a_repeat_from_a_real_transition() {
        assert_eq!(call(6, 6).kind, RF_LIFECYCLE_NO_CHANGE);
        assert_ne!(call(6, 7).kind, RF_LIFECYCLE_NO_CHANGE);
    }
}
