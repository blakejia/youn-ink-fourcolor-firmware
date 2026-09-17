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
/// Magic of `rf_panel_record_t` (see `shim_power.h`): gated together with the
/// valid byte, so an all-zero record (power-on RTC memory) reads as unknown.
const PANEL_MAGIC: u32 = 0x50414E31;

/// 48-byte `rf_panel_record_t` with the alignment the device side assumes:
/// shim.cpp publishes the record with a struct assignment (`*out = g_panel_rec`)
/// whose members are 4-byte, so the buffer must be 4-aligned (`[u8; 48]` alone
/// only guarantees 1-byte alignment).
#[repr(C, align(4))]
struct PanelRecord([u8; 48]);

fn read_panel_record() -> PanelRecord {
    let mut rec = PanelRecord([0u8; 48]);
    unsafe { shim::rf_panel_record_get(rec.0.as_mut_ptr()) };
    rec
}

fn record_magic_ok(rec: &[u8; 48]) -> bool {
    u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]) == PANEL_MAGIC
}

/// Index the record says is on the glass (-1 = the empty hint).
fn record_index(rec: &[u8; 48]) -> i32 {
    i32::from_le_bytes([rec[44], rec[45], rec[46], rec[47]])
}
/// Fallbacks when the server sends no policy: the old hardcoded 10 s poll was
/// 60x the server's intent and kept the radio up all day.
const DEFAULT_POLL_S: u32 = 600;
const DEFAULT_SLEEP_POLL_S: u32 = 3600;
const MAX_POLL_MINUTES: i64 = 1440;
const SCHEDULE_TIMEOUT_MS: i32 = 10_000;
const BITMAP_TIMEOUT_MS: i32 = 15_000;
const SCHEDULE_BUF: usize = 8192;
// (No poll loop, no task stack: the device is duty-cycled — each wake runs one
// `sync_once`, then `paint_if_changed`.)

// ── screen ownership (lock-free: read under the display mutex) ──
static DISPLAYING: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// Whether the last schedule poll reached the server (status-bar indicator).
static SERVER_REACHABLE: AtomicBool = AtomicBool::new(false);
/// Whether the last [`sync_once`] succeeded (the power wiring reads it).
static LAST_SYNC_OK: AtomicBool = AtomicBool::new(false);
/// Task 4: what the last schedule body said about the notification queue.
/// Lock-free (like SERVER_REACHABLE): `application.cc` reads it right after
/// `page_sync_sync_once` returns, while `sync_schedule` may run under no or
/// any lock — an atomic avoids a second lock order.
/// Default true: an old server sends no field, and the first wake's sync may
/// fail, both of which must behave exactly as today (fetch).
static NOTIFY_PENDING: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy)]
struct Page {
    md5: [u8; MD5_LEN],
    bitmap: *mut u8,
}

