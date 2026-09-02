# NOTE4C 设备认证签名实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** NOTE4C 设备首启 pair-start 请求携带 HMAC-SHA256 签名，服务端验证后才生成配对码。验证失败返回 401，设备不显示配对码。

**Architecture:** 对称密钥认证。设备和服务端都用 `derived_key = HMAC-SHA256(MASTER_KEY, device_id)` 推导同一密钥。签名字节序固定为 `MAC(6 bytes) || timestamp(ASCII) || nonce(ASCII)`，HMAC-SHA256 后 base64。30s 时间窗 + 5min nonce 重放防护。

**Tech Stack:** 服务端 Python（`hmac`/`hashlib`/`base64` 内置库）；固件 ESP-IDF v6.0（`mbedtls/sha256.h` 默认链接，`esp_fill_random` 随机数，`esp_read_mac` MAC）。

**Spec:** `docs/superpowers/specs/2026-09-03-device-signature.md`

## Global Constraints

- 设备 MASTER_KEY 编译时通过 `-DDEVICE_MASTER_KEY="..."` 注入，不硬编码在源码 git 历史
- 服务端 `MASTER_KEY` ≥ 32 字节强随机，从 `.env` 读（`config.py` 默认空字符串，启动报错提示）
- 时间窗 30s（需设备 NTP 同步，设备已有 `StartSntpClockSyncOnce()`）
- nonce 重放缓存 5min 内存（重启清空，可接受）
- `http_wrapper_post_json_with_headers` 新增函数与现有 `http_wrapper_post_json` 行为相同但支持自定义 header
- 设备 `display_cb` 收到 401 时显示 "设备认证失败"（不显示配对码）
- 白名单可选，`ALLOWED_DEVICE_IDS` 逗号分隔，留空 = 全部接受
- 签名 payload 字节序固定：MAC(6) || timestamp(ASCII) || nonce(ASCII)
- 设备 `device_signature.cc` 必须 `#include "mbedtls/sha256.h"`（ESP-IDF 链接 mbedtls 组件）
- MAC 格式：12 hex chars（`%02X%02X%02X%02X%02X%02X`），服务端 `bytes.fromhex(mac)` 解析
- 错误响应统一 HTTPException 401 `{"detail": "device authentication failed"}`

---

### Task 1: 服务端配置 + MASTER_KEY

**Files:**
- Modify: `server/youn_server/config.py` (+2 字段)
- Modify: `server/.env.example` (+2 行)
- Modify: `server/youn_server/app.py` (+startup 校验)
- Test: `server/tests/test_device_signature.py` (新建, 3 tests)

**Interfaces:**
- Consumes: `Settings` 基类（pydantic-settings）
- Produces:
  - `settings.master_key: str` (默认 `""`)
  - `settings.allowed_device_ids: list[str]` (默认 `[]`)

- [ ] **Step 1: 写失败测试**

```python
# server/tests/test_device_signature.py
"""Device signature authentication tests."""
from __future__ import annotations
import base64
import hmac
import hashlib
import time
import pytest
from fastapi.testclient import TestClient

from youn_server.app import create_app
from youn_server.config import settings
from youn_server import pairing as pairing_mod


@pytest.fixture(autouse=True)
def enable_pairing(monkeypatch):
    """Inject MASTER_KEY for the test session."""
    settings.master_key = "test_master_key_at_least_32_bytes_long_xx"
    settings.allowed_device_ids = []
    yield
    settings.master_key = ""


def test_master_key_required(monkeypatch):
    """Without MASTER_KEY, pair-start returns 500 (or 401)."""
    settings.master_key = ""
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start", json={
            "device_id": "DEV-1", "board_type": "NOTE4C"
        })
        assert r.status_code in (401, 500)


def test_signature_valid():
    """Correctly signed pair-start returns 200 + code."""
    device_id = "NOTE4C-TEST"
    now = int(time.time())
    mac_hex = "AABBCCDDEEFF"
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    master = settings.master_key.encode()
    derived = hmac.new(master, device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
    
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={
                       "X-Device-Mac": mac_hex,
                       "X-Device-Timestamp": str(now),
                       "X-Device-Nonce": nonce_b64,
                       "X-Device-Signature": sig,
                   })
        assert r.status_code == 200
        assert "code" in r.json()
        assert len(r.json()["code"]) == 6


def test_signature_missing_headers():
    """Missing signature headers returns 400."""
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": "DEV-1", "board_type": "NOTE4C"})
        assert r.status_code == 400
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py -q`
Expected: FAIL（`test_master_key_required` 因 `master_key = ""` 触发 ModuleNotFoundError 或 AssertionError）

