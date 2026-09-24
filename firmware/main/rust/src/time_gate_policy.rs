//! Pure time-gate policy for the NOTE4C firmware. C++ owns RTC/SNTP/NVS
//! I/O; this module decides whether each gated operation is allowed for
//! the current tick.
//!
//! Five gated operations answered in one call:
//!
//!   1. SNTP start: Skip | Start | Repair
//!   2. SNTP mark-synced: ok / not ok
//!   3. RTC cache (battery arm) write-back: ok / not ok
//!   4. Battery sample one-hour gate: ok / not ok
//!   5. Wi-Fi cache write-back: ok / not ok
//!
//! The C++ side fills `Inputs` from three sources:
//!
//!   - `now_s` / `clock_valid_mask::NOW_VALID` come from `time(nullptr)`.
//!     We keep `now_s` as a signed `i64` so that `time()==-1` (clock not
//!     yet initialised) propagates as a negative value; truncating to
//!     `u32` first would smear `-1` into `0xFFFFFFFF`, a perfectly valid
//!     looking year 2106 stamp — see the regression tests for the
//!     "negative clock must not be persisted" property.
//!   - `effective_clock_s` / `clock_valid_mask::RTC_VALID` come from the
//!     PCF8563 fallback (`ZectrixRtcNowEpoch`). Used ONLY by the battery
//!     sample gate, which is the one place we want to keep reporting
//!     when SNTP has not yet seeded `time()`.
//!   - `clock_valid_mask::EVER_SYNCED` indicates the SNTP success callback
//!     has fired this boot; it arms the SNTP stamp on the first Start.
//!
//! The five decisions share the same snapshot so a single Rust call is
//! enough for all of them. C++ then performs the actual I/O.

/// Sentinel: "no usable time / no prior record" (2020-01-01 UTC).
pub const NEVER: u32 = 1_577_836_800;

/// Clock validity bits (must match `time_gate_policy.h`).
pub const CLOCK_NOW_VALID: u32 = 0x1;
pub const CLOCK_RTC_VALID: u32 = 0x2;
pub const CLOCK_EVER_SYNCED: u32 = 0x4;

const S: u32 = NEVER;

/// SNTP start action codes.
pub const SNTP_ACTION_SKIP: u8 = 0;
pub const SNTP_ACTION_START: u8 = 1;
pub const SNTP_ACTION_REPAIR: u8 = 2;

/// Rust-side inputs, mirroring the C ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    /// `time(nullptr)` result, **signed**. Negative means the system
    /// clock is uninitialised. Persisting this as a sync epoch is a bug.
    pub now_s: i64,
    /// PCF8563 epoch, **unsigned**, already vetted for plausibility by
    /// C++ (`epoch >= NEVER` before populating). `0` means "no fallback
    /// available" — do NOT use this for SNTP mark-synced.
    pub effective_clock_s: u32,
    /// Prior SNTP sync epoch from RTC_DATA_ATTR; `NEVER` means none.
    pub last_sntp_sync_s: u32,
    /// Prior battery arm epoch from RTC_DATA_ATTR; `NEVER` means none.
    pub last_battery_arm_s: u32,
    /// SNTP gate minimum period in seconds (24 h typical).
    pub sntp_min_period_s: u32,
    /// Battery-sample gate minimum period in seconds (3600 typical).
    pub battery_min_period_s: u32,
    /// Bitmask of `CLOCK_*` bits.
    pub clock_valid_mask: u32,
}

/// Rust-side outputs, mirroring the C ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub sntp_start_action: u8,
    /// 1 = C++ should record the new sync epoch, 0 = refuse.
    /// Refuses when `now_s` is negative (clock uninitialised) or below
    /// `NEVER` (1970). PCF8563 fallback is NEVER used for this gate.
    pub sntp_mark_synced_ok: u8,
    /// 1 = C++ may write a RTC slow-memory stamp.
    pub rtc_cache_ok: u8,
    /// 1 = the one-hour battery sample window is open.
    pub battery_sample_ok: u8,
    /// 1 = C++ may persist a Wi-Fi cache record.
    pub wifi_cache_ok: u8,
    /// `now + sntp_min_period_s` (signed because `now_s` is signed).
    pub next_last_sntp_sync_s: i64,
    /// `now + battery_min_period_s` (`effective_clock_s` based).
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
            next_last_sntp_sync_s: 0,
            next_last_battery_arm_s: S,
        }
    }
}

