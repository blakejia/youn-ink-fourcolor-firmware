//! Button routing. Pure: the C side owns the screen, the canvas, the popup and
//! the UI manager, this decides which of them a gesture belongs to.
//!
//! Why this and not `TransitionLifecycle`: the lifecycle states are reported by
//! whichever module finished a step — there is no decision in them. The routing
//! is where the precedence lives, and it is the part that has hurt: a canvas
//! that keeps the arrow keys after the user left it, an overlay drawn over a
//! notification, a loud dismiss that repaints the canvas into the screen switch
//! that follows. Each of those is a row in the table below.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Boot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Click,
    DoubleClick,
    LongPress,
    /// UP and DOWN held together.
    ComboLongPress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    pub button: Button,
    pub gesture: Gesture,
    /// A notification is on the panel.
    pub notify_active: bool,
    /// The canvas is the thing currently drawn.
    pub canvas_displaying: bool,
    /// The UI's current page is Settings.
    pub on_settings: bool,
    /// The UI's previous page is Settings (so leaving Settings would go nowhere).
    pub previous_is_settings: bool,
    /// The provisioning access point is up.
    pub config_mode: bool,
    /// Lifecycle is ApProvision.
    pub provisioning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Swallow the gesture.
    Ignore,
    /// Hand it to the UI manager's own input handler.
    UiInput,
    NotifyAck { agree: bool },
    /// BOOT click: close it without answering.
    NotifyDismiss,
    NotifyFetchNext,
    CanvasPrev,
    CanvasNext,
    /// Enter Settings.
    EnterSettings(SettingsEntry),
    /// Leave Settings and give the panel back to the canvas.
    /// `switch_to_previous` is false when the previous page is Settings itself:
    /// there is nowhere to go, but the panel still has to be handed back.
    LeaveSettings { switch_to_previous: bool },
    EnterWifiConfig,
    /// Leave the config AP, then take the Settings entry below.
    ExitWifiConfig(SettingsEntry),
    /// Give the panel back, then take the Settings entry below.
    StopCanvas(SettingsEntry),
}

/// How to enter Settings. All three ways in share it, because the old code
/// shared one function (`OnDownLongPress`) between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsEntry {
    pub enter: bool,
    /// A popup is still up. Leaving it alive would let it keep consuming
    /// UP/DOWN as agree/reject from a screen the user has already left.
    pub drop_orphan_notification: bool,
}

