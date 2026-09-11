# 休眠与省电重新设计

日期：2026-09-11
状态：待实现
涉及：`server/youn_server/`、`firmware/main/`、`firmware/main/rust/`、`firmware/main/components/78__esp-wifi-connect/`

## 1. 目标与已定决策

把设备从"保持联网待机 + 空闲 30 分钟深睡"改成**占空比**模型，并在插着 USB 时保持可调试。

| # | 决策 | 选择 |
|---|---|---|
| D1 | 插着 USB 时 | **完全不睡**（串口全程在线），并支持插 USB 自动唤醒 |
| D2 | 电池待机形态 | **占空比轮询**：深睡 → 定时醒 → 连 WiFi → 同步 → 必要时绘屏 → 关射频 → 再睡 |
| D3 | 轮换真相源 | **服务端权威**：`/api/pages/schedule` 给出"现在该显示第几页"和"距下次翻页多少秒" |
| D4 | 唤醒时通知 | **每次唤醒都拉** `/api/notifications/next`，有待确认即展示 |

### 非目标（明确不做）

- 语音唤醒期间的省电——睡眠时麦克风与音频轨本就断电，无法唤醒。
- NFC（`power_gpio=21`）省电。当前从无关闭代码，单独评估。
- `esp_pm` 自动浅睡 / 动态调频——与本方案的显式深睡冲突。
- 服务端下发静态 IP。
- 局域网 HTTP 文件传输（该项已在早前移除）。

## 2. 为什么不是浅睡（背景事实，不是设计选择）

- **深睡**：USB-Serial-JTAG 属数字域，断电 → 主机侧 `USB disconnect`。IDF 不提供 USB 唤醒源。
- **浅睡**：`SOC_USB_SERIAL_JTAG_SUPPORT_LIGHT_SLEEP` 在 `esp32s3/include/soc/soc_caps.h` 中**未定义**，因此 `esp_hw_support/sleep_console.c` 会编译 `sleep_console_usj_pad_backup_and_disable()`，主动关掉 USJ pad。S3 上浅睡同样丢串口。

结论：保住串口只能不睡 SoC。故 D1 采用"插电不睡"，而不是浅睡。

## 3. 服务端

### 3.1 `/api/pages/schedule` 新增两个字段

```json
{ "schedule_md5": "…", "server_time": "…", "screen_active": true,
  "policy": { … },
  "current_index": 1,
  "seconds_until_next_page": 240,
  "pages": [ … ] }
```

计算（`pages.py` 新增纯函数 `schedule_position(entries, now_ts)`）：

```
cycle_s = Σ max(e.duration_minutes, 1) * 60
若 entries 为空 → current_index = 0, seconds_until_next_page = null
pos_s = int(now_ts) % cycle_s          ← 秒级，不是分钟级
按 entries 顺序累加 duration，找到 pos_s 所在区间 → current_index
seconds_until_next_page = 区间结束秒 - pos_s
```

- **秒级而非分钟级**（2026-09-11 修正）：字段名与用途都是秒。分钟量化会把剩余时间系统性放大到下一个整分钟（永远返回 60 的倍数，最坏晚 59 秒翻页）；秒级在剩余 ≥ 60 秒时精确，< 60 秒时被固件侧 60 秒下限兜住，最坏同为晚 59 秒但平均更准。
- 求和前仍夹到 `≥ 1` 分钟：这是为了容忍盘上可能已存在的 0 分钟条目（保存路径本来就会拒绝，见 §3.2），保证本函数是全函数。
- `schedule_md5` 仍只哈希 `(md5, duration, order)`，新字段不参与 → 设备的"排期未变"快路径与服务端既有测试都不受影响。

### 3.2 保存校验：保持不动

`POST /api/pages` **本来就会**用 `settings.canvas_min_page_duration_minutes` 下限校验，低于下限返回 400（`test_min_duration_validation` 覆盖）。本节原先写的是"改成静默夹取"——那个前提是错的（我误以为保存时没校验），静默修正用户输入比明确拒绝更糟，且会推翻一个既有且已测的契约。**保持 400 校验与原测试不变。**

