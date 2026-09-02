# NOTE4C 设备认证签名（Device Signature）设计

日期：2026-09-03
状态：approved（章节 1-5 全部经用户确认）
分类：Architectural
相关：配对鉴权（pair-start / pair-claim）、Web Admin UI

## 1. 背景与目标

NOTE4C 设备首启时需要与服务端建立信任关系（配对 → 拿 token）。当前流程：
设备 POST `/api/devices/pair-start` → 服务端**无验证**直接生成 6 位配对码。任何能伪造 `device_id` 的客户端都能获取配对码。

**问题**：无法区分「合法设备」与「冒充者」。

**目标**：设备首启时使用本地预置密钥对配对请求做 HMAC 签名，服务端验证签名后才生成配对码。验证失败返回 401，设备不显示配对码。

**非目标（YAGNI）**：
- 不做设备证书/CA 链
- 不做密钥轮换（先固定主密钥）
- 不做配对码之外的接口签名（pair-claim 已有 code 即可）
- 不做白名单外的设备远程拉黑

## 2. 架构

```
设备首启 (无 NVS token) →
  1. 设备 POST /api/devices/pair-start
     Headers: X-Device-MAC, X-Device-Timestamp, X-Device-Nonce, X-Device-Signature
     Body: {device_id, board_type}
  2. 服务端:
     - derived_key = HMAC_SHA256(MASTER_KEY, device_id)
     - payload = MAC_bytes || timestamp_str || nonce_str
     - expected_sig = HMAC_SHA256(derived_key, payload)
     - 验签: hmac.compare_digest(provided, expected)
     - 时间窗口: |now - timestamp| ≤ 30s
     - 验签通过: 缓存 nonce 5min (防重放), 生成 6 位配对码, 返回 200
     - 验签失败: 401 {detail:"device authentication failed"}
  3. 设备:
     - 收到 200: 显示配对码, 进入原 pair-claim 流程
     - 收到 401: 屏幕显示"设备认证失败", 不显示配对码
```

**关键**：pair-start 现在**带签名认证**。pair-claim / pair-confirm 流程不变（已有 code 即可）。

**密钥推导**：两端都用 `derived_key = HMAC_SHA256(MASTER_KEY, device_id)`，不存储 derived_key，每次实时推导。

## 3. 协议字段

| 字段 | 位置 | 来源 | 长度 |
|---|---|---|---|
| `X-Device-MAC` | HTTP header | 设备 base64(MAC 6 bytes) | 16 chars |
| `X-Device-Timestamp` | HTTP header | 设备 unix timestamp (seconds) | 10 digits |
| `X-Device-Nonce` | HTTP header (optional) | 设备随机 16 bytes (base64) | 24 chars |
| `X-Device-Signature` | HTTP header | 设备 base64(HMAC-SHA256) | 44 chars |

**签名 payload 拼接**（按此顺序拼接后 HMAC-SHA256）：
```
MAC_bytes (6) || timestamp_str (ASCII) || nonce_str (ASCII)
```

**MAC 编码**：`AABBCCDDEEFF` (12 hex chars, 6 bytes) — ESP32 `esp_read_mac()` 返回此格式。

## 4. 密钥管理

**主密钥**：`MASTER_KEY` (≥32 字节强随机字符串)
- 服务端：`server/.env` + `config.py` 字段（默认生成占位值提醒配置）
- 设备端：固件硬编码（`firmware/main/boards/zectrix-s3-epaper-4.2/config.h` 的 `DEVICE_MASTER_KEY` 宏）

**派生**（两端对称）：
```python
# Python 服务端
import hmac, hashlib
derived_key = hmac.new(MASTER_KEY.encode(), device_id.encode(), hashlib.sha256).digest()
```

```c
// 固件
#include "mbedtls/sha256.h"
// derived_key = HMAC-SHA256(MASTER_KEY, device_id)
// signature = HMAC-SHA256(derived_key, MAC || timestamp || nonce)
```

`derived_key` **不持久化**——每次签名/验签实时计算。

**白名单（可选）**：`server/.env` 加 `ALLOWED_DEVICE_IDS=NOTE4C-XXXXXX,NOTE4C-YYYYYY`，逗号分隔。留空 = 全部接受。服务端验签成功后查白名单，不在列表返回 401。
| `X-Device-MAC` | HTTP header | 设备 hex(MAC 6 bytes) | 12 chars |
| `X-Device-Timestamp` | HTTP header | 设备 unix timestamp (seconds) | 10 digits |

**`POST /api/devices/pair-start`** 新增 headers：
```http
POST /api/devices/pair-start
Content-Type: application/json
X-Device-MAC: AABBCCDDEEFF
X-Device-Timestamp: 1788392000
X-Device-Nonce: a3f8... (24 chars)
X-Device-Nonce-NULL: (无 nonce 时此 header 为空字符串)
X-Device-Signature: dGhpc19pc19zaWduYXR1cmU... (44 chars base64)

{ "device_id": "NOTE4C-3400FC", "board_type": "zectrix-s3-epaper-4.2" }
```

