# NOTE4C 固件策略逻辑 Rust 迁移设计

日期：2026-09-24
状态：设计已确认，等待用户复核文档
范围：charge_status、LED 策略、Wi-Fi 快连/重连与 endpoint/OTA、配对响应分类、EPD diff/刷新资格

## 1. 目标与原则

把 C++ 中可脱离硬件验证的业务规则迁移到 Rust，同时保留 C++/ESP-IDF 的机制层。目标是让复杂规则能在 host 上用 `cargo test` 验证，不改变无关的硬件行为和固件架构。

统一分工：

```text
Rust：纯规则、状态决策、解析、缓存编解码、Action 选择
C++：GPIO、I2C、SPI、NVS、FreeRTOS、Wi-Fi driver、任务和硬件执行
FFI：只传事实和动作，不暴露 Rust 内部状态
```

原则：

- 不把 ESP-IDF 或硬件 API 直接搬进 Rust。
- C++ 继续拥有实际状态存储、任务生命周期和副作用执行。
- Rust 不保存跨任务的隐式全局状态；跨轮状态由 C++ 以 POD 快照传入。
- 使用稳定的、purpose-named C ABI；不让 C++ 直接依赖 Rust 内部类型。
- 迁移优先逐格保持旧行为；只有已有真机/日志证据支持的问题才允许顺带修复。
- 每个迁移批次独立提交，可单独回滚。

## 2. 当前边界

当前已 Rust 化的策略和状态包括：

- `input.rs`：按键/手势优先级和动作选择。
- `lifecycle.rs`：生命周期转换合法性。
- `power.rs`：睡眠/清醒策略和射频恢复规则。
- `pairing.rs`：配对时序、退避、放弃策略。
- `settings.rs`：设置菜单结构、导航和确认规则。
- `page_sync.rs`：页面同步、缓存、轮换和绘制协调。
- `notify.rs`：通知状态机、HTTP 拉取协调和显示协调。

C++/ESP-IDF 仍负责：

- 启动、应用总编排和任务生命周期。
- Wi-Fi driver、AP 配网页面、NVS、HTTP transport。
- EPD framebuffer、SPI/GPIO、波形和 refresh task。
- ADC、GPIO、I2C、RTC、充电检测和电源轨。
- UI renderer、RawDraw、音频和 streaming 机制。

`firmware/main/rust/shim.cpp` 是 Rust 访问 ESP-IDF/硬件的实现面；`firmware/main/rust/include/*.h` 是 C++ 调 Rust 的稳定 ABI 面。

## 3. 迁移顺序

```text
阶段 0：定位 rr=4 panic
阶段 1：charge_status 状态机
阶段 2：LED 策略
阶段 3：Wi-Fi cache + 快连/重连/endpoint/OTA 策略
阶段 4：配对响应分类
阶段 5：EPD diff/刷新资格
```

阶段 0 的诊断可以单独结案，但 EPD 迁移只有在取得正向因果归因后才开始；
若仍无法归因，EPD 迁移保持阻塞，除非另有明确批准的豁免。其他阶段按顺序独立
推进，任何阶段出现回归都停在该阶段回滚。

用户已裁定的集成约束：

- Wi‑Fi 使用组件 shim 回调，不新建第二个 Rust staticlib。
- EPD diff 在 `dirty_mutex` 外对已完成的只读 snapshot 执行。
- rr=4 采用诊断先行，不预先增加新的 panic 元数据协议。

## 4. 阶段 0：rr=4 panic 诊断

`ESP_RST_PANIC=4` 只说明发生过 panic/abort，不区分 C++ `assert`、ESP-IDF fatal
check、Rust panic。当前证据显示 bitmap 成功后的 EPD 刷新路径是优先调查对象，
但尚未定位到具体行。

阶段 0 不改变 EPD 刷新行为。先复核：

- `custom_lcd_display.cc` 的 SPI init/add-device、SPI polling transmit、EPD busy/状态和 framebuffer 分配。
- bitmap 成功到下一次 `schedule` 缺失之间的 EPD refresh 状态。
- 是否可以通过不改业务逻辑的持久化 panic 元数据，在下一次启动携带有限诊断字段。

优先使用现有轮询参数和 RTC 保留区；完整 backtrace 不直接塞入 schedule query。
若需要新增诊断字段，字段数量和长度固定，且独立提交、可整体回滚。

