//! Pending-notification module: BOOT pulls the next notification, UP/DOWN ack
//! it, BOOT or a 5-minute timeout dismisses it.
//!
//! Port of `main/common/notify.cc`. The state is an atomic instead of a plain
//! enum read across tasks, and the notification id lives behind the shared
//! mutex rather than being raced on.
//!
//! Threading: `notify_init`/`request_next`/`is_active`/`post_ack`/`dismiss*` run
//! in the button-callback context; only the fetch/ack/timeout tasks and the
//! FreeRTOS timer callback cross into another context, and the fetch task only
//! runs while the state is `FETCHING`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::json;
use crate::page_sync;
use crate::shim::{self, CBuf};
use crate::{log_e, log_i, log_w};
use crate::notify_policy::{self, CONSUME_NONE_EMPTY, CONSUME_RETRY_BACKOFF,
    CONSUME_SHOW, CONSUME_SKIP_DUPLICATE, RESPONSE_BINARY_NOTIFY,
    RESPONSE_EMPTY, RESPONSE_JSON_FALLBACK, RESPONSE_JSON_NOTIFY, ResponseFacts};

const TAG: &str = "Notify";
const HTTP_TIMEOUT_MS: i32 = 10_000;
/// base64(30000 B) + metadata, with headroom.
const RESPONSE_BUF: usize = 45056;
/// Aligned with the server's 300 s notification TTL.
const TIMEOUT_MS: u32 = 5 * 60 * 1000;
const FETCH_STACK: u32 = 8192;
const TIMEOUT_STACK: u32 = 4096;
const TASK_PRIORITY: u8 = 3;
/// uuid hex(32) + NUL.
const ID_CAP: usize = 40;
/// ack block layout: id at 0, JSON body at 40.
const ACK_ID_OFF: usize = 40;
const ACK_BLOCK: usize = ACK_ID_OFF + 64;

const IDLE: u8 = 0;
const FETCHING: u8 = 1;
const NOTIFYING: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(IDLE);

struct Shared(UnsafeCell<[u8; ID_CAP]>);
// SAFETY: only accessed through `with_id`, which holds the state mutex.
unsafe impl Sync for Shared {}
static ID: Shared = Shared(UnsafeCell::new([0; ID_CAP]));

/// Dedup memory: the id of the last notification shown this boot. Kept
/// separate from [`ID`] because dismiss clears that one — a server re-offer
/// of an already-consumed id must skip instead of repainting after the
/// popup went away.
static LAST_ID: Shared = Shared(UnsafeCell::new([0; ID_CAP]));

fn with_id<R>(f: impl FnOnce(&mut [u8; ID_CAP]) -> R) -> R {
    unsafe { shim::rf_state_lock() };
    let out = f(unsafe { &mut *ID.0.get() });
    unsafe { shim::rf_state_unlock() };
    out
}

fn with_last<R>(f: impl FnOnce(&mut [u8; ID_CAP]) -> R) -> R {
    unsafe { shim::rf_state_lock() };
    let out = f(unsafe { &mut *LAST_ID.0.get() });
    unsafe { shim::rf_state_unlock() };
    out
}

fn state() -> u8 {
    STATE.load(Ordering::Acquire)
}

fn set_state(next: u8) {
    STATE.store(next, Ordering::Release);
}

fn store_id(id: &[u8]) {
    with_id(|slot| {
        slot.fill(0);
        let n = id.len().min(ID_CAP - 1);
        slot[..n].copy_from_slice(&id[..n]);
    });
}

/// Record the id as consumed for dedup purposes (survives dismiss).
fn remember_id(id: &[u8; ID_CAP]) {
    with_last(|slot| {
        slot.fill(0);
        slot.copy_from_slice(id);
    });
}

/// True when this id was already shown this boot (server re-offer).
fn is_duplicate(id: &[u8; ID_CAP]) -> bool {
    with_last(|prev| prev == id)
}

fn copy_id_into(dst: *mut u8) {
    with_id(|slot| unsafe {
        core::ptr::copy_nonoverlapping(slot.as_ptr(), dst, ID_CAP);
    });
}

