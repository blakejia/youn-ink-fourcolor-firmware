# Task 4 Fix Round 1 Review: 固件 notify 模块修复复查

- **Reviewer**: Task4FixRound1Reviewer
- **Date**: 2026-09-02
- **Commit under review**: `a9eb393` (`fix(firmware): notify review fixes — 204 state reset, deferred timeout dismiss, ack id race, canvas exit on BOOT long-press`)
- **Review method**: 对比原 task-4-review.md 的 4 项 findings 与 fix round 1 diff（`task-4-fix-round-1-review-package.txt`），并对照磁盘上的实际源码（`firmware/main/common/notify.cc`、`firmware/main/application.cc`、`firmware/main/common/page_sync.cc`、`firmware/main/ui/rawdraw_ui_manager.cc`）逐一验证。不重新构建（project-wide validation 由 main agent 负责；report 提供了干净链接的构建日志）。

## Findings 状态

### 1. 204 分支永不重置 `s_state` — **ADDRESSED** ✅

Diff: `fetch_task` 的 `if (status == 204)` 分支新增 `s_state = NotifyState::IDLE;`（notify.cc:169）。

- 磁盘源码确认（notify.cc:168-169）：
  ```cpp
  if (status == 204) {
      ESP_LOGI(kTag, "no pending notification (204)");
      s_state = NotifyState::IDLE;
  }
  ```
- 204（最常见的「无待办」路径）现在正确回到 IDLE，后续 `notify_request_next()` 不再被 `if (s_state != IDLE) return;` 永久挡下。原 review 的一行修复按承诺落地。✅

### 2. `timeout_timer_cb` 在 timer daemon 上下文做显示工作 — **ADDRESSED** ✅

Diff: 原 `timeout_timer_cb` 直接调 `dismiss_locked_state()`（内含 `xSemaphoreTake` + page_sync 重绘）。修复拆分为：
- `timeout_timer_cb` 只做两件事：检查 `s_state == NOTIFYING`、`xTaskCreate(timeout_dismiss_task, 4096, …)`（notify.cc:95-101）。timer 回调不再阻塞。
- `timeout_dismiss_task` 在新任务上下文里执行 `dismiss_locked_state()` + `vTaskDelete(nullptr)`（notify.cc:88-93）。

- 磁盘源码确认（notify.cc:88-101）与 diff 一致。修复方向正确：dismiss 的显示工作（framebuffer mutex + `RequestUrgentFullRefresh` + `page_sync_prev/next`）现在跑在独立短任务中，符合 FreeRTOS「timer 回调不得阻塞」的约束。
- 任务创建失败仅记日志——timer 是一次性（`pdFALSE`），失败即丢失本次超时 dismiss。这是可接受的降级：模块仍可经 BOOT 短按/ack 路径 dismiss；5min 后用户手动 dismiss 兜底。非新问题。

### 3. ack POST 竞态到空 id — **ADDRESSED** ✅

Diff: `notify_post_ack` 把 id + body 一并按值复制进单个 PSRAM 块（前 40B id + 后 64B body），作为 `ack_task` 的任务参数；`ack_task` 不再读全局 `s_notification_id`（notify.cc:205-225）。

- 磁盘源码确认（notify.cc:205-225）：
  ```cpp
  char* block = static_cast<char*>(heap_caps_malloc(40 + 64, MALLOC_CAP_SPIRAM));
  ...
  snprintf(block, 40, "%s", s_notification_id);
  snprintf(block + 40, 64, "{\"decision\":\"%s\"}", decision);
  if (xTaskCreate(ack_task, "notify_ack", kFetchTaskStack, block, 3, nullptr) != pdPASS) { ... }
  notify_dismiss();  // 清空全局，不再影响 ack
  ```