/// The routing table. Guards run in this order, and the order is the design:
/// a live popup answers before the canvas, the canvas before the UI, and the
/// UI last.
pub fn decide(i: &Inputs) -> Action {
    match (i.button, i.gesture) {
        (Button::Up, Gesture::Click) => {
            if i.notify_active {
                Action::NotifyAck { agree: true }
            } else if i.canvas_displaying {
                Action::CanvasPrev
            } else {
                Action::UiInput
            }
        }
        (Button::Down, Gesture::Click) => {
            if i.notify_active {
                Action::NotifyAck { agree: false }
            } else if i.canvas_displaying {
                Action::CanvasNext
            } else {
                Action::UiInput
            }
        }
        (Button::Boot, Gesture::Click) => {
            // Closing a popup from BOOT is not an answer: the device is asking
            // the user something, and BOOT says "not now", not "no".
            if i.notify_active {
                Action::NotifyDismiss
            } else if i.canvas_displaying {
                Action::NotifyFetchNext
            } else {
                Action::UiInput
            }
        }
        (Button::Up, Gesture::DoubleClick) => {
            // The quick-switch overlay is drawn over whatever owns the panel and
            // keeps a snapshot to restore; with a popup or the canvas up there is
            // nothing sound to restore.
            if i.notify_active || i.canvas_displaying {
                Action::Ignore
            } else {
                Action::UiInput
            }
        }
        (Button::Up, Gesture::LongPress) => {
            // Only Settings answers. With nowhere to go back to it stays put
            // but still hands the panel over — the old handler skipped the
            // page switch in that case, not the hand-back.
            if i.on_settings {
                Action::LeaveSettings { switch_to_previous: !i.previous_is_settings }
            } else {
                Action::Ignore
            }
        }
        (Button::Down, Gesture::LongPress) => {
            if i.provisioning {
                Action::Ignore
            } else {
                Action::EnterSettings(SettingsEntry {
                    enter: true,
                    drop_orphan_notification: i.notify_active,
                })
            }
        }
        // The board decides which side reports the combo; both route the same.
        (Button::Up, Gesture::ComboLongPress) | (Button::Down, Gesture::ComboLongPress) => {
            Action::EnterWifiConfig
        }
        (Button::Boot, Gesture::LongPress) => {
            // Both of these hand the panel over first and consult the
            // provisioning guard only for the Settings half, which is the order
            // the old code had: the panel is given back either way.
            if i.config_mode {
                // Boot-long reuses the same Settings entry, so an orphan popup
                // comes down here too.
                Action::ExitWifiConfig(SettingsEntry {
                    enter: !i.provisioning,
                    drop_orphan_notification: i.notify_active,
                })
            } else if i.canvas_displaying {
                Action::StopCanvas(SettingsEntry {
                    enter: !i.provisioning,
                    drop_orphan_notification: i.notify_active,
                })
            } else {
                Action::UiInput
            }
        }
        // Gestures the board never reports: there is no DOWN double-click and
        // no BOOT double-click or combo. Listing them keeps the match total, so
        // a new gesture cannot slip through unmatched.
        (Button::Down, Gesture::DoubleClick)
        | (Button::Boot, Gesture::DoubleClick)
        | (Button::Boot, Gesture::ComboLongPress) => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            button: Button::Up,
            gesture: Gesture::Click,
            notify_active: false,
            canvas_displaying: false,
            on_settings: false,
            previous_is_settings: false,
            config_mode: false,
            provisioning: false,
        }
    }

    // ── clicks ──────────────────────────────────────────────────────────
    #[test]
    fn up_agrees_with_a_visible_notification() {
        let i = Inputs { notify_active: true, canvas_displaying: true, ..base() };
        assert_eq!(decide(&i), Action::NotifyAck { agree: true });
    }

    #[test]
    fn down_rejects_a_visible_notification() {
        let i = Inputs {
            button: Button::Down,
            notify_active: true,
            canvas_displaying: true,
            ..base()
        };
        assert_eq!(decide(&i), Action::NotifyAck { agree: false });
    }

    #[test]
    fn up_pages_the_canvas_back() {
        let i = Inputs { canvas_displaying: true, ..base() };
        assert_eq!(decide(&i), Action::CanvasPrev);
    }

    #[test]
    fn down_pages_the_canvas_forward() {
        let i = Inputs { button: Button::Down, canvas_displaying: true, ..base() };
        assert_eq!(decide(&i), Action::CanvasNext);
    }

    #[test]
    fn up_reaches_the_ui_when_nothing_owns_the_screen() {
        assert_eq!(decide(&base()), Action::UiInput);
    }

    #[test]
    fn down_reaches_the_ui_when_nothing_owns_the_screen() {
        let i = Inputs { button: Button::Down, ..base() };
        assert_eq!(decide(&i), Action::UiInput);
    }

    #[test]
    fn boot_dismisses_a_notification_without_answering_it() {
        let i = Inputs { button: Button::Boot, notify_active: true, ..base() };
        assert_eq!(decide(&i), Action::NotifyDismiss);
    }

    #[test]
    fn boot_pulls_the_next_notification_on_the_canvas() {
        let i = Inputs { button: Button::Boot, canvas_displaying: true, ..base() };
        assert_eq!(decide(&i), Action::NotifyFetchNext);
    }

    #[test]
    fn boot_reaches_the_ui_when_nothing_owns_the_screen() {
        let i = Inputs { button: Button::Boot, ..base() };
        assert_eq!(decide(&i), Action::UiInput);
    }

    // ── double click ────────────────────────────────────────────────────
    #[test]
    fn double_click_is_ignored_while_a_notification_is_up() {
        // The overlay is drawn over the popup and loses the snapshot behind it.
        let i = Inputs { gesture: Gesture::DoubleClick, notify_active: true, ..base() };
        assert_eq!(decide(&i), Action::Ignore);
    }

    #[test]
    fn double_click_is_ignored_on_the_canvas() {
        let i = Inputs { gesture: Gesture::DoubleClick, canvas_displaying: true, ..base() };
        assert_eq!(decide(&i), Action::Ignore);
    }

    #[test]
    fn double_click_reaches_the_ui_otherwise() {
        let i = Inputs { gesture: Gesture::DoubleClick, ..base() };
        assert_eq!(decide(&i), Action::UiInput);
    }

    // ── long press ──────────────────────────────────────────────────────
    #[test]
    fn up_long_leaves_settings_for_the_previous_page() {
        let i = Inputs { gesture: Gesture::LongPress, on_settings: true, ..base() };
        assert_eq!(decide(&i), Action::LeaveSettings { switch_to_previous: true });
    }

    #[test]
    fn up_long_does_nothing_outside_settings() {
        let i = Inputs { gesture: Gesture::LongPress, ..base() };
        assert_eq!(decide(&i), Action::Ignore);
    }

    #[test]
    fn up_long_hand_the_panel_back_even_with_nowhere_to_go() {
        // The old handler called page_sync_allow_display() whenever Settings
        // answered, including when the previous page was Settings too — it
        // skipped the page switch, not the hand-back.
        let i = Inputs {
            gesture: Gesture::LongPress,
            on_settings: true,
            previous_is_settings: true,
            ..base()
        };
        assert_eq!(decide(&i), Action::LeaveSettings { switch_to_previous: false });
    }

    #[test]
    fn up_long_outside_settings_is_ignored() {
        let i = Inputs { gesture: Gesture::LongPress, ..base() };
        assert_eq!(decide(&i), Action::Ignore);
    }

    #[test]
    fn down_long_enters_settings() {
        let i = Inputs { button: Button::Down, gesture: Gesture::LongPress, ..base() };
        assert_eq!(
            decide(&i),
            Action::EnterSettings(SettingsEntry { enter: true, drop_orphan_notification: false })
        );
    }

    #[test]
    fn down_long_asks_for_an_orphan_notification_to_be_dropped() {
        // Otherwise the popup keeps taking UP/DOWN as agree/reject from a screen
        // the user has already left.
        let i = Inputs {
            button: Button::Down,
            gesture: Gesture::LongPress,
            notify_active: true,
            ..base()
        };
        assert_eq!(
            decide(&i),
            Action::EnterSettings(SettingsEntry { enter: true, drop_orphan_notification: true })
        );
    }

    #[test]
    fn down_long_is_ignored_while_provisioning() {
        // The provisioning page needs the user; there is nothing to escape to.
        let i = Inputs {
            button: Button::Down,
            gesture: Gesture::LongPress,
            provisioning: true,
            ..base()
        };
        assert_eq!(decide(&i), Action::Ignore);
    }

    #[test]
    fn the_up_down_combo_enters_wifi_config() {
        let i = Inputs { gesture: Gesture::ComboLongPress, ..base() };
        assert_eq!(decide(&i), Action::EnterWifiConfig);
    }

    #[test]
    fn the_combo_is_the_same_whichever_button_reports_it() {
        // The board decides which side reports the combo; both must route
        // identically or the AP entry depends on a hardware detail.
        let i = Inputs {
            button: Button::Down,
            gesture: Gesture::ComboLongPress,
            ..base()
        };
        assert_eq!(decide(&i), Action::EnterWifiConfig);
    }

    // ── boot long press ─────────────────────────────────────────────────
    #[test]
    fn boot_long_stops_the_canvas_and_enters_settings() {
        let i = Inputs { button: Button::Boot, gesture: Gesture::LongPress, canvas_displaying: true, ..base() };
        assert_eq!(
            decide(&i),
            Action::StopCanvas(SettingsEntry { enter: true, drop_orphan_notification: false })
        );
    }

    #[test]
    fn boot_long_leaves_the_wifi_config_ap() {
        let i = Inputs { button: Button::Boot, gesture: Gesture::LongPress, config_mode: true, ..base() };
        assert_eq!(
            decide(&i),
            Action::ExitWifiConfig(SettingsEntry { enter: true, drop_orphan_notification: false })
        );
    }

    #[test]
    fn boot_long_gives_the_panel_back_even_while_provisioning() {
        // The panel is handed over before the provisioning guard is consulted,
        // exactly as the old code ordered it; only the settings entry is skipped.
        let i = Inputs {
            button: Button::Boot,
            gesture: Gesture::LongPress,
            canvas_displaying: true,
            provisioning: true,
            ..base()
        };
        assert_eq!(
            decide(&i),
            Action::StopCanvas(SettingsEntry { enter: false, drop_orphan_notification: false })
        );
    }

    #[test]
    fn boot_long_also_drops_an_orphan_popup() {
        // Boot-long shares the Settings entry with DOWN-long, so a popup that is
        // still up has to come down the same way — otherwise it goes on taking
        // UP/DOWN as agree/reject from a screen the user has left.
        let i = Inputs {
            button: Button::Boot,
            gesture: Gesture::LongPress,
            canvas_displaying: true,
            notify_active: true,
            ..base()
        };
        assert_eq!(
            decide(&i),
            Action::StopCanvas(SettingsEntry { enter: true, drop_orphan_notification: true })
        );
    }

    #[test]
    fn boot_long_reaches_the_ui_when_nothing_owns_the_screen() {
        let i = Inputs { button: Button::Boot, gesture: Gesture::LongPress, ..base() };
        assert_eq!(decide(&i), Action::UiInput);
    }
}