/// Length of the NUL-terminated string at `p`, capped.
fn c_str_len(p: *const u8, cap: usize) -> usize {
    let mut n = 0;
    while n < cap && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

// ── screen ─────────────────────────────────────────────────────────────────

fn show_bitmap(bitmap: *const u8) {
    let fb = unsafe { shim::rf_fb_begin() };
    let fb_len = unsafe { shim::rf_fb_len() } as usize;
    if fb.is_null() || fb_len != page_sync::PAGE_BITMAP_SIZE {
        unsafe { shim::rf_fb_end() };
        log_e!(TAG, "framebuffer size mismatch: fb_len={} want={}", fb_len,
               page_sync::PAGE_BITMAP_SIZE);
        return;
    }
    // 2bpp MSB-first, same as the server's packer: a plain copy.
    unsafe { core::ptr::copy_nonoverlapping(bitmap, fb, page_sync::PAGE_BITMAP_SIZE) };
    unsafe { shim::rf_fb_end() };
    unsafe { shim::rf_request_full_refresh() };
    page_sync::stop_display(); // the notification owns the panel while it is up
}

// ── state transitions ──────────────────────────────────────────────────────

fn clear_state() {
    set_state(IDLE);
    with_id(|slot| slot.fill(0));
    unsafe { shim::rf_timer_stop() };
}

fn dismiss_locked_state() {
    clear_state();
    // Hand the screen back and repaint once: the old resume+prev+next did two
    // full panel refreshes, and with no pages configured it did not repaint at
    // all, leaving the notification bitmap on screen forever.
    page_sync::resume_display();
    page_sync::redraw_current();
}

// ── tasks ──────────────────────────────────────────────────────────────────

/// Extract the base64 bitmap and the notification id from a `/next` response.
///
/// Pure, so it is unit-tested on the host; the base64 decode happens in
/// [`decode_next`] because it needs a PSRAM destination.
pub fn parse_next(body: &[u8]) -> Option<(&[u8], [u8; ID_CAP])> {
    let b64 = json::member(body, 0, "bitmap_base64")
        .and_then(|at| json::str_value(body, at))?;
    let id = json::path(body, &["notification", "id"])
        .and_then(|at| json::str_value(body, at))?;
    if id.is_empty() || id.len() >= ID_CAP {
        log_w!(TAG, "notification id length {} out of range", id.len());
        return None;
    }
    let mut out = [0u8; ID_CAP];
    out[..id.len()].copy_from_slice(id);
    Some((b64, out))
}

/// Length of the id prefix in a `.bin` /next response: 32 ASCII hex chars.
pub const BINARY_ID_LEN: usize = 32;

/// Full binary body: id (32 ASCII hex) + bitmap (30000 B 2bpp BWRY).
pub fn binary_body_len() -> usize {
    BINARY_ID_LEN + page_sync::PAGE_BITMAP_SIZE
}

/// Extract the id and raw bitmap from a `.bin` /next response.
///
/// Pure, so it is unit-tested on the host. The server writes
/// `id(32 hex) || bitmap`; a body of any other length is malformed.
pub fn parse_binary_next(body: &[u8]) -> Option<(&[u8], [u8; ID_CAP])> {
    if body.len() != binary_body_len() {
        log_w!(TAG, "binary next body {} bytes, want {}", body.len(), binary_body_len());
        return None;
    }
    let id_bytes = &body[..BINARY_ID_LEN];
    if core::str::from_utf8(id_bytes).is_err()
        || !id_bytes.iter().all(|b| b.is_ascii_hexdigit())
    {
        log_w!(TAG, "binary next id is not 32 ascii hex chars");
        return None;
    }
    let mut out = [0u8; ID_CAP];
    out[..BINARY_ID_LEN].copy_from_slice(id_bytes);
    Some((&body[BINARY_ID_LEN..], out))
}

/// Run the Rust consume decision for one (maybe) parsed notification and
/// perform the show side effects only when Rust says show. Returns the
/// consume action so `fetch_once` can book the pull outcome (a retry means
/// the pull failed and the failure streak must advance).
fn consume_parsed(class: u8, id: Option<&[u8; ID_CAP]>, bitmap: *mut u8) -> u8 {
    let duplicate = id.is_some_and(|i| is_duplicate(i));
    let action = notify_policy::decide_consume(class, id.is_some(), duplicate);
    let label = id
        .map(|i| {
            let n = i.iter().position(|b| *b == 0).unwrap_or(i.len());
            core::str::from_utf8(&i[..n]).unwrap_or("?")
        })
        .unwrap_or("?");
    match action {
        CONSUME_SHOW => {
            // decide_consume only returns Show for a parsed id.
            let id = id.expect("show implies a parsed id");
            store_id(id);
            remember_id(id);
            show_bitmap(bitmap);
            set_state(NOTIFYING);
            unsafe { shim::rf_timer_start() };
            log_i!(TAG, "notify {} displaying", label);
            action
        }
        CONSUME_SKIP_DUPLICATE => {
            log_i!(TAG, "duplicate notification {} skipped", label);
            action
        }
        _ => {
            log_w!(TAG, "notification body unusable (class={})", class);
            action
        }
    }
}

/// Apply a `.bin` /next response body (status already checked by caller);
/// returns the consume action (see [`consume_parsed`]).
fn handle_binary_next(body: &[u8], bitmap: *mut u8) -> u8 {
    match parse_binary_next(body) {
        Some((raw, id)) => {
            let dst = unsafe {
                core::slice::from_raw_parts_mut(bitmap, page_sync::PAGE_BITMAP_SIZE)
            };
            dst.copy_from_slice(raw);
            consume_parsed(RESPONSE_BINARY_NOTIFY, Some(&id), bitmap)
        }
        None => consume_parsed(RESPONSE_BINARY_NOTIFY, None, bitmap),
    }
}

/// JSON `/next` fallback for servers predating `.bin` (404 on the binary
/// path). Runs its own GET; the caller's `buf` still holds the failed binary
/// response, so this allocates a fresh one. The fallback shares handle_next
fn fetch_json_fallback(device_id: &[u8], bitmap: *mut u8) -> u8 {
    use core::fmt::Write as _;
    let mut path = CBuf::<96>::new();
    let _ = write!(
        path,
        "/api/notifications/next?device_id={}",
        core::str::from_utf8(device_id).unwrap_or("")
    );
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!(TAG, "fallback: cannot build endpoint");
        return CONSUME_RETRY_BACKOFF;
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };
    let buf = unsafe { shim::rf_alloc(RESPONSE_BUF + 1) };
    if buf.is_null() {
        log_e!(TAG, "fallback: alloc failed");
        return CONSUME_RETRY_BACKOFF;
    }
    let mut len = (RESPONSE_BUF + 1) as i32;
    let status = unsafe {
        shim::rf_http_get(url.as_ptr(), token.as_ptr(), buf as *mut core::ffi::c_char, &mut len,
                          HTTP_TIMEOUT_MS)
    };
    let len = (len.max(0) as usize).min(RESPONSE_BUF + 1);
    let body = unsafe { core::slice::from_raw_parts(buf, len) };
    let action = handle_next(status, body, bitmap);
    unsafe { shim::rf_free(buf) };
    action
}

