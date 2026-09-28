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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::battery_activity_policy;
use crate::json;
use crate::log_upload_policy;
use crate::page_compare_policy;
use crate::protocol_parse;
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
// MAX_POLL_MINUTES now lives in `protocol_parse` (Task 6, design §7);
// `minutes_to_s` routes through it.
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
/// "No new information" reads as pending (fetch, exactly as today): an old
/// server sends no field, and every sync failure (alloc/fetch/parse) resets
/// to true so a stale `false` from an earlier cycle can never suppress a poll.
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
    let mut panel = [0u32; 2];
    unsafe { shim::rf_panel_activity_counters(&mut panel[0], &mut panel[1]) };
    let rr = unsafe { shim::rf_last_reset_reason() };
    let _ = write!(path,
        "?w={}&a={}&r={}&g={}&f={}&er={}&eb={}&rr={}",
        c[0], c[1], c[2], c[3], c[4], panel[0], panel[1], rr);

    // Battery relative-activity report (task 4, design §5): C++ supplies the
    // facts, Rust owns accept / outlier-filter / direction / relative
    // activity / next-gate. The cheap gate runs BEFORE the 10-sample ADC
    // burst, so inside the window with no direction change no read happens
    // at all; v/p/c ride only when the policy accepts, and the RTC stamp
    // advances only after an accepted sample — one ADC hiccup can't silence
    // an hour of telemetry. `c=` carries Rust's `direction`; `p=` stays the
    // C++ display mapping (protocol value, deliberately not a Rust output).
    let mut bat = battery_activity_policy::SampleInputs {
        voltage_mv: 0,
        charge: battery_activity_policy::DIRECTION_UNKNOWN,
        has_sample: false,
        _pad: [0; 4],
        now_s: 0,
        last_sample_s: -1,
        prev_mv: 0,
        prev_charge: battery_activity_policy::DIRECTION_UNKNOWN,
        prev_valid: false,
        min_interval_s: 0,
    };
    if unsafe { shim::rf_battery_activity_context(&mut bat) } == 1
        && battery_activity_policy::gate_open(&bat)
    {
        let mut pct: u8 = 0;
        if unsafe { shim::rf_battery_activity_read(&mut bat, &mut pct) } == 1 {
            let out = battery_activity_policy::decide(&bat);
            if out.accept == 1 {
                let _ = write!(path, "&v={}&p={}&c={}", bat.voltage_mv, pct, out.direction);
                unsafe {
                    shim::rf_battery_activity_commit(
                        out.next_sample_s, bat.voltage_mv, out.direction);
                }
            }
            log_i!("PageSync",
                   "battery v={} p={} dir={} act={} accept={} filtered={}",
                   bat.voltage_mv, pct, out.direction, out.relative_activity,
                   out.accept, out.filtered);
        }
    }

    // Task 5b: report the device's OWN switch opinion so the admin UI can say
    // "the device has it off" instead of showing an enabled service setting
    // that appears to do nothing. Read straight from the shim: there is no
    // Inputs struct at uplink time, and flattening the three states here would
    // make the server store 0 (off) for a device that has no opinion.
    let _ = write!(path, "&lo={}", unsafe { shim::rf_log_upload_local_get() });

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
    /// The service's log-upload opinion (Task 5), three-state:
    /// OPINION_NONE / OFF / ON. `null` and an absent key both mean "no
    /// opinion" — deliberately NOT the same as "off", or the device's own
    /// switch could never take effect.
    pub log_upload: u8,
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
    // Task 6 (design §7): the scaling rule lives in `protocol_parse`;
    // JSON extraction (the DOM walk) stays here at the call site.
    let (present, value) = match minutes {
        Some(m) => (1, m),
        None => (0, 0),
    };
    protocol_parse::policy_minutes_to_s(present, value, fallback)
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