`schedule_position` 里的 `max(duration_minutes, 1)` 与保存校验不是一回事：它只负责让函数对盘上任何历史数据都成立，不改变写入行为。

## 4. 固件：电源状态与唤醒路径

### 4.1 启动路径二选一

用 `esp_sleep_get_wakeup_cause()` 判定：

| 条件 | 路径 |
|---|---|
| `ESP_SLEEP_WAKEUP_TIMER` | **quiet**：不创建 `RawDrawUiManager`（因此状态栏不更新，屏上只有画板内容本身）、不进配网页、不做面板上电初始化 |
| 其它（`EXT0`=BOOT / `EXT1`=充电插入 / 上电 / 软件复位） | **interactive**：现有完整启动（UI、生命周期、配对、首次绘屏） |

`ESP_RST_DEEPSLEEP` 且 cause 为 TIMER 时才是 quiet；上电复位（拔电重插）一律 interactive。

### 4.2 三条电源形态

- **kMains**：`ChargeStatus::Get().power_present == true`。永不深睡，不装载休眠定时器。串口在线，等同当前行为。
- **kInteractive**：用户主动唤醒（BOOT / 充电插入）或冷启动后的状态。有按键活动即续期，空闲超过 `power:idle_grace_min`（默认 3 分钟）后转入 kDutyCycle。
- **kDutyCycle**：电池供电的静默轮询。

### 4.3 允许入睡的条件（全部满足）

1. `!ChargeStatus::Get().power_present`
2. `SleepManager::CanSleepNow()`（现有：lifecycle == SyncIdle && busy_mask == 0 && hold_count == 0 && 到活动 deadline）
3. `!notify_is_active()`
4. 距最后一次用户活动 > `power:idle_grace_min`
5. **屏幕归属**：若当前屏不是画板（如在 Settings），可以睡，但必须作废 RTC 面板记录（见 5.2），使下次唤醒强制重绘画板——否则会出现"RTC 说是画板、屏上其实是 Settings"的分叉。

### 4.4 睡多久

```
next_wake_s = min(seconds_until_next_page, cap)
cap = screen_active ? policy.poll_interval_minutes*60 : policy.sleep_poll_interval_minutes*60
下限 60 秒（防服务端返回 0 导致热循环）
```

`seconds_until_next_page` 为 null（空排期）时 `next_wake_s = cap`。

固件侧的交接约定：固件从 page_sync 取这个值时用 **-1 表示未知/空排期**，再由 `power::decide` 把负数映射成 `None`（用 cap）。不能用 0——0 会被当成“还剩 0 秒”而夹到 60 秒下限，空排期就永远睡不满 cap。

同步失败（网络或服务端不可达）：不绘屏，退避 `60s → 120s → …` 翻倍，上限 `cap`；成功后重置退避。

### 4.5 一次 wake cycle 的顺序

1. （quiet）跳过 UI/配网初始化
2. `esp_wifi` 连接（沿用现有 `WifiManager`，含 3.4 的快速重连缓存持久化）
3. `page_sync_sync_once()`：拉 `/api/pages/schedule`
4. `notify_request_next()`：拉 `/api/notifications/next`
5. `page_sync_paint_if_changed()`：**仅当**目标页 md5 ≠ RTC 记录中屏上的 md5 时才绘屏
6. 若通知待确认：展示，等待 5 分钟 TTL 或 UP/DOWN/BOOT 交互；此期间不睡
7. 关射频（`esp_wifi_disconnect` + `esp_wifi_stop`）、断音频轨、必要时断 EPD 轨
8. `esp_sleep_enable_timer_wakeup(next_wake_s)` + `esp_sleep_enable_ext0_wakeup(BOOT, 0)` + `esp_sleep_enable_ext1_wakeup(充电检测脚)` → 深睡

