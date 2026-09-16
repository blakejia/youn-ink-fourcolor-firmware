# Task 4 Report: http_wrapper_post_json_with_headers

**Status:** ✅ COMPLETE

**Commit:** `8a7227a9aa91a0b5f0e31339b4d580fc80112790`

## Changes

### `firmware/main/common/http_client_wrapper.h`
- Added `http_header_t` typedef struct with `const char *key` and `const char *value`
- Added declaration for `http_wrapper_post_json_with_headers`

### `firmware/main/common/http_client_wrapper.cc`
- Implemented `http_wrapper_post_json_with_headers` following the same pattern as `http_wrapper_post_json`
- Sets `Authorization: Bearer <token>` if token is provided
- Sets `Content-Type: application/json`
- Iterates `extra_headers` array and calls `esp_http_client_set_header(client, key, value)` for each
- Returns HTTP status code or -1 on error

## Build Output

```
[10/12] Linking CXX executable xiaozhi.elf
[11/12] Generating binary image from built executable
...
Project build complete. To flash, run:
  idf.py flash
...
xiaozhi.bin binary size 0x2b1bc0 bytes. Smallest app partition is 0x3f0000 bytes. 0x13e440 bytes (32%) free.
Wall time: 59.80 seconds
```

**Build: SUCCESS** — 0 errors, 0 undefined references.

## Function Signature (as implemented)

```c
typedef struct {
    const char *key;
    const char *value;
} http_header_t;

int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const http_header_t *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms);
```

## Concerns

None. The implementation mirrors the existing `http_wrapper_post_json` exactly with the extra headers loop added. No breaking changes to existing functions.
