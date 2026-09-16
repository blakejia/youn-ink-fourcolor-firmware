# Task 4 Report: 固件 — BSP 改名 + notify 模块 + 接线

- **Status**: DONE — build success, 0 undefined references
- **Commit**: `163bbcdb972a44a22b6eea9ff2b3292103b6c8b6` (branch `2bp`), message: `feat(firmware): notification module + BSP GPIO wiring`
- **Date**: 2026-09-02

## Files changed

| File | Change |
|---|---|
| `firmware/main/common/notify.h` | **Created** — 按 plan Step 2 原样的公开 API（`notify_init/deinit/request_next/is_active/post_ack/dismiss`），补 `#include <stdbool.h>`（plan 片段遗漏，C 调用方需要） |
| `firmware/main/common/notify.cc` | **Created** — 状态机 IDLE/FETCHING/NOTIFYING 完整实现 |
| `firmware/main/boards/zectrix-s3-epaper-4.2/config.h` | `TODO_UP_BUTTON_GPIO`→`UP_BUTTON_GPIO` (GPIO_NUM_39)、`TODO_DOWN_BUTTON_GPIO`→`DOWN_BUTTON_GPIO` (GPIO_NUM_18)、`TODO_CONFIRM_BUTTON_GPIO`→`CONFIRM_BUTTON_GPIO` (GPIO_NUM_0) |
| `firmware/main/boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc` | 三处宏引用更新；`kBoardConfirmButtonGpio` 从 `BOOT_BUTTON_GPIO` 改为 `CONFIRM_BUTTON_GPIO`（同值 GPIO_NUM_0，行为不变） |
| `firmware/main/application.cc` | `#include "common/notify.h"`；`ServerPairingTaskTrampoline` 在 `page_sync_start()` 后调 `notify_init()`；OnBootClick/OnUpClick/OnDownClick 加 notify 分支 |
| `firmware/main/CMakeLists.txt` | SRCS 加 `"common/notify.cc"`（紧随 `common/page_sync.cc`） |

## 实现要点（超出 plan 占位符的部分）

1. **异步 GET（plan 的 `// TODO: run in background task`）**：`notify_request_next` 只切 FETCHING 并 `xTaskCreate(fetch_task, ...)`（8KB 栈、优先级 3，与 page_sync 任务同级），HTTP GET 在后台任务中同步执行，按钮回调立即返回。任务创建失败回退 IDLE。
2. **JSON 解析 + base64（plan 的 `// Parse JSON...`）**：cJSON 解析 `bitmap_base64` 与 `notification.id`；`mbedtls_base64_decode` 解码进 PSRAM 缓冲，校验解码长度 == `PAGE_BITMAP_SIZE`(30000)。GET 响应缓冲 45056B（PSRAM 分配）——40000B base64 + JSON 元数据余量。
3. **framebuffer 安全路径**：与 `page_sync.cc::show_page` 完全相同的写法——`xSemaphoreTake(GetMutex())` → `memcpy` 30000B → `RequestUrgentFullRefresh()`。不直接调 `DisplayRaw4ColorImage`，避免与后台 refresh_task 并发抢 EPD（NOTE4C 已知卡死陷阱）。显示时调 `page_sync_stop_display()` 标记画板不再主导屏幕。
4. **5min FreeRTOS timer**：`xTimerCreate("notify_to", 5min, pdFALSE)` 在 `notify_init` 创建，进入 NOTIFYING 时 start，dismiss/timeout 时 stop，对齐服务端 ttl 300s。
5. **ack 也是异步**：`notify_post_ack` 复制 body 到 PSRAM、`xTaskCreate(ack_task)` 做 POST（不阻塞按钮回调），随后无条件 `notify_dismiss()`（乐观关闭；ack 失败时通知保持 shown，服务端 5min ttl 过期兜底——选择乐观关闭而非重试，避免屏幕状态与服务器状态分叉）。
6. **dismiss 恢复画板**：`page_sync_resume_display()` + `prev()/next()` 自旋一圈重绘当前页并重置轮换计时（替代原始内容快照——无需额外 30KB 缓冲，复用 page_sync 缓存位图）。

## 与服务端契约的对齐（读 server/youn_server/app.py 实际实现确认）

- **GET `/api/notifications/next?device_id=<id>`**：服务端实现（Task 2 已落地，`app.py:458`）要求 `device_id` query 参数且与 Bearer token 派生设备匹配，不匹配返回 401。固件经 `server_pairing_get_device_id()` 带上该参数。
- **响应**：`{"bitmap_base64": ..., "notification": {"id": uuid.hex(32), ...}}`；无待办时 204 空体——固件 204 → 回 IDLE 并记日志。
- **POST `/api/notifications/{nid}/ack`** body `{"decision":"agree|reject"}`。

## Build output（最终一次）