`next_wake_s` 在步骤 3 之后即确定，步骤 5/6 可能延长本次清醒（通知在屏时不睡），不影响下次唤醒时刻的计算基准。

## 5. 固件：面板内容记忆

### 5.1 RTC 记录

`shim.cpp` 中声明（ESP-IDF 专属细节留在 shim 层）：

```cpp
RTC_DATA_ATTR static struct {
    uint32_t magic;
    bool     valid;
    char     displayed_md5[33];
    int      displayed_index;
    char     schedule_md5[33];
} g_panel_rec;
```

Rust 侧通过 `rf_panel_record_*` 访问。`RTC_DATA_ATTR` 跨深睡与软件复位保留；上电复位丢失（magic 失效 → 强制重绘）。

### 5.2 写入时机（防"记成画完了但实际中断"）

- 绘屏前：`rf_panel_mark_pending(md5, index)` 记下"正在请求画这张"。
- 刷新完成：shim 已注册 `CustomLcdDisplay::SetOnRefreshIdle` 回调，在其 trampoline 里把 pending 提交进 `g_panel_rec`（`valid = true`）。
- 作废：`rf_panel_record_invalidate()`，用于 4.3 第 5 条与冷启动。

### 5.3 绘制决策

`paint_if_changed()`：`g_panel_rec.valid && strcmp(displayed_md5, target_md5) == 0` → 不绘屏，直接返回 false。否则下载位图（若未缓存）并全屏绘制。

## 6. 固件：惰性显示初始化（最大的一块）

### 6.1 问题（实测证据）

`CustomLcdDisplay` 构造函数中：

```
EPD_Init() → EPD_Clear()（全屏刷白）→ EPD_Display()
```

对应启动日志：

```
W (5674)  CustomLcdDisplay: EPD busy wait: 5000 ms
W (10674) CustomLcdDisplay: EPD busy wait: 10000 ms
W (15674) CustomLcdDisplay: EPD busy wait: 15000 ms
I (20294) Application: State 0 -> 1
```

显示对象在板级 `Initialize()` 里 `new CustomLcdDisplay(...)`，即**每次深睡唤醒先白刷约 20 秒**才开始连 WiFi。按 10 分钟一页计，144 次/天 × 20 秒 ≈ 48 分钟纯白刷，另有 144 次可见闪白。

### 6.2 改法

构造函数拆两半：

- **分配/绑定**（总是做）：framebuffer、mutex、`start_refresh_task()`、字段初始化。
- **面板上电初始化** `BringUpPanel(bool blank)`：
  - `blank == true`（冷启动）：`EPD_Init()` + `EPD_Clear()` + 首次 `EPD_Display()`，并置 `prev_buffer_synced = true`。
  - quiet 路径**不调用**；首次真正绘屏时由 `refresh_task_loop` 的两个分支各自调用 `EPD_Init()`（现有行为），`prev_buffer_synced == false` 使首帧走 FULL。
- **绘屏入口的责任**：`page_sync_paint_if_changed()` 在发出刷新前必须确保面板可用——Stage 1 只需保证显示对象已构造；Stage 2 起还要 `PowerEpdOn()`（EPD 轨在睡前被切断）。这条不能依赖调用顺序碰巧成立。

### 6.3 风险

构造函数与 `Board::Initialize()` / `Application::Initialize` / UI 建 framebuffer 的顺序纠缠，需要把启动路径正式拆成 quiet / interactive 两个变体。这是本次改动最重、最容易引入回归的一块。

## 7. 固件：供电轨

- 睡前：`PowerAudioOff()` + `PowerAmpOff()` + LED 灭。
- 唤醒需要绘屏：`PowerEpdOn()`；要放音：`PowerAudioOn()`。
- 守卫：`IsRefreshPending()` 为真时**绝不**切 EPD 轨。
- **EPD 轨分两阶段**：Stage 1 只断音频/功放/LED；EPD 轨断电放到 Stage 2，做完真机验证矩阵第 8 条再启用。
- 现状记录：`PowerAudioOn()` / `PowerEpdOn()` 开机调用后再无任何 `*Off()` 调用点，`PowerEpdOff` / `PowerAudioOff` / `PowerAmpOff` 全树零调用。

