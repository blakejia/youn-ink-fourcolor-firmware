//! Red-first integration tests for the Task 3 notify pull/rate policy.
//!
//! They pin the migrated decision table against `notify.rs`' observed
//! behaviour plus the new rate/backoff/dedup gates from the ABCDE design:
//!
//! - idle pull: IDLE + quiet panel + no recent pull/failure -> Pull
//! - EPD-busy defer: panel refresh in flight -> DeferBusy (caller re-arms)
//! - duplicate id/etag skip: re-offered id -> SkipDuplicate, never re-shown
//! - success consume: 200 + parsed body + fresh id -> ShowConsume, streak reset
//! - failure backoff: transport/status failure -> streak+1, capped backoff
//! - classification: 200 .bin vs 200 JSON vs 204 vs 404-fallback vs errors
//!
//! TDD: this file is written BEFORE `notify_policy.rs` exists, so the first
//! `cargo test` must fail (missing module). Minimal implementation follows.

use rust_firmware::notify_policy::*;

fn pull(
    state: u8,
    busy: bool,
    now: i64,
    last_pull: i64,
    last_fail: i64,
    streak: u32,
) -> PullOutput {
    decide_pull(&PullInputs {
        state,
        epd_busy: busy,
        _pad: [0; 6],
        now_s: now,
        last_pull_s: last_pull,
        last_failure_s: last_fail,
        fail_streak: streak,
        min_interval_s: 5,
        base_backoff_s: 30,
        max_backoff_s: 900,
    })
}

// ── pull gate ─────────────────────────────────────────────────────────────

#[test]
fn idle_quiet_panel_with_no_history_pulls() {
    let out = pull(NOTIFY_STATE_IDLE, false, 1000, -1, -1, 0);
    assert_eq!(out.action, PULL_ACTION_PULL);
}

#[test]
fn epd_busy_defers_the_pull() {
    // A refresh wave is in flight (15-25 s): the GET + show would fight the
    // panel, so defer and let the caller re-arm instead of pulling now.
    let out = pull(NOTIFY_STATE_IDLE, true, 1000, -1, -1, 0);
    assert_eq!(out.action, PULL_ACTION_DEFER_BUSY);
}

#[test]
fn non_idle_state_never_pulls() {
    // Mirrors `request_next`'s `state != IDLE -> return` guard.
    assert_eq!(
        pull(NOTIFY_STATE_FETCHING, false, 1000, -1, -1, 0).action,
        PULL_ACTION_SKIP_NOT_IDLE
    );
    assert_eq!(
        pull(NOTIFY_STATE_NOTIFYING, false, 1000, -1, -1, 0).action,
        PULL_ACTION_SKIP_NOT_IDLE
    );
}

#[test]
fn unset_clock_disables_time_gates_but_keeps_state_and_busy_gates() {
    // Cold boot before first sync: `now_s < 0` must not wedge the pull
    // behind a wall-clock gate, matching today's unconditional fetch.
    let out = pull(NOTIFY_STATE_IDLE, false, -1, 1000, 1000, 7);
    assert_eq!(out.action, PULL_ACTION_PULL);
    // Busy still defers even without a clock.
    let out = pull(NOTIFY_STATE_IDLE, true, -1, -1, -1, 0);
    assert_eq!(out.action, PULL_ACTION_DEFER_BUSY);
}

#[test]
fn recent_pull_is_rate_limited() {
    let out = pull(NOTIFY_STATE_IDLE, false, 1000, 998, -1, 0);
    assert_eq!(out.action, PULL_ACTION_BACKOFF);
    assert_eq!(out.wait_s, 3);
}

// ── failure backoff ───────────────────────────────────────────────────────

#[test]
fn failure_increments_streak_and_success_resets_it() {
    assert_eq!(record_result(false, 0), 1);
    assert_eq!(record_result(false, 2), 3);
    assert_eq!(record_result(true, 3), 0);
}

