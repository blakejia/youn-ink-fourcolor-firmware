//! The Settings menu: what it holds and where the cursor is.
//!
//! The page is a renderer, and renderers stay in C++ — fonts, framebuffer,
//! theme, the 关于 info panel and the values read off the device (SSID, IP,
//! RSSI, reachability) are all mechanism. What is policy is the shape of the
//! menu and the rules that move a cursor around it, and that is what lives
//! here: two levels, three sections, and a cursor that never comes to rest on
//! a row that cannot be acted on.
//!
//! That last rule is not decoration. A row the user can select but not confirm
//! is how the reported bug felt from the outside — a confirm that went nowhere.

/// `rf_settings_kind_t` in `rust/include/settings.h`. Keep in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// BOOT runs it.
    Action,
    /// BOOT flips it.
    Toggle,
    /// Read-out. Shown, never selected, never confirmed.
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub id: u8,
    pub label: &'static core::ffi::CStr,
    pub kind: Kind,
}

pub struct Section {
    pub label: &'static core::ffi::CStr,
    pub items: &'static [Item],
}

// ── Item ids. The C++ switches on these to fill a value or run an effect, so
// ── they are the whole contract; `every_item_id_is_distinct` holds it.

pub const ITEM_RESTART: u8 = 0;
pub const ITEM_RESET_NETWORK: u8 = 1;
/// Retired as a row while 系统 hides 省电模式. The number stays reserved so the
/// ids below it never move; `application.cc` still handles the id.
pub const ITEM_SLEEP: u8 = 2;
pub const ITEM_WIFI_TOGGLE: u8 = 3;
pub const ITEM_WIFI_STATE: u8 = 4;
pub const ITEM_WIFI_IP: u8 = 5;
pub const ITEM_WIFI_SIGNAL: u8 = 6;
pub const ITEM_SERVER: u8 = 7;
pub const ITEM_WIFI_SSID: u8 = 8;
/// An Action row, unlike its neighbours: the reveal has to be asked for.
pub const ITEM_WIFI_PASSWORD: u8 = 9;
pub const ITEM_WIFI_ERROR: u8 = 10;
/// Wi-Fi credentials *and* the pairing: the device has to be set up again from
/// the provisioning page, code and all.
pub const ITEM_RESET_DEVICE: u8 = 11;

/// Rows that wipe something and therefore ask twice before running. The
/// renderer draws them in the danger colour, the application arms and confirms
/// them, and both read the list from here.
pub const DESTRUCTIVE: &[u8] = &[ITEM_RESET_NETWORK, ITEM_RESET_DEVICE];

pub fn is_destructive(id: u8) -> bool {
    DESTRUCTIVE.contains(&id)
}

/// How long a first press stays armed. Five seconds: long enough to press
/// twice deliberately, short enough that the row is not left armed while the
/// user walks away from it.
pub const CONFIRM_WINDOW_MS: u64 = 5_000;

/// One press on a destructive row: `(armed row, when it was armed, run it now)`.
/// Pressing the armed row again inside the window runs it; pressing anything
/// else (or pressing after the window) arms that row instead, so a stray click
/// can never be the second half of a pair it did not start.
pub fn confirm_step(
    armed: Option<u8>,
    armed_at_ms: u64,
    pressed: u8,
    now_ms: u64,
) -> (Option<u8>, u64, bool) {
    let fresh = armed == Some(pressed) && now_ms.saturating_sub(armed_at_ms) <= CONFIRM_WINDOW_MS;
    (Some(pressed), now_ms, fresh)
}

/// How many dots a masked password draws at most. A longer secret is still
/// covered, just not counted out in full on a panel the room can read.
pub const PASSWORD_MASK_CAP: usize = 16;

/// The number of masking dots for a secret of `len` bytes. Pure, so the rule
/// ("as many dots as the password has characters, up to the cap") is asserted
/// here rather than in the renderer.
pub fn masked_len(len: usize) -> usize {
    len.min(PASSWORD_MASK_CAP)
}

/// Which Wi-Fi name the settings page shows: the one the station is associated
/// with when there is one, and otherwise the saved one the device would try
/// next.
///
/// A device that is not connected still knows where it is meant to connect, so
/// an unconfigured-looking row is a lie — and it is the one row a user checks
/// before taking the device somewhere else. The password row follows this name,
/// which is how the saved key stays visible while the radio is down.
pub fn shown_ssid<'a>(connected: &'a str, saved: &'a str) -> &'a str {
    if connected.is_empty() { saved } else { connected }
}