/// The service's log-upload opinion from `policy.log_upload`, as a three-state
/// value. Pure, so it is unit-tested on the host.
///
/// JSON `null` and an absent key BOTH mean "no opinion" (OPINION_NONE) — not
/// "off". Collapsing the two would make the service permanently override the
/// device's own switch, which is the whole point of a nullable column.
///
/// The distinction `int_value` cannot make: it parses with `parse::<i64>()`, so
/// `null` and "key absent" both come back `None`. `member` does distinguish
/// them — `None` = no such key, `Some(at)` = the key is there — and at that
/// offset a leading `n` is the literal `null`.
fn parse_log_upload_opinion(body: &[u8]) -> u8 {
    let Some(at) = json::path(body, &["policy", "log_upload"]) else {
        return log_upload_policy::OPINION_NONE;  // key absent = old server
    };
    if body.get(at) == Some(&b'n') {
        return log_upload_policy::OPINION_NONE;  // explicit JSON null
    }
    match json::int_value(body, at) {
        Some(1) => log_upload_policy::OPINION_ON,
        Some(0) => log_upload_policy::OPINION_OFF,
        // Anything else (a string, a float, an out-of-range number) is not an
        // opinion we can act on; treat it as silence rather than guessing.
        _ => log_upload_policy::OPINION_NONE,
    }
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
    let log_upload = parse_log_upload_opinion(body);
    let mut out = ParsedSchedule {
        md5,
        pages: [EMPTY_PAGE; MAX_PAGES],
        count: 0,
        policy: parse_policy(body),
        current_index,
        seconds_until_next_page,
        notify_pending,
        log_upload,
    };
    json::for_each_item(body, pages_at, &mut |item| {
        if out.count >= MAX_PAGES {
            return false;
        }
        // Task 6 (design §7): entry usability + duration classification
        // lives in `protocol_parse`; the DOM reads stay here.
        let md5_len = json::member(body, item, "md5")
            .and_then(|at| json::str_value(body, at))
            .map(|v| v.len() as u32)
            .unwrap_or(0);
        let (has_duration, dur_min) = json::member(body, item, "duration_minutes")
            .and_then(|at| json::int_value(body, at))
            .map(|v| (1u8, v))
            .unwrap_or((0, 0));
        let entry = protocol_parse::decide_schedule_entry(
            &protocol_parse::ScheduleEntryFacts {
                md5_len,
                has_duration,
                _pad: [0; 3],
                duration_minutes: dur_min,
            },
        );
        if entry.usable == 0 {
            return true;
        }
        let Some(page_md5) = json::member(body, item, "md5")
            .and_then(|at| json::str_value(body, at))
            .and_then(md5_from)
        else {
            return true;
        };
        out.pages[out.count] = ParsedPage { md5: page_md5, duration_s: entry.duration_s };
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
        // No new information about the queue: stay fetchable (as today).
        NOTIFY_PENDING.store(true, Ordering::Release);
        return false;
    }
    // Same one-byte-over-allocation as the notify response: the wrapper's
    // capacity includes the terminator it appends.
    let body = unsafe { core::slice::from_raw_parts_mut(raw, SCHEDULE_BUF + 1) };
    let ok = if let Some(len) = fetch_schedule(body) {
        sync_schedule(&body[..len])
    } else {
        // Fetch failed (non-200/timeout): this cycle knows nothing about the
        // queue, so read as pending — exactly today's unconditional fetch.
        NOTIFY_PENDING.store(true, Ordering::Release);
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
        // Parsed nothing: no queue information either — read as pending.
        NOTIFY_PENDING.store(true, Ordering::Release);
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
    // Same reasoning for the log-upload opinion: commit on every parsed body,
    // including the unchanged-md5 fast path below, so a service that stops
    // sending the key falls back to "no opinion" instead of keeping a stale
    // "on" forever.
    log_upload_server_set(parsed.log_upload);

    let policy_changed = with_table(|t| {
        let prev = t.policy;
        t.policy = new_policy;
        prev != new_policy
    });
    if policy_changed {
        log_i!("PageSync", "policy: poll={}s sleep_poll={}s screen_active={}",
            new_policy.poll_s, new_policy.sleep_poll_s, new_policy.screen_active);
    }

    // 99% path (Task 5, design §6): the schedule comparison decision lives
    // in `page_compare_policy::decide_schedule` — same-hash short-circuit
    // vs changed-hash/cold-cache rebuild. The position answer applies and
    // the manual override releases on both update paths; no bitmaps are
    // involved either way (the paint path fetches on demand itself).
    let sched_decision = {
        let (cached, have) = with_table(|t| (t.schedule_md5, t.have_schedule_md5));
        page_compare_policy::decide_schedule(&page_compare_policy::ScheduleInputs {
            fetch_ok: 1,
            body_usable: 1,
            has_cached: have as u8,
            _pad: [0; 5],
            cached_md5: cached,
            server_md5: new_md5,
        })
    };
    if sched_decision.action == page_compare_policy::COMPARE_SKIP_SAME {
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

// ── page-level comparison (Task 5, design §6) ─────────────────────────────
// The record-trust + glass-match comparison lives in
// `page_compare_policy::decide_page`; these helpers only translate the
// caller's facts (RTC record bytes, RAM residency, sync result) into the
// policy inputs. Magic AND valid gate trust (an all-zero power-on record
// is "no known content", never a match); the index participates only in
// `prepare_paint` (the target must be the recorded one), while
// `paint_if_changed` keys Content on the md5 alone — matching today's
// exact behaviour.
fn record_trusted(rec: &[u8; 48]) -> bool {
    record_magic_ok(rec) && rec[4] != 0
}

/// Page decision for the `prepare_paint` path (index-scoped).
fn prepare_decision(idx: usize, md5: &[u8; MD5_LEN], resident: bool, rec: &[u8; 48]) -> page_compare_policy::PageDecision {
    page_compare_policy::decide_page(&page_compare_policy::PageInputs {
        bitmap_resident: resident as u8,
        record_trusted: record_trusted(rec) as u8,
        glass_matches: (record_index(rec) == idx as i32 && rec[8..40] == md5[..]) as u8,
        sync_ok: 1,
        has_target: 1,
        _pad: [0; 3],
    })
}

/// Page decision for the `paint_if_changed` path (md5-scoped, index-free).
fn paint_decision(md5: &[u8; MD5_LEN], rec: &[u8; 48]) -> page_compare_policy::PageDecision {
    let resident = false; // decided by the caller via ensure_bitmap below
    page_compare_policy::decide_page(&page_compare_policy::PageInputs {
        bitmap_resident: resident as u8,
        record_trusted: record_trusted(rec) as u8,
        glass_matches: (rec[8..40] == md5[..]) as u8,
        sync_ok: LAST_SYNC_OK.load(Ordering::Acquire) as u8,
        has_target: 1,
        _pad: [0; 3],
    })
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
    let resident = !with_table(|t| t.pages[idx].bitmap.is_null());
    // Task 5 (design §6): the fetch/skip decision is the policy's —
    // SkipSame (trusted record matches index+md5) and UseCache (bitmap
    // already resident) both mean "nothing to download"; Fetch and
    // InvalidateCache fall through to the on-demand download.
    let decision = prepare_decision(idx, &md5, resident, &rec.0);
    match decision.action {
        page_compare_policy::COMPARE_SKIP_SAME
        | page_compare_policy::COMPARE_USE_CACHE => true,
        _ => !ensure_bitmap(idx, &md5).is_null(),
    }
}

/// Paint the target page only if the glass does not already show it.
pub fn paint_if_changed() -> bool {
    if SUSPENDED.load(Ordering::Acquire) {
        return false;
    }
    let Some(idx) = target_index() else {
        // Task 5 (design §6): the failure fallback is the policy's —
        // failed sync + empty table -> UseCache (keep the glass: every
        // deep-sleep wake starts empty, so painting here would white-refresh
        // over the last good page and spend a second refresh restoring it);
        // successful empty sync with the hint already recorded (index -1)
        // -> SkipSame; otherwise -> InvalidateCache, draw the hint. The
        // hint is a recorded STATE, not unrecorded draw: keyed on the index
        // only (its md5 payload is 32 zero bytes, which the shim stores as
        // an empty string, so md5 comparison here would never match).
        let rec = read_panel_record();
        let decision = page_compare_policy::decide_page(&page_compare_policy::PageInputs {
            bitmap_resident: 0,
            record_trusted: record_trusted(&rec.0) as u8,
            glass_matches: (record_index(&rec.0) == -1) as u8,
            sync_ok: LAST_SYNC_OK.load(Ordering::Acquire) as u8,
            has_target: 0,
            _pad: [0; 3],
        });
        if decision.continue_paint == 0 {
            return false;
        }
        show_empty_hint();
        return true;
    };
    let md5 = with_table(|t| if idx < t.count { Some(t.pages[idx].md5) } else { None });
    let Some(md5) = md5 else { return false; };
    let rec = read_panel_record();
    // Task 5 (design §6): the same-glass short-circuit is the policy's
    // decision — magic AND valid gate trust, md5 equality gates the match.
    // The log line stays at the call site (mechanism, not policy).
    if paint_decision(&md5, &rec.0).action == page_compare_policy::COMPARE_SKIP_SAME {
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


// ── device log upload (Task 5) ──────────────────────────────────────────────
// The decision lives in `log_upload_policy.rs` (pure); this owns the
// orchestration: snapshot the ring, ask the gate, POST, and ack only what the
// server accepted.

/// The service's log-upload opinion from the last schedule response, as a
/// three-state value. Kept in the RTC snapshot with the power counters, so a
/// deep-sleep wake (RAM cleared) reads OPINION_NONE until the next sync.
pub fn log_upload_server_get() -> u8 {
    unsafe { shim::rf_log_upload_server_get() }
}

pub fn log_upload_server_set(v: u8) {
    unsafe { shim::rf_log_upload_server_set(v) }
}

/// The read is capped at the policy's own per-upload ceiling, so
/// `take == read_len` always holds and the ack range can never run past what
/// was actually sent. (Reading more and then truncating is the bug this
/// comment replaces: `seq_hi` came from the untruncated line count, so a
/// backlog over 1024 B acked lines that never left the device.)
const LOG_READ_BUF: usize = log_upload_policy::MAX_UPLOAD_BYTES as usize;
const LOG_B64_BUF: usize = 2048;   // base64(1024 B) = 1368 chars, rounded up
const LOG_BODY_BUF: usize = 2048;  // envelope + base64 + NUL
const LOG_URL_BUF: usize = 320;
const LOG_HTTP_TIMEOUT_MS: i32 = 10_000;

/// One upload attempt. Returns true when the server accepted the batch.
///
/// Caller must already hold no lock: HTTP can block for seconds.
///
/// The three working buffers live in PSRAM, not on the stack: this runs on the
/// esp_timer task (8 KB) and the base64 + JSON + read buffers would add ~5.6 KB
/// of frame on top of the mbedtls chain — and a stack overflow there would
/// corrupt the very RTC ring this feature exists to protect. Same reason
/// `sync_once` keeps the schedule buffer in PSRAM.
pub fn log_upload_try_once() -> bool {
    let mut dropped = 0u32;
    let mut used = 0u32;
    unsafe { shim::rf_logbuf_stats(&mut dropped, &mut used) };

    // Read BEFORE building the inputs: `rf_logbuf_read` is the only source
    // that knows how many whole frames the pending bytes contain, and that
    // count is what the ack arithmetic needs. Reading is a bounded byte copy
    // under the ring lock — cheap, unlike the HTTP that `decide` gates.
    // `seq_lo` is meaningful only when `lines > 0`.
    let read_ptr = unsafe { shim::rf_alloc(LOG_READ_BUF + 1) };
    let b64_ptr = unsafe { shim::rf_alloc(LOG_B64_BUF) };
    let body_ptr = unsafe { shim::rf_alloc(LOG_BODY_BUF) };
    if read_ptr.is_null() || b64_ptr.is_null() || body_ptr.is_null() {
        log_e!("PageSync", "log upload alloc failed");
        unsafe {
            if !read_ptr.is_null() { shim::rf_free(read_ptr); }
            if !b64_ptr.is_null() { shim::rf_free(b64_ptr); }
            if !body_ptr.is_null() { shim::rf_free(body_ptr); }
        }
        return false;
    }
    // Guards keep the two exit paths below from duplicating the frees.
    let ok = log_upload_with_buffers(read_ptr, b64_ptr, body_ptr, used, dropped);
    unsafe {
        shim::rf_free(read_ptr);
        shim::rf_free(b64_ptr);
        shim::rf_free(body_ptr);
    }
    ok
}

/// The attempt proper, with all three buffers already in PSRAM.
fn log_upload_with_buffers(
    read_ptr: *mut u8,
    b64_ptr: *mut u8,
    body_ptr: *mut u8,
    used: u32,
    dropped: u32,
) -> bool {
    let mut seq_lo = 0u32;
    let mut lines = 0u32;
    unsafe {
        shim::rf_logbuf_read(read_ptr as *mut core::ffi::c_char, LOG_READ_BUF as i32,
                             &mut seq_lo, &mut lines);
    }
    // `rf_logbuf_read` NUL-terminates within cap, so scan for it in place.
    let read_slice = unsafe { core::slice::from_raw_parts(read_ptr, LOG_READ_BUF) };
    let read_len = read_slice.iter().position(|b| *b == 0).unwrap_or(LOG_READ_BUF);

    let mut last_fail_s = -1i64;
    let mut fail_streak = 0u32;
    unsafe { shim::rf_log_upload_fail_state(&mut last_fail_s, &mut fail_streak) };
    let inputs = log_upload_policy::Inputs {
        // Both opinions are three-state: the schedule policy is 0=none/1=off/
        // 2=on, and the device's own NVS switch uses the same encoding.
        local_set: unsafe { shim::rf_log_upload_local_get() },
        server_set: log_upload_server_get(),
        has_pending: if used > 0 { 1 } else { 0 },
        wifi_ready: 1,  // the caller only reaches here on a completed sync
        pending_bytes: used,
        pending_lines: lines,
        fail_streak,
        _pad: [0; 4],
        last_fail_s,
        now_s: unsafe { shim::rf_time_now_s() },
    };
    let d = unsafe { shim::rf_log_upload_decide(&inputs) };
    if d.action != log_upload_policy::UPLOAD {
        return false;
    }
    // A UPLOAD verdict with nothing to send: `pending_bytes` was non-zero but
    // no whole frame fit the read buffer. Acking here would compute
    // `seq_lo + 0 - 1` and underflow to 0xFFFFFFFF, acking the ENTIRE ring.


    // THE zero-line guard, and the only one. `lines == 0` implies an empty
    // read, so the `seq_hi` computed just below would be `seq_lo + 0 - 1` =
    // 0xFFFFFFFF and the ack would swallow the entire ring. A second early
    // return catching the same input silently hides this line from the tests:
    // a `n == 0` base64 check did exactly that (encode_slice on an empty slice
    // returns Ok(0)), and the suite stayed green after this guard was deleted.
    if lines == 0 {
        log_w!("PageSync", "log upload gated on but no whole line fits; not acking");
        return false;
    }

    // The read was capped at MAX_UPLOAD_BYTES and the ring hands out whole
    // frames only, so everything read is inside the policy's byte budget and
    // `seq_hi` below covers exactly the payload that was sent.
    let take = read_len;
    let seq_hi = seq_lo + lines - 1;
    let b64 = unsafe { core::slice::from_raw_parts_mut(b64_ptr, LOG_B64_BUF) };
    let n = B64.encode_slice(&read_slice[..take], b64).unwrap_or(0);
    // The envelope head is ~48 bytes of ASCII JSON and stays on the stack; the
    // base64 payload and the assembled body are what live in PSRAM. base64
    // output is ASCII, so no JSON escaping is needed.
    use core::fmt::Write as _;
    let mut head = CBuf::<64>::new();
    let _ = write!(head, r#"{{"seq_hi":{},"dropped":{},"lines":"#, seq_hi, dropped);
    if head.overflowed() {
        log_w!("PageSync", "log upload envelope overflow");
        record_log_upload(false, fail_streak);
        return false;
    }
    let body = unsafe { core::slice::from_raw_parts_mut(body_ptr, LOG_BODY_BUF) };
    let b64_slice = unsafe { core::slice::from_raw_parts(b64_ptr, n) };
    let mut written = 0usize;
    // +3: the closing quote, the closing brace and the NUL.
    if head.as_bytes().len() + n + 3 > body.len() {
        log_w!("PageSync", "log upload body overflow");
        record_log_upload(false, fail_streak);
        return false;
    }
    body[..head.as_bytes().len()].copy_from_slice(head.as_bytes());
    written = head.as_bytes().len();
    body[written] = b'"';
    written += 1;
    body[written..written + n].copy_from_slice(b64_slice);
    written += n;
    body[written] = b'"';
    written += 1;
    body[written] = b'}';
    written += 1;
    body[written] = 0;

    let mut path = CBuf::<64>::new();
    path.push("/api/device-log");
    let mut url = CBuf::<LOG_URL_BUF>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), LOG_URL_BUF as i32) } == 0 {
        log_w!("PageSync", "cannot build device-log endpoint");
        record_log_upload(false, fail_streak);
        return false;
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };
    // Response buffer stays on the stack: the endpoint answers a tiny JSON.
    let mut resp = CBuf::<128>::new();
    let mut resp_len = 128i32;
    let body_ptr_c = body_ptr as *const core::ffi::c_char;
    let status = unsafe {
        shim::rf_http_post_json(url.as_ptr(), token.as_ptr(), body_ptr_c,
                                resp.as_mut_ptr(), &mut resp_len,
                                LOG_HTTP_TIMEOUT_MS)
    };
    // 201 Created is the endpoint's declared success code.
    if status == 201 {
        unsafe { shim::rf_logbuf_ack(seq_hi) };
        record_log_upload(true, fail_streak);
        log_i!("PageSync", "log upload ok: {} lines, {} B", lines, take);
        return true;
    }
    log_w!("PageSync", "log upload failed (status={}), {} lines stay pending",
           status, lines);
    record_log_upload(false, fail_streak);
    false
}

/// Book one attempt's outcome for the backoff ladder: a success clears the
/// streak, a failure advances it (capped, so the shift stays bounded).
fn record_log_upload(ok: bool, streak: u32) {
    let next = if ok { 0 } else { (streak + 1).min(8) };
    unsafe { shim::rf_log_upload_fail_record(shim::rf_time_now_s(), if ok { 0 } else { 1 }, next) }
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

/// One log-upload attempt inside the caller's power cycle. True when the
/// server accepted the batch.
#[unsafe(no_mangle)]
pub extern "C" fn page_sync_log_upload_once() -> bool {
    log_upload_try_once()
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

    #[test]
    fn a_failed_sync_leaves_notifications_fetchable() {
        // F1: a failed fetch or an unparseable body carries no queue
        // information — the gate must read pending (today's behaviour),
        // never a stale `false` from an earlier cycle.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_notify(0x11, &[], Some(false)));
        assert!(sync_once());
        assert!(!notify_pending());
        // Path 1: fetch fails (non-200).
        shim::host::script_get("/api/pages/schedule", 500, b"");
        assert!(!sync_once());
        assert!(notify_pending(), "fetch failure must not reuse the stale false");
        // Path 2: fetch succeeds but the body does not parse.
        shim::host::script_ok("/api/pages/schedule", &schedule_json_with_notify(0x11, &[], Some(false)));
        assert!(sync_once());
        assert!(!notify_pending());
        shim::host::script_ok("/api/pages/schedule", b"not json");
        assert!(!sync_once());
        assert!(notify_pending(), "unparseable body must not reuse the stale false");
    }
    /// `schedule_json` plus a `policy.log_upload` field carrying `raw`
    /// verbatim, so a test can send `1`, `0` or JSON `null`.
    fn schedule_json_log_upload_raw(raw: &str) -> Vec<u8> {
        format!(
            r#"{{"schedule_md5":"{}","policy":{{"log_upload":{}}},"pages":[]}}"#,
            md5hex(0x11),
            raw,
        )
        .into_bytes()
    }

    fn schedule_json_log_upload(n: i32) -> Vec<u8> {
        schedule_json_log_upload_raw(&n.to_string())
    }

    fn schedule_json_log_upload_null() -> Vec<u8> {
        schedule_json_log_upload_raw("null")
    }

    #[test]
    fn schedule_policy_log_upload_is_parsed_as_three_states() {
        // The service switch arrives in the policy block (the query string has
        // no room: CBuf::<160> is already ~93 bytes and overflow voids the
        // whole GET). null must survive as OPINION_NONE -- collapsing it to 0
        // would make the service permanently override the local switch.
        let _g = shim::host::lock();

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload(1));
        assert!(sync_once());
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_ON);

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload(0));
        assert!(sync_once());
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_OFF);

        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json_log_upload_null());
        assert!(sync_once());
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_NONE,
                   "null is 'no opinion', not 'off'");

        // Old firmware / old server: the key is absent entirely. Same meaning.
        reset_for_test();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        assert!(sync_once());
        assert_eq!(log_upload_server_get(), log_upload_policy::OPINION_NONE);
    }

    #[test]
    fn an_accepted_upload_acks_exactly_the_lines_it_sent() {
        // The ack arithmetic is the dangerous part: `seq_lo + lines - 1` with
        // lines == 0 underflows to 0xFFFFFFFF and acks the whole ring.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_ON, log_upload_policy::OPINION_NONE);
        shim::host::stage_log_lines(&["boot line", "sync line"]);
        shim::host::script_get("/api/device-log", 201, b"{}");

        assert!(log_upload_try_once(), "201 -> accepted");
        // The stub reports seq_lo = 7 for 2 lines -> seq_hi = 8.
        assert_eq!(shim::host::log_acked(), Some(8));
        assert_eq!(shim::host::log_fail_state().1, 0, "success clears the streak");

        let posted = shim::host::calls_matching("http_post");
        assert_eq!(posted.len(), 1, "exactly one POST per attempt");
        assert!(posted[0].contains("/api/device-log"), "posted to the log endpoint");
        assert!(posted[0].contains("seq_hi"), "carries seq_hi");
        // The whole point of the POST is a body the server can parse; a bare
        // substring check did NOT catch the doubled-quote bug the final review
        // found (every upload was a 422). Parse the body with the crate's own
        // scanner and require the three fields the endpoint needs. The body is
        // the third whitespace-separated token of the recorded "http_post" line.
        let body = posted[0].splitn(3, ' ').nth(2).expect("http_post carries the body");
        let b = body.as_bytes();
        assert!(crate::json::skip_value(b, 0).is_some(), "body is well-formed JSON: {body}");
        let hi = crate::json::member(b, 0, "seq_hi").and_then(|at| crate::json::int_value(b, at));
        assert!(hi.is_some(), "seq_hi parses as an int: {body}");
        let dr = crate::json::member(b, 0, "dropped").and_then(|at| crate::json::int_value(b, at));
        assert!(dr.is_some(), "dropped parses as an int: {body}");
        let ln = crate::json::member(b, 0, "lines").and_then(|at| crate::json::str_value(b, at));
        assert!(matches!(ln, Some(s) if !s.is_empty()), "lines is a non-empty string: {body}");
    }

    #[test]
    fn the_drop_marker_is_reported_once_per_batch() {
        // g_ring.dropped is cumulative; acking a batch must account for the
        // drops it carried, or every later batch re-stamps the same stale
        // total into the file (the final-review P2). First upload reports the
        // backlog's drops; the next reports only what dropped in between.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_ON, log_upload_policy::OPINION_NONE);
        shim::host::stage_log_lines(&["first batch"]);
        shim::host::script_get("/api/device-log", 201, b"{}");

        let dropped_of = |posted: &[String]| {
            let body = posted[0].splitn(3, ' ').nth(2).unwrap();
            let b = body.as_bytes();
            crate::json::member(b, 0, "dropped").and_then(|at| crate::json::int_value(b, at))
        };

        assert!(log_upload_try_once());
        let first = shim::host::calls_matching("http_post");
        assert_eq!(dropped_of(&first), Some(3), "the first batch carries the backlog's drops");

        // A second batch with no new drops must report 0, not the stale 3.
        shim::host::stage_log_lines(&["second batch"]);
        assert!(log_upload_try_once());
        let all = shim::host::calls_matching("http_post");
        assert_eq!(all.len(), 2, "two uploads");
        assert_eq!(dropped_of(&all[1..]), Some(0), "ack accounted for the earlier drops");
    }

    #[test]
    fn a_rejected_upload_keeps_the_lines_and_advances_the_streak() {
        // read does not move the tail: a failed upload must leave the lines
        // pending, or the device silently discards the crash we wanted.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_ON, log_upload_policy::OPINION_NONE);
        shim::host::stage_log_lines(&["boot line"]);
        shim::host::script_get("/api/device-log", 500, b"");

        assert!(!log_upload_try_once(), "500 -> not accepted");
        assert_eq!(shim::host::log_acked(), None, "nothing acked on failure");
        assert_eq!(shim::host::log_fail_state().1, 1, "the streak climbs");
    }

    #[test]
    fn a_zero_line_read_never_acks() {
        // The line must be LONGER than the read cap, so `used > 0` (the gate
        // passes) while the read returns no whole frame (lines == 0). Staging
        // zero lines would return SKIP_EMPTY at the decide branch and never
        // reach the `lines == 0` guard at all — the test would pass with the
        // guard deleted, which is exactly what it did before.
        let long = "x".repeat(LOG_READ_BUF + 64);
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_ON, log_upload_policy::OPINION_NONE);
        shim::host::stage_log_lines(&[&long]);
        shim::host::script_get("/api/device-log", 201, b"{}");

        assert!(!log_upload_try_once());
        assert_eq!(shim::host::log_acked(), None, "zero lines must not ack");
    }

    #[test]
    fn a_backlog_over_the_cap_sends_everything_it_acks() {
        // The read is capped at MAX_UPLOAD_BYTES and the ring hands out whole
        // frames only, so `seq_hi` always covers exactly the payload that
        // went on the wire. Truncating after the read (the old code) acked
        // lines that were never sent.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_ON, log_upload_policy::OPINION_NONE);
        // Far more than one cap of text: the ring must give back whole frames
        // only, and the ack must stop at the last one actually sent.
        let filler = "a".repeat(300);
        let lines: Vec<&str> = core::iter::repeat_n(filler.as_str(), 20).collect();
        shim::host::stage_log_lines(&lines);
        shim::host::script_get("/api/device-log", 201, b"{}");

        assert!(log_upload_try_once(), "201 -> accepted");
        let posted = shim::host::calls_matching("http_post");
        assert_eq!(posted.len(), 1);
        assert!(posted[0].contains("/api/device-log"));
        // Whatever the frame count, the ack must equal what was sent; the
        // stub hands out seq_lo = 7, so seq_hi = 7 + lines - 1.
        assert_eq!(shim::host::log_acked(), Some(7 + 3 - 1),
                   "1024 B cap admits 3 x 300-char frames; ack must match exactly");
    }

    /// Run one schedule sync and return the single URL it fetched.
    fn schedule_get_url(local_set: u8) -> String {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_NONE, local_set);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        assert!(sync_once());
        let gets = shim::host::calls_matching("http_get");
        assert_eq!(gets.len(), 1, "exactly one schedule GET per sync");
        gets[0].clone()
    }

    #[test]
    fn the_uplink_reports_no_opinion_as_zero() {
        // Task 5b: `&lo=` tells the service what the device's own switch
        // says. Unset must read as 0 (no opinion) — never off, which would
        // make the service store a decision the device never made.
        let url = schedule_get_url(log_upload_policy::OPINION_NONE);
        assert!(url.contains("&lo=0"), "no local opinion -> lo=0, got {url}");
    }

    #[test]
    fn the_uplink_reports_the_local_opinion_unflattened() {
        // 2 (on) must not read as 1, and 1 (off) must not read as 0.
        let url = schedule_get_url(log_upload_policy::OPINION_ON);
        assert!(url.contains("&lo=2"), "local on -> lo=2, got {url}");
        let url = schedule_get_url(log_upload_policy::OPINION_OFF);
        assert!(url.contains("&lo=1"), "local off -> lo=1, got {url}");
    }

    #[test]
    fn a_disabled_switch_never_reaches_the_network() {
        // Both sides silent resolves to off (log_upload_policy::decide), so
        // the POST must not happen at all.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_NONE, log_upload_policy::OPINION_NONE);
        shim::host::stage_log_lines(&["boot line"]);

        assert!(!log_upload_try_once());
        assert!(shim::host::calls_matching("http_post").is_empty(), "no POST when off");
    }

    #[test]
    fn an_explicit_service_off_overrides_a_local_on() {
        // Two separate tests, not two halves of one: the harness lock is a
        // single Mutex, so a second `lock()` in the same test would deadlock.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_OFF, log_upload_policy::OPINION_ON);
        shim::host::stage_log_lines(&["boot line"]);
        assert!(!log_upload_try_once(), "an explicit service 'off' wins");
        assert!(shim::host::calls_matching("http_post").is_empty());
    }

    #[test]
    fn a_silent_service_defers_to_a_local_on() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_log_opinion(log_upload_policy::OPINION_NONE, log_upload_policy::OPINION_ON);
        shim::host::stage_log_lines(&["boot line"]);
        shim::host::script_get("/api/device-log", 201, b"{}");
        assert!(log_upload_try_once(), "no service opinion -> the local switch decides");
        assert_eq!(shim::host::log_acked(), Some(7));
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
    fn schedule_url_carries_the_last_reset_reason() {
        // BROWNOUT vs RTCWDT vs SW distinction rides the schedule query string
        // just like the power counters: no serial console needed to see why a
        // remote device rebooted.
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(7, 1234, 567, 2, 890);
        shim::host::set_reset_reason(0xf); // esp_reset_reason_t BROWNOUT_RST on s3
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("rr=15")),
                "reset reason (0xf=15) missing from the schedule URL: {gets:?}");
    }

    #[test]
    fn schedule_url_carries_battery_sample_when_one_exists() {
        let _g = shim::host::lock();
        shim::host::set_counters(7, 1234, 567, 2, 890);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample(3980, 76, 4); // discharging
        shim::host::set_battery_due(true);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("v=3980&p=76&c=4")),
                "battery sample missing from the schedule URL: {gets:?}");
    }

    #[test]
    fn schedule_url_omits_battery_params_when_no_sample() {
        // mv=0 / 未设样本 = 传感器缺席或缺电读数：缺键而非零值（与 rr 语义一致）。
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample_none();
        shim::host::set_battery_due(true);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        let sched = gets.iter().find(|c| c.contains("/api/pages/schedule?"))
            .expect("schedule GET");
        assert!(!sched.contains("v=") && !sched.contains("&p=") && !sched.contains("&c="),
                "no-sample must omit v/p/c entirely: {sched}");
    }

    #[test]
    fn battery_params_wait_outside_the_one_hour_gate() {
        // Sliding gate (spec 2026-09-23): inside the window the schedule GET
        // rides WITHOUT v/p/c; peeking must not advance the RTC stamp either.
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample(3980, 76, 4);
        shim::host::set_battery_due(false);
        shim::host::clear_battery_armed();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        let sched = gets.iter().find(|c| c.contains("/api/pages/schedule?"))
            .expect("schedule GET");
        assert!(!sched.contains("v="), "inside the window v/p/c must be omitted: {sched}");
        assert!(!shim::host::battery_armed(), "a peek must not arm the gate");
        // The cheap gate must also spare the 10-sample ADC burst inside the
        // window: no read, no report, no arm.
        assert!(shim::host::calls_matching("battery_read").is_empty(),
                "inside the window the ADC burst must be skipped");
    }

    #[test]
    fn battery_params_report_when_due_and_arm_the_gate() {
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample(3980, 76, 4);
        shim::host::set_battery_due(true);
        shim::host::clear_battery_armed();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("v=3980&p=76&c=4")),
                "due report missing from the schedule URL: {gets:?}");
        assert!(shim::host::battery_armed(), "a real report must slide the gate");
        assert!(!shim::host::calls_matching("battery_read").is_empty(),
                "a due wake must perform the ADC burst");
    }

    #[test]
    fn a_failed_sample_leaves_the_gate_unarmed() {
        // Due but no sensor reading: nothing rides the wire, so the stamp must
        // NOT advance — one ADC hiccup would otherwise silence an hour of
        // telemetry. The next wake simply retries.
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample_none();
        shim::host::set_battery_due(true);
        shim::host::clear_battery_armed();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        assert!(!shim::host::battery_armed(), "failed sample must not arm the gate");
    }

    #[test]
    fn a_direction_transition_reports_inside_the_window() {
        // Task 4 rule (design §5): a charging->discharging flip must not wait
        // out the one-hour window — the transition sample rides even though
        // the time gate says "not due", and `c=` carries Rust's direction.
        let _g = shim::host::lock();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_reset_reason(3);
        shim::host::set_battery_sample(4050, 80, 4);  // now: unplugged, discharging
        shim::host::set_battery_prev(4100, 2);         // last report: charging
        shim::host::set_battery_due(false);            // window closed
        shim::host::clear_battery_armed();
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();
        let gets = shim::host::calls_matching("http_get");
        assert!(gets.iter().any(|c| c.contains("v=4050&p=80&c=4")),
                "transition sample missing from the schedule URL: {gets:?}");
        assert!(shim::host::battery_armed(), "an accepted transition arms the gate");
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
    fn schedule_get_carries_panel_activity_counters() {
        let _g = shim::host::lock();
        reset_for_test();
        shim::host::set_counters(1, 100, 50, 1, 0);
        shim::host::set_panel_activity_counters(7, 12345);
        shim::host::script_ok("/api/pages/schedule", &schedule_json(&[]));
        sync_once();

        let gets = shim::host::calls_matching("http_get");
        let sched = gets.iter().find(|c| c.contains("/api/pages/schedule?"))
            .expect("schedule GET with a query string");
        let query = sched.split('?').nth(1).unwrap_or("");
        let mut wire = [0u32; 2];
        let mut seen = [false; 2];
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or(("", ""));
            if let (key, Ok(n)) = (k, v.parse::<u32>()) {
                match key {
                    "er" => { wire[0] = n; seen[0] = true; }
                    "eb" => { wire[1] = n; seen[1] = true; }
                    _ => {}
                }
            }
        }
        assert_eq!(seen, [true, true],
                   "schedule GET must carry er/eb panel activity keys: {query}");
        assert_eq!(wire, [7, 12345],
                   "staged panel activity must ride the schedule GET: {query}");
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