/// Decode `b64` straight into the caller's bitmap buffer.
fn decode_next(b64: &[u8], out_bitmap: *mut u8) -> bool {
    let dst = unsafe { core::slice::from_raw_parts_mut(out_bitmap, page_sync::PAGE_BITMAP_SIZE) };
    match B64.decode_slice(b64, dst) {
        Ok(n) if n == page_sync::PAGE_BITMAP_SIZE => true,
        Ok(n) => {
            log_w!(TAG, "bitmap decoded to {} bytes, want {}", n, page_sync::PAGE_BITMAP_SIZE);
            false
        }
        Err(e) => {
            log_w!(TAG, "bitmap base64 decode failed: {:?}", e);
            false
        }
    }
}

/// Apply a `/next` response on the JSON path: classify through the Rust
/// policy, then consume. Returns the consume action.
fn handle_next(status: i32, body: &[u8], bitmap: *mut u8) -> u8 {
    let class = notify_policy::classify_response(&ResponseFacts {
        status,
        binary_path: false,
        _pad: [0; 3],
    });
    match class {
        RESPONSE_JSON_NOTIFY => {
            let parsed = parse_next(body).map(|(b64, id)| (decode_next(b64, bitmap), id));
            match parsed {
                Some((true, id)) => consume_parsed(class, Some(&id), bitmap),
                _ => consume_parsed(class, None, bitmap),
            }
        }
        RESPONSE_EMPTY => {
            log_i!(TAG, "no pending notification (204)");
            CONSUME_NONE_EMPTY
        }
        other => {
            log_w!(TAG, "next fetch failed (status={})", status);
            notify_policy::decide_consume(other, false, false)
        }
    }
}

/// `/api/notifications/{id}/ack`.
fn ack_path(id: &[u8]) -> CBuf<96> {
    let mut path = CBuf::<96>::new();
    path.push("/api/notifications/");
    path.push_bytes(id);
    path.push("/ack");
    path
}

/// `{"decision":"…"}`.
fn ack_body(decision: &[u8]) -> CBuf<64> {
    let mut body = CBuf::<64>::new();
    body.push("{\"decision\":\"");
    body.push_bytes(decision);
    body.push("\"}");
    body
}

/// Clear all module state so a host test starts from a cold boot.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    set_state(IDLE);
    with_id(|slot| slot.fill(0));
    with_last(|slot| slot.fill(0));
}

extern "C" fn fetch_task(_arg: *mut c_void) {
    fetch_once();
    unsafe { shim::rf_task_exit() };
}

/// Persist one pull outcome for the Rust pull gate. The stamps and streak
/// live in RTC slow memory (shim.cpp owns the storage — RAM clears on every
/// duty-cycle wake); the streak step itself comes from
/// `notify_policy::record_result`.
fn record_pull_outcome(ok: bool) {
    let mut last_pull = 0i64;
    let mut last_failure = 0i64;
    let mut streak = 0u32;
    unsafe {
        shim::rf_notify_gate_stats(&mut last_pull, &mut last_failure, &mut streak);
        let next = notify_policy::record_result(ok, streak);
        shim::rf_notify_gate_record(shim::rf_time_now_s(), (!ok) as u8, next);
    }
}