/// The Wi-Fi row's own state: the switch is a control, not a mirror.
///
/// The row used to draw the connection *and* take its direction from it. With
/// the radio down that made both halves of one press invisible: the switch
/// already read OFF, so the confirm asked for ON, and the redraw asked the
/// connection again and drew OFF. Nothing moved, and the other direction was
/// unreachable — the reported "cannot turn it on or off".
///
/// `Unknown` is the honest state before the user has spoken, and after the
/// device changes the radio on its own (the config AP, the sleep teardown):
/// until then the connection is the only thing that knows anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiSwitch {
    /// Nobody has said; the switch shows the connection.
    Unknown,
    On,
    Off,
}

/// What one confirm on the row asks for: the opposite of what it shows.
pub fn wifi_switch_next(shown: bool) -> bool {
    !shown
}

/// What the switch draws. Once the user has spoken that is the answer, even
/// while the radio is still coming up: the connection has its own row
/// (`连接状态`) and its own failure reason, and a switch that mirrors the radio
/// cannot be used to turn the radio on.
pub fn wifi_switch_shown(intent: WifiSwitch, connected: bool) -> bool {
    match intent {
        WifiSwitch::Unknown => connected,
        WifiSwitch::On => true,
        WifiSwitch::Off => false,
    }
}

use Item as I;

const SYSTEM_ITEMS: &[Item] = &[
    I { id: ITEM_RESTART, label: c"重启", kind: Kind::Action },
    I { id: ITEM_RESET_NETWORK, label: c"重置网络", kind: Kind::Action },
    I { id: ITEM_RESET_DEVICE, label: c"重置设备", kind: Kind::Action },
];

const NETWORK_ITEMS: &[Item] = &[
    I { id: ITEM_WIFI_TOGGLE, label: c"Wi-Fi", kind: Kind::Toggle },
    I { id: ITEM_WIFI_STATE, label: c"连接状态", kind: Kind::Info },
    I { id: ITEM_WIFI_SSID, label: c"Wi-Fi 名称", kind: Kind::Info },
    I { id: ITEM_WIFI_PASSWORD, label: c"Wi-Fi 密码", kind: Kind::Action },
    I { id: ITEM_WIFI_IP, label: c"IP 地址", kind: Kind::Info },
    I { id: ITEM_WIFI_SIGNAL, label: c"信号强度", kind: Kind::Info },
    I { id: ITEM_WIFI_ERROR, label: c"失败原因", kind: Kind::Info },
    I { id: ITEM_SERVER, label: c"服务端", kind: Kind::Info },
];

/// 关于 draws its own info panel; it holds no options, so BOOT does nothing
/// there — which is what "暂时不动" means in the UI.
const ABOUT_ITEMS: &[Item] = &[];

pub const SECTIONS: &[Section] = &[
    Section { label: c"系统", items: SYSTEM_ITEMS },
    Section { label: c"网络", items: NETWORK_ITEMS },
    Section { label: c"关于", items: ABOUT_ITEMS },
];

pub const FOCUS_NAV: u8 = 0;
pub const FOCUS_OPTIONS: u8 = 1;

/// Which pane the cursor is in. Landing in the nav is what makes "press BOOT to
/// enter the second level" the obvious first move after arriving at the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Nav,
    Options,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub section: u8,
    pub focus: Focus,
    pub option: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    Activate(u8),
    Toggle(u8),
}

/// The button that was clicked. Long presses and the combo are routed elsewhere
/// (leaving the page, entering the config AP) and never reach the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Boot,
}

impl Cursor {
    /// Where the page starts: the nav, on the first section.
    pub fn start() -> Cursor {
        Cursor { section: 0, focus: Focus::Nav, option: 0 }
    }
}

fn section(index: u8) -> &'static Section {
    &SECTIONS[index as usize % SECTIONS.len()]
}

/// The first item in `s` the cursor may rest on, if any.
fn first_selectable(s: &Section) -> Option<u8> {
    s.items.iter().position(|i| i.kind != Kind::Info).map(|p| p as u8)
}

fn next_selectable(s: &Section, from: u8) -> Option<u8> {
    let start = from as usize + 1;
    s.items[start.min(s.items.len())..]
        .iter()
        .position(|i| i.kind != Kind::Info)
        .map(|p| (start + p) as u8)
}

fn prev_selectable(s: &Section, from: u8) -> Option<u8> {
    s.items[..(from as usize).min(s.items.len())]
        .iter()
        .rposition(|i| i.kind != Kind::Info)
        .map(|p| p as u8)
}

