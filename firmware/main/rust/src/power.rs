//! Sleep/wake policy. Pure: the C side gathers the inputs, this decides.

pub struct Inputs {
    pub mains: bool,
    pub notify_active: bool,
    pub busy: bool,
    pub sync_ok: bool,
    pub screen_active: bool,
    pub on_canvas: bool,
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

fn poll_cap(i: &Inputs) -> u32 {
    let cap = if i.screen_active { i.poll_s } else { i.sleep_poll_s };
    cap.min(i.max_sleep_s).max(MIN_SLEEP_S)
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

    let cap = poll_cap(i);
    let wake_s = if !i.sync_ok {
        // Nothing to paint until the server answers; back off instead of
        // hammering a server that is down.
        (BACKOFF_BASE_S << i.fail_streak.min(5)).min(cap)
    } else {
        match i.seconds_until_next_page {
            Some(s) => s.clamp(MIN_SLEEP_S, cap),
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
            screen_active: true, on_canvas: true, idle_ms: 600_000,
            grace_ms: 180_000, max_sleep_s: 3600, poll_s: 600, sleep_poll_s: 3600,
            fail_streak: 0, seconds_until_next_page: Some(240),
        }
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

    #[test]
    fn the_poll_cap_bounds_the_wake() {
        let i = Inputs { seconds_until_next_page: Some(9000), poll_s: 600, ..base() };
        assert_eq!(decide(&i), Action::Sleep { wake_s: 600, invalidate_panel: false });
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
    pub _pad: [u8; 2],
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