// ─────────────────────────── C boundary ───────────────────────────

/// `rf_input_action_t` in `rust/include/input.h`. Keep in step.
pub const RF_INPUT_ACTION_IGNORE: u8 = 0;
pub const RF_INPUT_ACTION_UI_INPUT: u8 = 1;
pub const RF_INPUT_ACTION_NOTIFY_ACK: u8 = 2;
pub const RF_INPUT_ACTION_NOTIFY_DISMISS: u8 = 3;
pub const RF_INPUT_ACTION_NOTIFY_FETCH_NEXT: u8 = 4;
pub const RF_INPUT_ACTION_CANVAS_PREV: u8 = 5;
pub const RF_INPUT_ACTION_CANVAS_NEXT: u8 = 6;
pub const RF_INPUT_ACTION_ENTER_SETTINGS: u8 = 7;
pub const RF_INPUT_ACTION_LEAVE_SETTINGS: u8 = 8;
pub const RF_INPUT_ACTION_ENTER_WIFI_CONFIG: u8 = 9;
pub const RF_INPUT_ACTION_EXIT_WIFI_CONFIG: u8 = 10;
pub const RF_INPUT_ACTION_STOP_CANVAS: u8 = 11;

#[repr(C)]
pub struct CInputs {
    pub button: u8, // 0 Up, 1 Down, 2 Boot
    pub gesture: u8, // 0 Click, 1 DoubleClick, 2 LongPress, 3 ComboLongPress
    pub notify_active: u8,
    pub canvas_displaying: u8,
    pub on_settings: u8,
    pub previous_is_settings: u8,
    pub config_mode: u8,
    pub provisioning: u8,
}

