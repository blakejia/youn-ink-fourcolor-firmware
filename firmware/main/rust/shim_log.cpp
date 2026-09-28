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

struct Ring {
    uint32_t magic;
    uint32_t seq;      // next line number to assign
    uint32_t head;     // write offset into data[]
    uint32_t tail;     // acked offset into data[]
    uint32_t used;     // bytes held; head==tail is NOT empty-vs-full safe alone
    uint32_t dropped;  // lines lost to overwrite
    uint8_t  data[kDataBytes];
};

RTC_NOINIT_ATTR static Ring g_ring;

portMUX_TYPE g_mux = portMUX_INITIALIZER_UNLOCKED;
vprintf_like_t g_prev_vprintf = nullptr;
bool g_hook_installed = false;

inline void ensure_init_locked() {
    if (g_ring.magic != kMagic) {
        g_ring.magic = kMagic;
        g_ring.seq = 0;
        g_ring.head = 0;
        g_ring.tail = 0;
        g_ring.used = 0;
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
void push_locked(const char* text, int len) {
    if (len <= 0) return;
    if (len > kMaxLine - 1) len = kMaxLine - 1;
    const int frame = 4 + 2 + len;
    if (frame > kDataBytes) return;

    // Make room from the tail until it fits. Occupancy is tracked explicitly:
    // with head/tail alone a ring holding exactly kDataBytes bytes has
    // head == tail and is indistinguishable from empty, which would discard
    // the whole backlog and never count it as dropped.
    while (g_ring.used + (uint32_t)frame > kDataBytes) {
        if (g_ring.used == 0) break;  // nothing left to drop
        uint16_t old_len = 0;
        ring_copy_out((uint8_t*)&old_len, (g_ring.tail + 4) % kDataBytes, 2);
        const uint32_t step = 4 + 2 + old_len;
        g_ring.tail = (g_ring.tail + step) % kDataBytes;
        g_ring.used -= step;
        g_ring.dropped++;
    }

    const uint32_t seq = g_ring.seq++;
    uint16_t len16 = (uint16_t)len;
    ring_copy_in(g_ring.head, (const uint8_t*)&seq, 4);
    g_ring.head = (g_ring.head + 4) % kDataBytes;
    ring_copy_in(g_ring.head, (const uint8_t*)&len16, 2);
    g_ring.head = (g_ring.head + 2) % kDataBytes;
    ring_copy_in(g_ring.head, (const uint8_t*)text, len);
    g_ring.head = (g_ring.head + (uint32_t)len) % kDataBytes;
    g_ring.used += (uint32_t)frame;
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

// The check and the exchange run under the same mux, so a concurrent second
// caller cannot both install and end up with g_prev_vprintf == capture_hook
// (which would forward to itself and recurse without bound). Safe to hold the
// leaf lock across esp_log_set_vprintf: it is a single __atomic_exchange_n
// (log/src/os/log_write.c:19-26), no logging, no other lock.
extern "C" void rf_logbuf_install_hook(void) {
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
    // Bound the walk by the byte budget, NOT by `at != head`: a ring holding
    // exactly kDataBytes bytes has head == tail, and a cursor comparison would
    // read that full ring as empty.
    int remaining = (int)g_ring.used;
    uint32_t lines = 0;
    uint32_t first_seq = 0;
    while (remaining >= 6) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        at = (at + 2) % kDataBytes;
        if (written + (int)len + 1 >= cap) break;
        ring_copy_out((uint8_t*)out + written, (uint32_t)at, len);
        written += len;
        out[written++] = '\n';
        at = (at + len) % kDataBytes;
        remaining -= 4 + 2 + (int)len;
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
extern "C" void rf_logbuf_ack(uint32_t seq_hi) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int at = (int)g_ring.tail;
    // Same budget-driven walk as read: `at != head` cannot distinguish a full
    // ring from an empty one.
    int remaining = (int)g_ring.used;
    while (remaining >= 6) {
        uint32_t seq = 0;
        uint16_t len = 0;
        ring_copy_out((uint8_t*)&seq, (uint32_t)at, 4);
        at = (at + 4) % kDataBytes;
        ring_copy_out((uint8_t*)&len, (uint32_t)at, 2);
        if (seq > seq_hi) break;
        // `at` points at the len field; the next frame starts 2 + len later,
        // and the whole frame (which `used` counts) is 4 + 2 + len.
        const uint32_t step = 4 + 2 + len;
        at = (at + 2 + len) % kDataBytes;
        g_ring.used -= step;
        g_ring.tail = at;
        remaining -= (int)step;
    }
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_stats(uint32_t* dropped, uint32_t* used) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    if (dropped) *dropped = g_ring.dropped;
    if (used) *used = g_ring.used;
    portEXIT_CRITICAL(&g_mux);
}
