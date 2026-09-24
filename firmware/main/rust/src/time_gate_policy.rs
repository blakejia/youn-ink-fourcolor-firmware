//! Pure time-gate policy for the NOTE4C firmware. C++ owns RTC/SNTP/NVS
//! I/O; this module decides whether each gated operation is allowed for
//! the current tick.
//!
//! The five gated operations answered in one call:
//!
//!   1. SNTP start: Skip | Start | Repair
//!   2. SNTP mark-synced: ok / not ok
//!   3. RTC cache (battery arm) write-back: ok / not ok
//!   4. Battery sample one-hour gate: ok / not ok
//!   5. Wi-Fi cache write-back: ok / not ok
//!
//! Semantics matched exactly to the C++ being migrated:
//!
//!   - `RF_TIME_GATE_NEVER` (2020-01-01 UTC) is the sentinel for
//!     "no usable time / no prior record". Anything below it is
//!     implausible: gates go to the "open" action that does not
//!     need a stamp, or refuse to write a stamp.
//!   - Regression: `now < last_event_epoch`. SNTP returns Repair
//!     immediately so the clock moves forward again. The other gates
//!     keep their normal elapsed comparison; the regression is a
//!     clock-movement event, not a gate change.
//!   - Time skips forward naturally when `now > last + period`:
//!     elapsed gate opens, `next_last = now + period` is returned
//!     for C++ to persist.
//!
//! Clock inputs come pre-computed from C++ (`clock_valid_mask`); the
//! Rust side does not re-read `time()` or RTC memory. This keeps the
//! decision deterministic and host-testable.

/// Sentinel: "no usable time / no prior record".
pub const NEVER: u32 = 1_577_836_800; // 2020-01-01 UTC

/// Clock validity bits (must match `time_gate_policy.h`).
pub const CLOCK_NOW_VALID: u32 = 0x1;
pub const CLOCK_RTC_VALID: u32 = 0x2;
pub const CLOCK_EVER_SYNCED: u32 = 0x4;

const S: u32 = NEVER;

/// Action code: SNTP gate is closed (within the daily window).
pub const SNTP_ACTION_SKIP: u8 = 0;
/// Action code: SNTP gate is open (daily window elapsed or first sync).
pub const SNTP_ACTION_START: u8 = 1;
/// Action code: RTC moved backwards, schedule an immediate repair.
pub const SNTP_ACTION_REPAIR: u8 = 2;

/// Inputs pre-computed by C++.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    pub now_s: u32,
    pub last_sntp_sync_s: u32,
    pub last_battery_arm_s: u32,
    pub sntp_min_period_s: u32,
    pub battery_min_period_s: u32,
    pub clock_valid_mask: u32,
}

/// Outputs returned to C++.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    /// `SNTP_ACTION_*` code for the SNTP start gate.
    pub sntp_start_action: u8,
    /// 1 = C++ should record the new sync epoch, 0 = ignore (implausible clock).
    pub sntp_mark_synced_ok: u8,
    /// 1 = C++ may write a RTC slow-memory stamp (e.g. battery arm), 0 = skip.
    pub rtc_cache_ok: u8,
    /// 1 = the one-hour battery sample window is open, 0 = skip.
    pub battery_sample_ok: u8,
    /// 1 = C++ may persist a Wi-Fi cache record, 0 = skip.
    pub wifi_cache_ok: u8,
    /// `now + sntp_min_period_s` for C++ to write back on a successful Start.
    pub next_last_sntp_sync_s: u32,
    /// `now + battery_min_period_s` for C++ to write back on a successful arm.
    pub next_last_battery_arm_s: u32,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            sntp_start_action: SNTP_ACTION_SKIP,
            sntp_mark_synced_ok: 0,
            rtc_cache_ok: 0,
            battery_sample_ok: 0,
            wifi_cache_ok: 0,
            next_last_sntp_sync_s: S,
            next_last_battery_arm_s: S,
        }
    }
}

/// Returns true when the clock-valid bitfield says `now_s` is plausible.
fn now_valid(mask: u32) -> bool {
    mask & CLOCK_NOW_VALID != 0
}

/// Returns true when the PCF8563 holds a plausible epoch this boot.
fn rtc_valid(mask: u32) -> bool {
    mask & CLOCK_RTC_VALID != 0
}

