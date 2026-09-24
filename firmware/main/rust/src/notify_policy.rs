//! Notification pull/rate policy for the NOTE4C firmware. C++ owns HTTP
//! transport, response buffers, file writes, the fetch/ack task lifecycle and
//! the panel; this module only decides pull/defer/dedup/retry/consume from
//! facts C++ already holds.
//!
//! Compatibility port of the `notify.rs` decision table — not a redesign:
//!
//! - pull gate mirrors `request_next` (`state != IDLE -> return`) plus the
//!   RunPowerCycle "already fetching / already showing" skips;
//! - response classes mirror `fetch_once`'s `match status` on the `.bin`
//!   path (200 binary / 204 empty / 404 JSON fallback / other error) and
//!   `handle_next`'s 200/204/other split on the JSON path;
//! - transport errors (negative status) were failures in both paths;
//! - duplicate ids were never re-shown: `handle_next`/`handle_binary_next`
//!   only reach NOTIFYING through a fresh parse, and a re-offered id must
//!   skip instead of repainting.
//!
//! New gates (rate limit + failure backoff) are additive: defaults keep
//! today's unconditional-fetch behaviour, and the unset-clock rule
//! (`now_s < 0 -> skip time gates`) preserves the cold-boot fetch.

/// Notify states, mirroring `notify.rs` (`IDLE`/`FETCHING`/`NOTIFYING`).
pub const NOTIFY_STATE_IDLE: u8 = 0;
pub const NOTIFY_STATE_FETCHING: u8 = 1;
pub const NOTIFY_STATE_NOTIFYING: u8 = 2;

/// Pull gate outcomes (`rf_notify_pull_action_t` in `notify_policy.h`).
pub const PULL_ACTION_PULL: u8 = 0;
pub const PULL_ACTION_DEFER_BUSY: u8 = 1;
pub const PULL_ACTION_SKIP_NOT_IDLE: u8 = 2;
pub const PULL_ACTION_BACKOFF: u8 = 3;

/// Response classes (`rf_notify_response_class_t`).
pub const RESPONSE_BINARY_NOTIFY: u8 = 0;
pub const RESPONSE_JSON_NOTIFY: u8 = 1;
pub const RESPONSE_EMPTY: u8 = 2;
pub const RESPONSE_JSON_FALLBACK: u8 = 3;
pub const RESPONSE_TRANSPORT_ERROR: u8 = 4;
pub const RESPONSE_STATUS_ERROR: u8 = 5;

/// Consume outcomes (`rf_notify_consume_action_t`).
pub const CONSUME_SHOW: u8 = 0;
pub const CONSUME_SKIP_DUPLICATE: u8 = 1;
pub const CONSUME_RETRY_BACKOFF: u8 = 2;
pub const CONSUME_NONE_EMPTY: u8 = 3;
pub const CONSUME_FETCH_JSON_FALLBACK: u8 = 4;

/// Facts for the pull gate. `#[repr(C)]` + explicit padding pins the layout
/// against `rf_notify_pull_inputs_t` in `rust/include/notify_policy.h`; a
/// layout contract test asserts the offsets and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct PullInputs {
    /// Notify state (`NOTIFY_STATE_*`).
    pub state: u8,
    /// Panel refresh in flight (EPD wave pending); defer instead of pulling.
    pub epd_busy: bool,
    /// Padding to align `now_s` on an 8-byte boundary.
    pub _pad: [u8; 6],
    /// Current wall clock in seconds; negative = unset (skip time gates).
    pub now_s: i64,
    /// Last pull attempt in seconds; negative = none yet.
    pub last_pull_s: i64,
    /// Last failed pull in seconds; negative = none yet.
    pub last_failure_s: i64,
    /// Consecutive failure count (C++ persists; see `record_result`).
    pub fail_streak: u32,
    /// Minimum seconds between pulls (0 = disabled).
    pub min_interval_s: u32,
    /// Backoff base in seconds (`streak 1 -> base`).
    pub base_backoff_s: u32,
    /// Backoff ceiling in seconds.
    pub max_backoff_s: u32,
}

/// Pull gate result. `wait_s` is the seconds to wait before retrying when
/// `action == PULL_ACTION_BACKOFF`; 0 otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct PullOutput {
    pub action: u8,
    pub _pad: [u8; 3],
    pub wait_s: u32,
}

/// Facts for response classification: the HTTP status C++ already has plus
/// which endpoint it came from. Negative `status` = transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ResponseFacts {
    pub status: i32,
    pub binary_path: bool,
    pub _pad: [u8; 3],
}