#[repr(C)]
pub struct CDecision {
    pub action: u8,
    /// NotifyAck: 1 when the answer is agree.
    pub agree: u8,
    /// An orphan popup has to be dropped on the way in.
    pub drop_orphan_notification: u8,
    /// The Settings entry is allowed (false = stop after the panel hand-back).
    pub enter_settings: u8,
    /// LeaveSettings: the page switch is allowed (false = hand the panel back
    /// and stay put, because the previous page is Settings itself).
    pub switch_to_previous: u8,
}

fn button_from_code(code: u8) -> Button {
    match code {
        1 => Button::Down,
        2 => Button::Boot,
        _ => Button::Up,
    }
}

fn gesture_from_code(code: u8) -> Gesture {
    match code {
        1 => Gesture::DoubleClick,
        2 => Gesture::LongPress,
        3 => Gesture::ComboLongPress,
        _ => Gesture::Click,
    }
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_input_decide(inp: *const CInputs, out: *mut CDecision) {
    let i = unsafe { &*inp };
    let a = decide(&Inputs {
        button: button_from_code(i.button),
        gesture: gesture_from_code(i.gesture),
        notify_active: i.notify_active != 0,
        canvas_displaying: i.canvas_displaying != 0,
        on_settings: i.on_settings != 0,
        previous_is_settings: i.previous_is_settings != 0,
        config_mode: i.config_mode != 0,
        provisioning: i.provisioning != 0,
    });
    let mut d = CDecision {
        action: RF_INPUT_ACTION_IGNORE,
        agree: 0,
        drop_orphan_notification: 0,
        enter_settings: 0,
        switch_to_previous: 0,
    };
    d.action = match a {
        Action::Ignore => RF_INPUT_ACTION_IGNORE,
        Action::UiInput => RF_INPUT_ACTION_UI_INPUT,
        Action::NotifyAck { agree } => {
            d.agree = agree as u8;
            RF_INPUT_ACTION_NOTIFY_ACK
        }
        Action::NotifyDismiss => RF_INPUT_ACTION_NOTIFY_DISMISS,
        Action::NotifyFetchNext => RF_INPUT_ACTION_NOTIFY_FETCH_NEXT,
        Action::CanvasPrev => RF_INPUT_ACTION_CANVAS_PREV,
        Action::CanvasNext => RF_INPUT_ACTION_CANVAS_NEXT,
        Action::EnterSettings(e) => {
            d.drop_orphan_notification = e.drop_orphan_notification as u8;
            d.enter_settings = e.enter as u8;
            RF_INPUT_ACTION_ENTER_SETTINGS
        }
        Action::LeaveSettings { switch_to_previous } => {
            d.switch_to_previous = switch_to_previous as u8;
            RF_INPUT_ACTION_LEAVE_SETTINGS
        }
        Action::EnterWifiConfig => RF_INPUT_ACTION_ENTER_WIFI_CONFIG,
        Action::ExitWifiConfig(e) => {
            d.drop_orphan_notification = e.drop_orphan_notification as u8;
            d.enter_settings = e.enter as u8;
            RF_INPUT_ACTION_EXIT_WIFI_CONFIG
        }
        Action::StopCanvas(e) => {
            d.drop_orphan_notification = e.drop_orphan_notification as u8;
            d.enter_settings = e.enter as u8;
            RF_INPUT_ACTION_STOP_CANVAS
        }
    };
    unsafe { *out = d };
}

#[cfg(test)]
mod ffi_tests {
    use super::*;

    fn base() -> CInputs {
        CInputs {
            button: 0,
            gesture: 0,
            notify_active: 0,
            canvas_displaying: 0,
            on_settings: 0,
            previous_is_settings: 0,
            config_mode: 0,
            provisioning: 0,
        }
    }

    fn call(i: &CInputs) -> CDecision {
        let mut d = CDecision {
            action: 255,
            agree: 255,
            drop_orphan_notification: 255,
            enter_settings: 255,
            switch_to_previous: 255,
        };
        // SAFETY: both pointers are to locals that outlive the call.
        unsafe { rf_input_decide(i, &mut d) };
        d
    }

    #[test]
    fn the_c_side_sees_the_popup_win_over_the_canvas() {
        let i = CInputs { notify_active: 1, canvas_displaying: 1, ..base() };
        let d = call(&i);
        assert_eq!(d.action, RF_INPUT_ACTION_NOTIFY_ACK);
        assert_eq!(d.agree, 1);
    }

    #[test]
    fn the_c_side_sees_down_as_disagree() {
        let i = CInputs { button: 1, notify_active: 1, ..base() };
        assert_eq!(call(&i).agree, 0);
    }

    #[test]
    fn the_c_side_is_told_which_entries_must_drop_a_popup() {
        let i = CInputs { button: 1, gesture: 2, notify_active: 1, ..base() };
        let d = call(&i);
        assert_eq!(d.action, RF_INPUT_ACTION_ENTER_SETTINGS);
        assert_eq!(d.drop_orphan_notification, 1);

        let i = CInputs { button: 2, gesture: 2, canvas_displaying: 1, notify_active: 1, ..base() };
        let d = call(&i);
        assert_eq!(d.action, RF_INPUT_ACTION_STOP_CANVAS);
        assert_eq!(d.drop_orphan_notification, 1);
    }

    #[test]
    fn the_c_side_is_told_to_stop_before_settings_while_provisioning() {
        let i = CInputs { button: 2, gesture: 2, canvas_displaying: 1, provisioning: 1, ..base() };
        let d = call(&i);
        assert_eq!(d.action, RF_INPUT_ACTION_STOP_CANVAS);
        assert_eq!(d.enter_settings, 0);
    }

    #[test]
    fn the_c_side_is_told_when_leaving_settings_cannot_switch_pages() {
        // Without this the C loop would switch to the previous page, which is
        // Settings itself: a no-op the user reads as "the key did nothing".
        let i = CInputs {
            on_settings: 1,
            previous_is_settings: 1,
            gesture: 2,
            ..base()
        };
        let d = call(&i);
        assert_eq!(d.action, RF_INPUT_ACTION_LEAVE_SETTINGS);
        assert_eq!(d.switch_to_previous, 0);

        let i = CInputs { on_settings: 1, gesture: 2, ..base() };
        assert_eq!(call(&i).switch_to_previous, 1);
    }

    #[test]
    fn every_button_and_gesture_code_the_c_side_sends_is_recognised() {
        // A shifted code would silently reroute a key, which is a bug the user
        // sees and the log does not.
        assert_eq!(button_from_code(0), Button::Up);
        assert_eq!(button_from_code(1), Button::Down);
        assert_eq!(button_from_code(2), Button::Boot);
        assert_eq!(gesture_from_code(0), Gesture::Click);
        assert_eq!(gesture_from_code(1), Gesture::DoubleClick);
        assert_eq!(gesture_from_code(2), Gesture::LongPress);
        assert_eq!(gesture_from_code(3), Gesture::ComboLongPress);

        let d = call(&CInputs { gesture: 3, ..base() });
        assert_eq!(d.action, RF_INPUT_ACTION_ENTER_WIFI_CONFIG);
    }
}