## 8. 固件：快速重连缓存持久化

`main/components/78__esp-wifi-connect/wifi_station.cc` 中 `static FastRcCache g_fast_cache;` 是纯 RAM 静态，深睡唤醒即丢失，所以日志里每次都是 `FAST_RC: … have=0 … reason=cache_miss`（即使 `fast_enable=1`）。

改法：加一份 **POD 镜像**存 `RTC_DATA_ATTR`（定长 char 数组 + 整型；`FastRcCache` 内含 `std::string`，不能直接放 RTC），读写时在 POD 与现有成员之间转换。预期每次唤醒的 WiFi 建连由 4-6 秒降到 1-2 秒。

## 9. 固件：`page_sync` 的收敛

移除：

- 轮询任务循环（`task_entry` 的 `loop`）与其节拍逻辑 `plan_tick`、`TICK_MS`、`POLL_MS`。
- 本地轮换状态与数学：`started_us`、`duration_s` 比较与自动推进（改为服务端 `current_index` 驱动）。
- 上一轮新增的 4 个 `plan_tick` 节奏测试（描述的是被移除的机制）。

保留：

- 调度拉取与解析、位图按 md5 缓存/复用、PSRAM 槽位管理。
- 屏幕归属（`SUSPENDED` / `stop_display` / `allow_display`）——交互模式下 UI 仍会接管屏幕。
- `server_reachable()`（状态栏 server 指示）。
- 手动翻页：交互模式下 UP/DOWN 仍触发 `page_sync_next()/prev()`，语义改为**本地覆盖**：`next()`/`prev()` 在当前服务端索引的基础上做 ±1，写进 `current` 并置 `override = true`；`override` 为真时渲染决策用该页的 md5，且**下一次成功同步清除 override**（重新服从服务端索引）。这样手动翻页不会与服务器排期互相扯。

新增 C ABI：

```c
bool     page_sync_sync_once(void);        // 拉取+解析+下载，返回是否成功
bool     page_sync_paint_if_changed(void); // 与 RTC 记录比对后决定是否绘屏
uint32_t page_sync_next_wake_s(void);      // 上次响应推导的下次唤醒秒数（已夹取）
```

`notify` 模块不变，仍复用 5 分钟 TTL 与 UP/DOWN 确认。

## 10. 配置

| 键 | 默认 | 含义 |
|---|---|---|
| NVS `power:idle_grace_min` | 3 | 交互模式下最后一次用户活动后的清醒宽限 |
| NVS `power:max_sleep_min` | 取 policy cap | 单次深睡上限 |

- 旧 NVS `sync:sync_interval`（默认 30 分钟）不再参与休眠；键保留但不再读取（避免迁移逻辑）。
- 删除死宏 `CHARGE_GPIO_AFFECT_SLEEP`（当前定义但零使用），行为直接实现：插电不睡 + `CHARGE_DETECT_GPIO`(GPIO2) 进 `ext1` 唤醒掩码。GPIO2 在 S3 上属 RTC 可用脚（RTC_GPIO2）。
- 唤醒电平需按 `CHARGE_DETECT_CHARGING_LEVEL`（0 = 充电为低）与实测空闲电平确定，实现时用万用表/日志确认，不要照抄假设。

## 11. 测试

### 服务端 pytest（`server/tests/test_schedule_api.py`）

- 2 页（10 + 5 分钟）循环内各边界点的 `current_index` / `seconds_until_next_page`（monkeypatch 时钟）。
- 单页、空排期（`seconds_until_next_page == null`）。
- `duration_minutes = 0` 被夹到 1（循环非零）。
- 新字段不影响 `schedule_md5`（同一排期加/不加新字段哈希一致）。
- 既有 `test_min_duration_validation`（低于下限返回 400）保持不动，仍须通过。