/// One click. Pure: the C++ performs the effect and hands the cursor back next
/// time.
pub fn step(c: Cursor, b: Button) -> (Cursor, Effect) {
    let last_section = SECTIONS.len() as u8 - 1;
    let index = if c.section <= last_section { c.section } else { 0 };
    let s = &SECTIONS[index as usize];
    let mut out = Cursor { section: index, focus: c.focus, option: c.option };

    match (c.focus, b) {
        (Focus::Nav, Button::Up) => {
            out.section = index.saturating_sub(1);
            (out, Effect::None)
        }
        (Focus::Nav, Button::Down) => {
            out.section = (index + 1).min(last_section);
            (out, Effect::None)
        }
        (Focus::Nav, Button::Boot) => {
            // Entering a section runs nothing by itself; a section with no
            // options (关于) stays in the nav, which is what "not touched" means.
            if let Some(option) = first_selectable(s) {
                out.focus = Focus::Options;
                out.option = option;
            }
            (out, Effect::None)
        }
        (Focus::Options, Button::Up) => match prev_selectable(s, c.option) {
            Some(option) => {
                out.option = option;
                (out, Effect::None)
            }
            // Nothing above the first option: that is the way back out.
            None => {
                out.focus = Focus::Nav;
                (out, Effect::None)
            }
        },
        (Focus::Options, Button::Down) => {
            if let Some(option) = next_selectable(s, c.option) {
                out.option = option;
            }
            (out, Effect::None)
        }
        (Focus::Options, Button::Boot) => match s.items.get(c.option as usize) {
            Some(item) => match item.kind {
                Kind::Action => (out, Effect::Activate(item.id)),
                Kind::Toggle => (out, Effect::Toggle(item.id)),
                // Info rows are read-outs; the cursor is not supposed to rest
                // on one, and a confirm there does nothing rather than pretend.
                Kind::Info => (out, Effect::None),
            },
            None => (out, Effect::None),
        },
    }
}

// ─────────────────────────── C boundary ───────────────────────────

/// `rf_settings_kind_t`. Keep in step.
pub const RF_SETTINGS_KIND_ACTION: u8 = 0;
pub const RF_SETTINGS_KIND_TOGGLE: u8 = 1;
pub const RF_SETTINGS_KIND_INFO: u8 = 2;

/// `rf_settings_focus_t`.
pub const RF_SETTINGS_FOCUS_NAV: u8 = 0;
pub const RF_SETTINGS_FOCUS_OPTIONS: u8 = 1;

/// `rf_settings_effect_t`.
pub const RF_SETTINGS_EFFECT_NONE: u8 = 0;
pub const RF_SETTINGS_EFFECT_ACTIVATE: u8 = 1;
pub const RF_SETTINGS_EFFECT_TOGGLE: u8 = 2;

#[repr(C)]
pub struct CSection {
    pub label: *const core::ffi::c_char,
    pub item_count: u8,
    pub _pad: [u8; 7],
}

#[repr(C)]
pub struct CItem {
    pub id: u8,
    pub kind: u8,
    pub _pad: [u8; 6],
    pub label: *const core::ffi::c_char,
}

#[repr(C)]
pub struct CStep {
    pub section: u8,
    pub focus: u8,
    pub option: u8,
    pub effect: u8,
    pub effect_item: u8,
    pub _pad: [u8; 3],
}

fn kind_code(k: Kind) -> u8 {
    match k {
        Kind::Action => RF_SETTINGS_KIND_ACTION,
        Kind::Toggle => RF_SETTINGS_KIND_TOGGLE,
        Kind::Info => RF_SETTINGS_KIND_INFO,
    }
}


#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_section_count() -> u8 {
    SECTIONS.len() as u8
}

/// How many masking dots to draw for a password of `len` bytes. The renderer
/// picks the glyph; the rule for how many live here.
#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_masked_len(len: usize) -> usize {
    masked_len(len)
}

/// Does `id` wipe something? The renderer colours the row with this.
#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_is_destructive(id: u8) -> u8 {
    if is_destructive(id) { 1 } else { 0 }
}

