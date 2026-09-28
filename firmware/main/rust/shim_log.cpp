// Device log ring buffer + ESP_LOGx capture hook.
//
// Threading: the hook can be invoked from any task and, per ESP-IDF's own
// contract for esp_log_set_vprintf, must be re-entrant. A portMUX spinlock
// guards the ring; the critical section is a bounded byte copy and takes no
// other lock, so it cannot deadlock against the display mutex.
//
// The hook must never call a log function: it IS the log path, so that would
// recurse without bound.
#include "shim_log.h"

#include <cstdarg>
#include <cstdio>
#include <cstring>

#include "esp_attr.h"
#include "esp_log_write.h"
#include "freertos/FreeRTOS.h"
#include "freertos/portmacro.h"

namespace {

constexpr uint32_t kMagic = 0x4c4f4731u;  // "LOG1"
constexpr int kDataBytes = 2048;
constexpr int kMaxLine = 256;             // one line's cap incl. NUL

// One byte is reserved (kCapBytes = kDataBytes - 1) so that head == tail
// unambiguously means EMPTY. Occupancy is then DERIVED from the two cursors
// rather than stored: a stored counter is updated non-atomically with the bytes
// it describes, and this buffer lives in `.rtc_noinit` — the one region a reset
// mid-write is guaranteed to preserve — so a stored counter can go stale and
// stay stale into the next boot. Cursor-derived occupancy cannot.
constexpr int kCapBytes = kDataBytes - 1;
// An absurd (or torn) `len` field can make the drop step a multiple of
// kDataBytes, which would stall the make-room loop forever; every walk is
// bounded by this, the most frames that can physically fit.
constexpr int kMaxFrames = kDataBytes / 6 + 1;

struct Ring {
    uint32_t magic;
    uint32_t seq;      // next line number to assign
    uint32_t head;     // write offset into data[]
    uint32_t tail;     // acked offset into data[]
    uint32_t dropped;  // lines lost to overwrite
    uint8_t  data[kDataBytes];
};

RTC_NOINIT_ATTR static Ring g_ring;

// Bytes currently held: derived, so it cannot disagree with the cursors.
inline uint32_t ring_used_locked() {
    return (g_ring.head + kDataBytes - g_ring.tail) % kDataBytes;
}

portMUX_TYPE g_mux = portMUX_INITIALIZER_UNLOCKED;
vprintf_like_t g_prev_vprintf = nullptr;
bool g_hook_installed = false;

inline void ensure_init_locked() {
    if (g_ring.magic != kMagic) {
        g_ring.magic = kMagic;
        g_ring.seq = 0;
        g_ring.head = 0;
        g_ring.tail = 0;
        g_ring.dropped = 0;
    }
}

// Copy `n` bytes into the ring at `at`, wrapping past the end. Every copy in
// this file goes through here: a multi-byte memcpy at an index near kDataBytes
// would otherwise run past data[] into the neighbouring `.rtc.force_slow`
// variable, which shares the RTC region with no padding between them.
void ring_copy_in(uint32_t at, const uint8_t* src, int n) {
    const int first = (at + (uint32_t)n <= kDataBytes) ? n : (kDataBytes - (int)at);
    std::memcpy(&g_ring.data[at], src, first);
    if (first < n) std::memcpy(&g_ring.data[0], src + first, n - first);
}

// Read `n` bytes out of the ring at `at`, wrapping. Mirror of ring_copy_in.
void ring_copy_out(uint8_t* dst, uint32_t at, int n) {
    const int first = (at + (uint32_t)n <= kDataBytes) ? n : (kDataBytes - (int)at);
    std::memcpy(dst, &g_ring.data[at], first);
    if (first < n) std::memcpy(dst + first, &g_ring.data[0], n - first);
}

// Append one framed line. Caller holds the mux.
//
// The frame is written at `head` and `head` is advanced LAST: a reset inside
// this function therefore leaves no half-frame inside [tail, head), so the next
// boot simply reads the previous backlog.
void push_locked(const char* text, int len) {
    if (len <= 0) return;
    if (len > kMaxLine - 1) len = kMaxLine - 1;
    const int frame = 4 + 2 + len;
    if (frame > kCapBytes) return;  // cannot be stored at all

    // Make room from the tail. Bounded twice: by the cursor-derived occupancy
    // (so it stops when genuinely empty) and by an explicit frame cap (so a
    // garbage `len` whose step is a multiple of kDataBytes cannot stall it).
    for (int guard = 0; guard <= kMaxFrames; guard++) {
        if (ring_used_locked() + (uint32_t)frame <= (uint32_t)kCapBytes) break;
        if (g_ring.tail == g_ring.head) break;  // empty and still too big
        uint16_t old_len = 0;
        ring_copy_out((uint8_t*)&old_len, (g_ring.tail + 4) % kDataBytes, 2);
        uint32_t step = 4 + 2 + old_len;
        // `>= kDataBytes`, not `>`: a step of exactly kDataBytes moves the
        // cursor nowhere (mod kDataBytes), so the loop would burn every guard
        // iteration without freeing a byte, inflate `dropped`, and — because
        // head then advances by frame onto tail — leave the ring reading as
        // empty and lose the whole backlog. Skip only the header in that case.
        if (step < 6 || step >= (uint32_t)kDataBytes) step = 6;
        g_ring.tail = (g_ring.tail + step) % kDataBytes;
        g_ring.dropped++;
    }
    // Belt and braces: the loop above is bounded, so it can exit without having
    // made room (garbage metadata). Never publish a frame that would overflow
    // the capacity — dropping this one line is the honest failure.
    if (ring_used_locked() + (uint32_t)frame > (uint32_t)kCapBytes) return;

    const uint32_t seq = g_ring.seq++;
    uint16_t len16 = (uint16_t)len;
    ring_copy_in(g_ring.head, (const uint8_t*)&seq, 4);
    ring_copy_in((g_ring.head + 4) % kDataBytes, (const uint8_t*)&len16, 2);
    ring_copy_in((g_ring.head + 6) % kDataBytes, (const uint8_t*)text, len);
    g_ring.head = (g_ring.head + (uint32_t)frame) % kDataBytes;  // publish last
}

int capture_hook(const char* fmt, va_list args) {
    char line[kMaxLine];
    va_list copy;
    va_copy(copy, args);
    const int n = vsnprintf(line, sizeof(line), fmt, copy);
    va_end(copy);

    // FreeRTOS printf from an ISR is not safe; skip the buffer there and keep
    // the console working. (xPortInIsrContext is a no-op cost off-ISR.)
    if (!xPortInIsrContext()) {
        portENTER_CRITICAL(&g_mux);
        ensure_init_locked();
        push_locked(line, n < (int)sizeof(line) ? n : (int)sizeof(line) - 1);
        portEXIT_CRITICAL(&g_mux);
    }

    if (g_prev_vprintf != nullptr) {
        return g_prev_vprintf(fmt, args);
    }
    return n;
}

}  // namespace

