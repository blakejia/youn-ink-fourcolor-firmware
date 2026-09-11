//! Canvas Loop page sync: poll the server schedule, cache bitmaps in PSRAM and
//! rotate them onto the panel.
//!
//! Port of `main/common/page_sync.cc`. Two things are deliberately different:
//!
//! * the page table is guarded by a real mutex instead of being raced on, and
//! * `displaying`/`suspended` are lock-free atomics, because renderers read them
//!   while already holding the display mutex. That keeps a single lock order
//!   (`state -> display`) with no path going the other way.
//!
//! The C++ version's `manual_hold` fields were written but never read by the
//! rotation loop, i.e. dead state; they are not carried over.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::json;
use crate::{log_e, log_i, log_w};

use crate::shim::{self, CBuf};

/// 400x300 at 2 bpp.
pub const PAGE_BITMAP_SIZE: usize = 30000;
const MAX_PAGES: usize = 5;
const MD5_LEN: usize = 32;
/// How often the task wakes to check rotation. Purely local (no radio): the
/// network poll runs on the server's `poll_interval_minutes` on top of this, so
/// a page turn still lands on time instead of at poll granularity.
const TICK_MS: u32 = 10_000;
/// Fallbacks when the server sends no policy: the old hardcoded 10 s poll was
/// 60x the server's intent and kept the radio up all day.
const DEFAULT_POLL_S: u32 = 600;
const DEFAULT_SLEEP_POLL_S: u32 = 3600;
const MAX_POLL_MINUTES: i64 = 1440;
const SCHEDULE_TIMEOUT_MS: i32 = 10_000;
const BITMAP_TIMEOUT_MS: i32 = 15_000;
const SCHEDULE_BUF: usize = 8192;
const TASK_STACK_BYTES: u32 = 8192;
const TASK_PRIORITY: u8 = 3;

// ── screen ownership (lock-free: read under the display mutex) ──
static DISPLAYING: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// Whether the last schedule poll reached the server (status-bar indicator).
static SERVER_REACHABLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Page {
    md5: [u8; MD5_LEN],
    duration_s: u32,
    bitmap: *mut u8,
}

impl Page {
    const fn empty() -> Self {
        Page { md5: [0; MD5_LEN], duration_s: 0, bitmap: core::ptr::null_mut() }
    }
    fn is_ram(&self) -> bool {
        !self.bitmap.is_null()
    }
}

struct Table {
    pages: [Page; MAX_PAGES],
    count: usize,
    schedule_md5: [u8; MD5_LEN],
    have_schedule_md5: bool,
    current: usize,
    started_us: u64,
    empty_hint_shown: bool,
    running: bool,
    /// Server policy (see [`Policy`]).
    policy: Policy,
}

impl Table {
    const fn new() -> Self {
        Table {
            pages: [Page::empty(); MAX_PAGES],
            count: 0,
            schedule_md5: [0; MD5_LEN],
            have_schedule_md5: false,
            current: 0,
            started_us: 0,
            empty_hint_shown: false,
            running: false,
            policy: Policy::DEFAULT,
        }
    }

    /// Free every cached bitmap. Caller holds the lock.
    fn free_pages(&mut self) {
        for i in 0..self.count {
            if !self.pages[i].bitmap.is_null() {
                unsafe { shim::rf_free(self.pages[i].bitmap) };
            }
            self.pages[i] = Page::empty();
        }
        self.count = 0;
    }
}

struct Shared(UnsafeCell<Table>);
// SAFETY: every access goes through `with_table`, which holds the state mutex.
unsafe impl Sync for Shared {}

static TABLE: Shared = Shared(UnsafeCell::new(Table::new()));

fn with_table<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    unsafe { shim::rf_state_lock() };
    let out = f(unsafe { &mut *TABLE.0.get() });
    unsafe { shim::rf_state_unlock() };
    out
}

/// Drop all cached state so a host test starts from a cold boot.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    with_table(|t| {
        t.free_pages();
        *t = Table::new();
    });
    DISPLAYING.store(false, Ordering::Release);
    SUSPENDED.store(false, Ordering::Release);
}

