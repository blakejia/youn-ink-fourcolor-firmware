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

fn with_id<R>(f: impl FnOnce(&mut [u8; ID_CAP]) -> R) -> R {
    unsafe { shim::rf_state_lock() };
    let out = f(unsafe { &mut *ID.0.get() });
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

extern "C" fn fetch_task(_arg: *mut c_void) {
    log_i!(TAG, "fetch task started");

    let mut device_id = CBuf::<32>::new();
    if unsafe { shim::rf_get_device_id(device_id.as_mut_ptr(), 32) } == 0
        || device_id.is_empty()
    {
        log_w!(TAG, "no device_id, abort fetch");
        set_state(IDLE);
        unsafe { shim::rf_task_exit() };
    }

    use core::fmt::Write as _;
    let mut path = CBuf::<96>::new();
    let _ = write!(
        path,
        "/api/notifications/next?device_id={}",
        core::str::from_utf8(device_id.as_bytes()).unwrap_or("")
    );
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!(TAG, "cannot build endpoint");
        set_state(IDLE);
        unsafe { shim::rf_task_exit() };
    }
    let mut token = CBuf::<80>::new();
    unsafe { shim::rf_get_token(token.as_mut_ptr(), 80) };

    let buf = unsafe { shim::rf_alloc(RESPONSE_BUF) };
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
        unsafe { shim::rf_task_exit() };
    }

    let mut len = RESPONSE_BUF as i32;
    let status = unsafe {
        shim::rf_http_get(url.as_ptr(), token.as_ptr(), buf as *mut core::ffi::c_char, &mut len,
                          HTTP_TIMEOUT_MS)
    };

    match status {
        204 => {
            log_i!(TAG, "no pending notification (204)");
            set_state(IDLE);
        }
        200 => {
            let len = (len.max(0) as usize).min(RESPONSE_BUF);
            match parse_next(unsafe { core::slice::from_raw_parts(buf, len) }) {
                Some((b64, id)) if decode_next(b64, bitmap) => {
                    store_id(&id);
                    show_bitmap(bitmap);
                    set_state(NOTIFYING);
                    unsafe { shim::rf_timer_start() };
                    let id_len = id.iter().position(|b| *b == 0).unwrap_or(id.len());
                    log_i!(TAG, "notify {:?} displaying",
                           core::str::from_utf8(&id[..id_len]).unwrap_or("?"));
                }
                _ => set_state(IDLE),
            }
        }
        other => {
            log_w!(TAG, "next fetch failed (status={})", other);
            set_state(IDLE);
        }
    }

    unsafe {
        shim::rf_free(buf);
        shim::rf_free(bitmap);
    }
    unsafe { shim::rf_task_exit() };
}

/// Fire-and-forget `POST /api/notifications/{id}/ack`.
///
/// `arg` is a PSRAM block: id at [`ACK_ID_OFF`] bytes in, body after it. The id
/// is copied in so a concurrent dismiss clearing the module state cannot race
/// with the task reading it.
extern "C" fn ack_task(arg: *mut c_void) {
    let block = arg as *mut u8;
    let id_len = c_str_len(block, ACK_ID_OFF);

    let mut path = CBuf::<96>::new();
    path.push("/api/notifications/");
    path.push_bytes(unsafe { core::slice::from_raw_parts(block, id_len) });
    path.push("/ack");
    let mut url = CBuf::<320>::new();
    if unsafe { shim::rf_build_endpoint(path.as_ptr(), url.as_mut_ptr(), 320) } == 0 {
        log_w!(TAG, "cannot build ack endpoint");
        unsafe {
            shim::rf_free(block);
            shim::rf_task_exit()
        };
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

    unsafe {
        shim::rf_free(block);
        shim::rf_task_exit()
    };
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
    if state() == NOTIFYING {
        dismiss_locked_state();
        log_i!(TAG, "notify timeout, auto dismissed");
    }
    unsafe { shim::rf_task_exit() };
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
    let mut body = CBuf::<64>::new();
    body.push("{\"decision\":\"");
    body.push_bytes(decision);
    body.push("\"}");
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

/// # Safety
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
}