extern "C" void rf_logbuf_install_hook(void) {
    // The check AND the exchange are under the lock: a concurrent second caller
    // could otherwise observe the hook installed and capture capture_hook as
    // `g_prev_vprintf`, making the hook forward to itself (unbounded recursion).
    // esp_log_set_vprintf is a single __atomic_exchange_n, so holding this leaf
    // lock across it is safe.
    portENTER_CRITICAL(&g_mux);
    if (!g_hook_installed) {
        g_prev_vprintf = esp_log_set_vprintf(&capture_hook);
        g_hook_installed = true;
        ensure_init_locked();
    }
    portEXIT_CRITICAL(&g_mux);
}

// `out` holds exactly `cap` bytes INCLUDING the NUL terminator — the same
// convention as CBuf::push_bytes (`len + n >= N` reserves the last byte), so a
// caller can pass a CBuf's size and C satisfies the whole contract within it.
// Admitting a line only when `written + len + 1 < cap` keeps out[written]
// in bounds.
extern "C" void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines) {
    if (out_seq_lo) *out_seq_lo = 0;
    if (out_lines) *out_lines = 0;
    if (out == nullptr || cap <= 1) return;

    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int written = 0;
    int at = (int)g_ring.tail;
    // Walk the occupancy span, bounded by both the span and a frame cap: the
    // span alone would stall on a torn/garbage length field.
    uint32_t remaining = ring_used_locked();
    uint32_t lines = 0;
    uint32_t first_seq = 0;
    for (int guard = 0; guard <= kMaxFrames && remaining >= 6; guard++) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        at = (at + 2) % kDataBytes;
        if (written + (int)len + 1 >= cap) break;
        if (6u + len > remaining) break;  // never read past the span
        ring_copy_out((uint8_t*)out + written, (uint32_t)at, len);
        written += len;
        out[written++] = '\n';
        at = (at + len) % kDataBytes;
        remaining -= 6u + len;
        if (lines == 0) first_seq = seq;
        lines++;
    }
    out[written] = 0;
    portEXIT_CRITICAL(&g_mux);

    // Meaningful only when *out_lines > 0: seq 0 is a real frame number, so a
    // caller that acks with lines == 0 would compute seq_lo + 0 - 1 and ack
    // everything.
    if (out_seq_lo) *out_seq_lo = first_seq;
    if (out_lines) *out_lines = lines;
}

// Contract: pass seq_lo + lines - 1 from the read that produced the payload.
// Acks only frames the reader actually returned (contiguous from the tail).
extern "C" void rf_logbuf_ack(uint32_t seq_hi, uint32_t reported_dropped) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int at = (int)g_ring.tail;
    // Same bounded walk as read.
    uint32_t remaining = ring_used_locked();
    for (int guard = 0; guard <= kMaxFrames && remaining >= 6; guard++) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        if (seq > seq_hi) break;
        if (6u + len > remaining) break;
        // `at` points at the len field; the next frame starts 2 + len later.
        at = (at + 2 + len) % kDataBytes;
        g_ring.tail = at;
        remaining -= 6u + len;
    }
    // `dropped` is reported per upload. Subtract the count this batch
    // reported (sampled before the POST), NOT clear: lines pushed during the
    // up-to-10s upload window are drops too, and clearing would discard them
    // unreported. Saturating so a corrupt `reported` can't underflow.
    g_ring.dropped -= g_ring.dropped < reported_dropped ? g_ring.dropped
                                                        : reported_dropped;
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_stats(uint32_t* dropped, uint32_t* used) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    if (dropped) *dropped = g_ring.dropped;
    if (used) *used = ring_used_locked();
    portEXIT_CRITICAL(&g_mux);
}