#[test]
fn backoff_delay_doubles_per_streak_and_caps() {
    assert_eq!(backoff_delay_s(0, 30, 900), 0);
    assert_eq!(backoff_delay_s(1, 30, 900), 30);
    assert_eq!(backoff_delay_s(2, 30, 900), 60);
    assert_eq!(backoff_delay_s(5, 30, 900), 480);
    // 30 << 5 = 960 > 900: capped, never grows past max.
    assert_eq!(backoff_delay_s(6, 30, 900), 900);
    assert_eq!(backoff_delay_s(40, 30, 900), 900);
}

#[test]
fn pull_inside_the_backoff_window_waits_the_remainder() {
    // streak 2 -> 60 s window from last failure at t=1000.
    let out = pull(NOTIFY_STATE_IDLE, false, 1030, -1, 1000, 2);
    assert_eq!(out.action, PULL_ACTION_BACKOFF);
    assert_eq!(out.wait_s, 30);
}

#[test]
fn pull_after_the_backoff_window_is_allowed() {
    let out = pull(NOTIFY_STATE_IDLE, false, 1070, -1, 1000, 2);
    assert_eq!(out.action, PULL_ACTION_PULL);
}

// ── response classification (JSON vs .bin, transport/status) ─────────────

#[test]
fn ok_on_the_binary_path_is_a_binary_notification() {
    assert_eq!(
        classify_response(&ResponseFacts {
            status: 200,
            binary_path: true,
            _pad: [0; 3],
        }),
        RESPONSE_BINARY_NOTIFY
    );
}

#[test]
fn ok_on_the_json_path_is_a_json_notification() {
    assert_eq!(
        classify_response(&ResponseFacts {
            status: 200,
            binary_path: false,
            _pad: [0; 3],
        }),
        RESPONSE_JSON_NOTIFY
    );
}

#[test]
fn no_content_means_empty_on_both_paths() {
    for binary_path in [true, false] {
        assert_eq!(
            classify_response(&ResponseFacts {
                status: 204,
                binary_path,
                _pad: [0; 3],
            }),
            RESPONSE_EMPTY,
            "binary_path={binary_path}"
        );
    }
}

#[test]
fn missing_binary_endpoint_falls_back_to_json() {
    // Old server without /next.bin: 404 keeps working via the JSON endpoint
    // (rolling deploy). A 404 on the JSON path itself is a plain error.
    assert_eq!(
        classify_response(&ResponseFacts {
            status: 404,
            binary_path: true,
            _pad: [0; 3],
        }),
        RESPONSE_JSON_FALLBACK
    );
    assert_eq!(
        classify_response(&ResponseFacts {
            status: 404,
            binary_path: false,
            _pad: [0; 3],
        }),
        RESPONSE_STATUS_ERROR
    );
}

#[test]
fn transport_and_status_errors_classify_as_errors() {
    assert_eq!(
        classify_response(&ResponseFacts {
            status: -1,
            binary_path: true,
            _pad: [0; 3],
        }),
        RESPONSE_TRANSPORT_ERROR
    );
    assert_eq!(
        classify_response(&ResponseFacts {
            status: 500,
            binary_path: true,
            _pad: [0; 3],
        }),
        RESPONSE_STATUS_ERROR
    );
}

// ── consume decisions ─────────────────────────────────────────────────────

#[test]
fn fresh_notification_with_parsed_body_is_consumed() {
    // Success consume: both wire formats, fresh id.
    for class in [RESPONSE_BINARY_NOTIFY, RESPONSE_JSON_NOTIFY] {
        assert_eq!(
            decide_consume(class, true, false),
            CONSUME_SHOW,
            "class={class}"
        );
    }
}

#[test]
fn duplicate_id_or_etag_is_skipped_never_reshown() {
    // The server re-offered the already-consumed id (ack race): skip on
    // both wire formats instead of showing it again.
    for class in [RESPONSE_BINARY_NOTIFY, RESPONSE_JSON_NOTIFY] {
        assert_eq!(
            decide_consume(class, true, true),
            CONSUME_SKIP_DUPLICATE,
            "class={class}"
        );
    }
}