/// Which name the 网络 section's `Wi-Fi 名称` row draws: `connected` when the
/// station is associated, else `saved` (the head of the device's saved list).
/// Either may be NULL or empty; the caller keeps ownership of both, and one of
/// the two pointers comes back — nothing is allocated.
///
/// # Safety
/// Each non-NULL pointer must be NUL-terminated and stay valid for as long as
/// the returned pointer is used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_settings_shown_ssid(
    connected: *const core::ffi::c_char,
    saved: *const core::ffi::c_char,
) -> *const core::ffi::c_char {
    use core::ffi::CStr;

    let connected_empty = connected.is_null()
        || { unsafe { CStr::from_ptr(connected) }.to_bytes().is_empty() };
    if connected_empty { saved } else { connected }
}

/// `rf_settings_wifi_switch_t`. Keep in step.
pub const RF_SETTINGS_WIFI_SWITCH_UNKNOWN: u8 = 0;
pub const RF_SETTINGS_WIFI_SWITCH_ON: u8 = 1;
pub const RF_SETTINGS_WIFI_SWITCH_OFF: u8 = 2;

/// What a confirm on a toggle row asks for, given what the row drew. The
/// renderer calls this and hands the answer to the item handler, so the
/// handler never has to infer a direction from the connection — which is how
/// the press came to be a no-op in both directions.
#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_wifi_switch_next(shown: u8) -> u8 {
    if wifi_switch_next(shown != 0) { 1 } else { 0 }
}

/// What the switch draws: the user's intent once there is one, else the
/// connection. `intent` is a `rf_settings_wifi_switch_t`.
#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_wifi_switch_shown(intent: u8, connected: u8) -> u8 {
    let intent = match intent {
        RF_SETTINGS_WIFI_SWITCH_ON => WifiSwitch::On,
        RF_SETTINGS_WIFI_SWITCH_OFF => WifiSwitch::Off,
        _ => WifiSwitch::Unknown,
    };
    if wifi_switch_shown(intent, connected != 0) { 1 } else { 0 }
}

/// How long the confirm window is, in milliseconds, for the prompt the panel
/// shows. Exposed so the sentence on the row cannot drift from the rule in
/// `confirm_step`.
#[unsafe(no_mangle)]
pub extern "C" fn rf_settings_confirm_window_ms() -> u64 {
    CONFIRM_WINDOW_MS
}

/// `rf_settings_confirm_t`. Keep in step. `armed_id == 0xFF` means nothing is
/// armed.
#[repr(C)]
pub struct CConfirm {
    pub armed_id: u8,
    pub act: u8,
    pub _pad: [u8; 6],
    pub armed_at_ms: u64,
}

/// One press on a destructive row. The caller keeps `armed_id`/`armed_at_ms`
/// between calls and runs the effect when `act` comes back 1.
///
/// # Safety
/// `out` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_settings_confirm(
    armed_id: u8,
    armed_at_ms: u64,
    pressed_id: u8,
    now_ms: u64,
    out: *mut CConfirm,
) {
    if out.is_null() {
        return;
    }
    let armed = if armed_id == 0xFF { None } else { Some(armed_id) };
    let (next, at, act) = confirm_step(armed, armed_at_ms, pressed_id, now_ms);
    unsafe {
        *out = CConfirm {
            armed_id: next.unwrap_or(0xFF),
            act: if act { 1 } else { 0 },
            _pad: [0; 6],
            armed_at_ms: at,
        };
    }
}

/// # Safety
/// `out` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_settings_get_section(index: u8, out: *mut CSection) {
    let d = unsafe { &mut *out };
    match SECTIONS.get(index as usize) {
        Some(s) => {
            d.label = s.label.as_ptr();
            d.item_count = s.items.len() as u8;
        }
        None => {
            d.label = core::ptr::null();
            d.item_count = 0;
        }
    }
}

/// # Safety
/// `out` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_settings_get_item(section: u8, index: u8, out: *mut CItem) {
    let d = unsafe { &mut *out };
    match SECTIONS
        .get(section as usize)
        .and_then(|s| s.items.get(index as usize))
    {
        Some(i) => {
            d.id = i.id;
            d.kind = kind_code(i.kind);
            d.label = i.label.as_ptr();
        }
        None => {
            d.id = 0xFF;
            d.kind = 0xFF;
            d.label = core::ptr::null();
        }
    }
}