/// Pull `/next` and act on it. Runs in its own task on the device so the button
/// callback returns immediately; the body is called directly by the host tests.
fn fetch_once() {
    log_i!(TAG, "fetch task started");

    let mut device_id = CBuf::<32>::new();
    let ok = unsafe { shim::rf_get_device_id(device_id.as_mut_ptr(), 32) } != 0;
    device_id.set_len_from_terminator();
    if !ok || device_id.is_empty() {
        log_w!(TAG, "no device_id, abort fetch");
        set_state(IDLE);
        return;
    }

    use core::fmt::Write as _;
    let mut path = CBuf::<96>::new();
    let _ = write!(
        path,
        "/api/notifications/next.bin?device_id={}",
        core::str::from_utf8(device_id.as_bytes()).unwrap_or("")
    );
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!(TAG, "cannot build endpoint");
        set_state(IDLE);
        return;
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };

    // +1: the wrapper's capacity includes the terminator it appends.
    let buf = unsafe { shim::rf_alloc(RESPONSE_BUF + 1) };
    // +1: http_wrapper_get NUL-terminates one byte past the reported length.
    let bitmap = unsafe { shim::rf_alloc(page_sync::PAGE_BITMAP_SIZE + 1) };
    if buf.is_null() || bitmap.is_null() {
        log_e!(TAG, "alloc failed");
        unsafe {
            if !buf.is_null() {
                shim::rf_free(buf);
            }
            if !bitmap.is_null() {
                shim::rf_free(bitmap);
            }
        }
        set_state(IDLE);
        return;
    }

    let mut len = (RESPONSE_BUF + 1) as i32;
    let status = unsafe {
        shim::rf_http_get(url.as_ptr(), token.as_ptr(), buf as *mut core::ffi::c_char, &mut len,
                          HTTP_TIMEOUT_MS)
    };

    let len = (len.max(0) as usize).min(RESPONSE_BUF + 1);
    let body = unsafe { core::slice::from_raw_parts(buf, len) };
    // Classification and the consume decision come from the Rust policy
    // (notify_policy.rs): 200 binary / 204 empty / 404 -> JSON fallback /
    // anything else -> failure, exactly like the old `match status`.
    let class = notify_policy::classify_response(&ResponseFacts {
        status,
        binary_path: true,
        _pad: [0; 3],
    });
    let action = match class {
        RESPONSE_BINARY_NOTIFY => handle_binary_next(body, bitmap),
        RESPONSE_EMPTY => {
            log_i!(TAG, "no pending notification (204, binary)");
            CONSUME_NONE_EMPTY
        }
        // Older server without /next.bin: fall back to the JSON endpoint so a
        // firmware newer than its server keeps working (rolling deploy).
        RESPONSE_JSON_FALLBACK => fetch_json_fallback(device_id.as_bytes(), bitmap),
        other => {
            log_w!(TAG, "binary next fetch failed (status={})", status);
            notify_policy::decide_consume(other, false, false)
        }
    };
    if action != CONSUME_SHOW && state() == FETCHING {
        // Neither path moved the state machine to NOTIFYING; reset the guard.
        set_state(IDLE);
    }
    // Book the outcome so the pull gate's failure backoff can climb (or
    // reset) on the next evaluation.
    record_pull_outcome(action != CONSUME_RETRY_BACKOFF);

    unsafe {
        shim::rf_free(buf);
        shim::rf_free(bitmap);
    }
}

/// Fire-and-forget `POST /api/notifications/{id}/ack`.
///
/// `arg` is a PSRAM block: id at [`ACK_ID_OFF`] bytes in, body after it. The id
/// is copied in so a concurrent dismiss clearing the module state cannot race
/// with the task reading it.
extern "C" fn ack_task(arg: *mut c_void) {
    send_ack(arg as *mut u8);
    unsafe { shim::rf_task_exit() };
}

/// POST the ack for the block built by [`post_ack`]; frees it.
fn send_ack(block: *mut u8) {
    let id_len = c_str_len(block, ACK_ID_OFF);

    let path = ack_path(unsafe { core::slice::from_raw_parts(block, id_len) });
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!(TAG, "cannot build ack endpoint");
        unsafe { shim::rf_free(block) };
        return;
    }

    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };
    let mut resp = CBuf::<256>::new();
    let mut resp_len = 256i32;
    let body = unsafe { block.add(ACK_ID_OFF) } as *const core::ffi::c_char;
    let status = unsafe {
        shim::rf_http_post_json(url.as_ptr(), token.as_ptr(), body, resp.as_mut_ptr(),
                                &mut resp_len, HTTP_TIMEOUT_MS)
    };
    if status == 200 {
        log_i!(TAG, "ack ok");
    } else {
        log_w!(TAG, "ack failed: status={} (stays shown until the server TTL)", status);
    }

    unsafe { shim::rf_free(block) };
}

/// The timer callback runs in the FreeRTOS timer daemon, and dismissing touches
/// the display; hop to a task.
extern "C" fn timeout_cb() {
    if state() != NOTIFYING {
        return;
    }
    let created = unsafe {
        shim::rf_task_create(
            timeout_task,
            c"notify_todis".as_ptr(),
            TIMEOUT_STACK,
            TASK_PRIORITY,
            core::ptr::null_mut(),
        )
    };
    if created != 0 {
        log_e!(TAG, "timeout dismiss task create failed");
    }
}

extern "C" fn timeout_task(_arg: *mut c_void) {
    on_timeout();
    unsafe { shim::rf_task_exit() };
}

