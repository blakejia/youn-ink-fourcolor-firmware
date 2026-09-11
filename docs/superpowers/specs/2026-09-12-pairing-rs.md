# pairing.rs：配对策略移植到 Rust

**日期**：2026-09-12
**目标文件**：`firmware/main/rust/src/pairing.rs`（新增）、`firmware/main/common/server_pairing.cc`（改写为步进循环）

## 为什么

`server_pairing.cc` 的 `server_pairing_run()` 是一个 444 行的 `while(true)` 阻塞循环，里面混了三件事：HTTP/NVS/屏显机制、以及**协议策略**（5 分钟超时重发、2 秒轮询、401/429 退避、失败上限、时钟可信判断）。

策略是唯一会出错的部分，而且历史上真的错过两次——注释自己记着：

- 旧实现 `while(true)` 只有 `return true`，Error 分支永远不可达 → 设备永远停在 PairStart，首屏从未收到配对码（无码、无错误、无提示）。
- 时钟未同步就发 pair-start → 签名时间戳是 1970 → 服务端必然 401 → 每次失败都消耗 per-IP 配额（5 次/300s）→ 打成 429 死循环。

这两条都是**运行时策略**，在设备上只能靠"等 5 分钟看会不会死循环"来验。移植后它们变成主机上 0.1 秒可断言的行为。

## 边界（与 power.rs 同一模板）

- **Rust 只做决策**：`Inputs -> decide() -> Action`，纯函数，无锁、无 IO、无 IDF 调用。
- **C++ 保留机制**：HTTP（`http_wrapper_post_json`）、NVS 读写、`esp_read_mac`、`vTaskDelay`、屏显回调。
- 常量单点定义在 Rust，C++ 通过 `rf_pairing_decide` 的输出使用（`Action::ClaimAfter{ms}` 等），不再各自定义一份。

## 接口

```rust
pub struct Inputs {
    pub has_code: bool,             // 屏上有一个尚未过期的配对码
    pub clock_ok: bool,             // 墙钟可信（SNTP 已同步）
    pub window_elapsed_s: u32,      // 当前 5 分钟窗口已过去多少秒
    pub pair_start_failures: u32,   // 连续 pair-start / 时钟失败次数
    pub last: Outcome,              // 上一个动作的结果
}

pub enum Outcome {
    None,              // 尚未开始
    ClockWaited,       // 等过时钟，仍不可信
    PairStarted,       // pair-start 成功，码已上屏
    PairStartFailed,   // pair-start 网络/解析失败
    ClaimPending,      // 200 {"status":"pending"}
    ClaimGranted,      // 200 + token
    ClaimRejected,     // 401 / 429
    ClaimNetworkError, // 其它状态码 / 连接失败
    TokenWriteFailed,  // 拿到 token 但 NVS 落盘失败
}

pub enum Action {
    PairStart,
    ReissueCode,                              // 窗口到期：丢码重发，窗口重置
    ReissueCodeWithBackoff { ms: u32 },       // 401/429：丢码 + 退避（per-IP 限流）
    WaitForClock { ms: u32 },                 // 时钟不可信：等，不发请求
    Claim,
    ClaimAfter { ms: u32 },
    Paired,
    GiveUp,
}
```

常量：`PAIRING_TIMEOUT_S=300`、`CLAIM_POLL_MS=2000`、`CLAIM_ERROR_BACKOFF_MS=5000`、`CLOCK_WAIT_MS=5000`、`MAX_PAIR_START_FAILURES=6`。

## 决策优先级（顺序即语义）

1. `last == TokenWriteFailed` → **GiveUp**。服务端已签发并置 trust，设备却存不下 → 不能当成功，也不能继续轮询（两侧状态分叉）。
2. `last == ClaimGranted` → **Paired**。
3. `has_code && window_elapsed_s >= 300` → **ReissueCode**（窗口到期）。
4. `has_code && last == ClaimRejected` → **ReissueCodeWithBackoff{5000}**（码已死，且必须退避：立即重发会被打成 429 死循环）。
5. `!has_code`：`failures >= 6` → **GiveUp**；`!clock_ok` → **WaitForClock{5000}**；否则 **PairStart**。
6. `has_code`：`ClaimPending | ClaimNetworkError` → **ClaimAfter{2000}**；其余（`PairStarted`/`None`）→ **Claim**（首发立即，不延迟）。