诊断阶段的结束条件：

- 能解释当前两次 `w=1&rr=4`，并把归因记录为 EPD、C++ 其他机制、Rust panic
  或其他明确类别；或
- 诊断工作已记录复现步骤、已排除范围和下一次诊断动作，但 EPD 迁移仍被阻塞。

第二种情况只允许关闭诊断任务，不授权 EPD policy 迁移，除非另有明确批准豁免。
在 EPD 迁移获得正向归因或豁免前，不改变 EPD 刷新资格。

## 5. 阶段 1：charge_status

### Rust 负责

新增：

```text
firmware/main/rust/src/charge_policy.rs
firmware/main/rust/include/charge_policy.h
```

Rust 纯决策输入为：

- 当前时间 `now_ms`。
- 经过板级极性归一化后的 `detect_charging`、`full_high` 布尔事实。
- 归一化条件起始时间，以及最近一次电源、detect、full 事件时间。

`charge_status.cc` 当前不是把物理高电平直接传给状态机：detect 先与
`CHARGE_DETECT_CHARGING_LEVEL` 比较，而该常量当前为 0，即物理低电平表示
charging。C++ 继续负责 GPIO 读取和这层极性归一化；Rust 收到的是已归一化的
`detect_charging=true/false`，不能按字段名自行假定物理高电平。测试必须覆盖
当前低电平充电、高电平未充电的极性边界，并锁定 C++ shim 的归一化契约。

Rust 输出为：

- `NoPower` / `Charging` / `Full` / `NoBattery`。
- `power_present`、`charging`、`full`、`no_battery`。
- 下一轮保留的时间戳。

C++ 继续维护 `ChargeStatus` 实例、原子发布 snapshot、触发 callback，并被
`BoardPowerBsp`、电量和应用层消费。必须保持既有时间窗口、状态编码、callback
时机、`IsPowerPresent()` 和电池上报编码不变。

## 6. 阶段 2：LED 策略

### Rust 负责

新增：

```text
firmware/main/rust/src/led_policy.rs
firmware/main/rust/include/led_policy.h
```

迁移现有 `led_decide` 的纯决策：

输入：override 标志、blink 标志、phase、charge snapshot、activity pulse 数。

输出必须保留当前 C++ `LedAction` 的结构，而不能只靠等待时长推断：

- GPIO `level`。
- `first_ms`。
- `has_second`。
- `second_level`。
- `second_ms`。
- `consume_pulse`。
- `first_wait_notify` 与 `second_wait_notify`，显式表示 `vTaskDelay` 或 `ulTaskNotifyTake`。

## 7. 阶段 3：Wi-Fi cache、快连/重连、endpoint/OTA

### Rust 负责

新增：

```text
firmware/main/rust/src/wifi_policy.rs
firmware/main/rust/include/wifi_policy.h
```

Rust 内部拆成四部分，不额外制造第二套 endpoint ABI：

```text
cache_codec
fast_connect
reconnect
endpoint_and_ota
```

Rust 接管：

- cache magic、固定布局和 SSID/BSSID/channel 校验。
- cache age 和有效性判断。
- 直接连接、probe、扫描、重试、停止的选择。
- Wi-Fi association cache 的清空决策。
- IP fast cache 的保留/清理决策。
- endpoint/OTA URL 缺失时的决策。

### Wi-Fi 组件 shim 回调

`78__esp-wifi-connect` 是独立 ESP-IDF component，不能直接调用 main 的 Rust
staticlib，也不复制第二套 Rust library。组件新增窄的 C ABI 注册/注销层，main
在初始化和退出边界注册 Rust policy 回调：

```text
component shim 注册/注销
        ↓
main adapter 持有回调上下文
        ↓
rust/wifi_policy 的固定 POD 输入/Action 输出
```

回调契约必须是同步、不可阻塞、不可回调组件、不可直接访问 Rust 内部状态；
只返回决策和需要组件执行的 Action。main adapter 在 Application/Board 初始化
完成后注册，在 Wi-Fi manager 停止/销毁边界注销；注销幂等。注册成功、注册失败、
注销幂等和未注册调用都必须有 C++ 编译级契约。

组件仍负责 `esp_wifi_*`、`esp_netif_*`、NVS、RTC_DATA_ATTR、DNS/ARP/TCP、
mutex、event group 和异步 callback；main adapter 负责把实际采样事实传入 Rust。