**响应**：
- `200 {"code": "639998", "expires_in": 300}`（验签通过）
- `400 {"detail": "missing device auth headers"}`（缺少签名头）
- `401 {"detail": "device authentication failed"}`（验签失败 / 时间窗口外）
- `401 {"detail": "device not in whitelist"}`（device_id 不在白名单）
- `429 {"detail": "too many requests, try again later"}`（5min 重放防护）

**`/pair-claim` 流程不变**——已有配对码即可 claim。

## 6. 端点实现

### 6.1 服务端（`server/youn_server/pairing.py` 新增方法）

```python
import hmac
import hashlib
import base64
import time

NONCE_CACHE_TTL_SEC = 300  # 5 minutes replay window
TIMESTAMP_WINDOW_SEC = 30   # ±30s clock skew

def verify_device_signature(self, device_id: str, mac: str, 
                             timestamp: int, nonce: str, 
                             signature_b64: str) -> bool:
    """Verify device signature using HMAC derived from MASTER_KEY.
    
    Returns True if:
      - |now - timestamp| <= TIMESTAMP_WINDOW_SEC
      - signature matches HMAC-SHA256(derived_key, MAC || timestamp || nonce)
      - nonce not in replay cache (within NONCE_CACHE_TTL_SEC)
    """
    master_key = settings.master_key
    if not master_key:
        log.error("MASTER_KEY not configured")
        return False
    
    # Check timestamp window
    now = int(time.time())
    if abs(now - timestamp) > TIMESTAMP_WINDOW_SEC:
        log.warning("signature failed: timestamp out of window device_id=%s", device_id)
        return False
    
    # Derive key
    derived_key = hmac.new(
        master_key.encode(),
        device_id.encode(),
        hashlib.sha256
    ).digest()
    
    # Reconstruct payload
    try:
        mac_bytes = bytes.fromhex(mac)
    except ValueError:
        log.warning("signature failed: bad MAC format device_id=%s", device_id)
        return False
    payload = mac_bytes + str(timestamp).encode() + nonce.encode()
    
    # Verify signature
    expected = hmac.new(derived_key, payload, hashlib.sha256).digest()
    try:
        provided = base64.b64decode(signature_b64)
    except Exception:
        return False
    if not hmac.compare_digest(expected, provided):
        log.warning("signature failed: invalid signature device_id=%s", device_id)
        return False
    
    # Check nonce replay (in-memory cache, simple)
    nonce_key = f"{device_id}:{nonce}"
    with self._lock:
        now_f = time.time()
        # Cleanup old
        self._nonce_cache = {k: v for k, v in self._nonce_cache.items() 
                             if now_f - v < NONCE_CACHE_TTL_SEC}
        if nonce_key in self._nonce_cache:
            log.warning("signature failed: nonce replay device_id=%s", device_id)
            return False
        self._nonce_cache[nonce_key] = now_f
    
    log.info("device signature verified device_id=%s", device_id)
    return True
```

### 6.2 `app.py` `pair_start` 端点

```python
@app.post("/api/devices/pair-start")
async def pair_start(body: _PairStartBody, request: Request) -> dict:
    # 提取签名头
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
    
    # 验证设备签名
    if not _pairing_store.verify_device_signature(
        body.device_id, mac, timestamp, nonce, sig
    ):
        raise HTTPException(401, detail="device authentication failed")
    
    # 白名单（可选）
    if settings.allowed_device_ids and body.device_id not in settings.allowed_device_ids:
        raise HTTPException(401, detail="device not in whitelist")
    
    # 已有逻辑：注册 + 创建 session
    registry.upsert(body.device_id, body.board_type)
    code, expires_in = _pairing_store.create_session(body.device_id)
    return {"code": code, "expires_in": expires_in}
```

### 6.3 固件（新模块 `firmware/main/common/device_signature.h/cc`）

```c
// device_signature.h
#ifndef DEVICE_SIGNATURE_H
#define DEVICE_SIGNATURE_H

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Sign a pair-start request.
 * 
 * @param device_id  Device ID string (e.g. "NOTE4C-3400FC")
 * @param mac_out    Buffer for hex MAC (12 chars + null)
 * @param ts_out     Buffer for timestamp string (10 digits + null)
 * @param nonce_out  Buffer for base64 nonce (24 chars + null)
 * @param sig_out    Buffer for base64 HMAC-SHA256 (44 chars + null)
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

```c
// device_signature.cc
#include "device_signature.h"
#include "boards/zectrix-s3-epaper-4.2/config.h"  // DEVICE_MASTER_KEY
#include <esp_mac.h>
#include <esp_timer.h>
#include <mbedtls/sha256.h>
#include <string.h>
#include <stdlib.h>

static void hmac_sha256(const uint8_t *key, size_t key_len,
                         const uint8_t *msg, size_t msg_len,
                         uint8_t out[32]) {
    mbedtls_sha256_context ctx;
    mbedtls_sha256_starts(&ctx, 1);  // HMAC mode
    // ... (mbedtls HMAC-SHA256 implementation)
}

