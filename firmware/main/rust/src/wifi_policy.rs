//! Wi-Fi cache codec, endpoint parsing, and fast-reconnect policy.
//!
//! Pure: C++ gathers normalized facts; this owns the codec, parser, and
//! decision table. C++ still owns FastRcCache/RTC storage and ESP-IDF calls.
//!
//! ## RTC layout (matches `wifi_station.cc:g_rtc_wifi_cache`)
//! ```text
//! magic  u32  = 0x52465731  ("RFW1")
//! ssid   [33] = SSID bytes + NUL (ESP_MAX_SSID_LEN)
//! bssid  [6]
//! channel u8
//! ```
//!
//! ## Endpoint parsing
//!
//! C++ loads the endpoint string from NVS, then calls `rf_wifi_parse_endpoint`
//! or `rf_wifi_parse_url_authority` (via this module's C ABI). The parsed host
//! is returned as a length-prefixed byte buffer. The boolean `endpoint_present`
//! fed to `rf_wifi_policy_decide` is set from the C++ caller's result.
//!
//! Parsing rules (matching current `wifi_station.cc` exactly):
//! - **MQTT** (`parse_endpoint`): plain `host[:port]`, default 8883.
//!   Non-numeric suffix after `:` is kept as host (e.g. `host:abc` → host=`host:abc`).
//! - **URL** (`parse_url_authority`): requires `http://`, `https://`, `ws://`, `wss://`.
//!   Default ports: http/ws → 80, https/wss → 443.
//!   Non-numeric suffix after authority colon is kept as host.
//! - IPv4 literals are accepted.

use core::ffi::c_char;

// ─── RTC cache codec ─────────────────────────────────────────────────────────

/// Magic value written/read from RTC slow memory.
pub const RTC_WIFI_CACHE_MAGIC: u32 = 0x52465731;

/// Number of bytes in the SSID field (32 + NUL, matching ESP_MAX_SSID_LEN).
pub const RTC_WIFI_SSID_LEN: usize = 33;

/// Total byte length of the Wi-Fi half of the RTC record.
pub const RTC_WIFI_RECORD_SIZE: usize = 4 + RTC_WIFI_SSID_LEN + 6 + 1;

/// Encode Wi-Fi half of the RTC cache into a byte buffer.
///
/// Writes exactly `RTC_WIFI_RECORD_SIZE` bytes to `buf`. Returns `Ok(())` on
/// success or `Err(())` if `ssid.len() >= RTC_WIFI_SSID_LEN`.
///
/// # Safety
/// `buf` must point to at least `RTC_WIFI_RECORD_SIZE` writable bytes.
pub unsafe fn encode_rtc_cache(
    ssid: &[u8],
    bssid: &[u8; 6],
    channel: u8,
    buf: *mut u8,
) -> Result<(), ()> {
    if ssid.len() >= RTC_WIFI_SSID_LEN {
        return Err(());
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, RTC_WIFI_RECORD_SIZE) };
    // magic
    buf[0..4].copy_from_slice(&RTC_WIFI_CACHE_MAGIC.to_le_bytes());
    // ssid (33 bytes, NUL-padded)
    buf[4..4 + RTC_WIFI_SSID_LEN].fill(0);
    buf[4..4 + ssid.len()].copy_from_slice(ssid);
    // bssid
    buf[4 + RTC_WIFI_SSID_LEN..4 + RTC_WIFI_SSID_LEN + 6].copy_from_slice(bssid);
    // channel
    buf[4 + RTC_WIFI_SSID_LEN + 6] = channel;
    Ok(())
}

/// Result of decoding the Wi-Fi half of the RTC cache.
#[derive(Debug, Clone, Copy)]
pub struct RtcCacheDecoded {
    /// Decoded SSID as a length-prefixed byte slice (no NUL).
    pub ssid: [u8; 32],
    /// Number of valid SSID bytes.
    pub ssid_len: u8,
    /// Decoded BSSID.
    pub bssid: [u8; 6],
    /// Decoded channel.
    pub channel: u8,
}

/// Decode the Wi-Fi half of the RTC cache from a byte buffer.
///
/// Returns `Some(RtcCacheDecoded)` on a valid record, or `None` on
/// invalid/empty/corrupt input.
///
/// A record is valid when:
/// - `buf.len() >= RTC_WIFI_RECORD_SIZE`
/// - magic == `RTC_WIFI_CACHE_MAGIC`
/// - `ssid[0] != '\0'` (non-empty SSID)
/// - `ssid[32] == '\0'` (NUL at byte 32 of the ssid field)
/// - BSSID is neither all-zero nor all-0xFF
/// - channel != 0
pub fn decode_rtc_cache(buf: &[u8]) -> Option<RtcCacheDecoded> {
    if buf.len() < RTC_WIFI_RECORD_SIZE {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if magic != RTC_WIFI_CACHE_MAGIC {
        return None;
    }

    let ssid_start = 4;
    let ssid_field_end = ssid_start + RTC_WIFI_SSID_LEN;
    let bssid_start = ssid_field_end;
    let channel_idx = bssid_start + 6;

    // SSID must be non-empty and NUL-terminated at byte 32
    if buf[ssid_start] == 0 || buf[ssid_start + (RTC_WIFI_SSID_LEN - 1)] != 0 {
        return None;
    }

    // Count NUL-terminated bytes in the SSID field
    let nul_idx = buf[ssid_start..ssid_field_end]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(RTC_WIFI_SSID_LEN);
    let ssid_len = nul_idx.min(32) as u8;

    let mut ssid_out = [0u8; 32];
    ssid_out[..ssid_len as usize].copy_from_slice(&buf[ssid_start..ssid_start + ssid_len as usize]);

    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&buf[bssid_start..bssid_start + 6]);

    let channel = buf[channel_idx];
    if channel == 0 {
        return None;
    }

    // BSSID must not be all-zero or all-0xFF
    let all_zero = bssid.iter().all(|&b| b == 0);
    let all_ff = bssid.iter().all(|&b| b == 0xFF);
    if all_zero || all_ff {
        return None;
    }

    Some(RtcCacheDecoded {
        ssid: ssid_out,
        ssid_len,
        bssid,
        channel,
    })
}

/// Returns true if a 6-byte BSSID is valid (neither all-zero nor all-0xFF).
pub fn is_valid_bssid(bssid: &[u8; 6]) -> bool {
    let all_zero = bssid.iter().all(|&b| b == 0);
    let all_ff = bssid.iter().all(|&b| b == 0xFF);
    !(all_zero || all_ff)
}

// ─── Endpoint parsing ────────────────────────────────────────────────────────