fn now_us() -> u64 {
    unsafe { shim::rf_now_us() }
}

// ── fetching ───────────────────────────────────────────────────────────────

/// GET `/api/pages/schedule` into `buf`; returns the body length.
///
/// The caller must hold no lock: `rf_build_endpoint`/HTTP can block for seconds.
fn fetch_schedule(buf: &mut [u8]) -> Option<usize> {
    let mut path = CBuf::<64>::new();
    path.push("/api/pages/schedule");
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!("PageSync", "cannot build schedule endpoint");
        return None;
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };

    let mut len = buf.len() as i32;
    let status = unsafe {
        shim::rf_http_get(url.as_ptr(), token.as_ptr(), buf.as_mut_ptr(), &mut len,
                          SCHEDULE_TIMEOUT_MS)
    };
    if status != 200 || len < 0 {
        SERVER_REACHABLE.store(false, Ordering::Release);
        log_w!("PageSync", "schedule fetch failed (status={})", status);
        return None;
    }
    SERVER_REACHABLE.store(true, Ordering::Release);
    Some(len as usize)
}

/// Download one bitmap straight into its PSRAM slot.
///
/// The slot is one byte larger than the panel: `http_wrapper_get` NUL-terminates
/// at `out_buf[len]`, so passing exactly `PAGE_BITMAP_SIZE` would write one byte
/// past the allocation (see the http_client_wrapper audit note).
fn download_bitmap(md5: &[u8; MD5_LEN], out: *mut u8) -> bool {
    let mut path = CBuf::<96>::new();
    path.push("/api/pages/bitmap/");
    path.push_bytes(md5);
    path.push(".bin");
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        return false;
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };

    // Capacity includes the wrapper's terminator, so ask for one byte more than
    // the payload; the wrapper caps the body at capacity - 1.
    let mut len = (PAGE_BITMAP_SIZE + 1) as i32;
    let status = unsafe {
        shim::rf_http_get(url.as_ptr(), token.as_ptr(), out as *mut core::ffi::c_char, &mut len,
                          BITMAP_TIMEOUT_MS)
    };
    let ok = status == 200 && len as usize == PAGE_BITMAP_SIZE;
    if !ok {
        log_w!("PageSync", "bitmap status={} len={}", status, len);
    }
    ok
}

/// Copy the md5 value out of the schedule JSON.
fn md5_from(value: &[u8]) -> Option<[u8; MD5_LEN]> {
    if value.len() != MD5_LEN {
        return None;
    }
    let mut out = [0u8; MD5_LEN];
    out.copy_from_slice(value);
    Some(out)
}

/// One page as it appears in the schedule JSON.
#[derive(Clone, Copy)]
pub struct ParsedPage {
    pub md5: [u8; MD5_LEN],
    pub duration_s: u32,
}

/// The schedule response, parsed.
pub struct ParsedSchedule {
    pub md5: [u8; MD5_LEN],
    pub pages: [ParsedPage; MAX_PAGES],
    pub count: usize,
    pub policy: Policy,
}

const EMPTY_PAGE: ParsedPage = ParsedPage { md5: [0; MD5_LEN], duration_s: 0 };

/// The `policy` block of `/api/pages/schedule`, plus `screen_active`.
///
/// The server owns the schedule: poll cadence, the sleep window and whether the
/// canvas should be on at all. The firmware used to hardcode both cadences.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Policy {
    pub poll_s: u32,
    pub sleep_poll_s: u32,
    /// False while the server's sleep window is open.
    pub screen_active: bool,
}

impl Policy {
    const DEFAULT: Policy = Policy {
        poll_s: DEFAULT_POLL_S,
        sleep_poll_s: DEFAULT_SLEEP_POLL_S,
        screen_active: true,
    };
}

fn minutes_to_s(minutes: Option<i64>, fallback: u32) -> u32 {
    match minutes {
        Some(m) if m > 0 => (m.min(MAX_POLL_MINUTES) as u32) * 60,
        _ => fallback,
    }
}