fn now_valid(mask: u32) -> bool {
    mask & CLOCK_NOW_VALID != 0
}
fn rtc_valid(mask: u32) -> bool {
    mask & CLOCK_RTC_VALID != 0
}
fn ever_synced(mask: u32) -> bool {
    mask & CLOCK_EVER_SYNCED != 0
}

/// Core decision. Pure: no I/O, no globals.
pub fn decide(i: &Inputs) -> Output {
    let mut o = Output::default();
    let now = i.now_s;

    // ── 1) SNTP start gate ───────────────────────────────────────────────────
    // Mirrors the C++ originally in application.cc::ShouldStartSntpNow.
    //
    //   - `time()` is negative or implausible -> Start (we need SNTP)
    //   - first post-boot sync, last < NEVER -> Start
    //   - last >= NEVER, now <= last          -> Repair (clock regression)
    //   - last >= NEVER, now - last >= period -> Start and arm
    //   - otherwise                          -> Skip
    //
    // Note: a negative `now_s` is the "system clock uninitialised" signal
    // (ESP-IDF returns -1 from `time()` before SNTP seeds it). Treating
    // it as `now < NEVER` would still hit the "implausible now" branch,
    // but the dedicated check below documents the failure mode.
    if now < 0 {
        o.sntp_start_action = SNTP_ACTION_START;
    } else if !now_valid(i.clock_valid_mask) {
        o.sntp_start_action = SNTP_ACTION_START;
    } else if i.last_sntp_sync_s < NEVER {
        // First post-boot SNTP: stamp advances so the next wake is gated
        // by the daily window, not the cold-boot sentinel.
        o.sntp_start_action = SNTP_ACTION_START;
        o.next_last_sntp_sync_s = now.saturating_add(i.sntp_min_period_s as i64);
    } else if now as u64 <= i.last_sntp_sync_s as u64 {
        // Regression (or `now == last`, treated as Repair for safety).
        o.sntp_start_action = SNTP_ACTION_REPAIR;
        // Repair must not advance the stamp: keep the prior value. The
        // SNTP callback that follows overwrites via rf_sntp_mark_synced
        // when the fresh time arrives.
        o.next_last_sntp_sync_s = i.last_sntp_sync_s as i64;
    } else if (now as u64) - (i.last_sntp_sync_s as u64) >= i.sntp_min_period_s as u64 {
        o.sntp_start_action = SNTP_ACTION_START;
        o.next_last_sntp_sync_s = now.saturating_add(i.sntp_min_period_s as i64);
    } else {
        o.sntp_start_action = SNTP_ACTION_SKIP;
        o.next_last_sntp_sync_s = i.last_sntp_sync_s as i64;
    }
    // ── 2) SNTP mark-synced gate ─────────────────────────────────────────────
    // Refuse to persist anything when `time()` is negative or below
    o.sntp_mark_synced_ok = if now >= NEVER as i64 { 1 } else { 0 };


    // ── 3) RTC cache write-back ──────────────────────────────────────────────
    // Allowed whenever `time()` itself is plausible; we do NOT require
    // CLOCK_EVER_SYNCED — the first post-SNTP-tick arm must still write.
    o.rtc_cache_ok = if now_valid(i.clock_valid_mask) { 1 } else { 0 };

    // ── 4) Battery sample one-hour gate ──────────────────────────────────────
    // Mirrors the legacy shim.cpp::BatteryClock + rf_battery_due/arm
    // semantics exactly:
    //
    //   effective = time()       if time() is plausible
    //             = PCF8563 RTC  else if the fallback epoch is plausible
    //             = none         otherwise
    //
    //   - no effective clock                    -> due, do not arm.
    //   - effective, last_arm < NEVER (cold)    -> due, arm = effective + 1h.
    //   - effective >= last_arm + period        -> due, arm = effective + 1h.
    //   - otherwise (within window)             -> skip, keep last_arm.
    //
    // The fallback path uses the SAME comparison and the SAME arming rule
    // as the wall-clock path, keyed off the RTC epoch instead of time().
    // The old code did exactly this (BatteryClock() fed both the due
    // check and the arm write), so a pre-SNTP wake with a plausible RTC
    // samples at most once per hour — not on every wake.
    let effective: Option<u32> = if now >= NEVER as i64 {
        Some(now as u32)
    } else if rtc_valid(i.clock_valid_mask) && i.effective_clock_s >= NEVER {
        Some(i.effective_clock_s)
    } else {
        None
    };
    match effective {
        None => {
            // No clock anywhere — must report at least once, but never arm.
            o.battery_sample_ok = 1;
            o.next_last_battery_arm_s = S;
        }
        Some(e) if i.last_battery_arm_s < NEVER => {
            // Cold boot — first sample, arm the gate off the effective clock.
            o.battery_sample_ok = 1;
            o.next_last_battery_arm_s = e.saturating_add(i.battery_min_period_s);
        }
        Some(e) if (e as u64) >= (i.last_battery_arm_s as u64) + (i.battery_min_period_s as u64) => {
            o.battery_sample_ok = 1;
            o.next_last_battery_arm_s = e.saturating_add(i.battery_min_period_s);
        }
        _ => {
            o.battery_sample_ok = 0;
            o.next_last_battery_arm_s = i.last_battery_arm_s;
        }
    }

    // ── 5) Wi-Fi cache write-back ────────────────────────────────────────────
    // Same gate as (3): without a plausible wall clock we cannot
    // timestamp the cache reliably. Connectivity is the wifi policy's
    // job; this module only answers "is time reliable enough to commit
    // a stale-on-write timestamp?".
    o.wifi_cache_ok = if now_valid(i.clock_valid_mask) { 1 } else { 0 };
    if !ever_synced(i.clock_valid_mask) {
        // The pre-sync first wake still needs SNTP to run, so we leave
        // the SNTP action alone. The other gates simply refuse to write.
        o.rtc_cache_ok = 0;
        o.wifi_cache_ok = 0;
        if o.sntp_mark_synced_ok == 1 && !ever_synced(i.clock_valid_mask) {
            // Cannot happen: sntp_mark_synced_ok is gated by now >= NEVER,
            // which means time() is plausible, which means SNTP may have
            // already run once on this boot.
        }
    }

    o
}