impl Page {
    const fn empty() -> Self {
        Page { md5: [0; MD5_LEN], bitmap: core::ptr::null_mut() }
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
    /// Index the server says should be showing now.
    server_index: usize,
    /// Set by manual paging; cleared by the next successful sync.
    override_index: Option<usize>,
    /// Seconds until the server's next page change (0 = unknown/empty).
    next_wake_s: u32,
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
            server_index: 0,
            override_index: None,
            next_wake_s: 0,
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
    LAST_SYNC_OK.store(false, Ordering::Release);
    NOTIFY_PENDING.store(true, Ordering::Release);
}


// ── fetching ───────────────────────────────────────────────────────────────

/// GET `/api/pages/schedule` into `buf`; returns the body length.
///
/// The caller must hold no lock: `rf_build_endpoint`/HTTP can block for seconds.
fn fetch_schedule(buf: &mut [u8]) -> Option<usize> {
    use core::fmt::Write as _;
    let mut path = CBuf::<160>::new();
    path.push("/api/pages/schedule");
    let mut c = [0u32; 5];
    unsafe { shim::rf_power_counters(&mut c[0], &mut c[1], &mut c[2], &mut c[3], &mut c[4]) };
    let _ = write!(path, "?w={}&a={}&r={}&g={}&f={}", c[0], c[1], c[2], c[3], c[4]);
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
    /// Index the server says should be showing now (0 when absent).
    pub current_index: usize,
    /// Seconds until the server's next page change (None when absent/empty).
    pub seconds_until_next_page: Option<u32>,
    /// Whether the server says a notification is waiting (Task 4: skip the
    /// empty `/api/notifications/next` poll). Absent = old server = true,
    /// i.e. behave exactly as today (fetch).
    pub notify_pending: bool,
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

    let current_index = json::member(body, 0, "current_index")
        .and_then(|at| json::int_value(body, at))
        .map(|v| v.max(0) as usize)
        .unwrap_or(0);
    let seconds_until_next_page = json::member(body, 0, "seconds_until_next_page")
        .and_then(|at| json::int_value(body, at))
        .map(|v| v.max(0) as u32);
    // Absent = old server = behave as today (fetch). A present field is
    // honoured verbatim.
    let notify_pending = json::member(body, 0, "notify_pending")
        .and_then(|at| json::bool_value(body, at))
        .unwrap_or(true);
    let mut out = ParsedSchedule {
        md5,
        pages: [EMPTY_PAGE; MAX_PAGES],
        count: 0,
        policy: parse_policy(body),
        current_index,
        seconds_until_next_page,
        notify_pending,
    };
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

/// One sync: fetch the schedule and apply the server's position answer. No
/// bitmap is downloaded here — the paint path fetches the one page it needs.
/// True when a fresh schedule was applied, which commits its md5.
pub fn sync_once() -> bool {
    // PSRAM, not the stack: this buffer is as large as the whole task stack
    // (the C++ original kept it in a file-scope static for the same reason).
    // +1 because `http_wrapper_get` NUL-terminates one byte past the length.
    let raw = unsafe { shim::rf_alloc(SCHEDULE_BUF + 1) };
    if raw.is_null() {
        log_e!("PageSync", "schedule buffer alloc failed");
        LAST_SYNC_OK.store(false, Ordering::Release);
        return false;
    }
    // Same one-byte-over-allocation as the notify response: the wrapper's
    // capacity includes the terminator it appends.
    let body = unsafe { core::slice::from_raw_parts_mut(raw, SCHEDULE_BUF + 1) };
    let ok = if let Some(len) = fetch_schedule(body) {
        sync_schedule(&body[..len])
    } else {
        false
    };
    unsafe { shim::rf_free(raw) };
    LAST_SYNC_OK.store(ok, Ordering::Release);
    ok
}

/// Apply a fetched `/api/pages/schedule` body. Returns false only when the
/// body does not parse. No bitmap is downloaded here: the page that gets
/// painted is fetched on demand by the paint path, so the table may well hold
/// entries with a null bitmap.
fn sync_schedule(body: &[u8]) -> bool {
    let Some(parsed) = parse_schedule(body) else {
        log_w!("PageSync", "schedule json unusable (missing schedule_md5/pages)");
        return false;
    };
    let new_md5 = parsed.md5;
    let new_policy = parsed.policy;
    let new_index = parsed.current_index;
    let new_wake_s = parsed.seconds_until_next_page.unwrap_or(0);
    // Commit the queue hint on every parsed body — including the unchanged-md5
    // fast path below, which returns early: staleness here would pin an old
    // answer across wakes.
    NOTIFY_PENDING.store(parsed.notify_pending, Ordering::Release);

    let policy_changed = with_table(|t| {
        let prev = t.policy;
        t.policy = new_policy;
        prev != new_policy
    });
    if policy_changed {
        log_i!("PageSync", "policy: poll={}s sleep_poll={}s screen_active={}",
            new_policy.poll_s, new_policy.sleep_poll_s, new_policy.screen_active);
    }

    // 99% path: the schedule is unchanged — but the server's position still
    // moved on, so the index/wake answer is picked up and any manual override
    // is released here. No bitmaps are involved either way: whatever the paint
    // path needs it fetches on demand itself.
    let unchanged = with_table(|t| t.have_schedule_md5 && t.schedule_md5 == new_md5);
    if unchanged {
        with_table(|t| {
            t.server_index = new_index.min(t.count.saturating_sub(1));
            t.next_wake_s = new_wake_s;
            t.override_index = None;
        });
        return true;
    }

    let new_count = parsed.count;
    let mut new_pages = [Page::empty(); MAX_PAGES];
    for i in 0..new_count {
        new_pages[i].md5 = parsed.pages[i].md5;
    }

    // Carry over bitmaps already in RAM for the same md5 (a re-sync within one
    // wake), and download NOTHING. The page that is going to be painted is
    // fetched on demand by `ensure_bitmap`, which is also where a failure is
    // handled: it leaves the RTC record stale so the next wake retries.
    //
    // Premise: the awake window (8-16 s) is far shorter than a page's dwell
    // (>= 10 min), so the rotation cannot move on while we are awake — the
    // other pages' bitmaps would never be used before the sleep clears them.
    let mut carried = 0;
    for i in 0..new_count {
        let carried_slot = with_table(|t| {
            for j in 0..t.count {
                if t.pages[j].is_ram() && t.pages[j].md5 == new_pages[i].md5 {
                    let bmp = t.pages[j].bitmap;
                    t.pages[j].bitmap = core::ptr::null_mut(); // ownership moves
                    return Some(bmp);
                }
            }
            None
        });
        if let Some(bmp) = carried_slot {
            new_pages[i].bitmap = bmp;
            carried += 1;
        }
    }

    with_table(|t| {
        t.free_pages();
        t.pages = new_pages;
        t.count = new_count;
        t.server_index = new_index.min(new_count.saturating_sub(1));
        t.next_wake_s = new_wake_s;
        t.override_index = None;
        // Commit unconditionally: the schedule was parsed and its md5s are
        // recorded. "Do not commit while the page you need is missing" is now
        // enforced where the page is actually fetched (paint_if_changed ->
        // ensure_bitmap): a failed fetch never reaches rf_panel_mark_pending,
        // so the RTC record stays stale and the next wake retries.
        t.schedule_md5 = new_md5;
        t.have_schedule_md5 = true;
    });

    log_i!("PageSync", "schedule updated: {} pages ({} carried from cache)",
        new_count,
        carried
    );
    true
}

// ── drawing ────────────────────────────────────────────────────────────────

/// The page that should be on the panel: the manual override when the user
/// paged, otherwise the server's answer. None when no page is configured.
fn target_index() -> Option<usize> {
    with_table(|t| {
        if t.count == 0 { return None; }
        Some(t.override_index.unwrap_or(t.server_index).min(t.count - 1))
    })
}

/// Make sure page `idx` has its bitmap in RAM, downloading it on demand.
/// Null when out of range or unreachable (nothing is recorded as displayed,
/// so the next wake tries again).
fn ensure_bitmap(idx: usize, md5: &[u8; MD5_LEN]) -> *mut u8 {
    let slot = with_table(|t| {
        if idx < t.count { t.pages[idx].bitmap } else { core::ptr::null_mut() }
    });
    if !slot.is_null() {
        return slot;
    }
    // +1 for the wrapper's terminator.
    let raw = unsafe { shim::rf_alloc(PAGE_BITMAP_SIZE + 1) };
    if raw.is_null() {
        log_e!("PageSync", "bitmap alloc failed");
        return core::ptr::null_mut();
    }
    if !download_bitmap(md5, raw) {
        unsafe { shim::rf_free(raw) };
        return core::ptr::null_mut();
    }
    with_table(|t| {
        if idx < t.count && t.pages[idx].bitmap.is_null() {
            t.pages[idx].bitmap = raw;
            return raw;
        }
        // A concurrent sync filled (or dropped) the page meanwhile: keep the
        // table's answer, never leak ours.
        unsafe { shim::rf_free(raw) };
        if idx < t.count { t.pages[idx].bitmap } else { core::ptr::null_mut() }
    })
}

/// Copy page `idx` to the framebuffer and spend one full refresh. Shared tail
/// of both paint paths; true when a refresh was actually spent.
///
/// The copy is verified under the state lock (lock order: state -> display),
/// so a concurrent `sync_once` cannot free the bitmap mid-copy — a torn blit
/// recorded as done would stick until the schedule changes, because the next
/// wake would read the record and skip the repaint.
fn blit_and_refresh(idx: usize, slot: *mut u8) -> bool {
    let copied = with_table(|t| {
        if slot.is_null() || idx >= t.count || t.pages[idx].bitmap != slot {
            return false;
        }
        let fb = unsafe { shim::rf_fb_begin() };
        let fb_len = unsafe { shim::rf_fb_len() } as usize;
        if !fb.is_null() && fb_len == PAGE_BITMAP_SIZE {
            // Same 2bpp MSB-first pixel order as the server's packer: plain copy.
            unsafe { core::ptr::copy_nonoverlapping(slot, fb, PAGE_BITMAP_SIZE) };
        } else {
            log_e!("PageSync", "framebuffer size mismatch: fb_len={} want={}", fb_len, PAGE_BITMAP_SIZE);
        }
        unsafe { shim::rf_fb_end() };
        true
    });
    if !copied {
        return false;
    }
    // Refresh-submit accounting lives at the single C++ exit
    // (rf_request_full_refresh in shim.cpp): canvas, empty-hint and notify
    // paths all funnel through it. SUBMIT time only — the panel's async
    // waveform tail is deliberately not included (see panel_ms note below).
    unsafe { shim::rf_request_full_refresh() };
    DISPLAYING.store(true, Ordering::Release);
    true
}

/// Fetch the page that is about to be painted, without touching the panel.
///
/// The power wiring turns the radio off before a full refresh (15-25 s of
/// waveform), so the download must happen first — `paint_if_changed` would
/// otherwise try to fetch it with the radio down and the page would never land.
/// Returns false only when the page is missing and could not be fetched.
pub fn prepare_paint() -> bool {
    if SUSPENDED.load(Ordering::Acquire) {
        return true; // canvas benched: nothing to fetch
    }
    let Some(idx) = target_index() else {
        return true; // empty hint: nothing to fetch
    };
    let rec = read_panel_record();
    let md5 = with_table(|t| t.pages[idx].md5);
    if !with_table(|t| t.pages[idx].bitmap.is_null()) {
        return true;
    }
    if record_magic_ok(&rec.0)
        && rec.0[4] != 0
        && record_index(&rec.0) == idx as i32
        && rec.0[8..40] == md5
    {
        return true; // glass already shows it: no fetch
    }
    !ensure_bitmap(idx, &md5).is_null()
}

/// Paint the target page only if the glass does not already show it.
pub fn paint_if_changed() -> bool {
    if SUSPENDED.load(Ordering::Acquire) {
        return false;
    }
    let Some(idx) = target_index() else {
        // Empty table after a FAILED sync means "schedule unknown", not "the
        // server says there are no pages": every deep-sleep wake starts with
        // an empty table, so painting the hint here would white-refresh over
        // the last good page (15-25 s with the radio up) and spend a second
        // full refresh restoring it on the next wake. Spec §13 requires
        // keeping the image while backing off, so a failed sync leaves the
        // glass alone. Gate on this module's own last-sync result.
        if !LAST_SYNC_OK.load(Ordering::Acquire) {
            return false;
        }
        // Empty schedule: the hint is a recorded state (index -1), not an
        // unrecorded draw. Key on the index only: the md5 payload is 32 zero
        // bytes, which the shim stores as an empty string, so comparing md5
        // here would never match. Returning true leaves the panel showing
        // something the canvas chose, which is what makes start()'s ownership
        // claim honest.
        let rec = read_panel_record();
        if record_magic_ok(&rec.0) && rec.0[4] != 0 && record_index(&rec.0) == -1 {
            return false;
        }
        show_empty_hint();
        return true;
    };
    let md5 = with_table(|t| if idx < t.count { Some(t.pages[idx].md5) } else { None });
    let Some(md5) = md5 else { return false; };
    let rec = read_panel_record();
    // Magic AND valid: an all-zero record (power-on RTC memory) is "no known
    // content", never a match — the device only ever sets the two together.
    if record_magic_ok(&rec.0) && rec.0[4] != 0 && rec.0[8..40] == md5 {
        log_i!("PageSync", "glass already shows {:?}, skipping repaint",
            core::str::from_utf8(&md5[..8]).unwrap_or("?"));
        return false;
    }
    let slot = ensure_bitmap(idx, &md5);
    if slot.is_null() {
        return false;
    }
    if !blit_and_refresh(idx, slot) {
        return false;
    }
    // Staged only after the copy landed: the device commits it when the
    // refresh goes idle, which is what makes "skip the repaint next wake"
    // safe against an interrupted refresh.
    unsafe { shim::rf_panel_mark_pending(md5.as_ptr() as *const core::ffi::c_char, idx as i32) };
    let count = with_table(|t| t.count);
    log_i!("PageSync", "show page {}/{} md5={:?}",
        idx + 1,
        count,
        core::str::from_utf8(&md5[..8]).unwrap_or("?")
    );
    true
}

/// Paint page `idx` unconditionally (manual paging or hand-back): the caller
/// asked for it, so the RTC record is not consulted. False when suspended or
/// when the bitmap is unavailable.
fn paint_index(idx: usize) -> bool {
    if SUSPENDED.load(Ordering::Acquire) {
        return false;
    }
    let md5 = with_table(|t| if idx < t.count { Some(t.pages[idx].md5) } else { None });
    let Some(md5) = md5 else { return false; };
    let slot = ensure_bitmap(idx, &md5);
    if slot.is_null() {
        return false;
    }
    if !blit_and_refresh(idx, slot) {
        return false;
    }
    unsafe { shim::rf_panel_mark_pending(md5.as_ptr() as *const core::ffi::c_char, idx as i32) };
    true
}

fn show_empty_hint() {
    if SUSPENDED.load(Ordering::Acquire) {
        return;
    }
    unsafe { shim::rf_draw_empty_hint() };
    unsafe { shim::rf_request_full_refresh() };
    // Record the hint as displayed_index -1 with EMPTY_PAGE.md5 (32 zero bytes)
    // as the payload. The zero bytes store as an empty string, which does not
    // matter: the reader keys on the index only. Marked here rather than at the
    // callers so every hand-back path (allow/resume/redraw/empty wake) stays truthful.
    unsafe {
        shim::rf_panel_mark_pending(EMPTY_PAGE.md5.as_ptr() as *const core::ffi::c_char, -1)
    };
    DISPLAYING.store(true, Ordering::Release);
    log_i!("PageSync", "show empty page hint (no pages configured)");
}
fn show_current() {
    // Force-paint, never `paint_if_changed`: while the UI owned the panel it
    // drew over the glass, so the RTC record may claim a page that is no
    // longer visible.
    match target_index() {
        Some(idx) => {
            paint_index(idx);
        }
        None => {
            show_empty_hint();
        }
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
/// the user does not stare at the UI page.
pub fn allow_display() {
    if !SUSPENDED.swap(false, Ordering::AcqRel) {
        return;
    }
    show_current();
}

/// Manual page step (wraps): a local override the next successful sync clears,
/// so browsing cannot fight the schedule.
pub fn next() {
    let idx = with_table(|t| {
        if t.count == 0 {
            return None;
        }
        let cur = t.override_index.unwrap_or(t.server_index).min(t.count - 1);
        let n = (cur + 1) % t.count;
        t.override_index = Some(n);
        Some(n)
    });
    if let Some(i) = idx {
        paint_index(i);
    }
}

pub fn prev() {
    let idx = with_table(|t| {
        if t.count == 0 {
            return None;
        }
        let cur = t.override_index.unwrap_or(t.server_index).min(t.count - 1);
        let n = (cur + t.count - 1) % t.count;
        t.override_index = Some(n);
        Some(n)
    });
    if let Some(i) = idx {
        paint_index(i);
    }
}

/// Start the canvas. Duty-cycled: no task is created — each wake runs one
/// [`sync_once`] plus [`paint_if_changed`] (see the power wiring). Starting
/// only hands the screen back to the canvas. Idempotent.
pub fn start() {
    SUSPENDED.store(false, Ordering::Release);
    DISPLAYING.store(true, Ordering::Release);
}

/// Seconds until the server's next page change. -1 when unknown or when no
/// page is configured — never 0, which the power wiring would read as
/// "0 seconds remain" and floor to its minimum, starving the sleep cap.
pub fn next_wake_s() -> i32 {
    with_table(|t| {
        if t.count == 0 || t.next_wake_s == 0 {
            -1
        } else {
            t.next_wake_s as i32
        }
    })
}

/// Whether the last [`sync_once`] succeeded.
pub fn sync_ok() -> bool {
    LAST_SYNC_OK.load(Ordering::Acquire)
}

/// Task 4: what the last parsed schedule said about the notification queue
/// (true = something is waiting, so fetch it). Defaults to true before the
/// first successful sync and when the server sends no field (old server).
pub fn notify_pending() -> bool {
    NOTIFY_PENDING.load(Ordering::Acquire)
}

/// `policy.poll_interval_minutes * 60`.
pub fn poll_s() -> u32 {
    with_table(|t| t.policy.poll_s)
}

/// `policy.sleep_poll_interval_minutes * 60`.
pub fn sleep_poll_s() -> u32 {
    with_table(|t| t.policy.sleep_poll_s)
}

/// False while the server's sleep window is open.
pub fn screen_active() -> bool {
    with_table(|t| t.policy.screen_active)
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

/// One sync: fetch + parse + apply the schedule. True on success.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_sync_once() -> bool {
    sync_once()
}

/// Paint the target page only when the glass is out of date. True = painted.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_paint_if_changed() -> bool {
    paint_if_changed()
}

#[unsafe(no_mangle)]
pub extern "C" fn page_sync_prepare_paint() -> bool {
    prepare_paint()
}

/// Seconds until the server's next page change; -1 = unknown/empty schedule.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_next_wake_s() -> i32 {
    next_wake_s()
}

/// Whether the last `page_sync_sync_once` succeeded.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_sync_ok() -> bool {
    sync_ok()
}

/// Task 4: whether the last parsed schedule says a notification is waiting.
/// True before the first successful sync and when the server sends no field.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_notify_pending() -> bool {
    notify_pending()
}

