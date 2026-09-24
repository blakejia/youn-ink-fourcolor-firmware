# NOTE4C Rust 策略迁移第二阶段设计：ABCDE

日期：2026-09-24
状态：设计草案，等待用户复核
范围：RTC/SNTP 时间门、通知拉取策略、电池相对活动估算、页面比较、协议解析

## 1. 目标与边界

本阶段把五个已经存在于固件流程中的纯策略/解析边界迁入现有 `librust_firmware.a`，不新增第二个 Rust archive。目标不是把所有 C++ 改成 Rust，而是把能脱离硬件验证的规则从机制层移到可 host 测试的 Rust 层。

统一边界：

```text
Rust：纯状态转换、时间门、限频、去重、分类、解析、比较、编码规则
C++/ESP-IDF：RTC/SNTP/NVS/GPIO/SPI/I2C/Wi-Fi/HTTP/FreeRTOS/EPD/文件与任务
C ABI：固定 #[repr(C)] POD 输入输出，只传事实和动作
```

五项独立提交；每项先红测试，再最小实现，再哨兵变异，再 cargo test + ESP-IDF build + scoped review。不得把 EPD diff/刷新资格混入本阶段。

## 2. 共享约束

- 继续使用单一 `librust_firmware.a`，不新增 `#[panic_handler]`。
- 新 Rust 文件必须加入 `firmware/main/CMakeLists.txt` 的 `RUST_SOURCES`。
- Rust 不访问 GPIO、I2C、SPI、NVS、FreeRTOS、Wi-Fi driver、HTTP、DNS/ARP/TCP、EPD waveform。
- C ABI 使用 `#[repr(C)]`、固定字段顺序、显式 padding（如需要）和数值枚举。
- 迁移优先保持旧行为；不借迁移收紧协议、修复未证实 bug 或改变任务生命周期。
- 每个新行为必须有能在实现前失败的测试；哨兵变异必须能让对应测试失败。
- host 绿不等于设备通过；每个硬件相关结果单独记录为未验证或真机观察。
- `rr=4` 仍 unresolved；EPD policy 迁移不在本阶段授权范围内。

## 3. A：RTC/SNTP 时间门

### Rust 负责

新增 `time_gate_policy.rs` / `time_gate_policy.h`。Rust 输入：

- RTC/SNTP 校时时间戳与是否有效；
- 当前 boot/time 快照；
- 上一次相关持久化写入时间；
- 日频门、小时门、报告门的周期配置；
- 时钟倒退/异常标记。

Rust 输出：是否允许 SNTP/校时桥、RTC 缓存回写、电量历史采样、Wi-Fi cache 回写；异常时使用何种降级动作。周期和值全部显式传入，不在 Rust 内读取环境。

### C++ 负责

读取 RTC、执行 SNTP、访问 NVS/RTC_DATA_ATTR、调用动作、维护任务时序。C++ 继续拥有实际时间和持久化副作用。

### 特别约束

- 保持现有 UTC/local 与 `time.Parse` 语义，不在迁移中改时区。
- 时间倒退不得让频率门永久打开；Rust 必须返回明确的降级动作。
- 不把时间门迁移变成 NVS 格式迁移。

## 4. B：通知拉取资格与限频

### Rust 负责

新增 `notify_policy.rs` / `notify_policy.h`。Rust 输入：

- 通知状态：空闲/拉取中/已有通知/失败；
- EPD busy 事实；
- 上次拉取/成功/失败时间；
- 当前通知 id/etag/content hash；
- endpoint 返回的 transport/status 分类事实；
- 旧 JSON 与 `.bin` 响应类别。

Rust 输出：是否拉取、是否 defer、是否去重、失败退避、是否等待 EPD、是否消费通知；不输出文件路径和 HTTP 对象。

### C++ 负责

HTTP 请求、响应读取、`.bin`/JSON 文件落盘、通知任务、面板刷新和 UI 通知。保持现有 JSON fallback 与二进制端点兼容语义。

## 5. C：电池相对活动估算策略

### Rust 负责

新增 `battery_activity_policy.rs` / `battery_activity_policy.h`。Rust 输入：

- ADC 原始电压或归一化电压；
- 充电/放电/充满事实；
- 上一次样本电压、时间、方向；
- 采样时间门配置。

Rust 输出：是否接受样本、异常值处理、方向/相对活动等级、下一采样门。输出字段明确命名为 `relative_activity`，不能命名为 `mAh`、`soc_percent` 或精确电量。

### C++ 负责

ADC、GPIO、充电状态、NVS/battery_history、HTTP 上报和 UI 数据。保持现有历史存储和展示文案。

### 特别约束

- 无电流传感器，不得输出或推导精确 mAh/SOC。
- 只能输出相对活动估算；充电/放电转换时保留样本，不得伪造连续百分比。
- ADC 极性、量程和校准数据不搬入 Rust。

## 6. D：页面同步与内容比较

### Rust 负责

新增 `page_compare_policy.rs` / `page_compare_policy.h`。Rust 输入：

- 本地缓存版本/hash/尺寸元数据；
- 服务器返回的版本/hash/content type；
- 当前页面绑定和轮换事实；
- 拉取失败/成功状态。

Rust 输出：`SkipSame`、`Fetch`、`UseCache`、`InvalidateCache` 等纯动作，以及是否需要继续拉取。

### C++ 负责

HTTP、JSON 解析、文件读写、页面存储、轮询和缓存落地。Rust 不读取 bitmap，不访问 EPD framebuffer。

### 特别约束

- 本阶段只做比较/选择，不迁移 EPD diff、partial/full refresh 资格。
- 同 hash 短路必须与现有页面缓存失效规则一致；无证据不得修改失效条件。

## 7. E：协议解析器

### Rust 负责

新增 `protocol_parse.rs` / `protocol_parse.h`，按现有协议拆成小模块或聚焦函数：

- schedule 条目字段解析；
- 电池/电源 query 参数分类；
- OTA manifest 字段解析；
- 通知响应字段解析；
- HTTP status 到业务类别的映射。

Rust 接收 C++ 已提取的 JSON 值/字节事实或明确的短字符串，输出固定结果，不做 JSON DOM、HTTP socket 或文件操作。

### C++ 负责

JSON 库、HTTP transport、TLS、响应缓冲、协议版本头、文件落盘和任务调度。

### 特别约束

- 保持当前宽松解析：MQTT 无 scheme 的 `host[:port]`、URL authority、缺字段和无效 JSON 的旧语义不能被 Rust 迁移意外收紧。
- 兼容旧服务端 JSON fallback；不在迁移中删除 fallback。

## 8. 实施与验证顺序

1. 共享模块/ABI 注册基础与 host fixture。
2. A 时间门。
3. B 通知策略。
4. C 电池相对活动策略。
5. D 页面比较。
6. E 协议解析。
7. 最终 Rust host suite、ESP-IDF full build、关键符号和 ABI 审计。
8. 最终 reviewer；Critical/Important 未清零不得宣称完成。

每个任务独立 TDD：

```text
RED：新增行为测试在旧实现/未实现状态下失败
GREEN：只实现通过测试所需策略
SENTINEL：变异关键决策，确认测试失败
RESTORE：恢复实现并跑全套
REVIEW：实现者报告 + scoped reviewer
```

## 9. 不在本阶段授权范围

- EPD diff、partial/full refresh、tiny state、waveform 或 `rr=4` 归因。
- 第二个 Rust archive。
- 将 C++ 硬件层整体重写为 Rust。
- 新的 NVS schema、设备协议版本或服务器 API 协议收紧。
- 未经真机证据支持的省电行为改变。