/// Read the policy out of a schedule body. Pure, so it is unit-tested on the host.
///
/// Every field falls back to the previous default rather than failing: a server
/// that stops sending policy must not stop the device from polling.
pub fn parse_policy(body: &[u8]) -> Policy {
    let poll_s = minutes_to_s(
        json::path(body, &["policy", "poll_interval_minutes"]).and_then(|at| json::int_value(body, at)),
        DEFAULT_POLL_S,
    );
    let sleep_poll_s = minutes_to_s(
        json::path(body, &["policy", "sleep_poll_interval_minutes"])
            .and_then(|at| json::int_value(body, at)),
        DEFAULT_SLEEP_POLL_S,
    );
    let screen_active = json::member(body, 0, "screen_active")
        .and_then(|at| json::bool_value(body, at))
        .unwrap_or(true);
    Policy { poll_s, sleep_poll_s, screen_active }
}

/// Parse `/api/pages/schedule`. Pure, so it is unit-tested on the host.
///
/// Entries without a usable md5/duration are skipped rather than failing the
/// whole response, and anything past [`MAX_PAGES`] is ignored.
pub fn parse_schedule(body: &[u8]) -> Option<ParsedSchedule> {
    let md5 = json::member(body, 0, "schedule_md5")
        .and_then(|at| json::str_value(body, at))
        .and_then(md5_from)?;
    let pages_at = json::member(body, 0, "pages")?;

    let mut out =
        ParsedSchedule { md5, pages: [EMPTY_PAGE; MAX_PAGES], count: 0, policy: parse_policy(body) };
    json::for_each_item(body, pages_at, &mut |item| {
        if out.count >= MAX_PAGES {
            return false;
        }
        let Some(page_md5) = json::member(body, item, "md5")
            .and_then(|at| json::str_value(body, at))
            .and_then(md5_from)
        else {
            return true;
        };
        let Some(dur_min) = json::member(body, item, "duration_minutes")
            .and_then(|at| json::int_value(body, at))
        else {
            return true;
        };
        out.pages[out.count] = ParsedPage {
            md5: page_md5,
            duration_s: dur_min.max(0) as u32 * 60,
        };
        out.count += 1;
        true
    })?;
    Some(out)
}

/// One poll: refresh the page table when the schedule changed.
fn sync_once() {
    // PSRAM, not the stack: this buffer is as large as the whole task stack
    // (the C++ original kept it in a file-scope static for the same reason).
    // +1 because `http_wrapper_get` NUL-terminates one byte past the length.
    let raw = unsafe { shim::rf_alloc(SCHEDULE_BUF + 1) };
    if raw.is_null() {
        log_e!("PageSync", "schedule buffer alloc failed");
        return;
    }
    // Same one-byte-over-allocation as the notify response: the wrapper's
    // capacity includes the terminator it appends.
    let body = unsafe { core::slice::from_raw_parts_mut(raw, SCHEDULE_BUF + 1) };
    if let Some(len) = fetch_schedule(body) {
        sync_schedule(&body[..len]);
    }
    unsafe { shim::rf_free(raw) };
}