/// Dismissing touches the panel, so it runs in a task on the device.
fn on_timeout() {
    if state() == NOTIFYING {
        dismiss_locked_state();
        log_i!(TAG, "notify timeout, auto dismissed");
    }
}

// ── public API ─────────────────────────────────────────────────────────────

pub fn init() {
    if unsafe { shim::rf_timer_create_once(c"notify_to".as_ptr(), TIMEOUT_MS, timeout_cb) } != 0 {
        log_e!(TAG, "timeout timer create failed");
    }
    set_state(IDLE);
    with_id(|slot| slot.fill(0));
    log_i!(TAG, "notify module initialized");
}

pub fn deinit() {
    unsafe {
        shim::rf_timer_stop();
        shim::rf_timer_delete();
    }
    set_state(IDLE);
    with_id(|slot| slot.fill(0));
}

pub fn is_active() -> bool {
    state() == NOTIFYING
}

/// True while a `/next` pull is outstanding and the radio must stay up.
///
/// `is_active` (NOTIFYING) is false during the GET itself, so it cannot be
/// the "do not cut the radio" signal on its own: the server marks the
/// notification `shown` the moment it hands it out
/// (`notify_store.py::next_for`), so a response stranded by a mid-flight
/// radio cut is lost forever. FETCHING is set synchronously in
/// `request_next` before the fetch task spawns — the caller (RunPowerCycle)
/// cannot miss its own cycle's fetch — and every terminal path of
/// `fetch_once` leaves FETCHING (success → NOTIFYING, anything else → IDLE),
/// so this cannot stick. Prefer skipping one radio cut over losing one
/// notification.
pub fn is_fetching() -> bool {
    state() == FETCHING
}

/// Non-blocking: the HTTP GET runs in a task so the button callback returns.
pub fn request_next() {
    if state() != IDLE {
        return;
    }
    set_state(FETCHING);
    let created = unsafe {
        shim::rf_task_create(
            fetch_task,
            c"notify_fetch".as_ptr(),
            FETCH_STACK,
            TASK_PRIORITY,
            core::ptr::null_mut(),
        )
    };
    if created != 0 {
        log_e!(TAG, "fetch task create failed");
        set_state(IDLE);
    }
}

/// # Safety
/// `decision` must point at a NUL-terminated `"agree"` or `"reject"`.
pub unsafe fn post_ack(decision: *const core::ffi::c_char) {
    if state() != NOTIFYING {
        return;
    }
    let block = unsafe { shim::rf_alloc(ACK_BLOCK) };
    if block.is_null() {
        log_e!(TAG, "ack block alloc failed, dismiss without ack");
        dismiss();
        return;
    }
    // Copy the id and build the body now: `dismiss` below clears the module
    // state, and `decision` belongs to the caller's frame.
    copy_id_into(block);
    let decision = if decision.is_null() {
        b"".as_slice()
    } else {
        let n = c_str_len(decision as *const u8, 8);
        unsafe { core::slice::from_raw_parts(decision as *const u8, n) }
    };
    let body = ack_body(decision);
    unsafe {
        core::ptr::copy_nonoverlapping(
            body.as_bytes().as_ptr(),
            block.add(ACK_ID_OFF),
            body.as_bytes().len(),
        );
        *block.add(ACK_ID_OFF + body.as_bytes().len()) = 0;
    }

    let created = unsafe {
        shim::rf_task_create(
            ack_task,
            c"notify_ack".as_ptr(),
            FETCH_STACK,
            TASK_PRIORITY,
            block as *mut c_void,
        )
    };
    if created != 0 {
        log_e!(TAG, "ack task create failed");
        unsafe { shim::rf_free(block) };
    }
    // Whatever happens to the ack, the popup goes away (the server TTL expires
    // the notification if the ack never lands).
    dismiss();
}

pub fn dismiss() {
    if state() != NOTIFYING {
        return;
    }
    dismiss_locked_state();
    log_i!(TAG, "notify dismissed");
}

/// Clear the popup without touching screen ownership: the caller (leaving the
/// current screen) takes the panel itself right after.
pub fn dismiss_quiet() {
    if state() != NOTIFYING {
        return;
    }
    clear_state();
    log_i!(TAG, "notify dismissed (screen left to caller)");
}

// ── C ABI ──────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn notify_init() {
    init();
}

#[unsafe(no_mangle)]
pub extern "C" fn notify_deinit() {
    deinit();
}

#[unsafe(no_mangle)]
pub extern "C" fn notify_request_next() {
    request_next();
}

#[unsafe(no_mangle)]
pub extern "C" fn notify_is_active() -> bool {
    is_active()
}

/// True while a `/next` pull is outstanding: the radio must stay up.
#[unsafe(no_mangle)]
pub extern "C" fn notify_is_fetching() -> bool {
    is_fetching()
}

/// The raw module state (`RF_NOTIFY_STATE_*`), read by the C++ pull gate.
#[unsafe(no_mangle)]
pub extern "C" fn notify_state() -> u8 {
    state()
}

