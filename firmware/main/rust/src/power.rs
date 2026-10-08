//! Sleep/wake policy. Pure: the C side gathers the inputs, this decides.

pub struct Inputs {
    pub mains: bool,
    pub notify_active: bool,
    pub busy: bool,
    pub sync_ok: bool,
    pub screen_active: bool,
    pub on_canvas: bool,
    pub sleep_mode: bool,
    pub idle_ms: u64,
    pub grace_ms: u32,
    pub max_sleep_s: u32,
    pub poll_s: u32,
    pub sleep_poll_s: u32,
    pub fail_streak: u32,
    pub seconds_until_next_page: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    StayAwake { retry_ms: u32, reason: &'static str },
    Sleep { wake_s: u32, invalidate_panel: bool },
}

/// 60 s floor: the server never asks for less, but a floor keeps a bad policy
/// value from turning the duty cycle into a hot loop.
const MIN_SLEEP_S: u32 = 60;
/// Re-evaluate cadence while held awake; short for hardware reasons, long for
/// the user-activity grace, so we do not spin on either.
const BUSY_RETRY_MS: u32 = 15_000;
const MAINS_RETRY_MS: u32 = 60_000;
const GRACE_RETRY_MS: u32 = 30_000;
const BACKOFF_BASE_S: u32 = 60;
/// How long the device may sleep while the service has told it how long until
/// the page rotation reaches it — the holder's accepted notification latency.
///
/// The service also sends `poll_interval_minutes` (10 min), and that used to
/// be the cap on every wake, which made a 30-minute page wake the device three
/// times per rotation: two of those wakes downloaded the bitmap and then
/// skipped the repaint. Measured over 13:43-16:21 on battery, 15 wakes with 9
/// repaints against a rotation that needs about 1.5.
///
/// `poll_s` is a poll cadence, not a sleep budget — it belongs to the "no idea
/// when the next page is" case and to the day/night cadences, which is what
/// [`poll_cap`] still owns. When the rotation IS known, this budget bounds the
/// wait instead, so a push cannot go unnoticed for longer than the holder
/// agreed to. It is a ceiling, not a licence: `max_sleep_min` still wins.
pub const ROTATION_WAIT_BUDGET_S: u32 = 900;

fn poll_cap(i: &Inputs) -> u32 {
    let cap = if i.screen_active { i.poll_s } else { i.sleep_poll_s };
    cap.min(i.max_sleep_s).max(MIN_SLEEP_S)
}

/// Whether a paint-cut station may be restored during a panel busy window.
/// Notify pulls are the sole exception: they hold awake and need the radio
/// before their bounded HTTP timeout expires.
pub fn should_resume_radio_after_paint(
    radio_cut: bool, display_busy: bool, notify_active: bool,
) -> bool {
    radio_cut && (!display_busy || notify_active)
}

pub fn decide(i: &Inputs) -> Action {
    // A device on USB is a development device: keep the console and stay
    // flashable, whatever the schedule says.
    if i.mains {
        return Action::StayAwake { retry_ms: MAINS_RETRY_MS, reason: "mains" };
    }
    // A notification is a question addressed to a human.
    if i.notify_active {
        return Action::StayAwake { retry_ms: BUSY_RETRY_MS, reason: "notify" };
    }
    // Panels and audio: cutting power mid-refresh corrupts a four-colour panel.
    if i.busy {
        return Action::StayAwake { retry_ms: BUSY_RETRY_MS, reason: "busy" };
    }
    if i.idle_ms < i.grace_ms as u64 {
        return Action::StayAwake { retry_ms: GRACE_RETRY_MS, reason: "grace" };
    }

    // The provisioning page's "Enable Sleep Mode" switch, wired in: off =
    // showroom/demo device — keep polling and painting on the mains-style
    // re-evaluation cadence, but never duty-cycle. Checked after the
    // user-activity holds, so their retry cadence is unaffected.
    if !i.sleep_mode {
        return Action::StayAwake { retry_ms: MAINS_RETRY_MS, reason: "sleep_mode_off" };
    }

    // Two different questions, two different ceilings:
    //
    //   * "I know when the page changes" — wait that long, bounded by the
    //     holder's latency budget and by `max_sleep_min`.
    //   * "I have no idea when the page changes" (no schedule, or a failed
    //     sync) — the poll cadence answers, since a push can only be noticed
    //     by polling.
    //
    // Both still pass through `MIN_SLEEP_S` so a bad service value cannot turn
    // the duty cycle into a hot loop, and both still pass through
    // `poll_cap` for the unknown case so the day/night cadences keep working.
    let cap = poll_cap(i);
    // A known rotation is NOT clamped by `cap`: `cap` is the poll cadence, and
    // letting it bound a rotation the service has already dated is what woke
    // the device three times per 30-minute page. The ceiling is the latency
    // budget (so a push is never unnoticed for longer than the holder agreed
    // to) and `max_sleep_min` (so a stale NVS value cannot park the device).
    let wait_cap = ROTATION_WAIT_BUDGET_S.clamp(MIN_SLEEP_S, i.max_sleep_s.max(MIN_SLEEP_S));
    let wake_s = if !i.sync_ok {
        // Nothing to paint until the server answers; back off instead of
        // hammering a server that is down.
        (BACKOFF_BASE_S << i.fail_streak.min(5)).min(cap)
    } else {
        match i.seconds_until_next_page {
            Some(s) => s.clamp(MIN_SLEEP_S, wait_cap),
            None => cap,
        }
    };

    // Sleeping while the UI owns the screen would resurrect the canvas on the
    // next wake while the RTC record still claims the canvas is up.
    Action::Sleep { wake_s, invalidate_panel: !i.on_canvas }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            mains: false, notify_active: false, busy: false, sync_ok: true,
            screen_active: true, on_canvas: true, sleep_mode: true, idle_ms: 600_000,
            grace_ms: 180_000, max_sleep_s: 3600, poll_s: 600, sleep_poll_s: 3600,
            fail_streak: 0, seconds_until_next_page: Some(240),
        }
    }

    /// The provisioning page's "Enable Sleep Mode" checkbox, wired in: off
    /// means a showroom/demo device that keeps polling and painting but
    /// never duty-cycles (same shape as the mains hold).
    #[test]
    fn sleep_mode_off_holds_the_device_awake() {
        let i = Inputs { sleep_mode: false, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 60_000, reason: "sleep_mode_off" });
    }

    /// Flip test: the only difference from the off case is the flag — so the
    /// flag, not some drift, is exactly what gates the duty cycle.
    #[test]
    fn sleep_mode_on_keeps_the_normal_duty_cycle() {
        let i = Inputs { sleep_mode: true, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 240, invalidate_panel: false });
    }

    #[test]
    fn mains_never_sleeps() {
        let i = Inputs { mains: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 60_000, reason: "mains" });
    }

    #[test]
    fn a_notification_on_screen_holds_the_device_awake() {
        let i = Inputs { notify_active: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 15_000, reason: "notify" });
    }

    // Wave-2 wiring note: the C++ side feeds
    // `notify_is_active() || notify_is_fetching()` into `notify_active`, so an
    // in-flight /next pull maps to this same hold — no separate decide() arm
    // is needed, and the stay-awake cannot stick (every fetch_once terminal
    // path leaves FETCHING, so the next re-arm re-evaluates to Sleep).

    #[test]
    fn active_notification_pull_restores_radio_during_busy_paint() {
        assert!(!should_resume_radio_after_paint(true, true, false));
        assert!(should_resume_radio_after_paint(true, true, true));
        assert!(should_resume_radio_after_paint(true, false, false));
        assert!(!should_resume_radio_after_paint(false, false, true));
    }

    #[test]
    fn a_busy_panel_or_audio_holds_the_device_awake() {
        let i = Inputs { busy: true, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 15_000, reason: "busy" });
    }

    #[test]
    fn user_activity_inside_the_grace_window_holds_the_device_awake() {
        let i = Inputs { idle_ms: 179_999, ..base() };
        assert_eq!(decide(&i), Action::StayAwake { retry_ms: 30_000, reason: "grace" });
    }

    #[test]
    fn wakes_at_the_next_page_boundary() {
        let i = Inputs { seconds_until_next_page: Some(240), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 240, invalidate_panel: false });
    }

    /// Regression for the measured defect: the service's 10-minute poll
    /// interval used to clamp every wake, so a 30-minute page still woke the
    /// device three times per rotation. `poll_s` is a *poll* cadence, not a
    /// sleep budget: when the service says the next page change is further
    /// out than the budget allows, the device sleeps for the budget.
    #[test]
    fn the_poll_interval_no_longer_clamps_a_known_rotation() {
        let i = Inputs { seconds_until_next_page: Some(9000), poll_s: 600, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: ROTATION_WAIT_BUDGET_S, invalidate_panel: false });
    }

    /// The holder's accepted notification latency: the schedule GET that a
    /// push waits on cannot be further out than this, however long the page
    /// rotation is.
    #[test]
    fn the_latency_budget_bounds_the_wait() {
        assert_eq!(ROTATION_WAIT_BUDGET_S, 900, "the budget is 15 minutes");
        for page in [901, 1800, 1799, 86_400] {
            let i = Inputs { seconds_until_next_page: Some(page), poll_s: 600, ..base() };
            assert_eq!(decide(&i), Action::Sleep { wake_s: 900, invalidate_panel: false },
                "page {page} s out must still wake within the 15 min budget");
        }
    }

    /// The budget is a ceiling the holder controls, not a licence to exceed
    /// `max_sleep_min`.
    #[test]
    fn the_latency_budget_does_not_override_max_sleep() {
        let i = Inputs { max_sleep_s: 300, seconds_until_next_page: Some(9000), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
    }

    /// The night cadence (`sleep_poll_s`, 60 min) is the *poll* ceiling and
    /// still owns the unknown-rotation case, but it must not shrink a known
    /// rotation that lands inside the latency budget: at 1 a.m. a page that
    /// changes in 5 minutes still has to be awake when it does.
    #[test]
    fn the_night_cadence_does_not_shrink_a_rotation_inside_the_budget() {
        let i = Inputs { screen_active: false, sleep_poll_s: 3600, poll_s: 600,
                         seconds_until_next_page: Some(300), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
    }

    /// Flip test for the ceiling arithmetic: the same inputs differing only in
    /// `seconds_until_next_page`, so the budget -- and not `poll_s`, nor the
    /// night cadence -- is what stops the wait.
    #[test]
    fn the_budget_and_not_the_poll_interval_is_what_bounds_a_known_rotation() {
        let inside = Inputs { screen_active: false, sleep_poll_s: 3600, poll_s: 600,
                              seconds_until_next_page: Some(899), ..base() };
        assert_eq!(decide(&inside), Action::Sleep { wake_s: 899, invalidate_panel: false });
        let far = Inputs { seconds_until_next_page: Some(3600), ..inside };
        assert_eq!(decide(&far), Action::Sleep { wake_s: 900, invalidate_panel: false },
            "a rotation a full night away must not hold the device for a full night");
    }

    /// A rotation closer than the budget is still honoured exactly: the device
    /// must be awake when the page actually changes.
    #[test]
    fn a_rotation_inside_the_budget_is_slept_exactly() {
        for page in [60, 240, 599, 900] {
            let i = Inputs { seconds_until_next_page: Some(page), poll_s: 60, ..base() };
            assert_eq!(decide(&i), Action::Sleep { wake_s: page, invalidate_panel: false },
                "page {page} s out must be slept exactly, not stretched to the budget");
        }
    }

    /// Without a rotation answer there is nothing to wait for, so the poll
    /// cadence (day or night) still owns the wake — unchanged.
    #[test]
    fn an_unknown_rotation_keeps_the_poll_cadence() {
        let i = Inputs { seconds_until_next_page: None, poll_s: 600, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
        let i = Inputs { screen_active: false, sleep_poll_s: 3600, poll_s: 600, ..i };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 3600, invalidate_panel: false });
    }

    #[test]
    fn the_sleep_window_uses_the_sleep_cadence() {
        let i = Inputs { screen_active: false, sleep_poll_s: 3600, poll_s: 600,
                         seconds_until_next_page: Some(300), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
        let i = Inputs { seconds_until_next_page: None, ..i };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 3600, invalidate_panel: false });
    }

    #[test]
    fn an_empty_schedule_sleeps_for_the_cap() {
        let i = Inputs { seconds_until_next_page: None, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
    }

    #[test]
    fn the_wake_never_goes_below_the_floor() {
        let i = Inputs { seconds_until_next_page: Some(0), ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 60, invalidate_panel: false });
    }

    #[test]
    fn the_cap_is_clamped_by_max_sleep() {
        let i = Inputs { max_sleep_s: 300, seconds_until_next_page: None, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 300, invalidate_panel: false });
    }

    #[test]
    fn a_failed_sync_backs_off_and_does_not_short_cycle() {
        let i = Inputs { sync_ok: false, fail_streak: 0, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 60, invalidate_panel: false });
        let i = Inputs { sync_ok: false, fail_streak: 1, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 120, invalidate_panel: false });
        let i = Inputs { sync_ok: false, fail_streak: 9, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
    }

    #[test]
    fn sleeping_from_a_non_canvas_page_invalidates_the_panel_record() {
        let i = Inputs { on_canvas: false, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 240, invalidate_panel: true });
    }
}