/// Apply a fetched `/api/pages/schedule` body.
fn sync_schedule(body: &[u8]) {
    let Some(parsed) = parse_schedule(body) else {
        log_w!("PageSync", "schedule json unusable (missing schedule_md5/pages)");
        return;
    };
    let new_md5 = parsed.md5;
    let new_policy = parsed.policy;

    let policy_changed = with_table(|t| {
        let prev = t.policy;
        t.policy = new_policy;
        prev != new_policy
    });
    if policy_changed {
        log_i!("PageSync", "policy: poll={}s sleep_poll={}s screen_active={}",
            new_policy.poll_s, new_policy.sleep_poll_s, new_policy.screen_active);
    }

    // 99% path: nothing changed.
    let unchanged = with_table(|t| t.have_schedule_md5 && t.schedule_md5 == new_md5);
    if unchanged {
        return;
    }

    let new_count = parsed.count;
    let mut new_pages = [Page::empty(); MAX_PAGES];
    for i in 0..new_count {
        new_pages[i].md5 = parsed.pages[i].md5;
        new_pages[i].duration_s = parsed.pages[i].duration_s;
    }

    // Reuse cached bitmaps with the same md5, download the rest.
    let mut downloaded = 0;
    for i in 0..new_count {
        let reused = with_table(|t| {
            for j in 0..t.count {
                if t.pages[j].is_ram() && t.pages[j].md5 == new_pages[i].md5 {
                    let bmp = t.pages[j].bitmap;
                    t.pages[j].bitmap = core::ptr::null_mut(); // ownership moves
                    return Some(bmp);
                }
            }
            None
        });
        if let Some(bmp) = reused {
            new_pages[i].bitmap = bmp;
            continue;
        }
        // +1 for the wrapper's terminator.
        let slot = unsafe { shim::rf_alloc(PAGE_BITMAP_SIZE + 1) };
        if slot.is_null() {
            log_e!("PageSync", "bitmap alloc failed");
            continue;
        }
        if download_bitmap(&new_pages[i].md5, slot) {
            new_pages[i].bitmap = slot;
            downloaded += 1;
        } else {
            log_w!("PageSync", "bitmap download failed: {:?}", core::str::from_utf8(&new_pages[i].md5).unwrap_or("?"));
            unsafe { shim::rf_free(slot) };
        }
    }

    let all_ready = new_pages[..new_count].iter().all(|p| p.is_ram());

    with_table(|t| {
        t.free_pages();
        t.pages = new_pages;
        t.count = new_count;
        if all_ready {
            // Only commit the schedule md5 once every bitmap is on RAM: the old
            // code stored it unconditionally, so a failed download was skipped
            // forever by the "unchanged" fast path.
            t.schedule_md5 = new_md5;
            t.have_schedule_md5 = true;
            t.current = 0;
            t.started_us = now_us();
        }
    });

    log_i!("PageSync", "schedule updated: {} pages, {} downloaded{}",
        new_count,
        downloaded,
        if all_ready { "" } else { " (retrying missing bitmaps)" }
    );
}

// ── drawing ────────────────────────────────────────────────────────────────

fn show_page(index: usize) {
    if SUSPENDED.load(Ordering::Acquire) {
        return;
    }
    // The blit runs with the state lock held (lock order: state -> display), so
    // a concurrent `sync_once` cannot free the bitmap we are copying from.
    let md5 = with_table(|t| {
        if index >= t.count || !t.pages[index].is_ram() {
            return None;
        }
        let page = t.pages[index];
        let fb = unsafe { shim::rf_fb_begin() };
        let fb_len = unsafe { shim::rf_fb_len() } as usize;
        if !fb.is_null() && fb_len == PAGE_BITMAP_SIZE {
            // Same 2bpp MSB-first pixel order as the server's packer: plain copy.
            unsafe { core::ptr::copy_nonoverlapping(page.bitmap, fb, PAGE_BITMAP_SIZE) };
        } else {
            log_e!("PageSync", "framebuffer size mismatch: fb_len={} want={}", fb_len, PAGE_BITMAP_SIZE);
        }
        unsafe { shim::rf_fb_end() };
        let mut head = [0u8; 8];
        head.copy_from_slice(&page.md5[..8]);
        Some(head)
    });
    let Some(md5) = md5 else { return };

    unsafe { shim::rf_request_full_refresh() };
    DISPLAYING.store(true, Ordering::Release);
    let count = with_table(|t| t.count);
    log_i!("PageSync", "show page {}/{} md5={:?}",
        index + 1,
        count,
        core::str::from_utf8(&md5).unwrap_or("?")
    );
}

fn show_empty_hint() {
    if SUSPENDED.load(Ordering::Acquire) {
        return;
    }
    unsafe { shim::rf_draw_empty_hint() };
    unsafe { shim::rf_request_full_refresh() };
    DISPLAYING.store(true, Ordering::Release);
    log_i!("PageSync", "show empty page hint (no pages configured)");
}

fn show_current() {
    let (count, current) = with_table(|t| (t.count, t.current));
    if count > 0 {
        show_page(current);
    } else {
        show_empty_hint();
        with_table(|t| t.empty_hint_shown = true);
    }
}

// ── public API (C ABI used by the firmware) ────────────────────────────────

