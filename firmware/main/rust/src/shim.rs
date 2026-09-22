//! Raw FFI to the shims in `../shim.cpp`.
//!
//! Every unsafe call the crate makes to ESP-IDF goes through here, so the rest
//! of the crate is ordinary safe Rust. The shims keep their C signatures plain
//! (`uint8_t *`, `int`, `size_t`) rather than mirroring IDF typedefs.

use core::ffi::{c_char, c_int, c_void};

pub const LOG_ERROR: c_int = 1;
pub const LOG_WARN: c_int = 2;
pub const LOG_INFO: c_int = 3;
pub const LOG_DEBUG: c_int = 4;

unsafe extern "C" {
    // ── runtime ──
    pub fn rf_abort() -> !;
    pub fn rf_log(level: c_int, tag: *const c_char, msg: *const c_char);
    /// PSRAM allocation; null on failure.
    pub fn rf_alloc(bytes: usize) -> *mut u8;
    pub fn rf_free(p: *mut u8);
    /// One FreeRTOS mutex guarding the Rust-side module state. It has priority
    /// inheritance, which a spinlock would not.
    ///
    /// Lock order is `state -> display`; the display mutex is never held while
    /// taking this one (`page_sync_is_displaying` is a lock-free atomic read
    /// precisely so renderers can call it under the display mutex).
    pub fn rf_state_lock();
    pub fn rf_state_unlock();
    /// `stack_bytes` follows ESP-IDF's byte-counted stacks. Returns 0 on success.
    pub fn rf_task_create(
        entry: extern "C" fn(*mut c_void),
        name: *const c_char,
        stack_bytes: u32,
        priority: u8,
        arg: *mut c_void,
    ) -> c_int;
    pub fn rf_task_exit() -> !;
    /// One-shot FreeRTOS timer; the callback runs in the timer daemon task.
    pub fn rf_timer_create_once(name: *const c_char, period_ms: u32, cb: extern "C" fn()) -> c_int;
    pub fn rf_timer_start();
    pub fn rf_timer_stop();
    pub fn rf_timer_delete();

    // ── http (http_client_wrapper.h) ──
    pub fn rf_http_get(
        url: *const c_char,
        token: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn rf_http_post_json(
        url: *const c_char,
        token: *const c_char,
        body: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        timeout_ms: c_int,
    ) -> c_int;

    // ── pairing helpers (server_pairing.h) ──
    pub fn rf_build_endpoint(path: *const c_char, out: *mut c_char, out_len: c_int) -> c_int;
    pub fn rf_get_token(out: *mut c_char, out_len: c_int) -> c_int;
    pub fn rf_get_device_id(out: *mut c_char, out_len: c_int) -> c_int;

    // ── display (CustomLcdDisplay) ──
    /// Injects the board display (mirrors the old `page_sync_set_display`).
    /// A null pointer falls back to `Board::GetInstance().GetDisplay()`.
    pub fn rf_set_display(display: *mut c_void);
    /// Takes the display mutex and returns the shared framebuffer, or null.
    /// Must be paired with [`rf_fb_end`].
    pub fn rf_fb_begin() -> *mut u8;
    /// Expected framebuffer size in bytes for this panel.
    pub fn rf_fb_len() -> c_int;
    pub fn rf_fb_end();
    pub fn rf_request_full_refresh();
    /// Paints the "no pages configured" screen from the C++ side (it needs
    /// `std::vector<TextItem>`, which is a display concern, not logic).
    pub fn rf_draw_empty_hint();
    // ── sleep power (shim_power.h) ──
    /// Copies the RTC panel record into `out` (a 48-byte `rf_panel_record_t`).
    pub fn rf_panel_record_get(out: *mut u8);
    pub fn rf_panel_mark_pending(md5: *const c_char, index: c_int);
    pub fn rf_panel_record_invalidate();
    /// Consecutive sync failures, persisted in RTC memory on device.
    pub fn rf_fail_streak_get() -> u32;
    pub fn rf_fail_streak_set(streak: u32);
    /// 0 = other, 1 = timer, 2 = ext0(BOOT), 3 = ext1(charger insert).
    pub fn rf_wakeup_cause() -> c_int;
    /// Audio + amp rail. Nonzero = on.
    pub fn rf_rails_audio(on: c_int);
    /// Power-accounting snapshot (shim.cpp owns the counters, Rust only reads).
    /// Null pointers are ignored, so callers may read a subset.
    /// `refresh_ms` is SUBMIT time booked at the single C++ exit
    /// (rf_request_full_refresh), never the panel waveform — see F3 note.
    pub fn rf_power_counters(wakes: *mut u32, awake_ms: *mut u32, radio_ms: *mut u32,
                             http_gets: *mut u32, refresh_ms: *mut u32);
    /// `esp_reset_reason()` at boot (shim.cpp caches it once; Rust only reads).
    pub fn rf_last_reset_reason() -> u32;
    pub fn rf_battery_sample(mv: *mut u16, pct: *mut u8, charge: *mut u8) -> i32;
}

/// `abort()`, used by the panic handler.
///
/// # Safety
/// Never returns.
pub unsafe fn abort() -> ! {
    unsafe { rf_abort() }
}

/// A fixed-capacity, NUL-terminated byte buffer for values handed to C.
pub struct CBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> CBuf<N> {
    pub const fn new() -> Self {
        CBuf { bytes: [0; N], len: 0, overflowed: false }
    }

    pub fn push(&mut self, text: &str) {
        self.push_bytes(text.as_bytes());
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        // Reserve one byte for the terminator.
        if self.len + bytes.len() >= N {
            self.overflowed = true;
            return;
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when something did not fit, i.e. the caller must not use the result.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Pointer to the NUL-terminated contents.
    pub fn as_ptr(&self) -> *const c_char {
        self.bytes.as_ptr() as *const c_char
    }

    /// Mutable pointer for C functions that write into the buffer.
    pub fn as_mut_ptr(&mut self) -> *mut c_char {
        self.bytes.as_mut_ptr() as *mut c_char
    }

    /// Recompute the length after a C function wrote a NUL-terminated string
    /// through [`Self::as_mut_ptr`].
    ///
    /// That write goes around the Rust side, so `len` still reports 0 and
    /// anything reading the buffer through Rust sees it as empty.
    pub fn set_len_from_terminator(&mut self) {
        self.len = self.bytes.iter().position(|b| *b == 0).unwrap_or(N);
        self.overflowed = false;
    }
}

impl<const N: usize> core::fmt::Write for CBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.overflowed {
            return Err(core::fmt::Error);
        }
        self.push(s);
        if self.overflowed { Err(core::fmt::Error) } else { Ok(()) }
    }
}

/// Host test harness: scriptable stand-ins for the `shim.cpp` symbols.
///
/// The device resolves these against `shim.cpp`; on the host they let the
/// ported state machines run for real — scripted HTTP responses, a working
/// allocator, a real mutex and a framebuffer the tests can inspect. Only the
/// genuinely hardware-bound parts (panel, Wi-Fi, RTOS scheduling) stay inert,
/// and those can only be verified on the device.
#[cfg(all(test, not(target_arch = "xtensa")))]
pub(crate) mod host {
    use core::ffi::{c_char, c_int, c_void};
    use std::sync::{Mutex, MutexGuard};

    /// Matches the panel, so `show_page`'s length check passes.
    pub const FB_LEN: usize = 30_000;

    struct Response {
        suffix: String,
        status: i32,
        body: Vec<u8>,
    }

    #[derive(Default)]
    struct Counters {
        refreshes: u32,
        hint_draws: u32,
        timers_started: u32,
    }

    static LOCK: Mutex<()> = Mutex::new(());
    static RESPONSES: Mutex<Vec<Response>> = Mutex::new(Vec::new());
    static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static TASKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static COUNTERS: Mutex<Counters> = Mutex::new(Counters {
        refreshes: 0,
        hint_draws: 0,
        timers_started: 0,
    });
    static FB: Mutex<Option<&'static mut [u8]>> = Mutex::new(None);
    /// Live allocations, so the HTTP stubs can prove they stay in bounds.
    static ALLOCS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

    fn counters<R>(f: impl FnOnce(&mut Counters) -> R) -> R {
        let mut g = COUNTERS.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    fn calls<R>(f: impl FnOnce(&mut Vec<String>) -> R) -> R {
        let mut g = CALLS.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    fn note(s: impl Into<String>) {
        calls(|c| c.push(s.into()));
    }

    /// Take the harness lock, resetting all recorded state.
    ///
    /// Tests share one process, so anything touching the module globals must
    /// hold this for its whole body.
    pub fn lock() -> MutexGuard<'static, ()> {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        RESPONSES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        CALLS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        LOGS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        TASKS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let mut c = COUNTERS.lock().unwrap_or_else(|e| e.into_inner());
        *c = Counters::default();
        drop(c);
        FB.lock().unwrap_or_else(|e| e.into_inner()).take();
        ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *FAIL_STREAK.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        *POWER.lock().unwrap_or_else(|e| e.into_inner()) = [0; 5];
        AUDIO_ON.store(true, std::sync::atomic::Ordering::SeqCst);
        guard
    }

    /// The staged RTC panel record: (magic, valid, md5 bytes, displayed index).
    /// A zero-length md5 stages as an empty string — exactly what the device
    /// stores when the empty hint is recorded (32 zero bytes start with NUL) —
    /// so readers must key the hint on the index (-1) only, never on the md5.
    static PANEL_REC: Mutex<Option<(u32, u8, Vec<u8>, i32)>> = Mutex::new(None);
    // rf_panel_record_get writes 48 bytes; see rf_panel_record_t.
    /// Consecutive schedule-sync failures (mirrors the device RTC word).
    static FAIL_STREAK: Mutex<u32> = Mutex::new(0);
    static AUDIO_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    /// Same value as RF_PANEL_MAGIC in shim_power.h; the stub must mirror the
    /// device record exactly (magic@0, valid@4, md5@8, index@44).
    const RF_PANEL_MAGIC: u32 = 0x50414E31;

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_record_get(out: *mut u8) {
        // Mirrors shim.cpp: a null buffer is a no-op, and None means all-zero
        // (magic 0 reads as invalid, exactly like power-on RTC memory).
        if out.is_null() {
            return;
        }
        let g = PANEL_REC.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            // sizeof(rf_panel_record_t) == 48; the caller's buffer is the struct,
            // and zeroing only the md5 slot would leave index/pad bytes stale.
            core::ptr::write_bytes(out, 0, 48);
            if let Some((magic, valid, md5, index)) = g.as_ref() {
                core::ptr::write_unaligned(out as *mut u32, *magic);
                *out.add(4) = *valid;
                core::ptr::copy_nonoverlapping(md5.as_ptr(), out.add(8), md5.len().min(32));
                core::ptr::write_unaligned(out.add(44) as *mut i32, *index);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_mark_pending(md5: *const c_char, index: c_int) {
        let md5 = cstr(md5).into_bytes();
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((RF_PANEL_MAGIC, 1, md5, index));
    }

    /// Stage an arbitrary panel record (tests only): writes the record directly
    /// instead of going through `rf_panel_mark_pending`, so a test can stage
    /// states the firmware never produces (wrong magic, cleared valid bit).
    pub fn stage_panel_record(magic: u32, valid: u8, md5: &[u8], index: i32) {
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((magic, valid, md5.to_vec(), index));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_panel_record_invalidate() {
        *PANEL_REC.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fail_streak_get() -> u32 {
        *FAIL_STREAK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fail_streak_set(streak: u32) {
        *FAIL_STREAK.lock().unwrap_or_else(|e| e.into_inner()) = streak;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_wakeup_cause() -> c_int {
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_rails_audio(on: c_int) {
        AUDIO_ON.store(on != 0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Scripted power-accounting snapshot (mirrors the shim.cpp counters).
    /// Staged by `set_counters`, read out by the `rf_power_counters` stub —
    /// the same "Rust only reads, C owns" shape as the device side.
    static POWER: Mutex<[u32; 5]> = Mutex::new([0; 5]);

    pub fn set_counters(wakes: u32, awake_ms: u32, radio_ms: u32, http_gets: u32, refresh_ms: u32) {
        *POWER.lock().unwrap_or_else(|e| e.into_inner()) =
            [wakes, awake_ms, radio_ms, http_gets, refresh_ms];
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_power_counters(
        wakes: *mut u32,
        awake_ms: *mut u32,
        radio_ms: *mut u32,
        http_gets: *mut u32,
        refresh_ms: *mut u32,
    ) {
        let c = *POWER.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            if !wakes.is_null() {
                *wakes = c[0];
            }
            if !awake_ms.is_null() {
                *awake_ms = c[1];
            }
            if !radio_ms.is_null() {
                *radio_ms = c[2];
            }
            if !http_gets.is_null() {
                *http_gets = c[3];
            }
            if !refresh_ms.is_null() {
                *refresh_ms = c[4];
            }
        }
    }

    /// The `refresh_ms` slot advances only through `rf_request_full_refresh`
    /// (mirroring the C++ single exit): tests stage the other four slots with
    /// `set_counters` and let real paints move `refresh_ms`.
    pub fn refresh_submit_ms() -> u32 {
        POWER.lock().unwrap_or_else(|e| e.into_inner())[4]
    }

    /// Scripted `esp_reset_reason()` value for the `?rr=` query param.
    static RESET_REASON: Mutex<u32> = Mutex::new(0);

    pub fn set_reset_reason(v: u32) {
        *RESET_REASON.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_last_reset_reason() -> u32 {
        *RESET_REASON.lock().unwrap_or_else(|e| e.into_inner())
    }

    // NOTE: host stubs below collide with the C++ rf_battery_sample at firmware link time; Task 2 adds #[cfg(test)]
    /// Scripted battery sample for the `?v=&p=&c=` query params. `None` = no
    /// valid reading this cycle (sensor absent, ADC failure, or mains-powered
    /// skip) — the URL omits the params entirely rather than sending zeros.
    static BATTERY: Mutex<Option<(u16, u8, u8)>> = Mutex::new(None);

    pub fn set_battery_sample(mv: u16, pct: u8, charge: u8) {
        *BATTERY.lock().unwrap_or_else(|e| e.into_inner()) = Some((mv, pct, charge));
    }
    pub fn set_battery_sample_none() {
        *BATTERY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_battery_sample(mv: *mut u16, pct: *mut u8, charge: *mut u8) -> i32 {
        let b = *BATTERY.lock().unwrap_or_else(|e| e.into_inner());
        match b {
            Some((m, p, c)) => {
                unsafe {
                    if !mv.is_null() { *mv = m; }
                    if !pct.is_null() { *pct = p; }
                    if !charge.is_null() { *charge = c; }
                }
                1
            }
            None => 0,
        }
    }


    /// Any request whose URL contains `suffix` gets `status` + `body`.
    ///
    /// Re-scripting the same suffix replaces the earlier response, so a test can
    /// model "the backend recovered on the next poll".
    pub fn script_get(suffix: &str, status: i32, body: &[u8]) {
        let mut g = RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|r| r.suffix != suffix);
        g.push(Response { suffix: suffix.to_string(), status, body: body.to_vec() });
    }

    /// Shortcut for the common case: a 200 whose body is `body`.
    pub fn script_ok(suffix: &str, body: &[u8]) {
        script_get(suffix, 200, body);
    }

    pub fn calls_matching(needle: &str) -> Vec<String> {
        calls(|c| c.iter().filter(|s| s.contains(needle)).cloned().collect())
    }

    pub fn refreshes() -> u32 {
        counters(|c| c.refreshes)
    }

    pub fn hint_draws() -> u32 {
        counters(|c| c.hint_draws)
    }

    pub fn timers_started() -> u32 {
        counters(|c| c.timers_started)
    }

    pub fn tasks() -> Vec<String> {
        TASKS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[allow(dead_code)]
    pub fn logs() -> Vec<String> {
        LOGS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Install the framebuffer the panel stub hands out.
    pub fn set_fb() {
        let buf = vec![0u8; FB_LEN].leak();
        *FB.lock().unwrap_or_else(|e| e.into_inner()) = Some(buf);
    }

    pub fn fb() -> Vec<u8> {
        let g = FB.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map(|b| b.to_vec()).unwrap_or_default()
    }

    pub fn fb_ptr() -> *mut u8 {
        FB.lock().unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .map(|b| b.as_mut_ptr())
            .unwrap_or(core::ptr::null_mut())
    }

    fn cstr(p: *const c_char) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut out = Vec::new();
        let mut i = 0isize;
        loop {
            let b = unsafe { *p.offset(i) } as u8;
            if b == 0 {
                break;
            }
            out.push(b);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn write_cstr(src: &str, out: *mut c_char, cap: usize) -> bool {
        let bytes = src.as_bytes();
        if bytes.len() >= cap {
            return false;
        }
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len()) };
        unsafe { *out.add(bytes.len()) = 0 };
        true
    }

    fn allocation_size(ptr: *mut u8) -> Option<usize> {
        let p = ptr as usize;
        ALLOCS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(base, _)| *base == p)
            .map(|(_, size)| *size)
    }

    /// Copy a scripted body into `buf` under the wrapper's contract: `cap` is
    /// the capacity *including* the terminator, and nothing may be written
    /// outside the allocation. This is the invariant that used to be off by one
    /// byte (`out_buf[ctx.len]` with `len == capacity`).
    fn write_response(body: &[u8], buf: *mut c_char, cap: usize) -> usize {
        // Only heap buffers can be checked; stack buffers (the ack response
        // CBuf) are the caller's declared capacity, which the reserve below
        // already respects.
        if let Some(size) = allocation_size(buf as *mut u8) {
            assert!(cap <= size, "HTTP capacity {cap} exceeds the {size}-byte allocation");
        }
        // Reserve the terminator byte, exactly like http_client_wrapper.cc.
        let n = body.len().min(cap.saturating_sub(1));
        if let Some(size) = allocation_size(buf as *mut u8) {
            assert!(
                n + 1 <= size,
                "terminator would be written at offset {n} of a {size}-byte allocation"
            );
        }
        unsafe {
            core::ptr::copy_nonoverlapping(body.as_ptr(), buf as *mut u8, n);
            *buf.add(n) = 0;
        }
        n
    }

    /// 16-byte header so `rf_free` can rebuild the layout.
    fn payload_layout(bytes: usize) -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(bytes + 16, 16).unwrap()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_abort() -> ! {
        panic!("rf_abort")
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_log(_level: c_int, tag: *const c_char, msg: *const c_char) {
        LOGS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}: {}", cstr(tag), cstr(msg)));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_alloc(bytes: usize) -> *mut u8 {
        if bytes == 0 {
            return core::ptr::null_mut();
        }
        unsafe {
            let p = std::alloc::alloc_zeroed(payload_layout(bytes));
            if p.is_null() {
                return p;
            }
            *(p as *mut usize) = bytes;
            let payload = p.add(16);
            ALLOCS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((payload as usize, bytes));
            payload
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_free(p: *mut u8) {
        if p.is_null() {
            return;
        }
        ALLOCS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(base, _)| *base != p as usize);
        unsafe {
            let base = p.sub(16);
            let bytes = *(base as *const usize);
            std::alloc::dealloc(base, payload_layout(bytes));
        }
    }

    /// A take-twice-detecting flag rather than a `std::sync::Mutex`: tests are
    /// single-threaded inside `lock()`, and a second take without a release is
    /// exactly the deadlock the device would hit, so it should panic here
    /// instead of hanging.
    static STATE_HELD: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_state_lock() {
        assert!(
            !STATE_HELD.swap(true, std::sync::atomic::Ordering::Acquire),
            "state lock taken while already held (reentrant take deadlocks on device)"
        );
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_state_unlock() {
        assert!(
            STATE_HELD.swap(false, std::sync::atomic::Ordering::Release),
            "state unlock without a matching lock"
        );
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_task_create(
        _entry: extern "C" fn(*mut c_void),
        name: *const c_char,
        _stack_bytes: u32,
        _priority: u8,
        _arg: *mut c_void,
    ) -> c_int {
        TASKS.lock().unwrap_or_else(|e| e.into_inner()).push(cstr(name));
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_task_exit() -> ! {
        // Task entries are never called by tests; they call the extracted bodies
        // instead. (Panicking here would abort: `extern "C"` cannot unwind.)
        panic!("rf_task_exit reached on the host")
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_create_once(
        _name: *const c_char,
        _period_ms: u32,
        _cb: extern "C" fn(),
    ) -> c_int {
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_start() {
        counters(|c| c.timers_started += 1);
        note("timer_start");
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_stop() {
        note("timer_stop");
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_timer_delete() {}

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_http_get(
        url: *const c_char,
        _token: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        _timeout_ms: c_int,
    ) -> c_int {
        let url = cstr(url);
        // Mirrors shim.cpp: the single GET exit counts every attempt.
        POWER.lock().unwrap_or_else(|e| e.into_inner())[3] += 1;
        note(format!("http_get {url}"));
        let cap = unsafe { *len }.max(0) as usize;
        let hit = {
            let g = RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
            g.iter()
                .find(|r| url.contains(&r.suffix))
                .map(|r| (r.status, r.body.clone()))
        };
        match hit {
            Some((status, body)) => {
                let n = write_response(&body, buf, cap);
                unsafe { *len = n as c_int };
                status
            }
            None => {
                unsafe { *len = 0 };
                -1
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_http_post_json(
        url: *const c_char,
        _token: *const c_char,
        body: *const c_char,
        buf: *mut c_char,
        len: *mut c_int,
        _timeout_ms: c_int,
    ) -> c_int {
        let url = cstr(url);
        let payload = cstr(body);
        note(format!("http_post {url} {payload}"));
        let cap = unsafe { *len }.max(0) as usize;
        let hit = {
            let g = RESPONSES.lock().unwrap_or_else(|e| e.into_inner());
            g.iter().find(|r| url.contains(&r.suffix)).map(|r| (r.status, r.body.clone()))
        };
        match hit {
            Some((status, body)) => {
                let n = write_response(&body, buf, cap);
                unsafe { *len = n as c_int };
                status
            }
            None => {
                unsafe { *len = 0 };
                -1
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_build_endpoint(
        path: *const c_char,
        out: *mut c_char,
        out_len: c_int,
    ) -> c_int {
        let full = format!("http://host{}", cstr(path));
        write_cstr(&full, out, out_len.max(0) as usize) as c_int
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_get_token(out: *mut c_char, out_len: c_int) -> c_int {
        write_cstr("test-token", out, out_len.max(0) as usize) as c_int
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_get_device_id(out: *mut c_char, out_len: c_int) -> c_int {
        write_cstr("NOTE4C-TEST", out, out_len.max(0) as usize) as c_int
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_set_display(_display: *mut c_void) {}

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_len() -> c_int {
        FB_LEN as c_int
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_begin() -> *mut u8 {
        fb_ptr()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_fb_end() {}

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_request_full_refresh() {
        counters(|c| c.refreshes += 1);
        // Single exit like the device: canvas, empty-hint and notify funnels
        // all land here. SUBMIT accounting only (a fixed 1 ms stand-in for
        // the device's queueing span) — never the panel waveform.
        POWER.lock().unwrap_or_else(|e| e.into_inner())[4] += 1;
        note("full_refresh");
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn rf_draw_empty_hint() {
        counters(|c| c.hint_draws += 1);
        note("empty_hint");
    }
}