#[test]
fn unparseable_body_is_a_retryable_failure() {
    for class in [RESPONSE_BINARY_NOTIFY, RESPONSE_JSON_NOTIFY] {
        assert_eq!(
            decide_consume(class, false, false),
            CONSUME_RETRY_BACKOFF,
            "class={class}"
        );
    }
}

#[test]
fn empty_and_error_classes_map_to_consume_actions() {
    assert_eq!(
        decide_consume(RESPONSE_EMPTY, false, false),
        CONSUME_NONE_EMPTY
    );
    assert_eq!(
        decide_consume(RESPONSE_TRANSPORT_ERROR, false, false),
        CONSUME_RETRY_BACKOFF
    );
    assert_eq!(
        decide_consume(RESPONSE_STATUS_ERROR, false, false),
        CONSUME_RETRY_BACKOFF
    );
    assert_eq!(
        decide_consume(RESPONSE_JSON_FALLBACK, false, false),
        CONSUME_FETCH_JSON_FALLBACK
    );
}

// ── C ABI ─────────────────────────────────────────────────────────────────

#[test]
fn c_abi_returns_the_same_decisions() {
    let inp = PullInputs {
        state: NOTIFY_STATE_IDLE,
        epd_busy: false,
        _pad: [0; 6],
        now_s: 1000,
        last_pull_s: -1,
        last_failure_s: -1,
        fail_streak: 0,
        min_interval_s: 5,
        base_backoff_s: 30,
        max_backoff_s: 900,
    };
    let mut out = PullOutput {
        action: 0xFF,
        _pad: [0; 3],
        wait_s: 0xFFFF_FFFF,
    };
    // SAFETY: both pointers are to locals that outlive the call.
    unsafe { rf_notify_pull_decide(&inp, &mut out) };
    assert_eq!(out.action, PULL_ACTION_PULL);

    let facts = ResponseFacts {
        status: 200,
        binary_path: true,
        _pad: [0; 3],
    };
    // SAFETY: pointer is to a local that outlives the call.
    assert_eq!(unsafe { rf_notify_classify_response(&facts) }, RESPONSE_BINARY_NOTIFY);
    assert_eq!(
        rf_notify_decide_consume(RESPONSE_JSON_NOTIFY, 1, 0),
        CONSUME_SHOW
    );
    assert_eq!(rf_notify_backoff_delay_s(2, 30, 900), 60);
    assert_eq!(rf_notify_record_result(0, 2), 3);
    assert_eq!(rf_notify_record_result(1, 2), 0);
}

#[test]
fn c_structs_match_the_header_layout() {
    // Layout contract with rust/include/notify_policy.h: field order, offsets
    // and sizes must agree or the FFI reads garbage. Same style as the
    // pairing_response layout test.
    use core::mem::{offset_of, size_of};
    assert_eq!(size_of::<PullInputs>(), 48);
    assert_eq!(offset_of!(PullInputs, state), 0);
    assert_eq!(offset_of!(PullInputs, epd_busy), 1);
    assert_eq!(offset_of!(PullInputs, now_s), 8);
    assert_eq!(offset_of!(PullInputs, last_pull_s), 16);
    assert_eq!(offset_of!(PullInputs, last_failure_s), 24);
    assert_eq!(offset_of!(PullInputs, fail_streak), 32);
    assert_eq!(offset_of!(PullInputs, min_interval_s), 36);
    assert_eq!(offset_of!(PullInputs, base_backoff_s), 40);
    assert_eq!(offset_of!(PullInputs, max_backoff_s), 44);

    assert_eq!(size_of::<PullOutput>(), 8);
    assert_eq!(offset_of!(PullOutput, action), 0);
    assert_eq!(offset_of!(PullOutput, wait_s), 4);

    assert_eq!(size_of::<ResponseFacts>(), 8);
    assert_eq!(offset_of!(ResponseFacts, status), 0);
    assert_eq!(offset_of!(ResponseFacts, binary_path), 4);
}