/// # Safety
/// `out` must point to a valid, correctly aligned struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_settings_step(
    section: u8, focus: u8, option: u8, button: u8, out: *mut CStep,
) {
    let focus = if focus == RF_SETTINGS_FOCUS_OPTIONS {
        Focus::Options
    } else {
        Focus::Nav
    };
    // Numbering is `rf_input_button_t` (input.h): 0 up, 1 down, 2 boot.
    let button = match button {
        0 => Button::Up,
        1 => Button::Down,
        _ => Button::Boot,
    };
    let (c, e) = step(Cursor { section, focus, option }, button);
    let d = unsafe { &mut *out };
    d.section = c.section;
    d.focus = if c.focus == Focus::Options {
        RF_SETTINGS_FOCUS_OPTIONS
    } else {
        RF_SETTINGS_FOCUS_NAV
    };
    d.option = c.option;
    match e {
        Effect::None => {
            d.effect = RF_SETTINGS_EFFECT_NONE;
            d.effect_item = 0;
        }
        Effect::Activate(id) => {
            d.effect = RF_SETTINGS_EFFECT_ACTIVATE;
            d.effect_item = id;
        }
        Effect::Toggle(id) => {
            d.effect = RF_SETTINGS_EFFECT_TOGGLE;
            d.effect_item = id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(section: u8) -> Cursor {
        Cursor { section, focus: Focus::Nav, option: 0 }
    }

    fn opts(section: u8, option: u8) -> Cursor {
        Cursor { section, focus: Focus::Options, option }
    }

    // ── structure ───────────────────────────────────────────────────────

    #[test]
    fn the_menu_is_system_network_about_in_that_order() {
        let labels: Vec<String> = SECTIONS.iter().map(|s| s.label.to_str().unwrap().to_string()).collect();
        assert_eq!(labels, vec!["系统", "网络", "关于"]);
    }

    #[test]
    fn system_holds_restart_and_the_two_resets() {
        let labels: Vec<String> = SYSTEM_ITEMS.iter().map(|i| i.label.to_str().unwrap().to_string()).collect();
        assert_eq!(labels, vec!["重启", "重置网络", "重置设备"]);
        assert!(SYSTEM_ITEMS.iter().all(|i| i.kind == Kind::Action));
        // 省电模式 is hidden for now; the entry point it drove is still wired.
        assert!(SYSTEM_ITEMS.iter().all(|i| i.id != ITEM_SLEEP));
    }

    #[test]
    fn only_the_two_resets_are_destructive() {
        let dangerous: Vec<u8> = SECTIONS
            .iter()
            .flat_map(|s| s.items.iter())
            .filter(|i| is_destructive(i.id))
            .map(|i| i.id)
            .collect();
        assert_eq!(dangerous, vec![ITEM_RESET_NETWORK, ITEM_RESET_DEVICE]);
        assert!(!is_destructive(ITEM_RESTART));
        assert!(!is_destructive(ITEM_WIFI_PASSWORD));
    }

    // ── the two-press confirm ───────────────────────────────────────────

    #[test]
    fn the_first_press_arms_and_the_second_inside_the_window_acts() {
        let (armed, at, acts) = confirm_step(None, 0, ITEM_RESET_DEVICE, 1_000);
        assert!(!acts, "the first press only arms");
        assert_eq!(armed, Some(ITEM_RESET_DEVICE));
        assert_eq!(at, 1_000);

        let (again, _, acts) = confirm_step(armed, at, ITEM_RESET_DEVICE, 5_000);
        assert!(acts, "the second press inside the window runs it");
        assert_eq!(again, Some(ITEM_RESET_DEVICE), "the arm is kept for its own timeout");
    }

    #[test]
    fn a_press_after_the_window_arms_again_instead_of_running() {
        let (armed, at, _) = confirm_step(None, 0, ITEM_RESET_DEVICE, 1_000);
        let (next, next_at, acts) = confirm_step(armed, at, ITEM_RESET_DEVICE, 1_000 + CONFIRM_WINDOW_MS + 1);
        assert!(!acts, "a stale arm must not run anything");
        assert_eq!(next, Some(ITEM_RESET_DEVICE));
        assert_eq!(next_at, 1_000 + CONFIRM_WINDOW_MS + 1);
    }

    /// The window is the user's rule, so it is pinned with literals: a second
    /// press just inside it acts, just outside it only arms again.
    #[test]
    fn the_confirm_window_is_five_seconds() {
        let (armed, at, _) = confirm_step(None, 0, ITEM_RESET_NETWORK, 0);
        let (_, _, acts) = confirm_step(armed, at, ITEM_RESET_NETWORK, 4_999);
        assert!(acts, "4.999 s is inside the window");

        let (armed, at, _) = confirm_step(None, 0, ITEM_RESET_NETWORK, 0);
        let (_, _, acts) = confirm_step(armed, at, ITEM_RESET_NETWORK, 5_001);
        assert!(!acts, "5.001 s is outside the window");
    }

    #[test]
    fn pressing_another_row_re_arms_rather_than_running_the_armed_one() {
        let (armed, at, _) = confirm_step(None, 0, ITEM_RESET_NETWORK, 1_000);
        let (next, _, acts) = confirm_step(armed, at, ITEM_RESET_DEVICE, 2_000);
        assert!(!acts, "RESET_NETWORK was armed; RESET_DEVICE is not a confirmation of it");
        assert_eq!(next, Some(ITEM_RESET_DEVICE));
    }

    #[test]
    fn network_shows_the_wifi_switch_and_the_current_network_status() {
        let labels: Vec<String> = NETWORK_ITEMS.iter().map(|i| i.label.to_str().unwrap().to_string()).collect();
        assert_eq!(
            labels,
            vec!["Wi-Fi", "连接状态", "Wi-Fi 名称", "Wi-Fi 密码", "IP 地址", "信号强度", "失败原因", "服务端"]
        );
        // Two rows can be acted on: the switch, and the password reveal. Every
        // other row is a read-out the cursor never rests on.
        let actionable: Vec<u8> = NETWORK_ITEMS
            .iter()
            .filter(|i| i.kind != Kind::Info)
            .map(|i| i.id)
            .collect();
        assert_eq!(actionable, vec![ITEM_WIFI_TOGGLE, ITEM_WIFI_PASSWORD]);
        assert_eq!(NETWORK_ITEMS[0].kind, Kind::Toggle);
        assert_eq!(NETWORK_ITEMS[3].kind, Kind::Action);
    }

    // ── what the 网络 rows show ─────────────────────────────────────────

    /// The reported complaint: with the radio down the SSID and password rows
    /// read "--", so the one place a user checks before taking the device
    /// somewhere else answered nothing.
    #[test]
    fn an_unconnected_device_still_shows_the_saved_network() {
        assert_eq!(shown_ssid("", "yi02"), "yi02");
        assert_eq!(shown_ssid("", ""), "", "nothing saved either: the row is empty");
    }

    #[test]
    fn a_live_connection_wins_over_the_saved_name() {
        assert_eq!(shown_ssid("cafe-guest", "yi02"), "cafe-guest");
        assert_eq!(shown_ssid("cafe-guest", ""), "cafe-guest");
    }

    // ── the Wi-Fi row's own state ───────────────────────────────────────

    #[test]
    fn a_press_flips_the_row_whatever_the_radio_is_doing() {
        assert!(wifi_switch_next(false), "OFF -> ON");
        assert!(!wifi_switch_next(true), "ON -> OFF");
    }

    /// The reported bug, as a test: with no connection the row read OFF, the
    /// confirm asked for ON, and the redraw read the connection again and drew
    /// OFF — a press that went nowhere, in the one direction that could have
    /// brought the radio back.
    #[test]
    fn turning_the_row_on_does_not_need_a_connection() {
        let shown = wifi_switch_shown(WifiSwitch::Unknown, false);
        assert!(!shown, "nothing connected yet, the row reads OFF");

        let target = wifi_switch_next(shown);
        assert!(target, "confirming an OFF row asks for ON");
        assert!(
            wifi_switch_shown(WifiSwitch::On, false),
            "the row must show what was asked, not what the radio is doing"
        );
    }

    /// And the other half: a row the user switched off stays off while the
    /// radio is still finishing a connection it started a moment ago.
    #[test]
    fn a_switch_off_row_does_not_read_on_when_the_radio_connects() {
        assert!(!wifi_switch_shown(WifiSwitch::Off, true));
    }

    #[test]
    fn the_connection_answers_only_before_the_user_does() {
        assert!(wifi_switch_shown(WifiSwitch::Unknown, true));
        assert!(!wifi_switch_shown(WifiSwitch::Unknown, false));
    }

    #[test]
    fn boot_on_the_password_row_asks_to_reveal_it() {
        let (c, e) = step(opts(1, 3), Button::Boot);
        assert_eq!(e, Effect::Activate(ITEM_WIFI_PASSWORD));
        assert_eq!(c, opts(1, 3), "revealing does not move the cursor");
    }

    #[test]
    fn the_cursor_walks_from_the_switch_straight_to_the_password_row() {
        // Index 1 and 2 are read-outs, so DOWN must skip them.
        let (c, e) = step(opts(1, 0), Button::Down);
        assert_eq!(c, opts(1, 3));
        assert_eq!(e, Effect::None);
    }

    // ── the masked password ─────────────────────────────────────────────

    #[test]
    fn the_mask_is_as_long_as_the_password_up_to_the_cap() {
        assert_eq!(masked_len(0), 0);
        assert_eq!(masked_len(8), 8);
        assert_eq!(masked_len(11), 11, "titi10-102 shows eleven dots");
        assert_eq!(masked_len(PASSWORD_MASK_CAP), PASSWORD_MASK_CAP);
        assert_eq!(masked_len(64), PASSWORD_MASK_CAP, "a long secret is capped, not counted out");
    }

    #[test]
    fn about_has_no_options_of_its_own() {
        assert!(ABOUT_ITEMS.is_empty());
    }

    #[test]
    fn every_item_id_is_distinct() {
        let mut ids: Vec<u8> = SECTIONS
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.id))
            .collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "the C++ switches on these ids");
    }

    // ── nav pane ────────────────────────────────────────────────────────

    #[test]
    fn the_page_starts_in_the_nav_on_the_first_section() {
        let c = Cursor::start();
        assert_eq!(c.focus, Focus::Nav);
        assert_eq!(c.section, 0);
    }

    #[test]
    fn up_and_down_move_between_sections() {
        let (c, e) = step(nav(0), Button::Down);
        assert_eq!((c.section, c.focus), (1, Focus::Nav));
        assert_eq!(e, Effect::None);
        let (c, _) = step(nav(1), Button::Up);
        assert_eq!(c.section, 0);
    }

    #[test]
    fn the_nav_clamps_at_the_ends() {
        assert_eq!(step(nav(0), Button::Up).0.section, 0);
        let last = (SECTIONS.len() - 1) as u8;
        assert_eq!(step(nav(last), Button::Down).0.section, last);
    }

    #[test]
    fn boot_in_the_nav_enters_the_sections_first_option() {
        let (c, e) = step(nav(0), Button::Boot);
        assert_eq!((c.focus, c.option), (Focus::Options, 0));
        assert_eq!(e, Effect::None, "entering a section does not run anything");
    }

    #[test]
    fn boot_in_the_nav_of_about_stays_in_the_nav() {
        let about = (SECTIONS.len() - 1) as u8;
        let (c, e) = step(nav(about), Button::Boot);
        assert_eq!(c.focus, Focus::Nav);
        assert_eq!(e, Effect::None);
    }

    // ── options pane ────────────────────────────────────────────────────

    #[test]
    fn up_at_the_first_option_returns_to_the_nav() {
        let (c, e) = step(opts(0, 0), Button::Up);
        assert_eq!(c.focus, Focus::Nav);
        assert_eq!(e, Effect::None);
    }

    #[test]
    fn down_moves_between_options_and_clamps() {
        let (c, _) = step(opts(0, 0), Button::Down);
        assert_eq!(c.option, 1);
        let (c, _) = step(opts(0, 1), Button::Up);
        assert_eq!(c.option, 0);
    }

    #[test]
    fn the_cursor_skips_info_rows_and_stops_at_the_last_option() {
        // 网络: 0 is the toggle, 1–2 are read-outs, 3 is the password reveal,
        // and everything after that is a read-out again.
        let (c, _) = step(opts(1, 0), Button::Down);
        assert_eq!(c.option, 3, "the read-outs between the two options are skipped");
        assert_eq!(first_selectable(&SECTIONS[1]), Some(0));
        assert_eq!(next_selectable(&SECTIONS[1], 0), Some(3));
        assert_eq!(next_selectable(&SECTIONS[1], 3), None, "the password is the last option");
    }

    #[test]
    fn boot_on_an_action_activates_that_item() {
        let (c, e) = step(opts(0, 1), Button::Boot);
        assert_eq!(e, Effect::Activate(ITEM_RESET_NETWORK));
        assert_eq!(c, opts(0, 1), "confirming does not move the cursor");
    }

    #[test]
    fn boot_on_a_toggle_asks_for_a_toggle() {
        let (_, e) = step(opts(1, 0), Button::Boot);
        assert_eq!(e, Effect::Toggle(ITEM_WIFI_TOGGLE));
    }

    // ── the C side ──────────────────────────────────────────────────────

    #[test]
    fn the_switch_codes_the_c_side_sends_are_the_ones_this_side_means() {
        // The C side keeps the intent as a byte, so this mapping *is* the
        // contract: get it wrong and a switched-off radio draws as ON.
        assert_eq!(rf_settings_wifi_switch_shown(RF_SETTINGS_WIFI_SWITCH_OFF, 1), 0);
        assert_eq!(rf_settings_wifi_switch_shown(RF_SETTINGS_WIFI_SWITCH_ON, 0), 1);
        assert_eq!(rf_settings_wifi_switch_shown(RF_SETTINGS_WIFI_SWITCH_UNKNOWN, 1), 1);
        assert_eq!(
            rf_settings_wifi_switch_shown(0xEE, 0),
            0,
            "an unknown code falls back to the connection"
        );
        assert_eq!(rf_settings_wifi_switch_next(0), 1);
        assert_eq!(rf_settings_wifi_switch_next(1), 0);
    }

    fn step_c(section: u8, focus: u8, option: u8, button: u8) -> CStep {
        let mut d = CStep {
            section: 255,
            focus: 255,
            option: 255,
            effect: 255,
            effect_item: 255,
            _pad: [0; 3],
        };
        // SAFETY: `d` outlives the call.
        unsafe { rf_settings_step(section, focus, option, button, &mut d) };
        d
    }

    #[test]
    fn the_button_codes_are_the_ones_the_input_router_sends() {
        // rf_input_button_t: RF_INPUT_UP = 0, RF_INPUT_DOWN = 1, RF_INPUT_BOOT = 2.
        assert_eq!(step_c(0, RF_SETTINGS_FOCUS_NAV, 0, 1).section, 1);
        assert_eq!(step_c(1, RF_SETTINGS_FOCUS_NAV, 0, 0).section, 0);
        assert_eq!(step_c(0, RF_SETTINGS_FOCUS_NAV, 0, 2).focus, RF_SETTINGS_FOCUS_OPTIONS);
    }

    #[test]
    fn every_kind_the_c_side_switches_on_is_reachable() {
        let mut seen = Vec::new();
        for (si, s) in SECTIONS.iter().enumerate() {
            for i in 0..s.items.len() as u8 {
                let mut d = CItem { id: 0, kind: 0, _pad: [0; 6], label: core::ptr::null() };
                // SAFETY: `d` outlives the call.
                unsafe { rf_settings_get_item(si as u8, i, &mut d) };
                seen.push((d.id, d.kind));
                assert!(!d.label.is_null());
                let label = unsafe { core::ffi::CStr::from_ptr(d.label) };
                assert_eq!(label, s.items[i as usize].label);
            }
        }
        assert!(seen.iter().any(|(_, k)| *k == RF_SETTINGS_KIND_ACTION));
        assert!(seen.iter().any(|(_, k)| *k == RF_SETTINGS_KIND_TOGGLE));
        assert!(seen.iter().any(|(_, k)| *k == RF_SETTINGS_KIND_INFO));
    }

    #[test]
    fn the_labels_cross_as_c_strings_the_renderer_can_draw() {
        assert_eq!(rf_settings_section_count() as usize, SECTIONS.len());
        for (i, s) in SECTIONS.iter().enumerate() {
            let mut d = CSection { label: core::ptr::null(), item_count: 255, _pad: [0; 7] };
            // SAFETY: `d` outlives the call.
            unsafe { rf_settings_get_section(i as u8, &mut d) };
            let label = unsafe { core::ffi::CStr::from_ptr(d.label) };
            assert_eq!(label, s.label);
            assert_eq!(d.item_count as usize, s.items.len());
        }
    }

    #[test]
    fn an_out_of_range_lookup_returns_nothing_instead_of_reading_past_the_table() {
        let mut d = CSection { label: core::ptr::null(), item_count: 255, _pad: [0; 7] };
        // SAFETY: `d` outlives the call.
        unsafe { rf_settings_get_section(200, &mut d) };
        assert!(d.label.is_null());
        assert_eq!(d.item_count, 0);

        let mut d = CItem { id: 0, kind: 0, _pad: [0; 6], label: core::ptr::null() };
        // SAFETY: `d` outlives the call.
        unsafe { rf_settings_get_item(200, 200, &mut d) };
        assert!(d.label.is_null());
    }

    #[test]
    fn a_section_with_only_info_rows_never_takes_the_cursor() {
        // The read-outs past the last option are no more reachable than the
        // ones between: DOWN from the password row stays where it is, and the
        // row it stays on is not an Info row.
        let (c, _) = step(opts(1, 3), Button::Down);
        assert_eq!(c.option, 3);
        assert!(NETWORK_ITEMS[c.option as usize].kind != Kind::Info);
    }
}