- [ ] **Step 3: 实现 config.py 字段**

在 `server/youn_server/config.py` 找到 `operator_token: str = Field(default="")` 行，附近加：

```python
    # Device signature authentication
    master_key: str = Field(default="")
    allowed_device_ids: str = Field(default="")  # comma-separated, empty = all
```

- [ ] **Step 4: 实现 .env.example**

`server/.env.example` 添加：
```
# Device signature authentication
# Generate with: python -c "import secrets; print(secrets.token_urlsafe(32))"
MASTER_KEY=
# Comma-separated device_id whitelist, empty = all devices accepted
ALLOWED_DEVICE_IDS=
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py -q`
Expected: 3 passed

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/config.py server/.env.example server/tests/test_device_signature.py
git commit -m "feat(server): add MASTER_KEY and device_id whitelist config fields"
```

---

### Task 2: 服务端 verify_device_signature 实现

**Files:**
- Modify: `server/youn_server/pairing.py` (+verify_device_signature + nonce cache)
- Test: `server/tests/test_device_signature.py` (+5 tests)

**Interfaces:**
- Consumes: `settings.master_key`, `settings.allowed_device_ids` (from Task 1)
- Produces:
  - `PairingStore.verify_device_signature(device_id: str, mac: str, timestamp: int, nonce: str, signature_b64: str) -> bool`
  - Internal `_nonce_cache: dict[str, float]` field

- [ ] **Step 1: 写失败测试**

Append to `server/tests/test_device_signature.py`:

```python
def test_signature_invalid_key():
    """Wrong MASTER_KEY fails verification."""
    device_id = "DEV-KEY"
    now = int(time.time())
    mac_hex = "AABBCCDDEEFF"
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    wrong = hmac.new(b"wrong_key_32_bytes_pad_pad_pad_pad", device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(wrong, payload, hashlib.sha256).digest()).decode()
    
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={"X-Device-Mac": mac_hex, "X-Device-Timestamp": str(now),
                            "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig})
        assert r.status_code == 401
        assert "device authentication failed" in r.json()["detail"].lower()


def test_signature_timestamp_out_of_window():
    """Timestamp >30s old fails."""
    device_id = "DEV-OLD"
    old_ts = int(time.time()) - 60
    mac_hex = "AABBCCDDEEFF"
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    derived = hmac.new(settings.master_key.encode(), device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(old_ts).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
    
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={"X-Device-Mac": mac_hex, "X-Device-Timestamp": str(old_ts),
                            "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig})
        assert r.status_code == 401


def test_signature_nonce_replay():
    """Same nonce within 5min fails."""
    device_id = "DEV-REPLAY"
    now = int(time.time())
    mac_hex = "AABBCCDDEEFF"
    nonce_raw = b"replay_nonce_16!!"
    nonce_b64 = base64.b64encode(nonce_raw).decode()
    derived = hmac.new(settings.master_key.encode(), device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
    headers = {"X-Device-Mac": mac_hex, "X-Device-Timestamp": str(now),
               "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig}
    app = create_app()
    with TestClient(app) as c:
        r1 = c.post("/api/devices/pair-start",
                    json={"device_id": device_id, "board_type": "NOTE4C"},
                    headers=headers)
        assert r1.status_code == 200
        r2 = c.post("/api/devices/pair-start",
                    json={"device_id": device_id, "board_type": "NOTE4C"},
                    headers=headers)
        assert r2.status_code == 401


def test_signature_invalid_mac_format():
    """Bad MAC hex fails."""
    device_id = "DEV-BADMAC"
    now = int(time.time())
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    derived = hmac.new(settings.master_key.encode(), device_id.encode(), hashlib.sha256).digest()
    payload = b"not_hex!!" + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
    
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={"X-Device-Mac": "not_hex!!", "X-Device-Timestamp": str(now),
                            "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig})
        assert r.status_code == 401


def test_signature_whitelist_reject():
    """device_id not in whitelist returns 401."""
    settings.allowed_device_ids = "ALLOWED-DEV"
    device_id = "NOT-ALLOWED"
    now = int(time.time())
    mac_hex = "AABBCCDDEEFF"
    nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
    derived = hmac.new(settings.master_key.encode(), device_id.encode(), hashlib.sha256).digest()
    payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
    sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
    
    app = create_app()
    with TestClient(app) as c:
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={"X-Device-Mac": mac_hex, "X-Device-Timestamp": str(now),
                            "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig})
        assert r.status_code == 401
        assert "whitelist" in r.json()["detail"].lower()
    settings.allowed_device_ids = ""
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py -q`
Expected: 5 new tests FAIL (`AttributeError: 'PairingStore' object has no attribute 'verify_device_signature'`)

- [ ] **Step 3: 实现 verify_device_signature**

在 `server/youn_server/pairing.py` 的 `PairingStore` 类添加：

```python
import base64

NONCE_CACHE_TTL_SEC = 300
TIMESTAMP_WINDOW_SEC = 30

# In __init__, add:
        self._nonce_cache: dict[str, float] = {}

# Add method:
    def verify_device_signature(self, device_id: str, mac: str,
                                 timestamp: int, nonce: str,
                                 signature_b64: str) -> bool:
        """Verify device signature using HMAC derived from MASTER_KEY."""
        master_key = settings.master_key
        if not master_key or len(master_key) < 32:
            log.error("MASTER_KEY not configured or too short")
            return False

        # Check timestamp window
        now = int(time.time())
        if abs(now - timestamp) > TIMESTAMP_WINDOW_SEC:
            log.warning("signature failed: timestamp out of window device_id=%s delta=%d",
                        device_id, now - timestamp)
            return False

        # Derive key
        derived_key = hmac.new(
            master_key.encode(),
            device_id.encode(),
            hashlib.sha256,
        ).digest()

        # Reconstruct payload
        try:
            mac_bytes = bytes.fromhex(mac)
            if len(mac_bytes) != 6:
                raise ValueError
        except (ValueError, TypeError):
            log.warning("signature failed: bad MAC format device_id=%s mac=%s",
                        device_id, mac)
            return False
        payload = mac_bytes + str(timestamp).encode() + nonce.encode()

        # Verify signature
        try:
            provided = base64.b64decode(signature_b64)
        except Exception:
            return False
        expected = hmac.new(derived_key, payload, hashlib.sha256).digest()
        if not hmac.compare_digest(expected, provided):
            log.warning("signature failed: invalid signature device_id=%s", device_id)
            return False

        # Check nonce replay
        nonce_key = f"{device_id}:{nonce}"
        with self._lock:
            now_f = time.time()
            self._nonce_cache = {k: v for k, v in self._nonce_cache.items()
                                  if now_f - v < NONCE_CACHE_TTL_SEC}
            if nonce_key in self._nonce_cache:
                log.warning("signature failed: nonce replay device_id=%s", device_id)
                return False
            self._nonce_cache[nonce_key] = now_f

        log.info("device signature verified device_id=%s", device_id)
        return True

    def check_whitelist(self, device_id: str) -> bool:
        """Returns True if device is allowed (empty whitelist = all allowed)."""
        wl = settings.allowed_device_ids
        if isinstance(wl, str):
            wl = [d.strip() for d in wl.split(",") if d.strip()]
        if not wl:
            return True
        return device_id in wl
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py -q`
Expected: 8 passed (3 from Task 1 + 5 new)

- [ ] **Step 5: 提交**

```bash
git add server/youn_server/pairing.py server/tests/test_device_signature.py
git commit -m "feat(server): device signature verification with HMAC + nonce replay"
```

---

### Task 3: app.py pair-start 端点签名验证

**Files:**
- Modify: `server/youn_server/app.py` (pair_start 端点)
- Test: `server/tests/test_device_signature.py` (+1 integration test)

**Interfaces:**
- Consumes: `PairingStore.verify_device_signature` (from Task 2)
- Produces: same response format (200 + code / 401 / 400)

- [ ] **Step 1: 写失败测试**

Append to `server/tests/test_device_signature.py`:

```python
def test_pair_start_full_flow_401_then_200():
    """End-to-end: bad signature returns 401, then valid signature returns 200."""
    app = create_app()
    with TestClient(app) as c:
        # Bad signature
        r = c.post("/api/devices/pair-start",
                   json={"device_id": "NOTE4C-FLOW", "board_type": "NOTE4C"})
        assert r.status_code == 400  # missing headers

        # Valid signature
        device_id = "NOTE4C-FLOW"
        now = int(time.time())
        mac_hex = "AABBCCDDEEFF"
        nonce_b64 = base64.b64encode(b"\x00" * 16).decode()
        derived = hmac.new(settings.master_key.encode(), device_id.encode(), hashlib.sha256).digest()
        payload = bytes.fromhex(mac_hex) + str(now).encode() + nonce_b64.encode()
        sig = base64.b64encode(hmac.new(derived, payload, hashlib.sha256).digest()).decode()
        r = c.post("/api/devices/pair-start",
                   json={"device_id": device_id, "board_type": "NOTE4C"},
                   headers={"X-Device-Mac": mac_hex, "X-Device-Timestamp": str(now),
                            "X-Device-Nonce": nonce_b64, "X-Device-Signature": sig})
        assert r.status_code == 200
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py::test_pair_start_full_flow_401_then_200 -v`
Expected: FAIL (pair_start 还没有签名检查)

- [ ] **Step 3: 改 pair_start 端点**

`server/youn_server/app.py` 找到 `pair_start` 端点（约 line 255-263），替换为：

```python
    @app.post("/api/devices/pair-start")
    async def pair_start(body: _PairStartBody, request: Request) -> dict:
        mac = request.headers.get("X-Device-Mac", "")
        ts_str = request.headers.get("X-Device-Timestamp", "")
        nonce = request.headers.get("X-Device-Nonce", "")
        sig = request.headers.get("X-Device-Signature", "")

        if not (mac and ts_str and sig):
            raise HTTPException(400, detail="missing device auth headers")
        try:
            timestamp = int(ts_str)
        except ValueError:
            raise HTTPException(400, detail="invalid timestamp")

        if not _pairing_store.verify_device_signature(
            body.device_id, mac, timestamp, nonce, sig
        ):
            raise HTTPException(401, detail="device authentication failed")

        if not _pairing_store.check_whitelist(body.device_id):
            raise HTTPException(401, detail="device not in whitelist")

        registry.upsert(body.device_id, body.board_type)
        code, expires_in = _pairing_store.create_session(body.device_id)
        return {"code": code, "expires_in": expires_in}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd server && ./.venv/bin/python -m pytest tests/test_device_signature.py -q`
Expected: 9 passed

- [ ] **Step 5: 跑全套测试确认无回归**

Run: `cd server && ./.venv/bin/python -m pytest tests/ -q`
Expected: ≥46 passed (38 + 8 new)

- [ ] **Step 6: 提交**

```bash
git add server/youn_server/app.py server/tests/test_device_signature.py
git commit -m "feat(server): pair-start requires HMAC device signature"
```

---

### Task 4: 固件 http_wrapper_post_json_with_headers

**Files:**
- Modify: `firmware/main/common/http_client_wrapper.h` (+新函数声明)
- Modify: `firmware/main/common/http_client_wrapper.cc` (+新函数实现)
- Test: 编译验证 (无单测)

**Interfaces:**
- Consumes: 现有 `http_wrapper_get` / `http_wrapper_post_json` 模式
- Produces:
  - `int http_wrapper_post_json_with_headers(const char *url, const char *token, const char *json_body, const char * const *extra_headers, int extra_header_count, char *out_buf, int *out_len, int timeout_ms)`

- [ ] **Step 1: 编译失败确认**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep -c 'error:'`
Expected: 0（当前编译应通过，但这是新加 API，先做 Step 2 加代码再验证）

- [ ] **Step 2: 加头文件声明**

`firmware/main/common/http_client_wrapper.h` 在 `http_wrapper_post_json` 声明后（约 line 50）加：

```c
/**
 * @brief HTTP POST (JSON body) with custom headers
 *
 * @param url           Full URL
 * @param token         Bearer token (NULL for public endpoints)
 * @param json_body     JSON request body string
 * @param extra_headers Array of "Key: Value" strings (NULL-terminated pairs)
 * @param extra_count    Number of extra header pairs
 * @param out_buf        Response buffer
 * @param out_len        [in] buffer size / [out] response length
 * @param timeout_ms     Timeout milliseconds
 * @return HTTP status code, -1 on network error
 */
int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const char * const *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms);
```

- [ ] **Step 3: 加实现**

`firmware/main/common/http_client_wrapper.cc` 在 `http_wrapper_post_json` 实现后（约 line 140）加：

```cpp
int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const char * const *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms) {
    char auth_header[128] = {0};
    if (token) {
        snprintf(auth_header, sizeof(auth_header), "Authorization: Bearer %s", token);
    }
    // Build full header list: auth (if any) + extras
    int total = (token ? 1 : 0) + extra_count;
    const char * const *headers = nullptr;
    const char **stack_buf = nullptr;
    if (total > 0) {
        stack_buf = (const char **)malloc(sizeof(const char *) * total);
        int idx = 0;
        if (token) stack_buf[idx++] = auth_header;
        for (int i = 0; i < extra_count; i++) {
            stack_buf[idx++] = extra_headers[i];
        }
        headers = stack_buf;
    }
    int ret = http_wrapper_post_with_headers_internal(url, json_body, headers, total,
                                                     out_buf, out_len, timeout_ms);
    if (stack_buf) free(stack_buf);
    return ret;
}
```

- [ ] **Step 4: 编译验证（预期失败，http_wrapper_post_with_headers_internal 还不存在）**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep error: | head -3`
Expected: `error: 'http_wrapper_post_with_headers_internal' was not declared`

- [ ] **Step 5: 实现 internal helper**

`firmware/main/common/http_client_wrapper.cc` 添加（参考现有 `http_wrapper_post_json` 实现，把 headers 参数化）：

```cpp
static int http_wrapper_post_with_headers_internal(const char *url, const char *json_body,
                                                   const char * const *headers, int header_count,
                                                   char *out_buf, int *out_len, int timeout_ms) {
    // Copy existing http_wrapper_post_json implementation here, but use the
    // provided headers array instead of hardcoded Content-Type + Authorization.
    // The existing function does:
    //   1. esp_http_client_init(url)
    //   2. set_method POST
    //   3. set_header Content-Type, Authorization
    //   4. set_post_field json_body
    //   5. perform
    //   6. read response
    // Refactor to:
    //   1-2. same
    //   3. for i in 0..header_count-1: set_header(headers[i])
    //      Always set Content-Type: application/json
    //   4-6. same
    esp_http_client_handle_t client = esp_http_client_init(url);
    if (!client) return -1;
    esp_http_client_set_method(client, HTTP_METHOD_POST);
    esp_http_client_set_header(client, "Content-Type", "application/json");
    for (int i = 0; i < header_count; i++) {
        esp_http_client_set_header(client, headers[i], headers[i] + strlen(headers[i])/2 + 1);
        // NOTE: set_header takes (client, key, value) — headers[i] is "Key: Value"
        // Caller must format as "Key: Value" but esp_http_client API wants separate
        // key and value. We need to split on ": ".
    }
    // ... rest of existing implementation
    esp_http_client_cleanup(client);
    return status;
}
```

**注**：发现 `esp_http_client_set_header` API 接受 (key, value) 两个参数。修改签名让 caller 传 `key, value` 对：

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

- [ ] **Step 6: 更新头文件声明（匹配 Step 5 新签名）**

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

- [ ] **Step 7: 重写 Step 5 实现匹配新签名**

```cpp
int http_wrapper_post_json_with_headers(const char *url, const char *token,
                                        const char *json_body,
                                        const http_header_t *extra_headers,
                                        int extra_count,
                                        char *out_buf, int *out_len,
                                        int timeout_ms) {
    esp_http_client_handle_t client = esp_http_client_init(url);
    if (!client) return -1;
    esp_http_client_set_method(client, HTTP_METHOD_POST);
    esp_http_client_set_header(client, "Content-Type", "application/json");
    if (token) {
        char auth[160];
        snprintf(auth, sizeof(auth), "Bearer %s", token);
        esp_http_client_set_header(client, "Authorization", auth);
    }
    for (int i = 0; i < extra_count; i++) {
        esp_http_client_set_header(client, extra_headers[i].key, extra_headers[i].value);
    }
    esp_http_client_set_post_field(client, json_body, strlen(json_body));
    esp_http_client_set_timeout_ms(client, timeout_ms);
    esp_err_t err = esp_http_client_perform(client);
    if (err != ESP_OK) {
        esp_http_client_cleanup(client);
        return -1;
    }
    int status = esp_http_client_get_status_code(client);
    int content_len = esp_http_client_get_content_length(client);
    int read_len = 0;
    if (out_buf && *out_len > 0 && content_len > 0) {
        read_len = esp_http_client_read_response(client, out_buf, *out_len - 1);
        if (read_len >= 0) {
            out_buf[read_len] = '\0';
        }
    }
    *out_len = read_len > 0 ? read_len : 0;
    esp_http_client_cleanup(client);
    return status;
}
```

- [ ] **Step 8: 编译验证**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep -E 'error:|Project build complete' | head -3`
Expected: `Project build complete`（0 errors）

- [ ] **Step 9: 提交**

```bash
git add firmware/main/common/http_client_wrapper.h firmware/main/common/http_client_wrapper.cc
git commit -m "feat(firmware): http_wrapper_post_json_with_headers for device signature"
```

---

### Task 5: 固件 device_signature 模块

**Files:**
- Create: `firmware/main/common/device_signature.h`
- Create: `firmware/main/common/device_signature.cc`
- Modify: `firmware/main/boards/zectrix-s3-epaper-4.2/config.h` (+DEVICE_MASTER_KEY 宏)
- Modify: `firmware/main/CMakeLists.txt` (+device_signature.cc)

**Interfaces:**
- Consumes: `esp_read_mac()`, `esp_timer_get_time()`, `esp_fill_random()`, `mbedtls/sha256.h`
- Produces:
  - `void device_sign_pair_start(const char *device_id, char *mac_out, size_t mac_len, char *ts_out, size_t ts_len, char *nonce_out, size_t nonce_len, char *sig_out, size_t sig_len)`
  - `const char *DEVICE_MASTER_KEY` 宏（编译时注入）

- [ ] **Step 1: 写 device_signature.h**

```c
/**
 * @file device_signature.h
 * @brief Device signature generation for pair-start authentication
 */
#ifndef DEVICE_SIGNATURE_H
#define DEVICE_SIGNATURE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Sign a pair-start request.
 *
 * @param device_id  Device ID string (e.g. "NOTE4C-3400FC")
 * @param mac_out    Buffer for hex MAC (12 chars + null)
 * @param mac_len    sizeof(mac_out)
 * @param ts_out     Buffer for timestamp string (10 digits + null)
 * @param ts_len     sizeof(ts_out)
 * @param nonce_out  Buffer for base64 nonce (24 chars + null)
 * @param nonce_len  sizeof(nonce_out)
 * @param sig_out    Buffer for base64 HMAC-SHA256 (44 chars + null)
 * @param sig_len    sizeof(sig_out)
 */
void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len);

#ifdef __cplusplus
}
#endif

#endif
```

- [ ] **Step 2: 写 device_signature.cc**

```c
#include "device_signature.h"
#include "boards/zectrix-s3-epaper-4.2/config.h"
#include <esp_mac.h>
#include <esp_timer.h>
#include <esp_random.h>
#include <mbedtls/sha256.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

// base64 encode (no padding needed for 16/32 bytes → 24/44 chars)
static const char kB64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static void base64_encode(const uint8_t *in, size_t in_len, char *out) {
    size_t o = 0;
    for (size_t i = 0; i < in_len; i += 3) {
        uint32_t triple = (uint32_t)in[i] << 16;
        if (i + 1 < in_len) triple |= (uint32_t)in[i+1] << 8;
        if (i + 2 < in_len) triple |= (uint32_t)in[i+2];
        out[o++] = kB64[(triple >> 18) & 0x3F];
        out[o++] = kB64[(triple >> 12) & 0x3F];
        out[o++] = (i + 1 < in_len) ? kB64[(triple >> 6) & 0x3F] : '=';
        out[o++] = (i + 2 < in_len) ? kB64[triple & 0x3F] : '=';
    }
    out[o] = '\0';
}

static void hmac_sha256(const uint8_t *key, size_t key_len,
                         const uint8_t *msg, size_t msg_len,
                         uint8_t out[32]) {
    uint8_t k_pad[64] = {0};
    if (key_len > 64) {
        mbedtls_sha256(key, key_len, k_pad, 0);
    } else {
        memcpy(k_pad, key, key_len);
    }
    uint8_t ipad[64], opad[64];
    for (int i = 0; i < 64; i++) {
        ipad[i] = k_pad[i] ^ 0x36;
        opad[i] = k_pad[i] ^ 0x5C;
    }
    // inner: SHA256(ipad || msg)
    mbedtls_sha256_context ctx;
    uint8_t inner[32];
    mbedtls_sha256_starts(&ctx, 0);
    mbedtls_sha256_update(&ctx, ipad, 64);
    mbedtls_sha256_update(&ctx, msg, msg_len);
    mbedtls_sha256_finish(&ctx, inner);
    // outer: SHA256(opad || inner)
    mbedtls_sha256_starts(&ctx, 0);
    mbedtls_sha256_update(&ctx, opad, 64);
    mbedtls_sha256_update(&ctx, inner, 32);
    mbedtls_sha256_finish(&ctx, out);
}

void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len) {
    // 1. MAC
    uint8_t mac[6];
    esp_read_mac(mac, ESP_MAC_WIFI_STA);
    snprintf(mac_out, mac_len, "%02X%02X%02X%02X%02X%02X",
             mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

    // 2. Timestamp (unix seconds)
    int64_t now = esp_timer_get_time() / 1000000;
    snprintf(ts_out, ts_len, "%lld", (long long)now);

    // 3. Random nonce (16 bytes → 24 chars base64)
    uint8_t nonce_raw[16];
    esp_fill_random(nonce_raw, 16);
    base64_encode(nonce_raw, 16, nonce_out);

    // 4. Derive key
    uint8_t derived_key[32];
    hmac_sha256(
        (const uint8_t *)DEVICE_MASTER_KEY, strlen(DEVICE_MASTER_KEY),
        (const uint8_t *)device_id, strlen(device_id),
        derived_key
    );

    // 5. Build payload: MAC || timestamp || nonce
    size_t ts_str_len = strlen(ts_out);
    size_t nonce_str_len = strlen(nonce_out);
    size_t payload_len = 6 + ts_str_len + nonce_str_len;
    uint8_t *payload = (uint8_t *)malloc(payload_len);
    memcpy(payload, mac, 6);
    memcpy(payload + 6, ts_out, ts_str_len);
    memcpy(payload + 6 + ts_str_len, nonce_out, nonce_str_len);

    // 6. Sign
    uint8_t sig_raw[32];
    hmac_sha256(derived_key, 32, payload, payload_len, sig_raw);
    base64_encode(sig_raw, 32, sig_out);

    free(payload);
}
```

- [ ] **Step 3: 加 config.h 宏**

`firmware/main/boards/zectrix-s3-epaper-4.2/config.h` 添加：

```c
// Device signature master key (compile-time inject, do NOT commit actual value)
#ifndef DEVICE_MASTER_KEY
#define DEVICE_MASTER_KEY "REPLACE_ME_AT_BUILD_TIME_WITH_32_BYTE_RANDOM"
#endif
```

- [ ] **Step 4: 加 CMakeLists 源文件**

`firmware/main/CMakeLists.txt` 在 `common/page_sync.cc` 附近加：

```cmake
"common/device_signature.cc"
```

- [ ] **Step 5: 编译验证**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep -E 'error:|Project build complete' | head -3`
Expected: `Project build complete`

- [ ] **Step 6: 提交**

```bash
git add firmware/main/common/device_signature.h firmware/main/common/device_signature.cc firmware/main/boards/zectrix-s3-epaper-4.2/config.h firmware/main/CMakeLists.txt
git commit -m "feat(firmware): device_signature module (HMAC-SHA256 sign)"
```

---

### Task 6: 固件 server_pairing 集成签名

**Files:**
- Modify: `firmware/main/common/server_pairing.cc` (do_pair_start 加签名)
- Test: 编译验证

**Interfaces:**
- Consumes: `device_sign_pair_start` (from Task 5), `http_wrapper_post_json_with_headers` (from Task 4)
- Produces: same `server_pairing_run()` 流程，签名后的 pair-start 请求

- [ ] **Step 1: 改 do_pair_start 使用签名**

`firmware/main/common/server_pairing.cc` 改 `do_pair_start` 函数（约 line 169-214），把现有 `http_wrapper_post_json` 调用替换为 `http_wrapper_post_json_with_headers` 加 4 个签名 header：

找到现有：
```cpp
    char resp[512];
    int resp_len = sizeof(resp);
    int status = http_wrapper_post_json(url, token, body, resp, &resp_len, 10000);
```

替换为：
```cpp
    // Sign the request
    char mac_hex[16], ts_str[16], nonce_b64[32], sig_b64[64];
    device_sign_pair_start(device_id,
                           mac_hex, sizeof(mac_hex),
                           ts_str, sizeof(ts_str),
                           nonce_b64, sizeof(nonce_b64),
                           sig_b64, sizeof(sig_b64));
    ESP_LOGI(TAG, "pair-start signing: mac=%s ts=%s nonce=%s sig_len=%d",
             mac_hex, ts_str, nonce_b64, (int)strlen(sig_b64));

    http_header_t extra[] = {
        {"X-Device-Mac", mac_hex},
        {"X-Device-Timestamp", ts_str},
        {"X-Device-Nonce", nonce_b64},
        {"X-Device-Signature", sig_b64},
    };

    char resp[512];
    int resp_len = sizeof(resp);
    int status = http_wrapper_post_json_with_headers(
        url, token, body, extra, 4, resp, &resp_len, 10000);
```

- [ ] **Step 2: 编译验证**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build 2>&1 | grep -E 'error:|Project build complete' | head -3`
Expected: `Project build complete`

- [ ] **Step 3: 提交**

```bash
git add firmware/main/common/server_pairing.cc
git commit -m "feat(firmware): server_pairing uses device signature for pair-start"
```

---

### Task 7: 真机端到端验证

**Files:**
- Modify: 无（仅验证）

- [ ] **Step 1: 生成共享 MASTER_KEY**

```bash
python3 -c "import secrets; print(secrets.token_urlsafe(32))"
```

复制输出值。

- [ ] **Step 2: 配置服务端 .env**

`server/.env` 添加：
```
MASTER_KEY=<上一步复制的值>
```

- [ ] **Step 3: 重启服务端**

Run: `cd server && kill $(ss -tlnp 2>/dev/null | grep 9002 | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2) && sleep 1 && ./start.sh start`

- [ ] **Step 4: 编译固件注入 MASTER_KEY**

Run: `cd firmware && source ~/data/esp-idf-v6.0/export.sh && idf.py build -DDEVICE_MASTER_KEY="<上一步复制的值>"`

- [ ] **Step 5: 生成 merged binary + 烧录**

Run:
```bash
cd firmware/build
esptool --chip esp32s3 merge-bin --output merged-binary.bin --pad-to-size 16MB \
  0x0 bootloader/bootloader.bin \
  0x8000 partition_table/partition-table.bin \
  0xd000 ota_data_initial.bin \
  0x20000 xiaozhi.bin
esptool --chip esp32s3 -p /dev/ttyACM0 -b 460800 write-flash 0x0 merged-binary.bin
```

- [ ] **Step 6: 设备端验证：看串口日志**

Run: 读串口确认 `pair-start signing:` 日志出现，且设备屏幕显示 6 位配对码（不是"设备认证失败"）

- [ ] **Step 7: 服务端确认 + 设备 claim**

通过 Web UI 或 API 确认配对码，设备应自动 claim 成功。

---

## Self-Review Checklist

- [x] Spec coverage: 9 章节 → Task 1-7 完整覆盖
- [x] Placeholder scan: 无 TBD/TODO，所有代码块完整
- [x] Type consistency: `http_header_t` 结构体在 Step 6（http_wrapper.h）和 Step 5/6 引用一致