### endpoint_missing 修复与 cache 语义

当前代码没有独立的 endpoint cache：MQTT/WebSocket/OTA 配置每次从 NVS 或编译配置
读取；`IpFastFallback()` 会清除 RAM IP fast cache，但不会清除 `g_fast_cache` 中
的 Wi-Fi association cache。

修复后的决策必须区分“endpoint 配置缺失”和“IP/ARP/GW/TCP 探测失败”：

```text
endpoint_missing
→ Rust 返回 DeferProbe { retain_ip: true }
→ C++ 停止本次 IP fast attempt，恢复 DHCP
→ 保留 IP fast cache，但本轮不继续 probe
→ 保留 association cache 和 RTC mirror
→ 不把缺失 endpoint 当作 DNS/TCP 失败

endpoint 配置之后恢复
→ 由下一次既有 STA connect/fast-attempt 触发重新读取 NVS/config
→ endpoint 有效且 IP cache 仍新鲜时，允许再次 probe
→ 不新增隐式后台重试；同一轮不要求配置热更新触发即时 probe
```

其他 probe 失败（IP invalid/stale、ARP conflict、GW 不可达、DNS/TCP 失败）仍按
旧行为清理 IP fast cache。SSID/BSSID 变化、明确的配置失效仍可清理 association
cache。该修复必须先有 host 红灯测试，再修改 C++ 调用点；不能只删除旧 C++ 分支
后重新编译。

## 8. 阶段 4：配对响应分类

新增：

```text
firmware/main/rust/src/pairing_response.rs
firmware/main/rust/include/pairing_response.h
```

C++ 继续负责 HTTP、response buffer、cJSON 解析、NVS token 写入、配对码显示和等待。
Rust 不直接调用 cJSON/NVS/HTTP。

跨语言输入固定为已经由 C++ 解析/判定的事实：

- pair-start：HTTP status、JSON 是否有效、`code` 是否为字符串、`expires_in` 是否为数字。
- pair-claim：HTTP status、JSON 是否有效、`token` 是否为非空字符串。

兼容规则不收紧：

- pair-start 当前只在 HTTP 200、JSON 有效、`code` 为字符串且 `expires_in` 为数字
  时成功；迁移后保持该规则。
- pair-claim 当前 HTTP 200 且 token 为任意非空字符串就认为配对成功；迁移后保持
  该规则，不新增长度、字符集或服务端格式验证。
- pair-claim HTTP 200 且没有 token（包括 JSON 无效）仍分类为 pending，保持当前
  `status==200 && token.empty()` 的外层行为；HTTP 401/429 分类为 rejected；
  其他正 HTTP 状态和负 transport status 分类为 network error。

Rust 负责这些分类以及“交回 `pairing.rs` 的 outcome”；不重复实现配对流程状态机。

## 9. 阶段 5：EPD diff/刷新资格

仅在阶段 0 取得正向因果归因或用户明确批准豁免后实施。

新增：

```text
firmware/main/rust/src/epd_policy.rs
firmware/main/rust/include/epd_policy.h
```

Rust 负责 frame diff 本身：C++ 先完成 `tx_buf` 快照并释放 `dirty_mutex`，然后
在锁外把只读 `prev_buffer`/`tx_buffer` 指针、buffer 长度、宽高及 `bytes_per_row`
交给固定 ABI；Rust 从这些指针计算 `diff_bits` 和 `diff_ratio`。Rust 不获取
FreeRTOS mutex、不调用 C++ 回调、不访问 framebuffer 生产路径。C++ 不再保留
`analyze_frame_diff` 的策略计算副本。

Rust 输入必须显式携带当前 C++ refresh loop 的所有策略事实：

- `prev_buffer_synced`、`prev_buffer_present`。
- `urgent`、`force_full`、`is_four_color`。
- `wake_baseline_candidate`（当前为 UI reason + deep-sleep wake）。
- tiny diff 的 streak、累计 bits、首次采样 tick、当前 tick。
- `partial_since_full`。

Rust 输出必须是结构化 Action，而不是一个隐含在日志/整数里的判断：

- `SyncBaselineAndSkip`。
- `SkipNoDiff`。
- `SkipTiny`。
- `RefreshFull { urgent, force_full }`。
- `RefreshPartial { urgent }`。