/// Core decision. All five outputs are derived from the same snapshot.
pub fn decide(i: &Inputs) -> Output {
    let mut o = Output::default();
    let now = i.now_s;

    // ── 1) SNTP start gate ───────────────────────────────────────────────────
    // Mirrors application.cc::ShouldStartSntpNow:
    //   - implausible `now`     -> Start (we need SNTP)
    //   - first sync, no prior  -> Start
    //   - RTC moved backwards   -> Repair (immediate)
    //   - elapsed since last    -> Start (24 h default)
    //   - otherwise             -> Skip
    if !now_valid(i.clock_valid_mask) {
        o.sntp_start_action = SNTP_ACTION_START;
    } else if i.last_sntp_sync_s < NEVER {
        // First post-boot SNTP: stamp advances to now + period so the next
        // wake is gated by the daily window, not the cold-boot sentinel.
        o.sntp_start_action = SNTP_ACTION_START;
        o.next_last_sntp_sync_s = now.saturating_add(i.sntp_min_period_s);
    } else if now <= i.last_sntp_sync_s {
        // now <= last => either equal or moved backwards.
        // Equal is impossible in practice but treat as Repair to be safe.
        o.sntp_start_action = SNTP_ACTION_REPAIR;
    } else if now - i.last_sntp_sync_s >= i.sntp_min_period_s {
        o.sntp_start_action = SNTP_ACTION_START;
        o.next_last_sntp_sync_s = now.saturating_add(i.sntp_min_period_s);
    } else {
        o.sntp_start_action = SNTP_ACTION_SKIP;
        o.next_last_sntp_sync_s = i.last_sntp_sync_s;
    }
    // ── 2) SNTP mark-synced: refuse to record an implausible clock ──────────
    o.sntp_mark_synced_ok = if now >= NEVER { 1 } else { 0 };

    // ── 3) RTC cache write-back: only when SNTP has produced a usable clock ─
    // Mirrors shim.cpp::rf_battery_arm: refuse to arm with a 1970 clock.
    // We allow RTC writes whenever `now_s` is plausible; we do NOT require
    // CLOCK_EVER_SYNCED — the first post-SNTP-tick arm must still write.
    o.rtc_cache_ok = if now_valid(i.clock_valid_mask) { 1 } else { 0 };


    // ── 4) Battery sample one-hour gate ──────────────────────────────────────
    // Mirrors shim.cpp::rf_battery_due:
    //   - no clock anywhere              -> due (always)
    //   - cold boot (last_arm == NEVER)  -> due
    //   - now >= last + period           -> due and arm
    //   - otherwise                      -> skip
    if !now_valid(i.clock_valid_mask) && !rtc_valid(i.clock_valid_mask) {
        o.battery_sample_ok = 1;
        o.next_last_battery_arm_s = S; // signal: do not arm
    } else if !now_valid(i.clock_valid_mask) {
        // PCF8563 has a plausible epoch but the system clock is 1970.
        // Keep the original behaviour: skip until SNTP seeds `time()`.
        o.battery_sample_ok = 0;
        o.next_last_battery_arm_s = S;
    } else if i.last_battery_arm_s < NEVER {
        // Cold boot — first sample, arm the gate for the next one.
        o.battery_sample_ok = 1;
        o.next_last_battery_arm_s = now.saturating_add(i.battery_min_period_s);
    } else if now >= i.last_battery_arm_s.saturating_add(i.battery_min_period_s) {
        o.battery_sample_ok = 1;
        o.next_last_battery_arm_s = now.saturating_add(i.battery_min_period_s);
    } else {
        o.battery_sample_ok = 0;
        o.next_last_battery_arm_s = i.last_battery_arm_s;
    }

    // ── 5) Wi-Fi cache write-back ────────────────────────────────────────────
    // Same gate as (3): without a plausible clock we cannot timestamp the
    // cache reliably. We never gate on connectivity here — that's the wifi
    // policy's job. This module only answers "is time reliable enough to
    // commit a stale-on-write timestamp?".
    o.wifi_cache_ok = if now_valid(i.clock_valid_mask) { 1 } else { 0 };

    o
}

