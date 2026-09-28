//! Log-upload gate. Pure: no I/O, no globals — C++ gathers the facts (both
//! switch opinions, ring occupancy, link state, RTC failure stamps) and this
//! decides whether to upload and how many bytes.
//!
//! Backoff reuses the shape already in `notify_policy` (`base * 2^(n-1)`,
//! capped) rather than inventing a second convention.

/// Single-upload cap. Bounded by the deep-sleep cycle's HTTP budget and by the
/// fixed request buffer on the C++ side.
pub const MAX_UPLOAD_BYTES: u32 = 1024;

/// Both switches are three-state. "No opinion" is not "off": it is precisely
/// what lets the other side decide.
pub const OPINION_NONE: u8 = 0;
pub const OPINION_OFF: u8 = 1;
pub const OPINION_ON: u8 = 2;

pub const UPLOAD: u8 = 0;
pub const SKIP_DISABLED: u8 = 1;
pub const SKIP_EMPTY: u8 = 2;
pub const SKIP_NO_NET: u8 = 3;
pub const SKIP_BACKOFF: u8 = 4;

pub const BASE_BACKOFF_S: u32 = 60;
pub const MAX_BACKOFF_S: u32 = 900;

/// Facts the C++ side already holds: the two switch opinions, ring occupancy,
/// link state and the RTC failure stamps. `#[repr(C)]` + explicit padding pins
/// the layout against `rf_log_upload_inputs_t` in
/// `rust/include/log_upload_policy.h`; a layout contract test asserts the
/// offsets and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Inputs {
    pub local_set: u8,
    pub server_set: u8,
    pub has_pending: u8,
    pub wifi_ready: u8,
    pub pending_bytes: u32,
    pub pending_lines: u32,
    pub fail_streak: u32,
    pub _pad: [u8; 4],
    pub last_fail_s: i64,
    pub now_s: i64,
}

/// The gate's answer. `max_bytes` is 0 for every skip; on `UPLOAD` it is
/// `min(pending_bytes, MAX_UPLOAD_BYTES)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Decision {
    pub action: u8,
    pub _pad: [u8; 3],
    pub max_bytes: u32,
}

/// The conflict rule, in one place: an explicit service opinion wins,
/// otherwise an explicit local one, otherwise off. The service is the
/// authority (it is the side an operator reaches remotely); the local switch
/// is the device holder's own consent, honoured when the service is silent.
pub fn resolve_enabled(local_set: u8, server_set: u8) -> bool {
    if server_set == OPINION_ON {
        return true;
    }
    if server_set == OPINION_OFF {
        return false;
    }
    local_set == OPINION_ON
}

/// Backoff window for a failure streak: `base * 2^(streak-1)`, capped at
/// `max`. Streak 0 means "no failure outstanding" — no wait.
pub fn backoff_delay_s(streak: u32, base_s: u32, max_s: u32) -> u32 {
    if streak == 0 {
        return 0;
    }
    let shift = streak.saturating_sub(1).min(31);
    let delay = (base_s as u64).saturating_mul(1u64 << shift);
    core::cmp::min(delay, max_s as u64) as u32
}

/// Whether to upload this wake, and how many bytes to take. Pure: no I/O,
/// no globals.
pub fn decide(i: &Inputs) -> Decision {
    let out = |action: u8, max_bytes: u32| Decision { action, _pad: [0; 3], max_bytes };
    // Composed switch first: a disabled device must not even look at the
    // buffer. Both sides silent resolves to false (off by default).
    if !resolve_enabled(i.local_set, i.server_set) {
        return out(SKIP_DISABLED, 0);
    }
    if i.has_pending == 0 || i.pending_bytes == 0 {
        return out(SKIP_EMPTY, 0);
    }
    if i.wifi_ready == 0 {
        return out(SKIP_NO_NET, 0);
    }
    // Unset clock (cold boot): skip the gate rather than compare against a
    // bogus stamp and wedge until SNTP lands.
    if i.now_s >= 0 && i.fail_streak > 0 && i.last_fail_s >= 0 {
        let window = backoff_delay_s(i.fail_streak, BASE_BACKOFF_S, MAX_BACKOFF_S);
        let elapsed = i.now_s.saturating_sub(i.last_fail_s).max(0) as u64;
        if elapsed < window as u64 {
            return out(SKIP_BACKOFF, 0);
        }
    }
    out(UPLOAD, i.pending_bytes.min(MAX_UPLOAD_BYTES))
}

// ─── C ABI ─────────────────────────────────────────────────────────────────
// Matches `rf_log_upload_*_t` and the `RF_LOG_*` codes in log_upload_policy.h.

/// Returns 1 when the composed switch is on, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn rf_log_upload_resolve(local_set: u8, server_set: u8) -> u8 {
    resolve_enabled(local_set, server_set) as u8
}

/// # Safety
/// `inp` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_log_upload_decide(inp: *const Inputs) -> Decision {
    decide(unsafe { &*inp })
}

#[unsafe(no_mangle)]
pub extern "C" fn rf_log_upload_backoff_s(streak: u32, base_s: u32, max_s: u32) -> u32 {
    backoff_delay_s(streak, base_s, max_s)
}