```
[8/10] Linking CXX executable xiaozhi.elf
[9/10] Generating binary image from built executable
Successfully created ESP32-S3 image.
Generated .../firmware/build/xiaozhi.bin
xiaozhi.bin binary size 0x2bbba0 bytes. Smallest app partition is 0x3f0000 bytes. 0x134460 bytes (31%) free.
Project build complete.
```

第一次构建有 2 个错误（编辑工具操作失误导致 `<cJSON.h>` include 与 `auto& board` 声明丢失），修复后第二次构建通过。`idf.py build` 全量链接无 undefined reference。

## Concerns / 后续关注

1. **位图内容被覆盖无快照恢复**：NOTIFYING 期间通知位图直接覆盖共享 framebuffer，dismiss 后靠 page_sync 缓存位图重绘当前页。若 page_sync 尚无缓存页（首次启动未同步），dismiss 后屏幕残留通知位图直到下一次 page_sync 显示。可接受（画板本来就该有页），如需严格恢复可加 30KB PSRAM 快照。
2. **`notify_is_active()` 跨上下文读裸枚举**：`s_state` 非 atomic，按钮回调上下文与 timer 回调/后台任务间存在理论竞态（与 page_sync 的 `s_displaying` 同级风险，本固件既有风格）。最坏情况是重复请求被 FETCHING 守卫挡下，无内存安全问题。
3. **CONFIRM 键定义变化**：`kBoardConfirmButtonGpio` 由 `BOOT_BUTTON_GPIO` 改为 `CONFIRM_BUTTON_GPIO`，两者同为 GPIO_NUM_0，无行为变化，仅语义对齐。
4. **按钮回调线程上下文**：notify 公开函数假定从主按钮回调上下文调用（与 page_sync 一致）。若未来从 ISR 直接调用需改 xTimerStop/xTaskCreate 的 FromISR 变体。
5. **未做真机验证**：本任务只验证编译链接通过；BOOT 拉取→位图显示→上/下键 ack→dismiss 恢复的端到端行为需真机或后续集成测试验证。

---

## Review Round 1 修复（commit `a9eb393`）

**Status**: FIXED — build success（`xiaozhi.bin` 0x2bbc40 bytes，0 undefined references）
**Commit**: `a9eb393b2db7254a502dadee9e2eb47132c474ab`，message: `fix(firmware): notify review fixes — 204 state reset, deferred timeout dismiss, ack id race, canvas exit on BOOT long-press`

### Important × 3

1. **204 分支状态泄漏（notify.cc fetch_task）**：`status == 204`（无待办通知）分支原先只记日志，不重置 `s_state`，模块永久卡在 FETCHING，后续 BOOT 拉取全部被 `if (s_state != IDLE) return` 挡下。修复：204 分支补 `s_state = NotifyState::IDLE;`。
2. **timer 回调做显示工作（timeout_timer_cb）**：原实现 5min 超时回调直接在 FreeRTOS timer daemon task 上下文调 `dismiss_locked_state()`，内含 `xSemaphoreTake` + page_sync 重绘——timer 回调不得阻塞。修复：`timeout_timer_cb` 只检查状态并 `xTaskCreate(timeout_dismiss_task, 4096B 栈)`，真正的 dismiss 在新任务上下文执行。
3. **ack 读空 id 竞态（notify_post_ack / ack_task）**：原实现 `notify_post_ack` 先 spawn ack_task（其读取全局 `s_notification_id`）再 `notify_dismiss()`（清空该全局），任务调度时序不定，ack POST 可能发到 `/api/notifications//ack`。修复：id 与 body 一并按值复制进单个 PSRAM 块（前 40B id + 后 64B body）作为任务参数，ack_task 不再读全局；dismiss 清空全局不再影响 ack。

### UX ruling（Main 裁决）

**BOOT 短按 = 拉取通知（spec 强制），画板退出移至 BOOT 长按。** 检查确认：原 `OnBootLongPress` 并未处理画板退出（只有 WiFi 配网退出 + rawdraw 分发，而 rawdraw 的 kBootLongPress 分支只处理 AP transfer/Gallery）。已在 `OnBootLongPress` 的 rawdraw 分发前新增分支：`page_sync_is_displaying()` → `page_sync_stop_display()` + `SwitchPage(Gallery)`。WiFi 配网模式的优先级保持在画板退出之前。

### Minor

- **s_lcd 绑定时机**：`show_bitmap()` 增加懒绑定兜底（`Board::GetInstance().GetDisplay()`），即使 `notify_init` 未跑过也不会静默无显示。

### Build output（修复后）

```
[8/10] Linking CXX executable xiaozhi.elf
Successfully created ESP32-S3 image.
xiaozhi.bin binary size 0x2bbc40 bytes. Smallest app partition is 0x3f0000 bytes. 0x1343c0 bytes (31%) free.
Project build complete.
```