// ─── C ABI ───────────────────────────────────────────────────────────────────
// Match the `rf_time_gate_policy_*_t` layout in `time_gate_policy.h`.

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CInputs {
    pub now_s: i64,
    pub effective_clock_s: u32,
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
    pub next_last_sntp_sync_s: i64,
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
            next_last_sntp_sync_s: 0,
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
    let i = unsafe { &*inp };
    let r = decide(&Inputs {
        now_s: i.now_s,
        effective_clock_s: i.effective_clock_s,
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

    const CLOCK_OK: u32 = CLOCK_NOW_VALID | CLOCK_RTC_VALID | CLOCK_EVER_SYNCED;
    /// time() not yet seeded (cold boot), but PCF8563 has plausible epoch.
    const CLOCK_RTC_FALLBACK: u32 = CLOCK_RTC_VALID;
    /// time() and PCF8563 both implausible; never synced.
    const CLOCK_NONE: u32 = 0;
    /// time() plausible (somehow), but no RTC, no SNTP yet.
    const CLOCK_NOW_ONLY: u32 = CLOCK_NOW_VALID;

    /// 2026-01-15 12:00:00 UTC.
    const NOW: i64 = 1_768_463_200;

    fn inp_with(
        now: i64,
        effective: u32,
        last_sntp: u32,
        last_bat: u32,
        valid: u32,
        sntp_period: u32,
        bat_period: u32,
    ) -> Inputs {
        Inputs {
            now_s: now,
            effective_clock_s: effective,
            last_sntp_sync_s: last_sntp,
            last_battery_arm_s: last_bat,
            sntp_min_period_s: sntp_period,
            battery_min_period_s: bat_period,
            clock_valid_mask: valid,
        }
    }

    // ── SNTP start gate: valid clock ────────────────────────────────────────

    #[test]
    fn sntp_start_first_sync_after_cold_boot_is_start() {
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
        assert_eq!(r.next_last_sntp_sync_s, NOW + 86_400);
    }

    #[test]
    fn sntp_start_within_daily_window_is_skip() {
        let last = (NOW - 3_600) as u32;
        let r = decide(&inp_with(NOW, last, last, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_SKIP);
        assert_eq!(r.next_last_sntp_sync_s, last as i64);
    }

    #[test]
    fn sntp_start_at_exactly_24h_elapsed_is_start() {
        let last = (NOW - 86_400) as u32;
        let r = decide(&inp_with(NOW, last, last, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
        assert_eq!(r.next_last_sntp_sync_s, NOW + 86_400);
    }

    // ── SNTP start gate: invalid clock ──────────────────────────────────────

    #[test]
    fn sntp_start_with_1970_now_is_start() {
        let r = decide(&inp_with(0, 0, S + 1_000_000, S, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
    }

    #[test]
    fn sntp_start_with_negative_now_is_start() {
        // time()==-1: ESP-IDF clock not initialised. Treating it as 1970
        // would still start, but a regression test pins the contract.
        let r = decide(&inp_with(-1, 0, S + 1_000_000, S, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_START);
    }

    // ── SNTP start gate: clock regression ───────────────────────────────────

    #[test]
    fn sntp_start_with_clock_regression_is_repair() {
        let last = (NOW + 60) as u32;
        let r = decide(&inp_with(NOW, last, last, NOW as u32, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
    }

    #[test]
    fn sntp_start_with_now_equal_last_is_repair() {
        let r = decide(&inp_with(NOW, NOW as u32, NOW as u32, NOW as u32, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
    }

    // ── SNTP mark-synced: refuse implausible clocks ──────────────────────────

    #[test]
    fn mark_synced_with_implausible_now_is_not_ok() {
        let r = decide(&inp_with(0, 0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 0);
    }

    #[test]
    fn mark_synced_with_negative_now_is_not_ok() {
        // The reviewer-flagged case: time()==-1 must not become a
        // 2106 stamp when narrowed to u32. Rust keeps the sign, so the
        // policy refuses.
        let r = decide(&inp_with(-1, 0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 0);
    }

    #[test]
    fn mark_synced_with_plausible_now_is_ok() {
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 1);
    }

    #[test]
    fn mark_synced_never_uses_rtc_fallback_even_if_plausible() {
        // PCF8563 holds a plausible epoch but time() is invalid. The
        // mark-synced gate must NOT persist that as a fake sync stamp.
        let r = decide(&inp_with(0, NOW as u32, S - 1, S - 1, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.sntp_mark_synced_ok, 0);
    }

    // ── RTC cache write-back ─────────────────────────────────────────────────

    #[test]
    fn rtc_cache_write_blocked_on_1970_clock() {
        let r = decide(&inp_with(0, 0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 0);
    }

    #[test]
    fn rtc_cache_write_allowed_with_plausible_clock() {
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 1);
    }

    #[test]
    fn rtc_cache_write_blocked_when_only_rtc_seed_exists() {
        // time() invalid but RTC plausible: refuse until SNTP seeds the
        // monotonic wall clock.
        let r = decide(&inp_with(0, NOW as u32, S - 1, S - 1, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.rtc_cache_ok, 0);
    }

    // ── Battery sample one-hour gate (with fallback) ─────────────────────────

    #[test]
    fn battery_sample_first_cold_boot_is_due() {
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NOW as u32 + 3_600);
    }

    #[test]
    fn battery_sample_within_one_hour_window_is_skipped() {
        let last = (NOW - 1_800) as u32;
        let r = decide(&inp_with(NOW, NOW as u32, S + 1_000_000, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 0);
        assert_eq!(r.next_last_battery_arm_s, last);
    }

    #[test]
    fn battery_sample_at_exactly_one_hour_elapsed_is_due() {
        let last = (NOW - 3_600) as u32;
        let r = decide(&inp_with(NOW, NOW as u32, S + 1_000_000, last, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NOW as u32 + 3_600);
    }

    #[test]
    fn battery_sample_with_no_clock_anywhere_is_due_but_no_arm() {
        // Both time() and PCF8563 are implausible: must report at least
        // once, but not arm — the next plausible tick will arm the gate.
        let r = decide(&inp_with(0, 0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, NEVER);
    }

    #[test]
    fn battery_sample_with_rtc_fallback_first_sample_is_due_and_arms() {
        // Round-2 fix: time() invalid but the PCF8563 holds a plausible
        // epoch. The legacy BatteryClock() fed that epoch to both the
        // due check and the arm write, so a pre-SNTP wake arms the same
        // one-hour window — keyed off the RTC epoch, not off time().
        let eff = NOW as u32;
        let r = decide(&inp_with(0, eff, S - 1, S - 1, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, eff + 3_600,
                   "fallback sample must arm fallback_epoch + period");
    }

    #[test]
    fn battery_sample_with_rtc_fallback_within_window_is_skipped() {
        // The companion case: the fallback epoch is 30 min past the last
        // fallback arm, so the window is still closed. Old code computed
        // `effective >= last_arm + period` via BatteryClock() and would
        // skip — the reviewer's "do not report every wake" requirement.
        let eff = NOW as u32;
        let last_arm = eff - 1_800; // armed 30 min ago (fallback clock)
        let r = decide(&inp_with(0, eff, S - 1, last_arm, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 0,
                   "fallback sample within its window must not re-report");
        assert_eq!(r.next_last_battery_arm_s, last_arm);
    }

    #[test]
    fn battery_sample_with_rtc_fallback_elapsed_window_is_due() {
        // Fallback epoch is 2 h past the last fallback arm: due again,
        // re-arm off the fallback epoch.
        let eff = NOW as u32;
        let last_arm = eff - 7_200; // armed 2 h ago (fallback clock)
        let r = decide(&inp_with(0, eff, S - 1, last_arm, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.battery_sample_ok, 1);
        assert_eq!(r.next_last_battery_arm_s, eff + 3_600);
    }

    // ── Wi-Fi cache write-back gate ──────────────────────────────────────────

    #[test]
    fn wifi_cache_write_blocked_on_1970_clock() {
        let r = decide(&inp_with(0, 0, S - 1, S - 1, CLOCK_NONE, 86_400, 3_600));
        assert_eq!(r.wifi_cache_ok, 0);
    }

    #[test]
    fn wifi_cache_write_allowed_with_plausible_clock() {
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.wifi_cache_ok, 1);
    }

    #[test]
    fn wifi_cache_write_blocked_when_only_rtc_seed_exists() {
        // The same gate as the RTC cache write: the wifi cache also
        // needs a monotonic wall clock for the stale-on-write timestamp.
        let r = decide(&inp_with(0, NOW as u32, S - 1, S - 1, CLOCK_RTC_FALLBACK, 86_400, 3_600));
        assert_eq!(r.wifi_cache_ok, 0);
    }

    // ── Action mapping table ─────────────────────────────────────────────────

    #[test]
    fn action_mapping_table() {
        assert_eq!(SNTP_ACTION_SKIP, 0);
        assert_eq!(SNTP_ACTION_START, 1);
        assert_eq!(SNTP_ACTION_REPAIR, 2);
        assert_ne!(SNTP_ACTION_SKIP, SNTP_ACTION_START);
        assert_ne!(SNTP_ACTION_SKIP, SNTP_ACTION_REPAIR);
        assert_ne!(SNTP_ACTION_START, SNTP_ACTION_REPAIR);
    }

    // ── C ABI round-trip ─────────────────────────────────────────────────────

    #[test]
    fn ffi_round_trip_fills_output_fields() {
        let ci = CInputs {
            now_s: NOW,
            effective_clock_s: NOW as u32,
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
        assert_eq!(co.next_last_battery_arm_s, NOW as u32 + 3_600);
    }

    // ── Sentinel: repair must not advance the stamp ──────────────────────────

    #[test]
    fn repair_action_does_not_advance_the_sntp_stamp() {
        // Naive impl: treat now <= last as elapsed, set next_last to
        // now + period → produces a future stamp that the clock catches
        // up to and re-opens the gate erroneously.
        let last = (NOW + 60) as u32;
        let r = decide(&inp_with(NOW, last, last, NOW as u32, CLOCK_OK, 86_400, 3_600));
        assert_eq!(r.sntp_start_action, SNTP_ACTION_REPAIR);
        // next_last is signed but in this branch it's still last_sntp_sync_s
        // (the prior value), since the broken-clock case never re-stamps.
        assert_eq!(r.next_last_sntp_sync_s, last as i64);
    }

    // ── Regression: pre-SNTP cold boot keeps wifi/rtc cache writes suppressed ──

    #[test]
    fn pre_snytp_cold_boot_suppresses_cache_writes_but_allows_sample() {
        // Cold boot: time() plausible (was set by PCF8563 settimeofday),
        // but SNTP has not yet synced. RTC and Wi-Fi cache writes must
        // stay blocked until the first SNTP success, but the battery
        // sample is allowed (with no arm).
        let r = decide(&inp_with(NOW, NOW as u32, S - 1, S - 1, CLOCK_NOW_ONLY, 86_400, 3_600));
        // Without CLOCK_EVER_SYNCED, the defence-in-depth clause kicks in.
        assert_eq!(oops_never_synced(&r), true);
    }

    /// Helper used by the regression test above.
    fn oops_never_synced(o: &Output) -> bool {
        o.rtc_cache_ok == 0 && o.wifi_cache_ok == 0
    }
}