use core::ffi::c_int;

#[repr(C)]
pub struct CInputs {
    pub mains: u8,
    pub notify_active: u8,
    pub busy: u8,
    pub sync_ok: u8,
    pub screen_active: u8,
    pub on_canvas: u8,
    pub sleep_mode: u8,
    pub _pad: [u8; 1],
    pub idle_ms: u64,
    pub grace_ms: u32,
    pub max_sleep_s: u32,
    pub poll_s: u32,
    pub sleep_poll_s: u32,
    pub fail_streak: u32,
    pub seconds_until_next_page: c_int,
}

#[repr(C)]
pub struct CDecision {
    pub sleep: u8,
    pub invalidate_panel: u8,
    pub _pad: [u8; 2],
    pub wake_s: u32,
    pub stay_awake_ms: u32,
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_power_decide(inp: *const CInputs, out: *mut CDecision) {
    let i = unsafe { &*inp };
    let a = decide(&Inputs {
        mains: i.mains != 0,
        notify_active: i.notify_active != 0,
        busy: i.busy != 0,
        sync_ok: i.sync_ok != 0,
        screen_active: i.screen_active != 0,
        on_canvas: i.on_canvas != 0,
        sleep_mode: i.sleep_mode != 0,
        idle_ms: i.idle_ms,
        grace_ms: i.grace_ms,
        max_sleep_s: i.max_sleep_s,
        poll_s: i.poll_s,
        sleep_poll_s: i.sleep_poll_s,
        fail_streak: i.fail_streak,
        seconds_until_next_page: if i.seconds_until_next_page < 0 {
            None
        } else {
            Some(i.seconds_until_next_page as u32)
        },
    });
    let d = unsafe { &mut *out };
    match a {
        Action::StayAwake { retry_ms, .. } => {
            d.sleep = 0;
            d.wake_s = 0;
            d.stay_awake_ms = retry_ms;
        }
        Action::Sleep { wake_s, invalidate_panel } => {
            d.sleep = 1;
            d.invalidate_panel = invalidate_panel as u8;
            d.wake_s = wake_s;
            d.stay_awake_ms = 0;
        }
    }
}

/// # Safety
/// This scalar-only FFI has no pointer preconditions.
#[unsafe(no_mangle)]
pub extern "C" fn rf_power_should_resume_radio(
    radio_cut: u8, display_busy: u8, notify_active: u8,
) -> u8 {
    should_resume_radio_after_paint(
        radio_cut != 0,
        display_busy != 0,
        notify_active != 0,
    ) as u8
}