## 验证

- **主机**：`cargo test`，≥12 个用例，每个对应用户可观察的协议行为（不是实现细节）。
- **真机端到端**（可自动化，无需人手）：擦掉设备 NVS 里的 token（保留 WiFi，用 `nvs_partition_gen` 直接写分区）→ 设备启动进入配对 → 从设备日志/服务器 `pair-pending` 取配对码 → 用 operator token 调 `POST /api/devices/pair-confirm` → 设备下次 `pair-claim` 拿到 token 并写 NVS → `page_sync` 恢复同步。这条路径覆盖改写后的整个循环。
- **未验证项要如实报**：NVS 落盘失败分支、429 真实限流分支无法在真机上按需触发；它们靠主机用例保证。

## 不做

- 不动 HTTP/NVS/屏显机制（`power.rs` 的成功模式就是不碰机制）。
- 不改协议本身、不改常量取值——本次是纯重构，行为必须与线上一致。

## 执行结果（2026-09-12）

**主机**：`cargo test` **85 passed**（配对 16 + FFI 契约 5，其余为既有模块）；`idf.py build` exit 0，`build/xiaozhi.bin` 2,899,392 B。

**哨兵证明**（技能要求"证明新检查会失败"）：把两个历史 bug 人为放回去，两条用例各自失败——
- 去掉 401/429 退避 → `a_rejected_claim_drops_the_code_and_backs_off` 失败
- 把 `Outcome` 编号挪一位 → `every_outcome_code_the_c_side_sends_is_recognised` 失败

**真机端到端**（移植版固件，`已失败` 文案在镜像中 = 1、旧文案 = 0，证明跑的是新代码）：

| 路径 | 观测 |
|---|---|
| 分类 | `Has base_url but no token → 需要配对`（NVS 三态） |
| 正常 | `发起 pair-start…` → 签名 POST → `配对码: 701681, 有效期: 300s` → 屏显回调 + `Switching page: 对话 -> 配对` → operator 确认 200 → **`配对成功！写入 token`** → `SyncIdle` |
| 失败 | 停掉服务器 → `ESP_ERR_HTTP_CONNECT` → **`pair-start 失败，5000 ms 后重试（已失败 N 次）`，间隔精确 5 秒** → 第 6 次 → **`连续失败 6 次，放弃本轮配对`** → 应用层 `第 1/3 次配对失败` |
| 窗口 | 旧固件那轮另证：码发放后整 **300 秒**触发 `配对超时 (300s)，重新发起 pair-start` |

放弃退出与 5 秒退避正是旧代码注释里"Error 分支永远不可达"和"打成 429 死循环"的两个 bug 点，现在两侧都有覆盖：主机用断言，真机用日志。

**移植版未观测**：`ClaimRejected`(401/429) 与 `WaitForClock` 需要码自然过期或冷钟，真机上要等 5 分钟以上；两者由主机用例覆盖。`ClaimPending` 的 2 秒轮询在移植版这一轮里因"确认发生在首次 claim 之前"而未走满。

## 附带发现：服务器只读库（与本次移植无关，但阻塞了端到端）

`pair-start` 持续返回 **500**，堆栈指向 `devices.py:124 upsert` → `sqlite3.OperationalError: attempt to write a readonly database`。而库文件权限正常（`pi:pi rw-rw-r--`）、目录可写、磁盘有余、代码里无 `query_only`/只读 URI。

根因：**SQLite 在打开时若文件不可写，会静默以降级为只读连接**，此后即使权限被修好，那条连接也永远写不了。`devices.db` 的 mtime 停在 `19:24`（最后一次成功写入），而服务进程自那之前就在运行 —— 也就是说写能力静默失效了约 6 小时，直到重启服务才恢复（`./start.sh restart` 后 `confirm -> 200`）。

**建议**（未做，属服务器侧改动）：启动时做一次探测写（或 `PRAGMA quick_check` 后的空 UPDATE），只读就立刻报错退出，别让它静默跑一整天。这是"静默失败"里最贵的一类。