pub fn set_display(display: *mut c_void) {
    unsafe { shim::rf_set_display(display) };
}

pub fn is_displaying() -> bool {
    DISPLAYING.load(Ordering::Acquire)
}

/// Whether the last schedule poll reached the server.
pub fn server_reachable() -> bool {
    SERVER_REACHABLE.load(Ordering::Acquire)
}

/// Hand the screen to the UI/notification: the canvas stops drawing until
/// [`resume_display`] or [`allow_display`].
pub fn stop_display() {
    SUSPENDED.store(true, Ordering::Release);
    DISPLAYING.store(false, Ordering::Release);
}

/// Take the screen back and repaint the current page (notification dismissed).
pub fn resume_display() {
    SUSPENDED.store(false, Ordering::Release);
    DISPLAYING.store(true, Ordering::Release);
}

/// Redraw the current page, or the empty hint when no page is configured.
pub fn redraw_current() {
    show_current();
}

/// Let the canvas take over again (leaving Settings). Repaints immediately so
/// the user does not stare at the UI page for a whole rotation interval.
pub fn allow_display() {
    if !SUSPENDED.swap(false, Ordering::AcqRel) {
        return;
    }
    show_current();
}

pub fn next() {
    let moved = with_table(|t| {
        if t.count == 0 {
            return None;
        }
        t.current = (t.current + 1) % t.count;
        t.started_us = now_us();
        Some(t.current)
    });
    if let Some(index) = moved {
        show_page(index);
    }
}

pub fn prev() {
    let moved = with_table(|t| {
        if t.count == 0 {
            return None;
        }
        t.current = (t.current + t.count - 1) % t.count;
        t.started_us = now_us();
        Some(t.current)
    });
    if let Some(index) = moved {
        show_page(index);
    }
}

pub fn start() {
    // Starting the canvas means the canvas owns the screen again.
    SUSPENDED.store(false, Ordering::Release);
    let already = with_table(|t| t.running);
    if already {
        return;
    }
    with_table(|t| t.running = true);
    let created = unsafe {
        shim::rf_task_create(
            task_entry,
            c"page_sync".as_ptr(),
            TASK_STACK_BYTES,
            TASK_PRIORITY,
            core::ptr::null_mut(),
        )
    };
    if created != 0 {
        log_e!("PageSync", "failed to create page_sync task");
        with_table(|t| t.running = false);
    }
}

extern "C" fn task_entry(_arg: *mut c_void) {
    let mut first = true;
    let mut last_poll_us: u64 = 0;
    loop {
        if !with_table(|t| t.running) {
            break;
        }

        // The server owns the poll cadence (and slows it down inside its sleep
        // window); the tick above only drives rotation and painting.
        let (poll_us, screen_active) = with_table(|t| {
            let interval = if t.policy.screen_active { t.policy.poll_s } else { t.policy.sleep_poll_s };
            (interval as u64 * 1_000_000, t.policy.screen_active)
        });
        let now = now_us();
        if last_poll_us == 0 || now.saturating_sub(last_poll_us) >= poll_us {
            sync_once();
            last_poll_us = now_us();
        }
        if first {
            // Measured after the deepest path (schedule fetch + bitmap
            // downloads) has returned; a tight margin here means the stack
            // constants at the top of this file need revisiting.
            log_i!("PageSync", "stack headroom {} bytes", unsafe {
                shim::rf_task_stack_free()
            });
        }

        if SUSPENDED.load(Ordering::Acquire) {
            // Keep the data fresh but do not touch the panel: the UI owns it.
            unsafe { shim::rf_delay_ms(TICK_MS) };
            continue;
        }

        if !screen_active && !first {
            // Sleep window: keep polling, but spend no panel cycles (a
            // four-colour refresh is >= 15 s with the radio up). The first paint
            // after boot still happens, so a wake press shows content.
            unsafe { shim::rf_delay_ms(TICK_MS) };
            continue;
        }

        let count = with_table(|t| t.count);
        if count == 0 {
            if !with_table(|t| t.empty_hint_shown) {
                show_empty_hint();
                with_table(|t| t.empty_hint_shown = true);
            }
        } else {
            with_table(|t| t.empty_hint_shown = false);
            let (elapsed, dur) = with_table(|t| {
                (now_us().saturating_sub(t.started_us), t.pages[t.current].duration_s)
            });
            if dur > 0 && elapsed >= dur as u64 * 1_000_000 {
                with_table(|t| {
                    t.current = (t.current + 1) % t.count;
                    t.started_us = now_us();
                });
                show_current();
            } else if first {
                show_current();
            }
        }
        first = false;
        unsafe { shim::rf_delay_ms(TICK_MS) };
    }
    unsafe { shim::rf_task_exit() };
}