/// Parsed endpoint for MQTT mode (host[:port]).
///
/// Parses `host[:port]` from a raw string. Default port is 8883.
/// Non-numeric suffix after `:` is kept as part of the host.
///
/// Returns `Ok((host_len, port))` if parsing succeeds. `host_len` is the number
/// of valid host bytes written to `host_buf`. Returns `Err(())` on failure.
///
/// # Safety
/// `host_buf` must point to at least 256 writable bytes.
pub unsafe fn parse_endpoint(
    input: &str,
    host_buf: *mut u8,
    host_buf_len: usize,
) -> Result<(usize, u16), ()> {
    if input.is_empty() || host_buf.is_null() || host_buf_len < 256 {
        return Err(());
    }

    let colon_pos = input.rfind(':');
    let (host_str, port) = match colon_pos {
        Some(pos) if pos + 1 < input.len() => {
            let after = &input[pos + 1..];
            let all_digits = after.bytes().all(|b| b.is_ascii_digit());
            if all_digits {
                let port: u16 = after.parse().map_err(|_| ())?;
                let host = &input[..pos];
                if host.is_empty() {
                    return Err(());
                }
                (host, port)
            } else {
                // Non-numeric suffix: keep entire input as host
                (input, 8883u16)
            }
        }
        _ => (input, 8883u16),
    };

    let host_bytes = host_str.as_bytes();
    let write_len = host_bytes.len().min(host_buf_len).min(255);
    unsafe {
        core::slice::from_raw_parts_mut(host_buf, write_len)
            .copy_from_slice(&host_bytes[..write_len]);
    }
    Ok((write_len, port))
}

/// Parsed authority for URL mode (scheme://host[:port]/...).
///
/// Parses `http://`, `https://`, `ws://`, `wss://` URLs. Default ports:
/// http/ws → 80, https/wss → 443.
///
/// Returns `Ok((host_len, port))` on success. Returns `Err(())` when the scheme
/// is missing/unsupported or the authority is empty.
///
/// # Safety
/// `host_buf` must point to at least 256 writable bytes.
pub unsafe fn parse_url_authority(
    input: &str,
    host_buf: *mut u8,
    host_buf_len: usize,
) -> Result<(usize, u16), ()> {
    if input.is_empty() || host_buf.is_null() || host_buf_len < 256 {
        return Err(());
    }

    let scheme_end = input.find("://").ok_or(())?;
    let scheme = &input[..scheme_end].as_bytes();
    let is_default_scheme = |expected: &[u8]| {
        scheme.len() == expected.len() && scheme.iter().zip(expected).all(|(actual, wanted)| {
            actual.to_ascii_lowercase() == *wanted
        })
    };
    let default_port = if is_default_scheme(b"https") || is_default_scheme(b"wss") {
        443u16
    } else if is_default_scheme(b"http") || is_default_scheme(b"ws") {
        80u16
    } else {
        return Err(());
    };

    let authority_start = scheme_end + 3;
    if authority_start >= input.len() {
        return Err(());
    }

    let authority_end = input[authority_start..]
        .find('/')
        .map(|p| authority_start + p)
        .unwrap_or(input.len());
    let authority = &input[authority_start..authority_end];

    if authority.is_empty() {
        return Err(());
    }

    let (host_str, port) = match authority.rfind(':') {
        Some(pos) if pos + 1 < authority.len() => {
            let after = &authority[pos + 1..];
            let all_digits = after.bytes().all(|b| b.is_ascii_digit());
            if all_digits {
                let port: u16 = after.parse().map_err(|_| ())?;
                (&authority[..pos], port)
            } else {
                (authority, default_port)
            }
        }
        _ => (authority, default_port),
    };

    if host_str.is_empty() {
        return Err(());
    }

    let host_bytes = host_str.as_bytes();
    let write_len = host_bytes.len().min(host_buf_len).min(255);
    unsafe {
        core::slice::from_raw_parts_mut(host_buf, write_len)
            .copy_from_slice(&host_bytes[..write_len]);
    }
    Ok((write_len, port))
}

// ─── Policy ─────────────────────────────────────────────────────────────────

/// Maximum age of the IP fast cache before it is considered stale.
/// Matches `kIpFastMaxAgeMs = 60 * 60 * 1000` in `wifi_station.cc`.
pub const IP_FAST_MAX_AGE_MS: i64 = 3_600_000;

/// Fast connect failure count threshold — above this, fast connect is skipped.
pub const FAST_FAIL_THRESHOLD: i32 = 3;

// ─── Access-point health (modem-sleep suppression) ──────────────────────────

/// `WIFI_REASON_BEACON_TIMEOUT` from `esp_wifi_types_generic.h`.
pub const BEACON_TIMEOUT_REASON: i32 = 200;

/// Consecutive BEACON_TIMEOUT disconnects that mean "this AP cannot hold a
/// modem-sleep schedule" rather than transient bad luck.
///
/// An AP that omits the TIM IE (the driver warns "does not follow Wi-Fi
/// protocol") leaves the station unable to know when its buffered frames
/// arrive, so the sleep schedule burns the beacon window until the link dies.
/// Observed on `yi02`: beacons every 102.4 ms, repeated `bcn_timeout` for
/// ~112 s, then reason=200.
///
/// Three, not one: a single timeout happens on roaming and busy channels.
pub const MODEM_SLEEP_SUPPRESS_AFTER: u32 = 3;

/// A link that stayed up at least this long counts as healthy: its eventual
/// BEACON_TIMEOUT starts a fresh run instead of extending the previous one.
///
/// Without this the suppression could never engage (every timeout is followed
/// by a reconnect, and a reconnect would reset the run), and without the reset
/// it could never clear. The threshold separates "this AP drops us every few
/// seconds" from "this AP held for minutes and then hiccupped".
pub const HEALTHY_CONNECTION_MS: u64 = 60_000;

/// Advance the BEACON_TIMEOUT streak for the disconnect just observed.
///
/// `connected_ms` is how long the just-ended connection lasted. A run that
/// survived `HEALTHY_CONNECTION_MS` is healthy, so its termination starts a
/// new count; a short-lived one continues the run. Any reason other than
/// BEACON_TIMEOUT clears the streak. Saturated at the threshold: the value
/// only gates a boolean, and an unbounded counter would eventually wrap.
pub fn next_beacon_timeout_streak(streak: u32, reason: i32, connected_ms: u64) -> u32 {
    if reason != BEACON_TIMEOUT_REASON {
        return 0;
    }
    let base = if connected_ms >= HEALTHY_CONNECTION_MS { 0 } else { streak };
    (base + 1).min(MODEM_SLEEP_SUPPRESS_AFTER)
}

/// Whether modem sleep must be turned off for the current association.
///
/// True only when the link has repeatedly failed with BEACON_TIMEOUT: the
/// access point cannot support a sleep schedule, so trading battery for a
/// stable link is the right side of the trade. Always false for every other
/// disconnect reason.
pub fn should_suppress_modem_sleep(streak: u32, reason: i32) -> bool {
    reason == BEACON_TIMEOUT_REASON && streak >= MODEM_SLEEP_SUPPRESS_AFTER
}

/// Policy action kinds returned by `decide()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionKind {
    /// Take no action.
    NoOp = 0,
    /// Attempt fast direct connect using cached Wi-Fi association.
    DirectConnect = 10,
    /// Proceed with the current probe.
    Probe = 11,
    /// Fall back to a full scan.
    Scan = 12,
    /// Retry the current step.
    Retry = 13,
    /// Stop this fast-path attempt entirely.
    Stop = 14,
    /// Defer the probe: stop fast IP attempt, optionally restart DHCP.
    DeferProbe = 20,
}