其中 `is_four_color == true` 必须保留现有无条件 FULL 规则；`force_full`、无
baseline、diff 达阈值和 `partial_since_full >= 10` 也必须映射到 `RefreshFull`。

跨轮 tiny-state 仍由 C++ 持有，Rust 不修改隐式全局状态。C++ ABI 固定携带
`next_tiny_streak`、`next_tiny_accum_bits`、`next_tiny_first_tick` 三个字段；
Rust 每次返回完整 next state，C++ 每次 Action 后原样写回，状态转换唯一：

- `SkipTiny`：返回递增后的 next tiny state；C++ 原样写回，等待下一轮。
- `SkipNoDiff`：返回三个零；C++ 清零。
- `SyncBaselineAndSkip`：返回当前 tiny state 原值；C++ 原样写回，保持旧 C++
  wake-baseline 分支不清零的行为。
- `RefreshFull` / `RefreshPartial`：返回三个零；C++ 完成 EPD 调用后写回零，
  FULL 额外把 `partial_since_full` 清零，PARTIAL 额外递增它。

不再保留“Action 是否携带 next state”的二选一设计；上述三个字段是 C ABI 必选
字段，host 测试必须验证每个 Action 的状态转换。

C++ 继续负责实际分派和所有机制：

- `RefreshFull` → `EPD_Init(); EPD_Display();`，成功后把 `partial_since_full` 清零。
- `RefreshPartial` → `EPD_Init(); EPD_DisplayPart();`，成功后递增 `partial_since_full`。
- EPD 波形函数最终仍汇聚到 `EPD_TurnOnDisplay()`。
- SPI、GPIO、`read_busy()`、framebuffer 分配、refresh task、mutex、notify 和 EPD fatal checks
  全部留在 C++。

EPD policy 与 EPD waveform 分开提交，确保出现硬件问题时可以单独回滚纯策略迁移。



## 10. Host 测试与 TDD

每个策略批次：

1. 先写 Rust 输入/输出类型和用例，保留旧行为。
2. 用 `unimplemented!()` 或缺失实现得到真正行为红灯。
3. 实现到绿。
4. 做一次哨兵变异，确认对应守卫会红。
5. 恢复实现并确认再次绿。
6. 添加 C ABI action 编号/字段映射契约测试。
7. 接线 C++。
8. 运行 Rust 全量 host 测试和真实固件编译门禁。

测试必须断言消费者可观察行为，不断言源码文本或“符号存在”。

每个批次至少覆盖：

- 正常路径。
- 边界时间窗口。
- 缺失/非法输入。
- 状态/缓存回退。
- 重复调用幂等。
- C ABI action 编号和 payload。

## 11. 固件与真机验收

每批 Rust 批次的固件门禁：

```text
cargo test
idf.py build
```

必须确认：

- `Project build complete`。
- 无 `undefined reference`。
- 新 ABI 符号在最终 ELF 中。

硬件行为不能由 host 测试替代，真机至少覆盖：

- 电池插入、拔出、充电、充满。
- Wi-Fi 断开、失败和重连。
- `endpoint_missing` 后 endpoint 恢复。
- 页面 bitmap 更新后的 EPD 刷新。
- 阶段 0 的 `rr=4` 是否消失或能继续定位。

## 12. 提交与回滚

迁移阶段与提交编号不是同一个粒度。阶段 3 拆成两个可独立回滚的提交：

```text
阶段/提交 0：panic 诊断
阶段/提交 1：charge_status
阶段/提交 2：LED
阶段/提交 3a：Wi-Fi cache + policy
阶段/提交 3b：endpoint_missing 修复
阶段/提交 4：配对响应分类
阶段/提交 5：EPD policy
```

3b 与 3a 分开提交，便于独立回滚。EPD 纯策略与 EPD 机制不混在同一提交。

## 13. 明确不做

- 不把 `custom_lcd_display.cc` 的 SPI/EPD 机制整体搬入 Rust。
- 不把 `wifi_station.cc` 的 ESP-IDF 驱动整体搬入 Rust。
- 不把 HTTP/NVS/GPIO/I2C 搬入 Rust。
- 不在迁移阶段顺手重写 UI、音频、streaming。
- 不把 `rr=4` 归因于 Rust 迁移或修复；阶段 0 先取得证据。
- 不把没有真机/日志证据的 Wi-Fi 或 EPD 行为变化混入语言迁移。