// ── C ABI ──────────────────────────────────────────────────────────────────

/// # Safety
/// `display` must be the board's `CustomLcdDisplay`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn page_sync_set_display(display: *mut c_void) {
    set_display(display);
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_start() {
    start();
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_is_displaying() -> bool {
    is_displaying()
}

/// True when the last schedule poll reached the server (status bar indicator).
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_server_reachable() -> bool {
    server_reachable()
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_stop_display() {
    stop_display();
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_allow_display() {
    allow_display();
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_next() {
    next();
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_prev() {
    prev();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(n: u8) -> String {
        core::iter::repeat_n(format!("{n:02x}"), 16).collect()
    }

    #[test]
    fn parses_md5_pages_and_durations() {
        let body = format!(
            r#"{{"schedule_md5":"{}","pages":[{{"md5":"{}","duration_minutes":10,"order":0}}]}}"#,
            hex32(0xab),
            hex32(0xcd)
        );
        let parsed = parse_schedule(body.as_bytes()).expect("parse");
        assert_eq!(&parsed.md5[..4], b"abab");
        assert_eq!(parsed.count, 1);
        assert_eq!(&parsed.pages[0].md5[..4], b"cdcd");
        assert_eq!(parsed.pages[0].duration_s, 600);
    }

    // ── flows driven through the module, against the scripted shim ──

    fn md5hex(tag: u8) -> String {
        format!("{tag:02x}").repeat(16)
    }

    fn schedule_json(entries: &[(u8, u32)]) -> Vec<u8> {
        let pages: Vec<String> = entries
            .iter()
            .map(|(tag, min)| {
                format!(r#"{{"md5":"{}","duration_minutes":{}}}"#, md5hex(*tag), min)
            })
            .collect();
        format!(
            r#"{{"schedule_md5":"{}","pages":[{}]}}"#,
            md5hex(0x11),
            pages.join(",")
        )
        .into_bytes()
    }

    /// A bitmap body whose every byte is `fill`, so the framebuffer shows which
    /// page was blitted.
    fn bitmap_body(fill: u8) -> Vec<u8> {
        vec![fill; PAGE_BITMAP_SIZE]
    }

    fn page0_byte() -> u8 {
        with_table(|t| unsafe { *t.pages[0].bitmap })
    }

    /// Script and apply a two-page schedule, markers 0xa1 / 0xb2.
    fn setup_two_pages() {
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        sync_once();
    }

    #[test]
    fn policy_defaults_when_the_server_sends_none() {
        let p = parse_policy(br#"{"schedule_md5":"x","pages":[]}"#);
        assert_eq!(p, Policy::DEFAULT);
        assert_eq!(p.poll_s, 600, "the old hardcoded 10 s poll was 60x the intent");
        assert!(p.screen_active);
    }

    #[test]
    fn policy_is_read_from_the_schedule_body() {
        let body = br#"{"screen_active":false,"policy":{"poll_interval_minutes":10,
            "sleep_poll_interval_minutes":60,"min_page_duration_minutes":10},
            "schedule_md5":"x","pages":[]}"#;
        let p = parse_policy(body);
        assert_eq!(p.poll_s, 600);
        assert_eq!(p.sleep_poll_s, 3600);
        assert!(!p.screen_active);
    }

    #[test]
    fn nonsense_policy_values_fall_back_instead_of_stopping_polling() {
        for body in [
            &br#"{"policy":{"poll_interval_minutes":0,"sleep_poll_interval_minutes":-5}}"#[..],
            &br#"{"policy":{"poll_interval_minutes":"ten"}}"#[..],
            &br#"{"policy":{}}"#[..],
        ] {
            let p = parse_policy(body);
            assert_eq!(p.poll_s, 600, "zero/negative/text cadence keeps the default");
            assert_eq!(p.sleep_poll_s, 3600);
        }
        // A huge value is clamped rather than overflowing the delay.
        let p = parse_policy(br#"{"policy":{"poll_interval_minutes":99999999}}"#);
        assert_eq!(p.poll_s, 1440 * 60);
    }

    #[test]
    fn the_schedule_response_carries_the_policy_through() {
        let body = format!(
            r#"{{"schedule_md5":"{}","screen_active":true,
                "policy":{{"poll_interval_minutes":5,"sleep_poll_interval_minutes":30}},
                "pages":[]}}"#,
            md5hex(0x11)
        );
        let parsed = parse_schedule(body.as_bytes()).expect("parse");
        assert_eq!(parsed.policy.poll_s, 300);
        assert_eq!(parsed.policy.sleep_poll_s, 1800);
    }

    #[test]
    fn fetching_the_schedule_updates_the_applied_policy() {
        let _g = shim::host::lock();
        reset_for_test();
        let body = format!(
            r#"{{"schedule_md5":"{}","screen_active":false,
                 "policy":{{"poll_interval_minutes":10,"sleep_poll_interval_minutes":60}},
                 "pages":[]}}"#,
            md5hex(0x11)
        );
        shim::host::script_ok("/api/pages/schedule", body.as_bytes());

        sync_once();

        let (poll_s, sleep_poll_s, active) =
            with_table(|t| (t.policy.poll_s, t.policy.sleep_poll_s, t.policy.screen_active));
        assert_eq!((poll_s, sleep_poll_s, active), (600, 3600, false));
        assert!(SERVER_REACHABLE.load(Ordering::Acquire), "a 200 marks the server reachable");
    }

    #[test]
    fn a_failed_poll_marks_the_server_unreachable() {
        let _g = shim::host::lock();
        reset_for_test();
        SERVER_REACHABLE.store(true, Ordering::Release);
        shim::host::script_get("/api/pages/schedule", 500, b"");

        sync_once();

        assert!(!SERVER_REACHABLE.load(Ordering::Acquire));
        assert!(!server_reachable());
    }

    #[test]
    fn sync_downloads_pages_and_commits_the_schedule() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_now_us(1_000_000);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));

        sync_once();

        let (count, current, committed, md5) =
            with_table(|t| (t.count, t.current, t.have_schedule_md5, t.schedule_md5));
        assert_eq!(count, 2);
        assert_eq!(current, 0, "a fresh schedule starts at the first page");
        assert!(committed, "every bitmap arrived, so the md5 is committed");
        assert_eq!(&md5[..2], b"11");
        assert_eq!(page0_byte(), 0xa1, "page 0 holds its own bitmap");
        assert_eq!(
            with_table(|t| unsafe { *t.pages[1].bitmap }),
            0xb2,
            "page 1 holds its own bitmap"
        );
        assert_eq!(shim::host::calls_matching("http_get").len(), 3, "schedule + 2 bitmaps");
    }

    #[test]
    fn unchanged_schedule_is_not_re_downloaded() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_now_us(1_000_000);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();

        let before = with_table(|t| t.pages[0].bitmap);
        let gets_before = shim::host::calls_matching("http_get").len();
        sync_once();

        assert_eq!(
            with_table(|t| t.pages[0].bitmap),
            before,
            "the cached bitmap is kept, not re-fetched"
        );
        assert_eq!(
            shim::host::calls_matching("http_get").len() - gets_before,
            1,
            "the second poll hit only the schedule endpoint"
        );
    }

    #[test]
    fn a_missing_bitmap_keeps_the_schedule_uncommitted_for_a_retry() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_get(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), 500, b"");

        sync_once();

        let (count, committed) = with_table(|t| (t.count, t.have_schedule_md5));
        assert_eq!(count, 2);
        assert!(
            !committed,
            "committing here would make the 'unchanged' fast path skip the missing page forever"
        );

        // The retry downloads only the page that is still missing.
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        sync_once();
        assert!(with_table(|t| t.have_schedule_md5));
        assert!(with_table(|t| t.pages[1].bitmap) != core::ptr::null_mut());
    }

    #[test]
    fn rotation_blits_the_next_page_and_a_suspend_blocks_it() {
        let _g = shim::host::lock();
        reset_for_test();
        setup_two_pages();
        shim::host::set_fb();

        next();
        assert_eq!(shim::host::fb()[0], 0xb2, "page 1 is on the panel");
        assert_eq!(shim::host::refreshes(), 1);

        prev();
        assert_eq!(shim::host::fb()[0], 0xa1, "back to page 0");
        assert_eq!(shim::host::refreshes(), 2);

        // Suspended = the UI owns the panel: rotation must not repaint.
        stop_display();
        next();
        assert_eq!(shim::host::refreshes(), 2, "no refresh while suspended");
        assert_eq!(shim::host::fb()[0], 0xa1, "framebuffer untouched");

        allow_display();
        assert_eq!(shim::host::fb()[0], 0xb2, "resuming repaints the current page");
        assert_eq!(shim::host::refreshes(), 3);
    }

    #[test]
    fn an_empty_schedule_still_repaints_when_the_screen_is_handed_back() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        assert_eq!(with_table(|t| t.count), 0);
        shim::host::set_fb();

        // With no pages the old code could take the screen back without
        // repainting, leaving the notification (or Settings) on the panel.
        stop_display();
        allow_display();
        assert_eq!(shim::host::hint_draws(), 1, "the empty hint is drawn");
        assert_eq!(shim::host::refreshes(), 1);
        assert!(is_displaying());

        // While the UI owns the panel a repaint is dropped, not drawn under it.
        stop_display();
        show_current();
        assert_eq!(shim::host::hint_draws(), 1);
        assert!(!is_displaying());

        resume_display();
        redraw_current();
        assert_eq!(shim::host::hint_draws(), 2, "resuming repaints");
    }

    #[test]
    fn caps_at_max_pages_and_skips_malformed_entries() {
        let entry = |n: u8| format!(r#"{{"md5":"{}","duration_minutes":10}}"#, hex32(n));
        // 7 entries: one is malformed, so 6 are valid and the cap must drop the
        // last one.
        let body = format!(
            r#"{{"schedule_md5":"{}","pages":[{},{},{{"md5":"short","duration_minutes":1}},{},{},{},{}]}}"#,
            hex32(1), entry(2), entry(3), entry(4), entry(5), entry(6), entry(7)
        );
        let parsed = parse_schedule(body.as_bytes()).expect("parse");
        assert_eq!(parsed.count, MAX_PAGES);
        assert_eq!(&parsed.pages[0].md5[..2], b"02");
        assert_eq!(&parsed.pages[4].md5[..2], b"06", "short md5 skipped, 7th over the cap");
    }

    #[test]
    fn rejects_unusable_responses() {
        assert!(parse_schedule(br#"{}"#).is_none());
        assert!(parse_schedule(br#"{"schedule_md5":"not32"}"#).is_none());
        assert!(parse_schedule(br#"{"schedule_md5":"0123456789abcdef0123456789abcdef"}"#).is_none());
        assert!(parse_schedule(br#"{"schedule_md5":"0123456789abcdef0123456789abcdef","pages":5}"#).is_none());
    }

    #[test]
    fn empty_page_list_is_valid_and_not_an_error() {
        let body = br#"{"schedule_md5":"0123456789abcdef0123456789abcdef","pages":[]}"#;
        let parsed = parse_schedule(body).expect("parse");
        assert_eq!(parsed.count, 0);
    }

    #[test]
    fn negative_duration_clamps_to_zero() {
        let body = format!(
            r#"{{"schedule_md5":"{}","pages":[{{"md5":"{}","duration_minutes":-5}}]}}"#,
            hex32(1), hex32(2)
        );
        let parsed = parse_schedule(body.as_bytes()).expect("parse");
        assert_eq!(parsed.pages[0].duration_s, 0);
    }
}