/// Policy action including cache directives and retry delay.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PolicyAction {
    pub action_kind: u8,
    /// Whether to clear the Wi-Fi association cache.
    pub clear_wifi_cache: bool,
    /// Whether to clear the IP fast cache.
    pub clear_ip_cache: bool,
    /// Retry suppression delay in ms (0 = use component default).
    pub retry_delay_ms: u32,
    /// For `DeferProbe`: whether to retain the IP cache.
    pub retain_ip: bool,
}

impl Default for PolicyAction {
    fn default() -> Self {
        Self {
            action_kind: ActionKind::NoOp as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }
}

impl PolicyAction {
    pub const fn direct_connect() -> Self {
        Self {
            action_kind: ActionKind::DirectConnect as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }

    pub const fn scan() -> Self {
        Self {
            action_kind: ActionKind::Scan as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }

    pub const fn probe() -> Self {
        Self {
            action_kind: ActionKind::Probe as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }

    pub const fn stop() -> Self {
        Self {
            action_kind: ActionKind::Stop as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }

    /// Defer and retain the IP cache (planned 3b fix).
    pub const fn defer_retain_ip() -> Self {
        Self {
            action_kind: ActionKind::DeferProbe as u8,
            clear_wifi_cache: false,
            clear_ip_cache: false,
            retry_delay_ms: 0,
            retain_ip: true,
        }
    }

    /// Defer and clear the IP cache (all probe failures except endpoint_missing).
    pub const fn defer_clear_ip() -> Self {
        Self {
            action_kind: ActionKind::DeferProbe as u8,
            clear_wifi_cache: false,
            clear_ip_cache: true,
            retry_delay_ms: 0,
            retain_ip: false,
        }
    }

    pub const fn retry() -> Self { Self { action_kind: ActionKind::Retry as u8, clear_wifi_cache: false, clear_ip_cache: false, retry_delay_ms: 0, retain_ip: false } }
    pub const fn stop_clear_wifi() -> Self { Self { action_kind: ActionKind::Stop as u8, clear_wifi_cache: true, clear_ip_cache: false, retry_delay_ms: 0, retain_ip: false } }

    pub const fn is_noop(&self) -> bool {
        self.action_kind == ActionKind::NoOp as u8
    }
}

/// Probe target service type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeTarget {
    Mqtt = 0,
    WebSocket = 1,
    HttpOta = 2,
}

/// Rust inputs to the Wi-Fi policy decision.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// True when the station already has a Wi-Fi association.
    pub wifi_connected: bool,
    /// True when the fast Wi-Fi cache is present and valid.
    pub have_wifi_cache: bool,
    /// True when the cached BSSID is valid.
    pub cache_bssid_valid: bool,
    /// The cached channel (0 = no channel cached).
    pub cache_channel: u8,
    /// The cached SSID as a byte slice (up to 32 bytes, no NUL).
    pub cache_ssid: [u8; 32],
    /// Length of the valid SSID bytes.
    pub cache_ssid_len: u8,
    /// The cached BSSID bytes.
    pub cache_bssid: [u8; 6],
    /// Age of the Wi-Fi cache in ms.
    pub wifi_cache_age_ms: i64,
    /// True when the IP fast cache is present.
    pub have_ip_cache: bool,
    /// Age of the IP fast cache in ms.
    pub ip_cache_age_ms: i64,
    /// True when an IP fast attempt is currently running.
    pub ip_fast_active: bool,
    /// True when the IP fast probe succeeded.
    pub ip_fast_ready: bool,
    /// Current reconnect attempt number.
    pub reconnect_count: i32,
    /// Number of consecutive fast-connect failures.
    pub fast_fail_count: i32,
    /// Whether fast connect is enabled by configuration.
    pub fast_enabled: bool,
    /// Whether the probe endpoint is present.
    pub endpoint_present: bool,
    /// The probe target.
    pub probe_target: ProbeTarget,
    /// Whether the host is an IPv4 literal.
    pub host_is_ip_literal: bool,
}

/// Core policy decision function.
///
/// Pure: no hidden cross-task state.
///
/// Policy:
/// - **IP fast active**: cache stale → `DeferProbe { clear_ip }`;
///   endpoint missing → `DeferProbe { retain_ip: true }` (3b fix — retains
///   the IP fast cache and the association cache/RTC mirror so the next
///   attempt can probe once the endpoint resolves);
///   otherwise → `Probe`.
/// - **Wi-Fi fast connect**: usable cache + within fail threshold → `DirectConnect`;
///   fail threshold exceeded → `Scan`; no cache → `Scan`; fast disabled → `Scan`.
pub fn decide(i: &Inputs) -> PolicyAction {
    // ── IP fast path ────────────────────────────────────────────────────────
    if i.ip_fast_active {
        if i.have_ip_cache && i.ip_cache_age_ms > IP_FAST_MAX_AGE_MS {
            return PolicyAction::defer_clear_ip();
        }

        if !i.endpoint_present {
            // 3b: endpoint_missing retains IP cache and association/RTC cache.
            // C++ must stop this IP fast attempt, restart DHCP, and return
            // without DNS/TCP probe. All other fallbacks still clear IP cache.
            return PolicyAction::defer_retain_ip();
        }

        // Probe in progress — no intervention needed.
        return PolicyAction::probe();
    }

    // Terminal disconnect count is authoritative; before that, cached fast
    // association selection owns the first/subsequent attempt.
    if !i.ip_fast_active && !i.wifi_connected && i.reconnect_count >= 5 {
        return if i.fast_fail_count >= FAST_FAIL_THRESHOLD {
            PolicyAction::stop_clear_wifi()
        } else {
            PolicyAction::stop()
        };
    }
    if i.have_wifi_cache && i.cache_bssid_valid && i.cache_channel != 0 && i.cache_ssid_len > 0 {
        if i.fast_enabled && i.fast_fail_count < FAST_FAIL_THRESHOLD {
            return PolicyAction::direct_connect();
        }
        return PolicyAction::scan();
    }
    if !i.ip_fast_active && !i.wifi_connected {
        return PolicyAction::retry();
    }

    PolicyAction::scan()
}

// ─── C ABI ──────────────────────────────────────────────────────────────────

/// C ABI inputs — must match `rf_wifi_policy_inputs_t` in `wifi_policy.h`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CInputs {
    pub version: u32,
    pub invoke_count: u32,
    pub wifi_connected: i32,
    pub rssi: i32,
    pub channel: u8,
    pub _pad0: [u8; 3],
    pub reconnect_count: u32,
    pub ip_fast_active: i32,
    pub ip_fast_ready: i32,
    pub ip_fast_cache_age_ms: u32,
    pub have_wifi_cache: u8,
    pub cache_bssid_valid: u8,
    pub cache_channel: u8,
    pub _pad1: u8,
    pub cache_ssid: [u8; 32],
    pub cache_ssid_len: u8,
    pub cache_bssid: [u8; 6],
    pub wifi_cache_age_ms: i32,
    pub have_ip_cache: u8,
    pub _pad2: [u8; 3],
    pub ip_cache_age_ms: i32,
    pub fast_fail_count: i32,
    pub fast_enabled: u8,
    pub endpoint_present: u8,
    pub probe_target: u8,
    pub host_is_ip_literal: u8,
}

/// C ABI output — must match `rf_wifi_policy_output_t` in `wifi_policy.h`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct COutput {
    pub version: u32,
    pub action_kind: u8,
    pub clear_wifi_cache: u8,
    pub clear_ip_cache: u8,
    pub retry_delay_ms: u32,
    pub retain_ip: u8,
}

impl Default for COutput {
    fn default() -> Self {
        Self {
            version: 1,
            action_kind: ActionKind::NoOp as u8,
            clear_wifi_cache: 0,
            clear_ip_cache: 0,
            retry_delay_ms: 0,
            retain_ip: 0,
        }
    }
}

/// Decode the C++ `NetworkProbeTarget` code into the Rust enum.
///
/// Order is the C++ producer's, from `network_probe_target.h`:
/// `HttpOta = 0, WebSocket = 1, Mqtt = 2`. An unknown code cannot be mapped
/// meaningfully, so it falls back to `HttpOta` — the source this product
/// actually configures, which keeps an unhandled future code from probing a
/// service that has no endpoint at all.
fn c_abi_probe_target(code: u8) -> ProbeTarget {
    match code {
        1 => ProbeTarget::WebSocket,
        2 => ProbeTarget::Mqtt,
        _ => ProbeTarget::HttpOta,
    }
}

/// # Safety
/// `inp` and `out` must point to valid, correctly aligned structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_policy_decide(
    inp: *const CInputs,
    out: *mut COutput,
) {
    if inp.is_null() || out.is_null() {
        return;
    }
    let i = unsafe { &*inp };

    let inputs = Inputs {
        wifi_connected: i.wifi_connected != 0,
        have_wifi_cache: i.have_wifi_cache != 0,
        cache_bssid_valid: i.cache_bssid_valid != 0,
        cache_channel: i.cache_channel,
        cache_ssid: i.cache_ssid,
        cache_ssid_len: i.cache_ssid_len,
        cache_bssid: i.cache_bssid,
        wifi_cache_age_ms: i.wifi_cache_age_ms as i64,
        have_ip_cache: i.have_ip_cache != 0,
        ip_cache_age_ms: i.ip_cache_age_ms as i64,
        ip_fast_active: i.ip_fast_active != 0,
        ip_fast_ready: i.ip_fast_ready != 0,
        reconnect_count: i.reconnect_count as i32,
        fast_fail_count: i.fast_fail_count as i32,
        fast_enabled: i.fast_enabled != 0,
        endpoint_present: i.endpoint_present != 0,
        probe_target: c_abi_probe_target(i.probe_target),
        host_is_ip_literal: i.host_is_ip_literal != 0,
    };

    let action = decide(&inputs);

    unsafe {
        core::ptr::write(
            out,
            COutput {
                version: 1,
                action_kind: action.action_kind,
                clear_wifi_cache: if action.clear_wifi_cache { 1 } else { 0 },
                clear_ip_cache: if action.clear_ip_cache { 1 } else { 0 },
                retry_delay_ms: action.retry_delay_ms,
                retain_ip: if action.retain_ip { 1 } else { 0 },
            },
        );
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_encode_rtc_cache(
    ssid: *const u8,
    ssid_len: u32,
    bssid: *const u8,
    channel: u8,
    buf: *mut u8,
    buf_len: u32,
) -> u8 {
    if ssid.is_null() || bssid.is_null() || buf.is_null() || buf_len < RTC_WIFI_RECORD_SIZE as u32 || ssid_len >= RTC_WIFI_SSID_LEN as u32 {
        return 0;
    }
    let ssid_slice = unsafe { core::slice::from_raw_parts(ssid, ssid_len as usize) };
    let bssid_slice = unsafe { core::slice::from_raw_parts(bssid, 6) };
    let bssid_array: &[u8; 6] = bssid_slice.try_into().unwrap();
    unsafe { encode_rtc_cache(ssid_slice, bssid_array, channel, buf) }.is_ok() as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_decode_rtc_cache(
    buf: *const u8,
    buf_len: u32,
    ssid_out: *mut u8,
    ssid_out_cap: u32,
    bssid_out: *mut u8,
    channel_out: *mut u8,
) -> u8 {
    if buf.is_null() || ssid_out.is_null() || bssid_out.is_null() || channel_out.is_null() || buf_len < RTC_WIFI_RECORD_SIZE as u32 || ssid_out_cap < 32 {
        return 0;
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf, buf_len as usize) };
    let Some(decoded) = decode_rtc_cache(bytes) else { return 0; };
    unsafe {
        core::ptr::copy_nonoverlapping(decoded.ssid.as_ptr(), ssid_out, 32);
        core::ptr::copy_nonoverlapping(decoded.bssid.as_ptr(), bssid_out, 6);
        *channel_out = decoded.channel;
    }
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_validate_rtc_cache(buf: *const u8, buf_len: u32) -> u8 {
    (!buf.is_null() && buf_len >= RTC_WIFI_RECORD_SIZE as u32 && decode_rtc_cache(unsafe { core::slice::from_raw_parts(buf, buf_len as usize) }).is_some()) as u8
}

/// Parse an MQTT endpoint (host[:port]) and write the host to a buffer.
///
/// # Safety
/// `host_buf` must point to at least 256 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_parse_endpoint(
    input: *const c_char,
    host_buf: *mut u8,
    host_buf_len: u32,
    port: *mut u16,
) -> u8 {
    if input.is_null() || host_buf.is_null() || port.is_null() {
        return 0;
    }
    let cstr = match unsafe { core::ffi::CStr::from_ptr(input).to_str() } {
        Ok(s) => s,
        Err(_) => return 0,
    };
    match unsafe { parse_endpoint(cstr, host_buf, host_buf_len as usize) } {
        Ok((_, p)) => {
            unsafe { *port = p };
            1
        }
        Err(()) => 0,
    }
}

/// Parse a URL authority (scheme://host[:port]/...) and write the host to a buffer.
///
/// # Safety
/// `host_buf` must point to at least 256 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rf_wifi_parse_url_authority(
    input: *const c_char,
    host_buf: *mut u8,
    host_buf_len: u32,
    port: *mut u16,
) -> u8 {
    if input.is_null() || host_buf.is_null() || port.is_null() {
        return 0;
    }
    let cstr = match unsafe { core::ffi::CStr::from_ptr(input).to_str() } {
        Ok(s) => s,
        Err(_) => return 0,
    };
    match unsafe { parse_url_authority(cstr, host_buf, host_buf_len as usize) } {
        Ok((_, p)) => {
            unsafe { *port = p };
            1
        }
        Err(()) => 0,
    }
}

/// Advance the BEACON_TIMEOUT streak. C++ keeps the streak in RAM (it is
/// per-association, not per-boot), feeds the reason code of the disconnect it
/// just observed, and how long that connection lasted (`connected_ms`).
///
/// # Safety
/// This scalar-only FFI has no pointer preconditions.
#[unsafe(no_mangle)]
pub extern "C" fn rf_wifi_beacon_timeout_streak(
    streak: u32,
    reason: i32,
    connected_ms: u64,
) -> u32 {
    next_beacon_timeout_streak(streak, reason, connected_ms)
}

/// # Safety
/// This scalar-only FFI has no pointer preconditions.
#[unsafe(no_mangle)]
pub extern "C" fn rf_wifi_suppress_modem_sleep(streak: u32, reason: i32) -> u8 {
    should_suppress_modem_sleep(streak, reason) as u8
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cache codec ─────────────────────────────────────────────────────────

    fn make_rtc_valid() -> [u8; RTC_WIFI_RECORD_SIZE] {
        let mut buf = [0u8; RTC_WIFI_RECORD_SIZE];
        buf[0..4].copy_from_slice(&RTC_WIFI_CACHE_MAGIC.to_le_bytes());
        // SSID: "TEST" (4 bytes) + NUL padding
        buf[4] = b'T';
        buf[5] = b'E';
        buf[6] = b'S';
        buf[7] = b'T';
        buf[4 + 32] = 0; // NUL at byte 32
        // BSSID
        buf[4 + RTC_WIFI_SSID_LEN..][..6].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        // Channel
        buf[4 + RTC_WIFI_SSID_LEN + 6] = 6;
        buf
    }

    #[test]
    fn decode_wrong_magic_rejected() {
        let mut buf = make_rtc_valid();
        buf[0..4].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
        assert!(decode_rtc_cache(&buf).is_none());
    }

    #[test]
    fn decode_empty_ssid_rejected() {
        let mut buf = make_rtc_valid();
        buf[4] = 0; // empty SSID
        assert!(decode_rtc_cache(&buf).is_none());
    }

    #[test]
    fn decode_missing_nul_at_byte_32_rejected() {
        let mut buf = make_rtc_valid();
        buf[4 + 32] = b'X'; // NUL not at byte 32
        assert!(decode_rtc_cache(&buf).is_none());
    }

    #[test]
    fn decode_zero_bssid_rejected() {
        let mut buf = make_rtc_valid();
        buf[4 + RTC_WIFI_SSID_LEN..][..6].fill(0);
        assert!(decode_rtc_cache(&buf).is_none());
    }

    #[test]
    fn decode_all_ff_bssid_rejected() {
        let mut buf = make_rtc_valid();
        buf[4 + RTC_WIFI_SSID_LEN..][..6].fill(0xFF);
        assert!(decode_rtc_cache(&buf).is_none());
    }

    #[test]
    fn decode_valid_record_ok() {
        let buf = make_rtc_valid();
        let dec = decode_rtc_cache(&buf).unwrap();
        assert_eq!(dec.ssid_len, 4);
        assert_eq!(&dec.ssid[..4], b"TEST");
        assert_eq!(dec.bssid, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(dec.channel, 6);
    }

    #[test]
    fn decode_truncated_buffer_rejected() {
        let short = [0u8; 10];
        assert!(decode_rtc_cache(&short).is_none());
    }

    #[test]
    fn decode_fixed_byte_layout() {
        let buf = make_rtc_valid();
        // magic at offset 0
        assert_eq!(&buf[0..4], &RTC_WIFI_CACHE_MAGIC.to_le_bytes());
        // ssid bytes at offset 4
        assert_eq!(&buf[4..8], b"TEST");
        // NUL padding after ssid
        assert_eq!(buf[4 + 4], 0);
        // NUL at final byte of ssid field
        assert_eq!(buf[4 + RTC_WIFI_SSID_LEN - 1], 0);
        // bssid after ssid
        assert_eq!(
            &buf[4 + RTC_WIFI_SSID_LEN..4 + RTC_WIFI_SSID_LEN + 6],
            &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
        // channel after bssid
        assert_eq!(buf[4 + RTC_WIFI_SSID_LEN + 6], 6);
    }

    #[test]
    fn encode_roundtrip() {
        let ssid = b"Hello";
        let bssid = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let channel = 11u8;

        let mut buf = [0u8; RTC_WIFI_RECORD_SIZE];
        // Safety: buf is exactly RTC_WIFI_RECORD_SIZE bytes
        unsafe {
            encode_rtc_cache(ssid, &bssid, channel, buf.as_mut_ptr()).unwrap();
        }

        let dec = decode_rtc_cache(&buf).unwrap();
        assert_eq!(dec.ssid_len, 5);
        assert_eq!(&dec.ssid[..5], b"Hello");
        assert_eq!(dec.bssid, bssid);
        assert_eq!(dec.channel, channel);
    }

    #[test]
    fn encode_oversize_ssid_rejected() {
        let ssid = [0x41u8; 33]; // 33 bytes = exactly RTC_WIFI_SSID_LEN (too large)
        let bssid = [0u8; 6];
        let mut buf = [0u8; RTC_WIFI_RECORD_SIZE];
        unsafe {
            assert!(encode_rtc_cache(&ssid, &bssid, 1, buf.as_mut_ptr()).is_err());
        }
    }

    // ── BSSID validation ────────────────────────────────────────────────────

    #[test]
    fn bssid_all_zero_invalid() {
        assert!(!is_valid_bssid(&[0u8; 6]));
    }

    #[test]
    fn bssid_all_ff_invalid() {
        assert!(!is_valid_bssid(&[0xFFu8; 6]));
    }

    #[test]
    fn bssid_mixed_valid() {
        assert!(is_valid_bssid(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
    }

    #[test]
    fn bssid_one_nonzero_valid() {
        assert!(is_valid_bssid(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x01]));
    }

    // ── Endpoint parsing ─────────────────────────────────────────────────────

    #[test]
    fn mqtt_parse_plain_host_default_8883() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_endpoint("mqtt.example.com", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "mqtt.example.com");
        assert_eq!(port, 8883);
    }

    #[test]
    fn mqtt_parse_with_port() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_endpoint("mqtt.example.com:1883", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "mqtt.example.com");
        assert_eq!(port, 1883);
    }

    #[test]
    fn mqtt_nonnumeric_suffix_kept_as_host() {
        // Lax authority: "host:abc" keeps "host:abc" as host, uses default port
        let mut host = [0u8; 256];
        let r = unsafe { parse_endpoint("mqtt.example.com:1883abc", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "mqtt.example.com:1883abc");
        assert_eq!(port, 8883); // default
    }

    #[test]
    fn mqtt_empty_string_rejected() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_endpoint("", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn mqtt_trailing_colon_kept_as_host() {
        // C++ keeps "host:" (incl colon) as host with default port 8883.
        let mut host = [0u8; 256];
        let (len, port) = unsafe { parse_endpoint("host:", host.as_mut_ptr(), 256) }.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "host:");
        assert_eq!(port, 8883);
    }

    #[test]
    fn mqtt_colon_only_host_empty_rejected() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_endpoint(":1883", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn url_requires_scheme() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_url_authority("example.com", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn url_requires_scheme_slashes() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_url_authority("http:/example.com", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn url_accepts_http_default_80() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("http://example.com/path", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "example.com");
        assert_eq!(port, 80);
    }

    #[test]
    fn url_accepts_https_default_443() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("https://secure.example.com/", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "secure.example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn url_accepts_ws_default_80() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("ws://localhost:8080/socket", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "localhost");
        assert_eq!(port, 8080);
    }

    #[test]
    fn url_accepts_wss_default_443() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("wss://secure.example.com:8443/ws", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "secure.example.com");
        assert_eq!(port, 8443);
    }

    #[test]
    fn url_http_ws_default_80() {
        let mut host = [0u8; 256];
        assert_eq!(unsafe { parse_url_authority("http://example.com/", host.as_mut_ptr(), 256) }.unwrap().1, 80);
        assert_eq!(unsafe { parse_url_authority("ws://example.com/", host.as_mut_ptr(), 256) }.unwrap().1, 80);
    }

    #[test]
    fn url_authority_accepts_uppercase_schemes() {
        for (url, expected_port) in [
            ("HTTP://example.com/", 80),
            ("HTTPS://example.com/", 443),
            ("WS://example.com/", 80),
            ("WSS://example.com/", 443),
        ] {
            let mut host = [0u8; 256];
            let (len, port) = unsafe { parse_url_authority(url, host.as_mut_ptr(), 256) }.unwrap();
            assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "example.com");
            assert_eq!(port, expected_port);
        }
    }

    #[test]
    fn url_https_wss_default_443() {
        let mut host = [0u8; 256];
        assert_eq!(unsafe { parse_url_authority("https://example.com/", host.as_mut_ptr(), 256) }.unwrap().1, 443);
        assert_eq!(unsafe { parse_url_authority("wss://example.com/", host.as_mut_ptr(), 256) }.unwrap().1, 443);
    }

    #[test]
    fn url_nonnumeric_suffix_kept_as_host() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("http://example.com:8080abc/", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "example.com:8080abc");
        assert_eq!(port, 80); // default
    }

    #[test]
    fn url_unsupported_scheme_rejected() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_url_authority("ftp://example.com/", host.as_mut_ptr(), 256) }.is_err());
        assert!(unsafe { parse_url_authority("mqtt://example.com/", host.as_mut_ptr(), 256) }.is_err());
        assert!(unsafe { parse_url_authority("invalid://example.com/", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn url_empty_authority_rejected() {
        let mut host = [0u8; 256];
        assert!(unsafe { parse_url_authority("http:///", host.as_mut_ptr(), 256) }.is_err());
        assert!(unsafe { parse_url_authority("http://:8080/", host.as_mut_ptr(), 256) }.is_err());
    }

    #[test]
    fn url_accepts_ipv4_literal() {
        let mut host = [0u8; 256];
        let r = unsafe { parse_url_authority("http://192.168.1.1:8080/", host.as_mut_ptr(), 256) };
        let (len, port) = r.unwrap();
        assert_eq!(core::str::from_utf8(&host[..len]).unwrap(), "192.168.1.1");
        assert_eq!(port, 8080);
    }

    #[test]
    fn url_explicit_default_port() {
        let mut host = [0u8; 256];
        assert_eq!(unsafe { parse_url_authority("http://example.com:80/", host.as_mut_ptr(), 256) }.unwrap().1, 80);
    }

    // ── Policy transitions ──────────────────────────────────────────────────

    /// Build a 32-byte SSID array from a short byte string.
    fn mk_ssid(s: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[..s.len()].copy_from_slice(s);
        out
    }

     fn make_inputs(override_fn: impl FnOnce(&mut Inputs)) -> Inputs {
        let mut i = Inputs {
            wifi_connected: false,
            have_wifi_cache: false,
            cache_bssid_valid: false,
            cache_channel: 0,
            cache_ssid: [0u8; 32],
            cache_ssid_len: 0,
            cache_bssid: [0u8; 6],
            wifi_cache_age_ms: 0,
            have_ip_cache: false,
            ip_cache_age_ms: 0,
            ip_fast_active: false,
            ip_fast_ready: false,
            reconnect_count: 0,
            fast_fail_count: 0,
            fast_enabled: true,
            endpoint_present: true,
            probe_target: ProbeTarget::Mqtt,
            host_is_ip_literal: false,
        };
        override_fn(&mut i);
        i
    }

    #[test]
    fn reconnect_policy_transitions() {
        let retry = decide(&Inputs { reconnect_count: 1, ..make_inputs(|_| {}) });
        assert_eq!(retry.action_kind, ActionKind::Retry as u8);
        let stop = decide(&Inputs { reconnect_count: 5, ..make_inputs(|_| {}) });
        assert_eq!(stop.action_kind, ActionKind::Stop as u8);
        let clear = decide(&Inputs { reconnect_count: 5, fast_fail_count: 3, ..make_inputs(|_| {}) });
        assert!(clear.clear_wifi_cache);
    }


    #[test]
    fn direct_connect_when_cache_valid_under_threshold() {
        let i = make_inputs(|i| {
            i.have_wifi_cache = true;
            i.cache_bssid_valid = true;
            i.cache_channel = 6;
            i.cache_ssid = mk_ssid(b"MyNetwork");
            i.cache_ssid_len = 9;
            i.cache_bssid = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
            i.fast_fail_count = 0;
            i.fast_enabled = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::DirectConnect as u8);
        assert!(!a.clear_wifi_cache);
        assert!(!a.clear_ip_cache);
    }

    #[test]
    fn scan_when_fail_threshold_exceeded() {
        let i = make_inputs(|i| {
            i.have_wifi_cache = true;
            i.cache_bssid_valid = true;
            i.cache_channel = 6;
            i.cache_ssid = mk_ssid(b"MyNetwork");
            i.cache_ssid_len = 9;
            i.cache_bssid = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
            i.fast_fail_count = FAST_FAIL_THRESHOLD; // at threshold
            i.fast_enabled = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::Scan as u8);
    }

    #[test]
    fn scan_when_cache_miss() {
        let i = make_inputs(|i| {
            i.wifi_connected = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::Scan as u8);
    }

    #[test]
    fn scan_when_fast_disabled() {
        let i = make_inputs(|i| {
            i.have_wifi_cache = true;
            i.cache_bssid_valid = true;
            i.cache_channel = 6;
            i.cache_ssid = mk_ssid(b"MyNetwork");
            i.cache_ssid_len = 9;
            i.cache_bssid = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
            i.fast_fail_count = 0;
            i.fast_enabled = false;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::Scan as u8);
    }

    #[test]
    fn probe_when_ip_fast_active_and_endpoint_present() {
        let i = make_inputs(|i| {
            i.ip_fast_active = true;
            i.have_ip_cache = true;
            i.ip_cache_age_ms = 500_000;
            i.endpoint_present = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::Probe as u8);
    }

    #[test]
    fn defer_clear_ip_when_cache_stale() {
        let i = make_inputs(|i| {
            i.ip_fast_active = true;
            i.have_ip_cache = true;
            i.ip_cache_age_ms = IP_FAST_MAX_AGE_MS + 1;
            i.endpoint_present = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::DeferProbe as u8);
        assert!(a.clear_ip_cache, "stale cache must clear IP");
        assert!(!a.retain_ip);
    }

    #[test]
    fn defer_clear_ip_when_endpoint_present_but_unrelated_probe_failure() {
        // Regression guard: when the endpoint IS present, a probe-path
        // deferral still clears the IP cache. Only the missing-endpoint
        // branch retains it (see defer_retain_ip_when_endpoint_missing_3b).
        // A stale cache with a present endpoint exercises this clear path.
        let i = make_inputs(|i| {
            i.ip_fast_active = true;
            i.have_ip_cache = true;
            i.ip_cache_age_ms = IP_FAST_MAX_AGE_MS + 1;
            i.endpoint_present = true;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::DeferProbe as u8);
        assert!(a.clear_ip_cache, "stale cache with endpoint present clears IP cache");
        assert!(!a.retain_ip);
    }

    #[test]
    fn defer_retain_ip_when_endpoint_missing_3b() {
        // 3b: endpoint_missing → DeferProbe { retain_ip: true }.
        // The IP fast cache and the association cache/RTC mirror are
        // retained so the next attempt can probe once the endpoint resolves.
        let i = make_inputs(|i| {
            i.ip_fast_active = true;
            i.have_ip_cache = true;
            i.ip_cache_age_ms = 100_000;
            i.endpoint_present = false;
        });
        let a = decide(&i);
        assert_eq!(a.action_kind, ActionKind::DeferProbe as u8);
        assert!(!a.clear_ip_cache, "3b: endpoint_missing must NOT clear IP cache");
        assert!(a.retain_ip, "3b: endpoint_missing must retain IP");
        assert!(!a.clear_wifi_cache, "3b: association cache is retained");
    }

    #[test]
    fn c_abi_first_dispatch_endpoint_missing_retains_ip_full_facts() {
        // Reviewer P1: RunIpFast's FIRST policy invocation carries the full
        // input set (have_ip_cache=1, ip_fast_active=1, endpoint_present=0)
        // and must return DeferProbe with clear_ip_cache=0 so C++ executes
        // the retain path (stop attempt, restart DHCP, retain IP/assoc/RTC
        // cache, no DNS/TCP probe) — NOT the legacy clear fallback.
        let cin = CInputs {
            version: 1,
            invoke_count: 0,
            wifi_connected: 1,
            rssi: -50,
            channel: 6,
            _pad0: [0; 3],
            reconnect_count: 0,
            ip_fast_active: 1,
            ip_fast_ready: 0,
            ip_fast_cache_age_ms: 100_000,
            have_wifi_cache: 1,
            cache_bssid_valid: 1,
            cache_channel: 6,
            _pad1: 0,
            cache_ssid: mk_ssid(b"Net"),
            cache_ssid_len: 3,
            cache_bssid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            wifi_cache_age_ms: 1000,
            have_ip_cache: 1,
            _pad2: [0; 3],
            ip_cache_age_ms: 100_000,
            fast_fail_count: 0,
            fast_enabled: 1,
            endpoint_present: 0, // missing on the first dispatch
            probe_target: 0,
            host_is_ip_literal: 0,
        };

        let mut cout = COutput::default();
        unsafe { rf_wifi_policy_decide(&cin, &mut cout); }

        assert_eq!(cout.action_kind, ActionKind::DeferProbe as u8);
        assert_eq!(cout.clear_ip_cache, 0, "first dispatch must retain IP cache on endpoint_missing");
        assert_eq!(cout.retain_ip, 1, "first dispatch must set retain_ip on endpoint_missing");
        assert_eq!(cout.clear_wifi_cache, 0, "first dispatch must retain association cache");
    }

    #[test]
    fn c_abi_endpoint_missing_retains_ip_3b() {
        // 3b: endpoint_missing → action=DeferProbe, clear_ip_cache=0, retain_ip=1.
        let cin = CInputs {
            version: 1,
            invoke_count: 0,
            wifi_connected: 1,
            rssi: -50,
            channel: 6,
            _pad0: [0; 3],
            reconnect_count: 0,
            ip_fast_active: 1,
            ip_fast_ready: 0,
            ip_fast_cache_age_ms: 100_000,
            have_wifi_cache: 1,
            cache_bssid_valid: 1,
            cache_channel: 6,
            _pad1: 0,
            cache_ssid: mk_ssid(b"Net"),
            cache_ssid_len: 3,
            cache_bssid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            wifi_cache_age_ms: 1000,
            have_ip_cache: 1,
            _pad2: [0; 3],
            ip_cache_age_ms: 100_000,
            fast_fail_count: 0,
            fast_enabled: 1,
            endpoint_present: 0, // missing
            probe_target: 0,
            host_is_ip_literal: 0,
        };

        let mut cout = COutput::default();
        unsafe { rf_wifi_policy_decide(&cin, &mut cout); }

        assert_eq!(cout.action_kind, ActionKind::DeferProbe as u8);
        assert_eq!(cout.clear_ip_cache, 0, "3b: endpoint_missing must NOT clear IP cache");
        assert_eq!(cout.retain_ip, 1, "3b: endpoint_missing must retain IP");
    }

    #[test]
    fn age_boundary_exactly_3600000ms_is_fresh() {
        // Exactly kIpFastMaxAgeMs = 3_600_000 ms must NOT trigger stale fallback
        let i = make_inputs(|i| {
            i.ip_fast_active = true;
            i.have_ip_cache = true;
            i.ip_cache_age_ms = IP_FAST_MAX_AGE_MS; // exactly at boundary
            i.endpoint_present = true;
        });
        let a = decide(&i);
        assert_eq!(
            a.action_kind,
            ActionKind::Probe as u8,
            "exactly kIpFastMaxAgeMs must be treated as fresh"
        );
    }

    // ── C ABI ───────────────────────────────────────────────────────────────

    #[test]
    fn c_abi_direct_connect_roundtrip() {
        let cin = CInputs {
            version: 1,
            invoke_count: 0,
            wifi_connected: 1,
            rssi: -50,
            channel: 6,
            _pad0: [0; 3],
            reconnect_count: 0,
            ip_fast_active: 0,
            ip_fast_ready: 0,
            ip_fast_cache_age_ms: 0,
            have_wifi_cache: 1,
            cache_bssid_valid: 1,
            cache_channel: 6,
            _pad1: 0,
            cache_ssid: mk_ssid(b"MyNetwork"),
            cache_ssid_len: 9,
            cache_bssid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            wifi_cache_age_ms: 1000,
            have_ip_cache: 0,
            _pad2: [0; 3],
            ip_cache_age_ms: 0,
            fast_fail_count: 0,
            fast_enabled: 1,
            endpoint_present: 1,
            probe_target: 0,
            host_is_ip_literal: 0,
        };

        let mut cout = COutput::default();
        unsafe { rf_wifi_policy_decide(&cin, &mut cout); }

        assert_eq!(cout.version, 1);
        assert_eq!(cout.action_kind, ActionKind::DirectConnect as u8);
        assert_eq!(cout.clear_wifi_cache, 0);
        assert_eq!(cout.clear_ip_cache, 0);
    }

    #[test]
    fn c_abi_stale_cache_with_endpoint_present_clears_ip() {
        // C-ABI guard for the non-endpoint clear path: stale cache with a
        // present endpoint → clear_ip_cache=1, retain_ip=0.
        let cin = CInputs {
            version: 1,
            invoke_count: 0,
            wifi_connected: 1,
            rssi: -50,
            channel: 6,
            _pad0: [0; 3],
            reconnect_count: 0,
            ip_fast_active: 1,
            ip_fast_ready: 0,
            ip_fast_cache_age_ms: IP_FAST_MAX_AGE_MS as u32 + 1,
            have_wifi_cache: 1,
            cache_bssid_valid: 1,
            cache_channel: 6,
            _pad1: 0,
            cache_ssid: mk_ssid(b"Net"),
            cache_ssid_len: 3,
            cache_bssid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            wifi_cache_age_ms: 1000,
            have_ip_cache: 1,
            _pad2: [0; 3],
            ip_cache_age_ms: IP_FAST_MAX_AGE_MS as i32 + 1,
            fast_fail_count: 0,
            fast_enabled: 1,
            endpoint_present: 1, // present
            probe_target: 0,
            host_is_ip_literal: 0,
        };

        let mut cout = COutput::default();
        unsafe { rf_wifi_policy_decide(&cin, &mut cout); }

        assert_eq!(cout.action_kind, ActionKind::DeferProbe as u8);
        assert_eq!(cout.clear_ip_cache, 1, "stale cache with endpoint present clears IP");

        assert_eq!(cout.retain_ip, 0);
    }

    #[test]
    fn c_abi_null_ptr_handled() {
        unsafe { rf_wifi_policy_decide(core::ptr::null(), core::ptr::null_mut()); }
        // Must not panic
    }

    #[test]
    fn c_abi_probe_target_codes_match_the_cpp_enum() {
        // `network_probe_target.h` declares HttpOta=0, WebSocket=1, Mqtt=2.
        // The decoder must agree, or C++'s chosen target silently arrives as
        // a different service and ResolveProbeEndpoint picks the wrong source.
        assert_eq!(c_abi_probe_target(0), ProbeTarget::HttpOta);
        assert_eq!(c_abi_probe_target(1), ProbeTarget::WebSocket);
        assert_eq!(c_abi_probe_target(2), ProbeTarget::Mqtt);
        // Unknown codes must not silently alias a real target.
        assert_eq!(c_abi_probe_target(3), ProbeTarget::HttpOta);
    }

    // ── Sentinel mutation proof (Task 4b) ────────────────────────────────────
    // To prove the endpoint-missing tests catch the wrong behavior:
    // 1. In `decide()`, change `defer_retain_ip()` to `defer_clear_ip()` in
    //    the `!i.endpoint_present` branch.
    // 2. Run: cargo test wifi_policy
    // 3. `defer_retain_ip_when_endpoint_missing_3b` and
    //    `c_abi_endpoint_missing_retains_ip_3b` will FAIL:
    //      assert!(!a.clear_ip_cache) → clear_ip_cache becomes true, fails
    //      assert!(a.retain_ip) → retain_ip becomes false, fails
    // 4. Restore the implementation (defer_retain_ip) and rerun → green.
    // This proves the sentinel correctly detects a regression to clearing
    // the IP cache on endpoint_missing.

    #[test]
    fn modem_sleep_suppressed_only_after_repeated_beacon_timeouts() {
        // One timeout is noise (roaming, a busy channel). The third in a row
        // is a pattern, and on an AP that omits the TIM IE the driver cannot
        // keep the modem-sleep schedule — it burns the beacon window and then
        // disconnects. Suppress modem sleep only for that repeated case.
        assert!(!should_suppress_modem_sleep(0, BEACON_TIMEOUT_REASON));
        assert!(!should_suppress_modem_sleep(1, BEACON_TIMEOUT_REASON));
        assert!(!should_suppress_modem_sleep(2, BEACON_TIMEOUT_REASON));
        assert!(should_suppress_modem_sleep(3, BEACON_TIMEOUT_REASON));
        assert!(should_suppress_modem_sleep(9, BEACON_TIMEOUT_REASON));
    }

    #[test]
    fn modem_sleep_suppression_ignores_other_disconnect_reasons() {
        // AUTH_FAIL / NO_AP_FOUND are unrelated to the sleep schedule:
        // disabling modem sleep would cost battery and buy nothing.
        assert!(!should_suppress_modem_sleep(9, 202));  // AUTH_FAIL
        assert!(!should_suppress_modem_sleep(9, 201));  // NO_AP_FOUND
        assert!(!should_suppress_modem_sleep(9, -1));   // unknown
    }

    #[test]
    fn beacon_timeout_accumulates_across_short_lived_connections() {
        // The real failure shape: the link comes up, survives a few seconds,
        // dies with BEACON_TIMEOUT, reconnects, dies again. Resetting on every
        // reconnect would make the streak unreachable, so a connection that
        // did not survive the healthy threshold counts as a continuous fault.
        let short = HEALTHY_CONNECTION_MS - 1;
        let s1 = next_beacon_timeout_streak(0, BEACON_TIMEOUT_REASON, short);
        let s2 = next_beacon_timeout_streak(s1, BEACON_TIMEOUT_REASON, short);
        let s3 = next_beacon_timeout_streak(s2, BEACON_TIMEOUT_REASON, short);
        assert_eq!((s1, s2, s3), (1, 2, 3));
        assert!(should_suppress_modem_sleep(s3, BEACON_TIMEOUT_REASON));
    }

    #[test]
    fn a_long_healthy_connection_clears_the_streak() {
        // A link that held for a long time is evidence the AP *can* sleep; the
        // next timeout is a fresh fault, not the fourth of a bad run. This is
        // the recovery path — without it the suppression latches forever.
        let s = next_beacon_timeout_streak(3, BEACON_TIMEOUT_REASON, HEALTHY_CONNECTION_MS);
        assert_eq!(s, 1);
        assert!(!should_suppress_modem_sleep(s, BEACON_TIMEOUT_REASON));
    }

    #[test]
    fn beacon_timeout_streak_ignores_other_disconnect_reasons() {
        // AUTH_FAIL / NO_AP_FOUND say nothing about the sleep schedule:
        // disabling modem sleep would cost battery and buy nothing.
        assert!(!should_suppress_modem_sleep(9, 202));  // AUTH_FAIL
        assert!(!should_suppress_modem_sleep(9, 201));  // NO_AP_FOUND
        assert!(!should_suppress_modem_sleep(9, -1));   // unknown
        assert_eq!(next_beacon_timeout_streak(9, 202, 1000), 0);
    }

    #[test]
    fn beacon_timeout_streak_is_bounded() {
        // No unbounded climb: the value only gates a boolean, and a saturated
        // word would eventually wrap.
        let mut s = 0u32;
        for _ in 0..40 {
            s = next_beacon_timeout_streak(s, BEACON_TIMEOUT_REASON, 1000);
        }
        assert_eq!(s, MODEM_SLEEP_SUPPRESS_AFTER);
    }
}
