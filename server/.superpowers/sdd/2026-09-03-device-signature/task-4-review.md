# Task 4 Review: http_wrapper_post_json_with_headers

**Reviewer:** Task4Reviewer (cpp-reviewer)
**Files reviewed:** `firmware/main/common/http_client_wrapper.h`, `firmware/main/common/http_client_wrapper.cc`
**Spec source:** `docs/superpowers/plans/2026-09-03-device-signature.md` Task 4 (lines 472–678)

---

## Spec Compliance

| Requirement | Status |
|---|---|
| `http_header_t` typedef: `{ const char *key; const char *value; }` | ✅ |
| `http_wrapper_post_json_with_headers` declaration in header | ✅ |
| Function signature matches plan (url, token, json_body, extra_headers, extra_count, out_buf, out_len, timeout_ms) | ✅ |
| Sets `Content-Type: application/json` | ✅ `http_client_wrapper.cc:189` |
| Sets `Authorization: Bearer <token>` (via `set_bearer_header`) | ✅ `http_client_wrapper.cc:188` |
| Iterates `extra_headers` array calling `esp_http_client_set_header(client, key, value)` | ✅ `http_client_wrapper.cc:191-196` |
| Returns HTTP status code or -1 on error | ✅ `http_client_wrapper.cc:207,210` |
| Build: 0 undefined references | ✅ reported build success |
| No regression to existing `http_wrapper_get` / `http_wrapper_post_json` | ✅ identical structure preserved |

**Spec compliance: ✅ PASS**

---

## Strengths

1. **Minimal, clean diff** — New function adds only ~58 lines; shares the full structure with `http_wrapper_post_json` so maintenance surface is obvious.
2. **Correct API usage** — `esp_http_client_set_header(client, key, value)` called with the struct's separate key/value fields, exactly as ESP-IDF expects.
3. **Parameter validation** — Guard at line 165 catches NULL/misconfigured callers before any side effects.
4. **No dynamic allocation** — The extra headers are passed directly to `esp_http_client_set_header` in the loop; no malloc/free dance needed. Clean and stack-safe.
5. **Graceful token handling** — Bearer auth only added when `token != NULL` via the existing `set_bearer_header` helper, matching the established pattern.
6. **Reuses existing event handler** — `HttpRespCtx` + `http_event_handler` already accumulate the response body; no duplication.
7. **Consistent null-termination** — `out_buf[ctx.len] = '\0'` at line 211 matches the sibling function's approach (line 152). No discrepancy.

---

## Issues

**None.**

### Minor / Informational

- **(Informational, not a bug)** The spec's pseudocode (Step 7, lines 656–657) suggested using `esp_http_client_get_content_length` + `esp_http_client_read_response`. The implementation uses the event-handler accumulation pattern (`ctx.len`) instead — same behavior, already proven correct in `http_wrapper_post_json`. This is the right call; the spec pseudocode was illustrative, not prescriptive.

---

## Verdict

**✅ APPROVE**

The implementation is correct, minimal, and fully spec-compliant. The `http_header_t` typedef matches the plan verbatim, header-setting calls use the correct ESP-IDF key/value split, Bearer auth is handled by the existing helper, and there are zero regression risks to `http_wrapper_get` or `http_wrapper_post_json`. Build is clean with 0 undefined references.