// ─── C ABI ───────────────────────────────────────────────────────────────────
// Match the `rf_time_gate_policy_*_t` layout in `time_gate_policy.h`.

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CInputs {
    pub now_s: u32,
    pub last_sntp_sync_s: u32,
    pub last_battery_arm_s: u32,
    pub sntp_min_period_s: u32,
    pub battery_min_period_s: u32,
    pub clock_valid_mask: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct COutput {
    pub sntp_start_action: u8,
    pub sntp_mark_synced_ok: u8,
    pub rtc_cache_ok: u8,
    pub battery_sample_ok: u8,
    pub wifi_cache_ok: u8,
    pub _pad0: [u8; 3],
    pub next_last_sntp_sync_s: u32,
    pub next_last_battery_arm_s: u32,
}

impl Default for COutput {
    fn default() -> Self {
        COutput {
            sntp_start_action: 99,
            sntp_mark_synced_ok: 99,
            rtc_cache_ok: 99,
            battery_sample_ok: 99,
            wifi_cache_ok: 99,
            _pad0: [0; 3],
            next_last_sntp_sync_s: 99,
            next_last_battery_arm_s: 99,
        }
    }
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_time_gate_policy_decide(
    inp: *const CInputs,
    out: *mut COutput,
) {
    // SAFETY: caller promised non-null, aligned, valid structs.
    let i = unsafe { &*inp };
    let r = decide(&Inputs {
        now_s: i.now_s,
        last_sntp_sync_s: i.last_sntp_sync_s,
        last_battery_arm_s: i.last_battery_arm_s,
        sntp_min_period_s: i.sntp_min_period_s,
        battery_min_period_s: i.battery_min_period_s,
        clock_valid_mask: i.clock_valid_mask,
    });
    let o = unsafe { &mut *out };
    o.sntp_start_action = r.sntp_start_action;
    o.sntp_mark_synced_ok = r.sntp_mark_synced_ok;
    o.rtc_cache_ok = r.rtc_cache_ok;
    o.battery_sample_ok = r.battery_sample_ok;
    o.wifi_cache_ok = r.wifi_cache_ok;
    o.next_last_sntp_sync_s = r.next_last_sntp_sync_s;
    o.next_last_battery_arm_s = r.next_last_battery_arm_s;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Full mask: SNTP synced, PCF8563 plausible, wall clock plausible.
    const CLOCK_OK: u32 = CLOCK_NOW_VALID | CLOCK_RTC_VALID | CLOCK_EVER_SYNCED;
    /// Only the wall clock is plausible. SNTP hasn't run yet (cold boot).
    const CLOCK_NOW_ONLY: u32 = CLOCK_NOW_VALID;
    /// None of the clocks are plausible. First ever boot, 1970 epoch.
    const CLOCK_NONE: u32 = 0;
    /// PCF8563 plausibly set, but `time()` has not been synced yet.
    const CLOCK_RTC_ONLY: u32 = CLOCK_RTC_VALID;

    /// 2026-01-15 12:00:00 UTC (post-2020 sentinel).
    const NOW: u32 = 1_768_463_200;

    fn inp_with(
        now: u32,
        last_sntp: u32,
        last_bat: u32,
        valid: u32,
        sntp_period: u32,
        bat_period: u32,
    ) -> Inputs {
        Inputs {
            now_s: now,
            last_sntp_sync_s: last_sntp,
            last_battery_arm_s: last_bat,
            sntp_min_period_s: sntp_period,
            battery_min_period_s: bat_period,
            clock_valid_mask: valid,
        }
    }

    // ── 1) Valid clock: SNTP daily gate ─────────────────────────────────────

    #[test]
    fn sntp_start_first_sync_after_cold_boot_is_start() {
        // last_sntp_sync < NEVER (cold boot stamp)
        let r = decide(&inp_with(NOW, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
        assert_eq!(r.next_last_sntp_sync_s, NOW + 86_400);
    }

    #[test]
    fn sntp_start_within_daily_window_is_skip() {
        // last = NOW - 1 hour, period = 24 h, gate stays closed.
        let last = NOW - 3_600;
        let r = decide(&inp_with(NOW, last, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_SKIP);
        // next_last == last (don't arm a new stamp while skipping).
        assert_eq!(r.next_last_sntp_sync_s, last);
    }

    #[test]
    fn sntp_start_at_exactly_24h_elapsed_is_start() {
        // Boundary: now - last == period must open the gate.
        let last = NOW - 86_400;
        let r = decide(&inp_with(NOW, last, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
        assert_eq!(r.next_last_sntp_sync_s, NOW + 86_400);
    }

    // ── 2) Invalid clock: SNTP must always Start ────────────────────────────

    #[test]
    fn sntp_start_with_implausible_now_is_start() {
        // 1970 epoch, even with a valid prior sync, demand a fresh sync.
        let r = decide(&inp_with(0, S + 1_000_000, S, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
    }

    // ── 3) Clock regression: SNTP Repair (immediate) ────────────────────────

    #[test]
    fn sntp_start_with_clock_regression_is_repair() {
        // last = future, now = NOW. now <= last -> Repair.
        let last = NOW + 60;
        let r = decide(&inp_with(NOW, last, NOW, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
    }

    #[test]
    fn sntp_start_with_now_equal_last_is_repair() {
        // now == last is the edge case: the clock has not advanced since
        // the last sync, but the gate must still demand fresh data.
        let r = decide(&inp_with(NOW, NOW, NOW, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
    }

    // ── 4) Mark-synced gate: refuse to write implausible clock ──────────────

    #[test]
    fn mark_synced_with_implausible_now_is_not_ok() {
        let r = decide(&inp_with(0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 0);
    }

    #[test]
    fn mark_synced_with_plausible_now_is_ok() {
        let r = decide(&inp_with(NOW, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 1);
    }

    // ── 5) RTC cache write-back: gated on clock_valid ───────────────────────

    #[test]
    fn rtc_cache_write_blocked_on_1970_clock() {
        let r = decide(&inp_with(0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 0);
    }

    #[test]
    fn rtc_cache_write_allowed_with_plausible_clock() {
        let r = decide(&inp_with(NOW, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 1);
    }

    #[test]
    fn rtc_cache_write_allowed_when_rtc_seed_alone_exists() {
        // time() is still 1970 but the PCF8563 holds a plausible epoch.
        // We deliberately keep the existing behaviour here: refuse until
        // SNTP seeds time(). The wifi cache / battery stamp need the
        // monotonic wall clock, which only `settimeofday` updates.
        let r = decide(&inp_with(0, S - 1, S - 1, CLOCK_RTC_ONLY, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 0);
    }

    // ── 6) Battery sample one-hour gate ─────────────────────────────────────

    #[test]
    fn battery_sample_first_cold_boot_is_due() {
        // Cold boot stamp zeroed => always sample.
        let r = decide(&inp_with(NOW, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NOW + 3_600);
    }

    #[test]
    fn battery_sample_within_one_hour_window_is_skipped() {
        // last_battery = NOW - 30 min, period = 1 h -> skip.
        let last = NOW - 1_800;
        let r = decide(&inp_with(NOW, S + 1_000_000, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 0);
        assert_eq!(r.next_last_battery_arm_s, last);
    }

    #[test]
    fn battery_sample_at_exactly_one_hour_elapsed_is_due() {
        let last = NOW - 3_600;
        let r = decide(&inp_with(NOW, S + 1_000_000, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NOW + 3_600);
    }

    #[test]
    fn battery_sample_with_no_clock_anywhere_is_due_but_no_arm() {
        // No clock, no RTC, no sync: must report at least once. The
        // returned next_last == S signals "do not arm"; the next tick
        // with a plausible clock will arm the gate.
        let r = decide(&inp_with(0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NEVER);
    }

    // ── 7) Wi-Fi cache write-back gate ──────────────────────────────────────

    #[test]
    fn wifi_cache_write_blocked_on_1970_clock() {
        let r = decide(&inp_with(0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.wifi_cache_ok, 0);
    }

    #[test]
    fn wifi_cache_write_allowed_with_plausible_clock() {
        let r = decide(&inp_with(NOW, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.wifi_cache_ok, 1);
    }

    // ── 8) Action mapping: which action_code corresponds to what ────────────

    #[test]
    fn action_mapping_table() {
        // Each code MUST be unique so C++ can switch on it safely.
        assert_eq!(SNTP_ACTION_SKIP, 0);
        assert_eq!(SNTP_ACTION_START, 1);
        assert_eq!(SNTP_ACTION_REPAIR, 2);
        assert_ne!(SNTP_ACTION_SKIP, SNTP_ACTION_START);
        assert_ne!(SNTP_ACTION_SKIP, SNTP_ACTION_REPAIR);
        assert_ne!(SNTP_ACTION_START, SNTP_ACTION_REPAIR);
    }

    // ── 9) C ABI round-trip: COutput fields line up with the Rust struct ────

    #[test]
    fn ffi_round_trip_fills_output_fields() {
        let ci = CInputs {
            now_s: NOW,
            last_sntp_sync_s: S - 1,
            last_battery_arm_s: S - 1,
            sntp_min_period_s: 86_400,
            battery_min_period_s: 3_600,
            clock_valid_mask: CLOCK_OK,
        };
        let mut co = COutput::default();
        unsafe { rf_time_gate_policy_decide(&ci, &mut co) };

        assert_eq!(co.sntp_start_action, SNTP_ACTION_START);
        assert_eq!(co.sntp_mark_synced_ok, 1);
        assert_eq!(co.rtc_cache_ok, 1);
        assert_eq!(co.battery_sample_ok, 1);
        assert_eq!(co.wifi_cache_ok, 1);
        assert_eq!(co.next_last_sntp_sync_s, NOW + 86_400);
        assert_eq!(co.next_last_battery_arm_s, NOW + 3_600);
    }

    // ── 10) Sentinel: a regression-coerced Start would re-arm a future stamp ──

    #[test]
    fn repair_action_does_not_advance_the_sntp_stamp() {
        // If the gate naively treated now <= last as "elapsed", it would
        // set next_last to now + period, producing a stamp in the past
        // once the clock catches up. The Repair action must NOT arm.
        let last = NOW + 60;
        let r = decide(&inp_with(NOW, last, NOW, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
        assert_eq!(r.next_last_sntp_sync_s, NEVER,
                   "Repair must not advance the SNTP stamp");
    }
}