### Rust 单测（`firmware/main/rust/src/`）

- 绘制决策：目标 md5 == RTC 记录 → 不画；不等 → 画；`valid == false` → 画。
- 手动覆盖：`next()` 后目标变为 ±1 页且不服从服务端索引；下一次成功同步把 `override` 清掉并回到服务端索引。
- 唤醒间隔：`min(seconds_until_next_page, cap)`、60 秒下限、窗口内外取不同 cap、`null` 时取 cap。
- 失败退避：60s 翻倍到 cap，成功后重置。
- 唤醒时通知决策：有待确认 → 展示且不睡；无 → 睡。
- 删除 `plan_tick` 相关 4 个测试。

### 真机验证矩阵（需按键/拔插，串口日志取证）

| # | 场景 | 期望 |
|---|---|---|
| 1 | 插电 35 分钟 | 不掉线（证明"插电不睡"压过定时器） |
| 2 | 拔电后静默周期 | 一次 timer 唤醒 + 一次同步 + **无** `[REFRESH]` |
| 3 | 到翻页点的唤醒 | **恰好一次** `[REFRESH] Performing FULL refresh: reason=canvas` |
| 4 | 插 USB（不按键） | 串口出现，`wakeup cause=ext1` |
| 5 | 唤醒时存在待确认通知 | 弹窗上屏 |
| 6 | 按 BOOT 唤醒 | 交互模式 + 3 分钟宽限（日志可证） |
| 7 | 冷启动（拔电重插） | 必定重绘 |
| 8 | Stage 2：EPD 轨断电后 | 下一轮绘制无花屏 |

省电幅度只能靠外部 USB 功率计实测（板子无电流计，`ChargeStatus` 只有充电/满/无电池三态）。

## 12. 分阶段落地

| 阶段 | 内容 | 验证 |
|---|---|---|
| Stage 1 | 服务端字段 + quiet/interactive 拆分 + RTC 面板记录 + 惰性显示初始化 + 插电不睡/ext1 + 音频轨断电 | 服务端 pytest、Rust 单测、真机 1-3、6-7 |
| Stage 2 | EPD 轨断电 | 真机 8，且需连续多轮无花屏 |
| Stage 3 | 快速重连缓存持久化 | 日志从 `cache_miss` 变为命中，唤醒时长下降 |

Stage 1 完成即应满足 D1-D4 的功能语义；Stage 2/3 是省电幅度优化，可独立延期。

**实现计划的范围是 Stage 1。** Stage 2（EPD 轨）与 Stage 3（快速重连缓存）各自需要独立的计划与真机验证，不在同一份计划里。

## 13. 剩余风险

- **WiFi 建连时间主导能耗**。Stage 3 是缓解而非根治；若建连仍 >6 秒，需要另案（静态 IP 或改用更轻的传输）。
- **EPD 轨断电的长期可靠性无数据支撑**，故放 Stage 2 并要求连续多轮验证。驱动每轮刷新都会 `EPD_Init()`，但面板电荷泵重新上电的时序未经验证。
- **惰性初始化改动面最大**（构造顺序、`prev_buffer_synced` 初值、UI/framebuffer 建構顺序），quiet 路径若漏建某个依赖会在第一次绘屏时崩溃。真机第 2/3 条正是为了压这一块。
- **服务端不可达时无法获得 `current_index`**：设备保持屏上原图并退避重试，期间不翻页。
- **D4 的代价**：一次"有待确认通知"的唤醒最多清醒 5 分钟（等 TTL 或人按）。若唤醒间隔 10 分钟，等于每次有通知就把射频开启时间从约 8 秒拉到 300 秒。这是 D4 的直接后果，接受它换来的是"通知延迟 = 唤醒间隔"。