/// `policy.poll_interval_minutes * 60`.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_poll_s() -> u32 {
    poll_s()
}

/// `policy.sleep_poll_interval_minutes * 60`.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_sleep_poll_s() -> u32 {
    sleep_poll_s()
}

/// False while the server's sleep window is open.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_screen_active() -> bool {
    screen_active()
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

    // ── one-shot sync + paint-if-changed (duty-cycled; no poll loop) ──

    fn scripted_schedule_with_position(index: usize, next_s: u32, entries: &[(u8, u32)]) {
        let pages: Vec<String> = entries.iter()
            .map(|(t, m)| format!(r#"{{"md5":"{}","duration_minutes":{}}}"#, md5hex(*t), m))
            .collect();
        let body = format!(
            r#"{{"schedule_md5":"{}","current_index":{},"seconds_until_next_page":{},"screen_active":true,"policy":{{"poll_interval_minutes":10,"sleep_poll_interval_minutes":60}},"pages":[{}]}}"#,
            md5hex(0x11), index, next_s, pages.join(","));
        shim::host::script_ok("/api/pages/schedule", body.as_bytes());
    }

    #[test]
    fn paints_when_the_servers_page_differs_from_the_glass() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(1, 240, &[(0xa1, 10), (0xb2, 5)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        assert!(sync_once());
        assert!(paint_if_changed(), "nothing recorded on the glass yet -> paint");
        assert_eq!(shim::host::fb()[0], 0xb2, "the server's current page is on the panel");
        assert_eq!(shim::host::refreshes(), 1);
    }

    #[test]
    fn skips_the_repaint_when_the_glass_already_shows_it() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed());
        assert_eq!(shim::host::refreshes(), 1);

        // Simulate the wake that follows: same page, same glass.
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(!paint_if_changed(), "same md5 as the glass -> no panel cycle");
        assert_eq!(shim::host::refreshes(), 1, "no extra refresh was spent");
    }

    #[test]
    fn a_record_with_wrong_magic_is_not_trusted() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        assert!(sync_once());
        // Garbage RTC: valid bit set and md5 matching, but the magic is wrong
        // (cold boot / corruption). Only the magic makes this safe to distrust.
        shim::host::stage_panel_record(0xDEAD_BEEF, 1, md5hex(0xa1).as_bytes(), 0);
        assert!(paint_if_changed(), "wrong magic -> repaint even though valid and md5 match");
        assert_eq!(shim::host::fb()[0], 0xa1);
        assert_eq!(shim::host::refreshes(), 1);
    }

    #[test]
    fn an_empty_schedule_paints_the_hint_once_and_records_it() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        assert!(sync_once());
        assert!(paint_if_changed(), "empty schedule still shows the hint");
        assert_eq!(shim::host::hint_draws(), 1);
        assert_eq!(shim::host::refreshes(), 1);
        // Second wake: the hint recorded as index -1 means the glass already
        // shows it — keyed on the index only, never the (empty) md5.
        assert!(!paint_if_changed(), "hint recorded as index -1 -> no panel cycle");
        assert_eq!(shim::host::refreshes(), 1);
    }
    #[test]
    fn a_failed_sync_reports_failure_and_does_not_paint() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        assert!(sync_once());
        assert!(paint_if_changed(), "first wake records the glass");
        let refreshes = shim::host::refreshes();

        // The next wake finds the server down: the previous table must survive.
        shim::host::script_get("/api/pages/schedule", 500, b"");
        assert!(!sync_once());
        assert!(!sync_ok());
        let (count, md5) = with_table(|t| (t.count, t.pages[0].md5));
        assert_eq!(count, 1, "the previous table is intact");
        assert_eq!(&md5[..], md5hex(0xa1).as_bytes(), "still page 0xa1's entry");
        assert_eq!(shim::host::refreshes(), refreshes, "no panel cycle on a failed sync");
    }
    // F23: an empty table after a FAILED first sync means "schedule unknown",
    // not "no pages" — the glass keeps the last good page, no hint, no cycle.
    #[test]
    fn failed_sync_with_a_page_on_the_glass_paints_nothing() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        // The RTC record says page 0 is on the glass (a previous boot's page).
        shim::host::stage_panel_record(0x50414E31, 1, md5hex(0xa1).as_bytes(), 0);
        // This wake never reaches the server: the table stays empty.
        shim::host::script_get("/api/pages/schedule", 500, b"");
        assert!(!sync_once());
        assert!(!sync_ok());
        assert_eq!(with_table(|t| t.count), 0);
        assert!(!paint_if_changed(), "failed sync leaves the glass alone");
        assert_eq!(shim::host::hint_draws(), 0, "no hint over the last good page");
        assert_eq!(shim::host::refreshes(), 0, "no panel cycle while backing off");
    }

    // (There was a second copy of this asserting the same three things with the
    // hint already on the glass. The gate returns before the RTC record is read
    // — LAST_SYNC_OK is checked first — so the glass contents are not an input
    // to this path at all; one test proves it.)

    // F23: a SUCCESSFUL sync with zero pages still draws and records the hint.
    #[test]
    fn successful_empty_sync_paints_and_records_the_hint() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        assert!(sync_once());
        assert!(sync_ok());
        assert!(paint_if_changed(), "successful empty sync shows the hint");
        assert_eq!(shim::host::hint_draws(), 1);
        assert_eq!(shim::host::refreshes(), 1);
        assert!(!paint_if_changed(), "hint recorded as index -1 -> no panel cycle");
        assert_eq!(shim::host::refreshes(), 1);
    }

    #[test]
    fn manual_paging_overrides_the_server_until_the_next_sync() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10), (0xb2, 5)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        assert!(sync_once());
        next();  // local override -> page 2
        assert_eq!(shim::host::fb()[0], 0xb2);
        prev();  // manual paging works both ways
        assert_eq!(shim::host::fb()[0], 0xa1);

        // Meanwhile the server moved on: the next sync re-imposes its index
        // and releases the override.
        scripted_schedule_with_position(1, 240, &[(0xa1, 10), (0xb2, 5)]);
        sync_once();
        assert!(paint_if_changed(), "override cleared -> the server's page");
        assert_eq!(shim::host::fb()[0], 0xb2);
    }

    #[test]
    fn next_wake_is_reported_from_the_response() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert_eq!(next_wake_s(), 240);
        assert_eq!(poll_s(), 600);
        assert_eq!(sleep_poll_s(), 3600);
        assert!(screen_active());
    }

    #[test]
    fn an_empty_schedule_reports_no_next_page() {
        // -1 而不是 0：0 会被 power::decide 当成“还剩 0 秒”并夹到 60 秒下限，
        // 空排期就永远睡不满 cap。
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        assert_eq!(next_wake_s(), -1);
    }

    // ── flows driven through the module, against the scripted shim ──

    fn md5hex(tag: u8) -> String {
        format!("{tag:02x}").repeat(16)
    }

    fn schedule_json_with_md5(sched_tag: u8, entries: &[(u8, u32)]) -> Vec<u8> {
        let pages: Vec<String> = entries
            .iter()
            .map(|(tag, min)| {
                format!(r#"{{"md5":"{}","duration_minutes":{}}}"#, md5hex(*tag), min)
            })
            .collect();
        format!(
            r#"{{"schedule_md5":"{}","pages":[{}]}}"#,
            md5hex(sched_tag),
            pages.join(",")
        )
        .into_bytes()
    }

    fn schedule_json(entries: &[(u8, u32)]) -> Vec<u8> {
        schedule_json_with_md5(0x11, entries)
    }

    /// Same as `schedule_json_with_md5` plus the Task 4 `notify_pending` flag.
    /// `None` emits no field at all (old server), which must read as "fetch".
    fn schedule_json_with_notify(sched_tag: u8, entries: &[(u8, u32)], pending: Option<bool>) -> Vec<u8> {
        let pages: Vec<String> = entries
            .iter()
            .map(|(tag, min)| {
                format!(r#"{{"md5":"{}","duration_minutes":{}}}"#, md5hex(*tag), min)
            })
            .collect();
        let pending_field = match pending {
            Some(true) => r#","notify_pending":true"#.to_string(),
            Some(false) => r#","notify_pending":false"#.to_string(),
            None => String::new(),
        };
        format!(
            r#"{{"schedule_md5":"{}","pages":[{}]{}}}"#,
            md5hex(sched_tag),
            pages.join(","),
            pending_field,
        )
        .into_bytes()
    }

    #[test]
    fn schedule_reports_whether_a_notification_is_waiting() {
        let _g = shim::host::lock();
        reset_for_test();
        assert!(parse_schedule(&schedule_json_with_notify(0x11, &[], Some(true)))
            .unwrap().notify_pending);
        assert!(!parse_schedule(&schedule_json_with_notify(0x11, &[], Some(false)))
            .unwrap().notify_pending);
        assert!(parse_schedule(&schedule_json_with_notify(0x11, &[], None))
            .unwrap().notify_pending, "absent field = old server -> behave as today (fetch)");
        // The application.cc gate reads the committed value, not the parse
        // struct — pin both, including the unchanged-md5 fast path.
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_notify(0x11, &[], Some(false)));
        assert!(sync_once());
        assert!(!notify_pending());
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_notify(0x11, &[], Some(true)));
        assert!(sync_once());
        assert!(notify_pending());
    }
    /// A bitmap body whose every byte is `fill`, so the framebuffer shows which
    /// page was blitted.
    fn bitmap_body(fill: u8) -> Vec<u8> {
        vec![fill; PAGE_BITMAP_SIZE]
    }

    fn page0_byte() -> u8 {
        with_table(|t| unsafe { *t.pages[0].bitmap })
    }


    #[test]
    fn the_bitmap_buffer_is_sized_for_the_wrapper_contract() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        // Exactly one panel of payload: the allocation must be one byte larger,
        // because the wrapper's capacity includes the terminator it appends.
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));

        setup_one_page();

        assert_eq!(
            with_table(|t| unsafe { *t.pages[0].bitmap }),
            0xa1,
            "a full-size bitmap still lands, terminator included"
        );
    }

    /// Apply an already-scripted schedule and fetch the target page. `sync_once`
    /// itself downloads nothing now, so the bitmap arrives via the paint path.
    fn setup_one_page() {
        sync_once();
        paint_if_changed();
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
    fn sync_records_the_pages_but_downloads_nothing() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        // No bitmap responses are scripted on purpose: a sync that fetches any
        // would fail here instead of silently passing.
        sync_once();

        let (count, server_index, committed) =
            with_table(|t| (t.count, t.server_index, t.have_schedule_md5));
        assert_eq!(count, 2);
        assert_eq!(server_index, 0, "no position in the response -> page 0");
        assert!(committed, "the schedule was received, so its md5 is committed");
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: the page that will be painted is fetched by the paint path"
        );
    }

    #[test]
    fn the_paint_path_fetches_exactly_the_target_page() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));

        sync_once();
        assert!(paint_if_changed(), "first paint draws the target page");

        let gets = shim::host::calls_matching("http_get");
        assert_eq!(gets.len(), 2, "schedule + the one page being painted");
        assert!(
            gets.iter().any(|c| c.contains(&md5hex(0xa1))),
            "the fetched bitmap is page 0's, not every page's"
        );
        assert_eq!(page0_byte(), 0xa1);
    }

    #[test]
    fn a_glass_that_already_shows_the_target_page_fetches_nothing() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::stage_panel_record(0x50414E31, 1, md5hex(0xa1).as_bytes(), 0);

        sync_once();
        assert!(!paint_if_changed(), "the glass already shows page 0");
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: nothing to paint, so nothing to download"
        );
    }

    #[test]
    fn an_unchanged_schedule_does_not_re_fetch_the_painted_page() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed(), "the paint path fetched the page");

        // Only now is there something cached to preserve, so this pair of
        // assertions means what it says.
        let before = with_table(|t| t.pages[0].bitmap);
        assert!(!before.is_null(), "the painted page's bitmap is in RAM");
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
    fn prepare_paint_fetches_the_page_without_touching_the_panel() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        shim::host::script_ok(
            &format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)),
            &bitmap_body(0xa1),
        );
        sync_once();
        let before = shim::host::refreshes();
        assert!(prepare_paint(), "the bitmap must be resident");
        assert_eq!(
            shim::host::refreshes(),
            before,
            "prepare must not refresh the panel"
        );
        assert!(paint_if_changed(), "then the paint still happens");
    }

    #[test]
    fn prepare_paint_is_true_when_nothing_needs_downloading() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        // No bitmap is scripted on purpose: the glass already shows the target
        // page, so prepare must not issue a bitmap GET at all.
        shim::host::stage_panel_record(0x50414E31, 1, md5hex(0xa1).as_bytes(), 0);
        sync_once();
        assert!(
            prepare_paint(),
            "same page on the glass -> no download needed"
        );
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: the panel-record match skips the bitmap fetch"
        );
    }

    #[test]
    fn prepare_paint_stays_quiet_while_suspended() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10)]));
        sync_once();
        stop_display();
        assert!(prepare_paint(), "suspended -> nothing to fetch");
        assert_eq!(
            shim::host::calls_matching("http_get").len(),
            1,
            "schedule only: suspended canvas issues no bitmap GET"
        );
    }

    #[test]
    fn a_changed_schedule_carries_over_the_kept_pages_bitmap() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        // First schedule: two pages, both painted so both bitmaps are resident.
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xb2)), &bitmap_body(0xb2));
        assert!(sync_once());
        assert!(paint_if_changed(), "page 0 paints and becomes resident");
        next(); // manual step paints page 1, so its bitmap is resident too
        let (page0_before, page1_before) = with_table(|t| (t.pages[0].bitmap, t.pages[1].bitmap));
        assert!(!page0_before.is_null() && !page1_before.is_null(), "both bitmaps resident before the re-sync");
        assert_ne!(page0_before, page1_before);
        let gets_before = shim::host::calls_matching("http_get").len();
        // Second schedule: a DIFFERENT schedule_md5 (0x22, so the unchanged
        // fast path cannot fire) keeping page 0 and replacing page 1.
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_md5(0x22, &[(0xa1, 10), (0xc3, 5)]));
        assert!(sync_once());
        let (page0_after, page1_after) = with_table(|t| (t.pages[0].bitmap, t.pages[1].bitmap));
        assert_eq!(page0_after, page0_before, "the kept page's bitmap moved into the new table, not re-downloaded");
        assert!(page1_after.is_null(), "the dropped page's slot is null, not a stale pointer");
        let fresh = &shim::host::calls_matching("http_get")[gets_before..];
        assert_eq!(fresh.len(), 1, "the second sync hit only the schedule endpoint");
        assert!(fresh.iter().all(|c| !c.contains(&md5hex(0xa1))), "no new GET for the carried-over page");
    }

    #[test]
    fn a_failed_page_fetch_records_nothing_so_the_next_wake_retries() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[(0xa1, 10), (0xb2, 5)]));
        shim::host::script_get(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), 500, b"");

        sync_once();
        assert!(
            with_table(|t| t.have_schedule_md5),
            "the sync commits on receipt; the retry is the paint path's job now"
        );
        assert!(!paint_if_changed(), "no bitmap -> nothing painted");

        // The retry mechanism is the RTC record: it is written only once a
        // bitmap has landed. This assertion is what replaces the old "do not
        // commit the schedule while a bitmap is missing" gate, so it is the
        // one this test must pin.
        let rec = read_panel_record();
        assert_ne!(
            &rec.0[8..40],
            md5hex(0xa1).as_bytes(),
            "a failed fetch must not be recorded as displayed"
        );

        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        sync_once();
        assert!(paint_if_changed(), "the retry paints once the bitmap arrives");
        let rec = read_panel_record();
        assert_eq!(&rec.0[8..40], md5hex(0xa1).as_bytes(), "and only then is it recorded");
    }

    #[test]
    fn a_suspended_canvas_does_not_paint_even_when_the_target_changes() {
        let _g = shim::host::lock();
        reset_for_test();
        setup_two_pages();
        shim::host::set_fb();

        // The UI owns the panel while the target moves: nothing may paint.
        stop_display();
        next();
        assert_eq!(shim::host::refreshes(), 0, "no refresh while suspended");
        assert!(!paint_if_changed(), "suspended -> never paints, even for a new target");
        assert_eq!(shim::host::refreshes(), 0);

        // Handing the screen back force-paints the current target once.
        allow_display();
        assert_eq!(shim::host::fb()[0], 0xb2, "the override target is on the panel");
        assert_eq!(shim::host::refreshes(), 1);
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

    #[test]
    fn schedule_url_carries_power_counters() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(7, 1234, 567, 2, 890);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("/api/pages/schedule?")),
                "schedule GET without a query string: {gets:?}");
        for kv in ["w=7", "a=1234", "r=567", "g=2", "f=890"] {
            assert!(gets.iter().any(|c| c.contains(kv)), "{kv} missing from {gets:?}");
        }
    }
    #[test]
    fn each_schedule_poll_advances_the_http_get_counter() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(0, 0, 0, 0, 0);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let mut c = [0u32; 5];
        unsafe { shim::rf_power_counters(&mut c[0], &mut c[1], &mut c[2], &mut c[3], &mut c[4]) };
        assert_eq!(c[3], 1, "one schedule GET attempted -> g advanced by one: {c:?}");
        sync_once();
        unsafe { shim::rf_power_counters(&mut c[0], &mut c[1], &mut c[2], &mut c[3], &mut c[4]) };
        assert_eq!(c[3], 2, "second poll -> g advanced again: {c:?}");
    }

    #[test]
    fn every_refresh_path_books_submit_time_at_the_single_exit() {
        // F2: canvas paint and the empty hint must both move f — accounting
        // lives in rf_request_full_refresh, not per caller. (Notify's popup
        // funnels through the same exit; its path is covered by notify tests
        // owning refreshes(), here we pin the counter wiring.)
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_fb();
        shim::host::set_counters(0, 0, 0, 0, 0);
        assert_eq!(shim::host::refresh_submit_ms(), 0);
        // 1) canvas paint path.
        scripted_schedule_with_position(0, 240, &[(0xa1, 10)]);
        shim::host::script_ok(&format!("/api/pages/bitmap/{}.bin", md5hex(0xa1)), &bitmap_body(0xa1));
        assert!(sync_once());
        assert!(paint_if_changed(), "first paint spends one refresh request");
        assert_eq!(shim::host::refresh_submit_ms(), 1, "canvas paint books one submit");
        // 2) empty-hint path (direct, no schedule needed).
        show_empty_hint();
        assert_eq!(shim::host::refresh_submit_ms(), 2, "empty hint books one submit");
    }

    #[test]
    fn the_snapshot_on_the_wire_is_the_staged_snapshot() {
        // F6: pin the sampling instant — the schedule GET must carry exactly
        // the counters staged before sync_once, not a re-read taken later.
        // Host RTC caveat (see report): the POWER stub is process RAM, not
        // RTC_DATA_ATTR, so this proves the wiring (sample-then-send), not
        // the deep-sleep persistence itself — persistence is by inspection
        // (RTC_DATA_ATTR next to g_fail_streak in shim.cpp).
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(7, 1234, 567, 2, 890);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        let sched = gets.iter().find(|c| c.contains("/api/pages/schedule?"))
            .expect("schedule GET with a query string");
        // Parse the ?w=&a=&r=&g=&f= back out of the URL and compare against
        // a fresh counter read: the wire snapshot must equal staged state.
        let query = sched.split('?').nth(1).unwrap_or("");
        let mut wire = [0u32; 5];
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or(("", ""));
            let n: u32 = v.parse().unwrap_or(999_999);
            match k {
                "w" => wire[0] = n,
                "a" => wire[1] = n,
                "r" => wire[2] = n,
                "g" => wire[3] = n,
                "f" => wire[4] = n,
                _ => {}
            }
        }
        // g on the wire is the pre-GET value (sampled before this cycle's
        // own GET increments the counter); the post-sync read is one higher.
        let mut c = [0u32; 5];
        unsafe { shim::rf_power_counters(&mut c[0], &mut c[1], &mut c[2], &mut c[3], &mut c[4]) };
        assert_eq!(&wire[..3], &c[..3], "w/a/r on the wire equal staged counters");
        assert_eq!(wire[3] + 1, c[3], "g on the wire is staged g (this GET counted after sampling)");
        assert_eq!(wire[4], c[4], "f on the wire equals staged f (no paint this cycle)");
    }
}