static void derive_key(const char *device_id, uint8_t out[32]) {
    hmac_sha256(
        (const uint8_t *)DEVICE_MASTER_KEY, strlen(DEVICE_MASTER_KEY),
        (const uint8_t *)device_id, strlen(device_id),
        out
    );
}

static void base64_encode(const uint8_t *in, size_t in_len, char *out) {
    // Standard base64
}

static void random_bytes(uint8_t *buf, size_t len) {
    esp_fill_random(buf, len);
}

void device_sign_pair_start(const char *device_id,
                            char *mac_out, size_t mac_len,
                            char *ts_out, size_t ts_len,
                            char *nonce_out, size_t nonce_len,
                            char *sig_out, size_t sig_len) {
    // 1. Read MAC
    uint8_t mac[6];
    esp_read_mac(mac, ESP_MAC_WIFI_STA);
    snprintf(mac_out, mac_len, "%02X%02X%02X%02X%02X%02X",
             mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    
    // 2. Timestamp
    int64_t now = esp_timer_get_time() / 1000000;
    snprintf(ts_out, ts_len, "%lld", (long long)now);
    
    // 3. Random nonce (16 bytes)
    uint8_t nonce_raw[16];
    random_bytes(nonce_raw, 16);
    base64_encode(nonce_raw, 16, nonce_out);
    
    // 4. Build payload
    size_t ts_str_len = strlen(ts_out);
    size_t nonce_str_len = strlen(nonce_out);
    size_t payload_len = 6 + ts_str_len + nonce_str_len;
    uint8_t *payload = malloc(payload_len);
    memcpy(payload, mac, 6);
    memcpy(payload + 6, ts_out, ts_str_len);
    memcpy(payload + 6 + ts_str_len, nonce_out, nonce_str_len);
    
    // 5. Derive key + sign
    uint8_t derived_key[32];
    derive_key(device_id, derived_key);
    
    uint8_t sig_raw[32];
    hmac_sha256(derived_key, 32, payload, payload_len, sig_raw);
    base64_encode(sig_raw, 32, sig_out);
    
    free(payload);
}
```

### 6.4 固件集成（`server_pairing.cc` `do_pair_start`）

```cpp
// Before POST, generate signature
char mac[16], timestamp[16], nonce[32], signature[64];
device_sign_pair_start(device_id, mac, sizeof(mac),
                       timestamp, sizeof(timestamp),
                       nonce, sizeof(nonce),
                       signature, sizeof(signature));

// Build headers (existing http_wrapper_post_json doesn't support custom headers,
// need to add new helper or use existing mechanism with header injection)
```

**注**：`http_wrapper_post_json` 当前不支持自定义 header。需扩展或新写一个 `http_wrapper_post_json_with_headers` 函数。

### 6.5 `config.h` 新增主密钥

```c
// firmware/main/boards/zectrix-s3-epaper-4.2/config.h
#define DEVICE_MASTER_KEY "REPLACE_WITH_32_BYTE_RANDOM_STRING_FROM_SERVER"
```

**生产**：编译时通过 `-DDEVICE_MASTER_KEY="..."` 注入（不要硬编码在源码）。

## 7. 测试

### 7.1 服务端（pytest，新增 `tests/test_device_signature.py`）
- `test_signature_valid` — 正确签名通过验证
- `test_signature_invalid_key` — 错误 master_key 失败
- `test_signature_invalid_payload` — payload 被篡改失败
- `test_signature_timestamp_out_of_window` — 时间偏差 >30s 失败
- `test_signature_nonce_replay` — 同一 nonce 5min 内第二次失败
- `test_signature_missing_headers` — 缺少 header 失败
- `test_pair_start_with_valid_signature` — 端到端 200
- `test_pair_start_with_invalid_signature` — 端到端 401
- `test_pair_start_with_whitelist` — 白名单拒绝

### 7.2 固件
- 编译 `idf.py build` 通过
- 真机：先不签名配对（应 401），然后签名配对（应 200 + 显示配对码）

## 8. 改动文件总览

```
server/
  youn_server/pairing.py              (改, +verify_device_signature +nonce_cache)
  youn_server/app.py                 (改, pair_start 端点加签名验证)
  youn_server/config.py              (改, +master_key +allowed_device_ids 字段)
  tests/test_device_signature.py     (新)

firmware/main/
  common/device_signature.h          (新)
  common/device_signature.cc         (新)
  common/server_pairing.cc           (改, do_pair_start 加签名 + 新 HTTP helper)
  common/http_client_wrapper.h       (改, +http_wrapper_post_json_with_headers)
  common/http_client_wrapper.cc      (改, +http_wrapper_post_json_with_headers 实现)
  boards/zectrix-s3-epaper-4.2/config.h  (改, +DEVICE_MASTER_KEY 宏)
  main/CMakeLists.txt                (改, +device_signature.cc)

docs/
  superpowers/specs/2026-09-03-device-signature.md  (本文件)
```

## 9. 提交计划

1. spec commit（本文档）
2. server commit：pairing.py + app.py + config.py + tests（8-10 tests）
3. firmware commit：device_signature.{h,cc} + http_client_wrapper 扩展 + server_pairing 集成 + config.h
4. 烧录 + 真机验证