- `ack_task` 内 `const char* id = block; const char* body = block + 40;`，`path` 用复制来的 id 构造（notify.cc:216），随后 `heap_caps_free(block)` 释放整个块。id 与 body 的生命周期都由任务持有，dismiss 清空全局不再影响已派发的 ack POST。原竞态根除。✅

### 4. UX Ruling — BOOT 短按拉取、画板退出移至长按 — **ADDRESSED** ✅

Diff: `OnBootLongPress` 在 rawdraw 分发前新增画板退出分支（application.cc:506-513）。

- 磁盘源码确认（application.cc:506-513）：
  ```cpp
  // 画板显示时 BOOT 长按退出画板，回到 Gallery（短按已改为拉取通知）
  if (page_sync_is_displaying()) {
      page_sync_stop_display();
      if (rawdraw_ui_manager_) {
          rawdraw_ui_manager_->SwitchPage(ui::RawDrawPageId::Gallery);
      }
      return;
  }
  ```
- 优先级：WiFi 配网模式退出（`IsConfigMode()`）保持在画板退出之前——配网是更强约束，正确。
- `SwitchPage` 会做 full clear + `RenderAll` 重绘（rawdraw_ui_manager.cc:471-503 确认），与 WiFi 分支使用的同一退出模式，画板退出后 Gallery 正常主导屏幕。BOOT 短按不再独占画板退出（OnBootClick 的 notify 分支是 spec 强制的），长按补上了唯一退出路径，UX ruling 完整落地。✅

## New breakage

**无新破坏。** 逐项核查：

1. **内存**：ack PSRAM 块（104B）在任务创建失败路径 `heap_caps_free(block)`（notify.cc:224），成功路径由 `ack_task` 末尾 `heap_caps_free(block)` 释放（notify.cc:228）。两条路径都覆盖，无泄漏。id/body 的 `snprintf` 均带长度上限，无溢出。
2. **竞态**：ack id 现在按值传递，与原 review 指出的「读清空后全局」竞态正交。dismiss 时序（`notify_post_ack` 末尾无条件 `notify_dismiss()`）与修复前相同，无回归。
3. **timer 使用**：`timeout_timer_cb` 现在只做状态检查 + 任务派发，无阻塞。`timeout_dismiss_task` 栈 4096B 足以容纳 dismiss 路径（page_sync 函数调用链不引入大栈对象；`show_page` 的位图操作在 PSRAM 上，不动栈）。timer 一次性 + `xTimerStop` 在 dismiss 路径中照旧，无双重 dismiss。
4. **跨上下文 `s_state`**：`s_state` 仍是非原子枚举，由 button 任务、fetch 任务、timer 回调/task 读写——与原 review 的 Minor #3 相同的既存风险等级，未因本轮修复放大（三个 writer 不变；最坏情况是被 FETCHING/NOTIFYING 守卫挡下，无内存安全问题）。报告 Concerns #2 已如实记录。
5. **BOOT 长按画板退出**：`page_sync_stop_display()` 后 `SwitchPage(Gallery)` 全量重绘，与 WiFi 分支同一模式，无残留画板状态。若当前已停在 Gallery（`current_page_ == Gallery`），`SwitchPage` 提前 return（rawdraw_ui_manager.cc:472-474）——画板本就在 Gallery 之上显示，stop_display 已把控制权交还 UI，行为正确。

## Verdict: **all findings addressed — approve**

4 项原 findings 全部 ADDRESSED，且有磁盘源码佐证；未发现新破坏（内存/竞态/timer 使用均核查无误）。既存 Minor 项（`s_state` 非原子、`notify_deinit` 不防并发、`s_lcd` 走 Board 单例）属于任务一已记录的低风险风格问题，不在本轮修复范围内。端到端真机行为（BOOT 拉取 → 位图显示 → 上/下键 ack → dismiss 恢复）仍未真机验证——本轮 diff 的 4 处改动均为小范围、可静态核实的修复，建议作为后续集成冒烟测试的一部分，但不应阻塞本轮合入。