/// `decision` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn notify_post_ack(decision: *const core::ffi::c_char) {
    unsafe { post_ack(decision) };
}

#[unsafe(no_mangle)]
pub extern "C" fn notify_dismiss() {
    dismiss();
}

#[unsafe(no_mangle)]
pub extern "C" fn notify_dismiss_quiet() {
    dismiss_quiet();
}
#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    /// base64 of `data`. The firmware builds without base64's `alloc` feature,
    /// so only the caller-provided-buffer variants exist here.
    fn encode(data: &[u8]) -> String {
        use base64::Engine as _;
        let mut out = vec![0u8; data.len().div_ceil(3) * 4];
        let n = STANDARD.encode_slice(data, &mut out).expect("fits");
        out.truncate(n);
        String::from_utf8(out).expect("ascii")
    }

    /// A `.bin` /next response: id(32 hex, NUL-padded) || bitmap tagged with
    /// `fill`. Mirrors the server's binary layout the production path parses.
    fn next_body(fill: u8, id: &str) -> Vec<u8> {
        // The server writes uuid4().hex: exactly 32 lowercase hex chars, no
        // padding. Short test ids are right-padded with '0' to keep every
        // byte a valid hex digit (the parser rejects anything else).
        let mut id_field = [b'0'; BINARY_ID_LEN];
        id_field[..id.len()].copy_from_slice(id.as_bytes());
        let mut body = id_field.to_vec();
        body.resize(BINARY_ID_LEN + page_sync::PAGE_BITMAP_SIZE, fill);
        body
    }

    /// Cold start with the canvas owning the screen, then one scripted pull.
    fn boot_with(fill: u8, id: &str) {
        page_sync::reset_for_test();
        reset_for_test();
        shim::host::set_fb();
        let body = next_body(fill, id);
        shim::host::script_ok("/api/notifications/next.bin", &body);
        fetch_once();
    }

    #[test]
    fn a_pull_displays_the_bitmap_and_arms_the_timeout() {
        let _g = shim::host::lock();
        boot_with(0x5a, "abcd1234");

        assert!(is_active(), "a 200 with a decodable bitmap moves to NOTIFYING");
        assert_eq!(shim::host::fb()[0], 0x5a, "the decoded bitmap is on the panel");
        assert_eq!(shim::host::refreshes(), 1);
        assert_eq!(shim::host::timers_started(), 1, "the dismiss timeout is armed");
        assert!(!page_sync::is_displaying(), "the notification owns the panel");
        assert_eq!(with_id(|slot| slot[..8].to_vec()), b"abcd1234".to_vec());
        assert_eq!(shim::host::calls_matching("/api/notifications/next").len(), 1);
    }

    #[test]
    fn request_next_is_non_blocking() {
        let _g = shim::host::lock();
        reset_for_test();

        request_next();

        assert_eq!(state(), FETCHING, "the button callback returns while the fetch runs");
        assert_eq!(shim::host::tasks(), vec!["notify_fetch".to_string()]);
    }

    #[test]
    fn fetching_is_visible_until_the_pull_settles() {
        // The pre-paint radio guard (application.cc) keys on this: FETCHING
        // must cover the whole in-flight window and nothing else — the poll
        // between request and settle is exactly when the radio must stay up.
        let _g = shim::host::lock();
        reset_for_test();
        assert!(!is_fetching(), "cold: nothing outstanding");

        request_next();
        assert!(is_fetching(), "request arms FETCHING synchronously");
        assert!(!is_active(), "NOTIFYING must not answer for an unfinished pull");

        page_sync::reset_for_test();
        shim::host::set_fb();
        shim::host::script_ok("/api/notifications/next.bin", &next_body(0x5a, "5e771e1"));
        fetch_once();
        assert!(!is_fetching(), "a settled pull clears the guard");
        assert!(is_active(), "a good pull lands on the panel");
    }

    #[test]
    fn a_failed_pull_clears_the_guard_so_the_cycle_cannot_stick() {
        // A stuck FETCHING would suppress every future radio cut (or, worse,
        // stretch every wake by the full settle budget). Every abort path of
        // fetch_once must return to IDLE.
        let _g = shim::host::lock();
        page_sync::reset_for_test();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_get("/api/notifications/next.bin", 204, b"");

        request_next();
        assert!(is_fetching());
        fetch_once();
        assert!(!is_fetching(), "an empty queue still settles the guard");
        assert_eq!(state(), IDLE);
    }

    #[test]
    fn an_empty_queue_leaves_the_device_idle() {
        let _g = shim::host::lock();
        page_sync::reset_for_test();
        reset_for_test();
        shim::host::set_fb();
        shim::host::script_get("/api/notifications/next.bin", 204, b"");

        fetch_once();

        assert_eq!(state(), IDLE);
        assert_eq!(shim::host::refreshes(), 0, "nothing was drawn");
    }

    #[test]
    fn a_short_or_corrupt_response_is_rejected() {
        let full = vec![0u8; page_sync::PAGE_BITMAP_SIZE];
        let cases: Vec<Vec<u8>> = vec![
            // valid base64, but not a whole panel
            br#"{"bitmap_base64":"AAAA","notification":{"id":"x1"}}"#.to_vec(),
            // truncated JSON
            next_body(0x11, "x2")[..40].to_vec(),
            // parsed, but nothing to ack with
            format!(r#"{{"bitmap_base64":"{}","notification":{{}}}}"#, encode(&full)).into_bytes(),
        ];
        for body in cases {
            let _g = shim::host::lock();
            page_sync::reset_for_test();
            reset_for_test();
            shim::host::set_fb();
            shim::host::script_get("/api/notifications/next.bin", 200, &body);

            fetch_once();

            assert_eq!(state(), IDLE, "never leaves a half-displayed notification");
            assert_eq!(shim::host::refreshes(), 0, "the panel is not touched");
        }
    }

    #[test]
    fn ack_posts_the_decision_for_the_stored_id() {
        let _g = shim::host::lock();
        boot_with(0x5a, "deadbeeffeedface");
        assert!(is_active());

        // Build the block the way `post_ack` does, then run its task body.
        let block = unsafe { shim::rf_alloc(ACK_BLOCK) };
        copy_id_into(block);
        let body = ack_body(b"agree");
        unsafe {
            core::ptr::copy_nonoverlapping(body.as_bytes().as_ptr(), block.add(ACK_ID_OFF), body.as_bytes().len());
            *block.add(ACK_ID_OFF + body.as_bytes().len()) = 0;
        }
        shim::host::script_ok("/api/notifications/deadbeeffeedface0000000000000000/ack", b"{\"status\":\"acked\"}");

        send_ack(block);

        let posts = shim::host::calls_matching("http_post");
        assert_eq!(posts.len(), 1, "exactly one ack is sent");
        assert!(
            posts[0].starts_with("http_post http://host/api/notifications/deadbeeffeedface0000000000000000/ack"),
            "{}",
            posts[0]
        );
        assert!(posts[0].ends_with(r#"{"decision":"agree"}"#), "{}", posts[0]);
    }

    #[test]
    fn post_ack_dismisses_whatever_the_ack_does() {
        let _g = shim::host::lock();
        boot_with(0x5a, "d15add1d");

        unsafe { post_ack(c"reject".as_ptr()) };

        assert_eq!(shim::host::tasks(), vec!["notify_ack".to_string()]);
        assert!(!is_active(), "the popup goes away even before the ack lands");
        assert!(page_sync::is_displaying(), "and the canvas takes the screen back");
    }

    #[test]
    fn post_ack_is_ignored_when_nothing_is_showing() {
        let _g = shim::host::lock();
        reset_for_test();

        unsafe { post_ack(c"agree".as_ptr()) };

        assert!(shim::host::tasks().is_empty(), "no ack task without a notification");
    }

    #[test]
    fn the_timeout_dismisses_and_repaints_the_canvas() {
        let _g = shim::host::lock();
        boot_with(0x5a, "71de0a7");
        assert!(is_active());

        on_timeout();

        assert_eq!(state(), IDLE, "the popup is cleared when the timer fires");
        assert_eq!(with_id(|slot| slot.to_vec()), vec![0u8; ID_CAP], "and the id with it");
        assert_eq!(shim::host::calls_matching("timer_stop").len(), 1);
        assert!(page_sync::is_displaying(), "the canvas owns the panel again");
        assert_eq!(shim::host::hint_draws(), 1, "and repaints instead of leaving the popup up");
    }

    #[test]
    fn a_quiet_dismiss_leaves_screen_ownership_to_the_caller() {
        let _g = shim::host::lock();
        boot_with(0x5a, "0a17d1d");
        let refreshes_before = shim::host::refreshes();

        dismiss_quiet();

        assert_eq!(state(), IDLE);
        assert!(
            !page_sync::is_displaying(),
            "a caller about to draw over the panel must not have the canvas repaint under it"
        );
        assert_eq!(
            shim::host::refreshes(),
            refreshes_before,
            "and no extra panel refresh is spent"
        );
    }

    #[test]
    fn extracts_bitmap_and_id() {
        let body = br#"{"bitmap_base64":"AAECAw==","notification":{"id":"deadbeef","title":"x"}}"#;
        let (b64, id) = parse_next(body).expect("parse");
        assert_eq!(b64, b"AAECAw==");
        assert_eq!(&id[..8], b"deadbeef");
        assert_eq!(id[8], 0, "id is NUL-terminated in the fixed slot");
    }

    #[test]
    fn rejects_responses_without_the_required_fields() {
        assert!(parse_next(br#"{"notification":{"id":"abc"}}"#).is_none());
        assert!(parse_next(br#"{"bitmap_base64":"AA=="}"#).is_none());
        assert!(parse_next(br#"{"bitmap_base64":"AA==","notification":{}}"#).is_none());
        assert!(parse_next(br#"{"bitmap_base64":1,"notification":{"id":"abc"}}"#).is_none());
        assert!(parse_next(b"").is_none());
    }

    #[test]
    fn rejects_empty_or_overlong_ids() {
        assert!(parse_next(br#"{"bitmap_base64":"AA==","notification":{"id":""}}"#).is_none());
        let long = "d".repeat(ID_CAP);
        let body = format!(
            r#"{{"bitmap_base64":"AA==","notification":{{"id":"{long}"}}}}"#
        );
        assert!(parse_next(body.as_bytes()).is_none());
        // One byte shorter fits.
        let ok = "d".repeat(ID_CAP - 1);
        let body = format!(r#"{{"bitmap_base64":"AA==","notification":{{"id":"{ok}"}}}}"#);
        assert!(parse_next(body.as_bytes()).is_some());
    }
    #[test]
    fn falls_back_to_json_when_server_lacks_binary_endpoint() {
        let _g = shim::host::lock();
        page_sync::reset_for_test();
        reset_for_test();
        shim::host::set_fb();
        let bitmap = vec![0x6Bu8; page_sync::PAGE_BITMAP_SIZE];
        let json = format!(
            r#"{{"bitmap_base64":"{}","notification":{{"id":"0123456789abcdef0123456789abcdef","title":"t"}}}}"#,
            encode(&bitmap)
        );
        shim::host::script_get("/api/notifications/next.bin", 404, b"not found");
        shim::host::script_ok("/api/notifications/next?device_id=", json.as_bytes());

        fetch_once();

        assert!(is_active(), "old server's JSON response still displays the notice");
        assert_eq!(shim::host::fb()[0], 0x6B);
        let calls = shim::host::calls_matching("http_get");
        assert_eq!(calls.len(), 2, "binary 404 is followed by exactly one JSON pull");
        assert!(calls[0].contains("/api/notifications/next.bin?"));
        assert!(calls[1].contains("/api/notifications/next?"));
    }

    #[test]
    fn parses_binary_next_body() {
        let id = "0123456789abcdef0123456789abcdef";
        let mut body = id.as_bytes().to_vec();
        body.resize(BINARY_ID_LEN + page_sync::PAGE_BITMAP_SIZE, 0xA5);
        let (raw, parsed) = parse_binary_next(&body).expect("parses");
        assert_eq!(&parsed[..32], id.as_bytes());
        assert_eq!(parsed[32], 0, "id slot is NUL-terminated in the fixed cap");
        assert_eq!(raw.len(), page_sync::PAGE_BITMAP_SIZE);
        assert_eq!(raw[0], 0xA5);
    }

    #[test]
    fn rejects_malformed_binary_next_bodies() {
        let short = vec![b'a'; binary_body_len() - 1];
        assert!(parse_binary_next(&short).is_none());
        let long = vec![b'a'; binary_body_len() + 1];
        assert!(parse_binary_next(&long).is_none());
        let mut bad = vec![b'z'; BINARY_ID_LEN];
        bad.resize(binary_body_len(), 0);
        assert!(parse_binary_next(&bad).is_none());
        assert!(parse_binary_next(&[]).is_none());
    }

    #[test]
    fn binary_layout_matches_server_contract() {
        assert_eq!(BINARY_ID_LEN, 32);
        assert_eq!(binary_body_len(), 32 + page_sync::PAGE_BITMAP_SIZE);
    }

    #[test]
    fn a_re_offered_notification_is_skipped_after_dismiss() {
        // The server re-offers the same id (ack race / handout already
        // marked shown elsewhere). The consume policy says skip: the pull
        // still runs, but the panel is not touched and no popup returns.
        let _g = shim::host::lock();
        boot_with(0x5a, "abcd1234abcd1234abcd1234abcd1234");
        assert!(is_active(), "the first handout shows");
        dismiss();
        let refreshes_after_dismiss = shim::host::refreshes();

        fetch_once();

        assert!(!is_active(), "a duplicate id must not come back on screen");
        assert_eq!(
            shim::host::refreshes(),
            refreshes_after_dismiss,
            "and the panel is not touched"
        );
        assert_eq!(
            shim::host::calls_matching("/api/notifications/next").len(),
            2,
            "the pull itself still ran"
        );
    }

    #[test]
    fn pull_outcomes_feed_the_failure_streak() {
        // A status error fails the pull (stamped + streaked); the next
        // healthy round-trip resets the streak but keeps the failure stamp.
        let _g = shim::host::lock();
        page_sync::reset_for_test();
        reset_for_test();
        shim::host::set_fb();

        shim::host::set_time_s(1_000);
        shim::host::script_get("/api/notifications/next.bin", 500, b"boom");
        fetch_once();
        assert_eq!(
            shim::host::notify_gate_stats(),
            (1_000, 1_000, 1),
            "a status error is a failure: attempt and failure stamped, streak 1"
        );

        shim::host::set_time_s(2_000);
        shim::host::script_get("/api/notifications/next.bin", 204, b"");
        fetch_once();
        assert_eq!(
            shim::host::notify_gate_stats(),
            (2_000, 1_000, 0),
            "a healthy 204 resets the streak and re-stamps only the attempt"
        );
    }

}
