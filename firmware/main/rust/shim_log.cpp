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
        g_ring.dropped = 0;
    }
}

// Append one framed line. Caller holds the mux.
void push_locked(const char* text, int len) {
    if (len <= 0) return;
    if (len > kMaxLine - 1) len = kMaxLine - 1;
    const int frame = 4 + 2 + len;
    if (frame > kDataBytes) return;

    // Make room: drop whole old lines from the tail until it fits.
    while (((g_ring.head + kDataBytes - g_ring.tail) % kDataBytes) + frame > kDataBytes) {
        if (g_ring.tail == g_ring.head) break;  // empty but still too big: give up
        uint16_t old_len = 0;
        std::memcpy(&old_len, &g_ring.data[(g_ring.tail + 4) % kDataBytes], 2);
        g_ring.tail = (g_ring.tail + 4 + 2 + old_len) % kDataBytes;
        g_ring.dropped++;
    }

    const uint32_t seq = g_ring.seq++;
    uint16_t len16 = (uint16_t)len;
    std::memcpy(&g_ring.data[g_ring.head], &seq, 4);
    g_ring.head = (g_ring.head + 4) % kDataBytes;
    std::memcpy(&g_ring.data[g_ring.head], &len16, 2);
    g_ring.head = (g_ring.head + 2) % kDataBytes;
    // The text can wrap the end of the ring; copy in two pieces.
    const int first = (g_ring.head + len <= kDataBytes) ? len : (kDataBytes - g_ring.head);
    std::memcpy(&g_ring.data[g_ring.head], text, first);
    if (first < len) std::memcpy(&g_ring.data[0], text + first, len - first);
    g_ring.head = (g_ring.head + len) % kDataBytes;
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
    if (g_hook_installed) return;
    g_prev_vprintf = esp_log_set_vprintf(&capture_hook);
    g_hook_installed = true;
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_read(char* out, int cap, uint32_t* out_seq_lo, uint32_t* out_lines) {
    if (out_seq_lo) *out_seq_lo = 0;
    if (out_lines) *out_lines = 0;
    if (out == nullptr || cap <= 1) return;

    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int written = 0;
    int at = (int)g_ring.tail;
    uint32_t lines = 0;
    uint32_t first_seq = 0;
    while (at != (int)g_ring.head) {
        uint32_t seq = 0;
        uint16_t len = 0;
        std::memcpy(&seq, &g_ring.data[at], 4);
        at = (at + 4) % kDataBytes;
        std::memcpy(&len, &g_ring.data[at], 2);
        at = (at + 2) % kDataBytes;
        if (written + (int)len + 1 > cap) break;
        const int first = (at + len <= kDataBytes) ? len : (kDataBytes - at);
        std::memcpy(out + written, &g_ring.data[at], first);
        if (first < len) std::memcpy(out + written + first, &g_ring.data[0], len - first);
        written += len;
        out[written++] = '\n';
        at = (at + len) % kDataBytes;
        if (lines == 0) first_seq = seq;
        lines++;
    }
    out[written] = 0;
    portEXIT_CRITICAL(&g_mux);

    if (out_seq_lo) *out_seq_lo = first_seq;
    if (out_lines) *out_lines = lines;
}

extern "C" void rf_logbuf_ack(uint32_t seq_hi) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    int at = (int)g_ring.tail;
    while (at != (int)g_ring.head) {
        uint32_t seq = 0;
        uint16_t len = 0;
        std::memcpy(&seq, &g_ring.data[at], 4);
        at = (at + 4) % kDataBytes;
        std::memcpy(&len, &g_ring.data[at], 2);
        at = (at + 2 + len) % kDataBytes;
        if (seq > seq_hi) break;
        g_ring.tail = at;
    }
    portEXIT_CRITICAL(&g_mux);
}

extern "C" void rf_logbuf_stats(uint32_t* dropped, uint32_t* used) {
    portENTER_CRITICAL(&g_mux);
    ensure_init_locked();
    if (dropped) *dropped = g_ring.dropped;
    if (used) *used = (g_ring.head + kDataBytes - g_ring.tail) % kDataBytes;
    portEXIT_CRITICAL(&g_mux);
}