/// Pull/defer/dedup gate. Pure: no I/O, no globals.
pub fn decide_pull(i: &PullInputs) -> PullOutput {
    let out = |action: u8, wait_s: u32| PullOutput { action, _pad: [0; 3], wait_s };
    // `request_next` returns unless IDLE; a fetch already in flight or a
    // notification on screen never starts another pull.
    if i.state != NOTIFY_STATE_IDLE {
        return out(PULL_ACTION_SKIP_NOT_IDLE, 0);
    }
    // A refresh wave owns the panel for 15-25 s; the GET + show would fight
    // it. Defer and let the caller re-arm instead of pulling now.
    if i.epd_busy {
        return out(PULL_ACTION_DEFER_BUSY, 0);
    }
    // Cold boot before the first sync: no wall clock to gate on. Today's
    // fetch is unconditional, so allow the pull rather than wedging behind
    // a time comparison against an unset clock.
    if i.now_s < 0 {
        return out(PULL_ACTION_PULL, 0);
    }
    // Failure backoff first: its window dominates the rate limit.
    if i.fail_streak > 0 && i.last_failure_s >= 0 {
        let window = backoff_delay_s(i.fail_streak, i.base_backoff_s, i.max_backoff_s);
        let elapsed = (i.now_s.saturating_sub(i.last_failure_s).max(0)) as u64;
        if elapsed < window as u64 {
            return out(PULL_ACTION_BACKOFF, (window as u64 - elapsed) as u32);
        }
    }
    // Minimum spacing between pulls.
    if i.last_pull_s >= 0 && i.min_interval_s > 0 {
        let elapsed = (i.now_s.saturating_sub(i.last_pull_s).max(0)) as u64;
        if elapsed < i.min_interval_s as u64 {
            return out(PULL_ACTION_BACKOFF, (i.min_interval_s as u64 - elapsed) as u32);
        }
    }
    out(PULL_ACTION_PULL, 0)
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

/// Next failure streak after one pull outcome. C++ persists the return.
/// Success resets; failure increments (saturating, never wraps to 0).
pub fn record_result(ok: bool, streak: u32) -> u32 {
    if ok {
        0
    } else {
        streak.saturating_add(1)
    }
}

/// Classify a `/next` response from the transport/status facts C++ already
/// holds. Mirrors `fetch_once` on the `.bin` path (200 binary / 204 empty /
/// 404 JSON fallback / other error) and `handle_next` on the JSON path
/// (200 parsed / 204 empty / other error); negative status was a failure in
/// both. No body inspection here — C++ parses, Rust classifies.
pub fn classify_response(f: &ResponseFacts) -> u8 {
    if f.status < 0 {
        return RESPONSE_TRANSPORT_ERROR;
    }
    match f.status {
        200 => {
            if f.binary_path {
                RESPONSE_BINARY_NOTIFY
            } else {
                RESPONSE_JSON_NOTIFY
            }
        }
        204 => RESPONSE_EMPTY,
        // Old server without /next.bin: the JSON endpoint keeps a newer
        // firmware working (rolling deploy). A 404 on the JSON path itself
        // is a plain error.
        404 => {
            if f.binary_path {
                RESPONSE_JSON_FALLBACK
            } else {
                RESPONSE_STATUS_ERROR
            }
        }
        _ => RESPONSE_STATUS_ERROR,
    }
}

/// Consume decision for a classified response: whether C++ shows the parsed
/// body, skips it as a duplicate, retries with backoff, or runs the JSON
/// fallback GET. `parsed` says C++ decoded a usable bitmap+id; `duplicate`
/// says the id/etag matches the already-consumed notification.
pub fn decide_consume(class: u8, parsed: bool, duplicate: bool) -> u8 {
    match class {
        RESPONSE_BINARY_NOTIFY | RESPONSE_JSON_NOTIFY => {
            if !parsed {
                CONSUME_RETRY_BACKOFF
            } else if duplicate {
                CONSUME_SKIP_DUPLICATE
            } else {
                CONSUME_SHOW
            }
        }
        RESPONSE_EMPTY => CONSUME_NONE_EMPTY,
        RESPONSE_JSON_FALLBACK => CONSUME_FETCH_JSON_FALLBACK,
        _ => CONSUME_RETRY_BACKOFF,
    }
}

// ─── C ABI ─────────────────────────────────────────────────────────────────
// Matches `rf_notify_*_t` and the `RF_NOTIFY_*` codes in `notify_policy.h`.

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_notify_pull_decide(inp: *const PullInputs, out: *mut PullOutput) {
    let r = decide_pull(unsafe { &*inp });
    unsafe { *out = r };
}

/// # Safety
/// `f` must point to a valid `ResponseFacts`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_notify_classify_response(f: *const ResponseFacts) -> u8 {
    classify_response(unsafe { &*f })
}

/// `parsed`/`duplicate` are 0/1 flags (C `uint8_t`).
#[unsafe(no_mangle)]
pub extern "C" fn rf_notify_decide_consume(class: u8, parsed: u8, duplicate: u8) -> u8 {
    decide_consume(class, parsed != 0, duplicate != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn rf_notify_backoff_delay_s(streak: u32, base_s: u32, max_s: u32) -> u32 {
    backoff_delay_s(streak, base_s, max_s)
}

/// `ok` is a 0/1 flag. Returns the streak C++ should persist.
#[unsafe(no_mangle)]
pub extern "C" fn rf_notify_record_result(ok: u8, streak: u32) -> u32 {
    record_result(ok != 0, streak)
}
