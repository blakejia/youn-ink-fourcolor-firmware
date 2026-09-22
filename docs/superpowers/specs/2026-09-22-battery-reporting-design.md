# 固件电量上报机制设计

日期：2026-09-22　分支：2bp　状态：已批准（用户「可以」，2026-09-22）

## 目标

关注电池电量的变化曲线，同时不增加设备功耗。上报通道复用设备既有的
轮询请求（「拿数据时同时上报」），服务端持久化历史并向前端提供曲线。

## 已拍板的决策

| 决策点 | 选择 |
|---|---|
| 采样密度 | 每次轮询都记一个点；服务端保留 90 天原始点 |
| 每点字段 | 电压 mv + 百分比 pct + 充电位 charge + 当时 power 计数器快照 |
| 传输通道 | 搭车 schedule GET 查询串（与 `w/a/r/g/f/rr` 同模式）|
| 消费端 | API + 设备详情页手绘 SVG 折线（零图表库）|

否决项（记录理由）：变化才记（平台期失真 + 设备端要 RTC 状态）、独立
端点（射频成本已为零，白增请求）、醒着窗口多次采样（压降故事计数器
增量讲得更好）、服务端下发采样间隔（配置回传 + 设备状态机，YAGNI）、
独立功耗分析页（YAGNI）。

## 数据链路

```
深睡唤醒 → C++ 读电池（ADC 10 采样平均 + ChargeStatus 快照，每周期一次）
        → shim FFI rf_battery_sample(&mv,&pct,&charge)
        → page_sync.rs 拼 schedule GET 查询串
        → 服务端解析 → battery_history 表追加一行
        → GET /api/devices/{id}/power-history?hours=N → 前端 SVG 折线
```

密度特性：采样点天然跟随唤醒节奏（轮询上限约 10 分钟，≈144 点/天/
设备）；90 天 ≈ 1.3 万行/设备，SQLite 单表可忽略。

## 固件侧（Rust 管策略、C++ 管机制）

### C++（机制）
`zectrix-s3-epaper-4.2.cc` 新增采样入口：
- 复用既有 `ReadBatteryStatus(voltage_mv, percent)`（`ADC1_CH3` ×2 分压、
  curve-fit 校准、10 次平均）。
- 复用 `charge_status_.Get()` 打包 charge 位。编码（自定枚举，服务端原样存）：
  `0=unknown, 1=no-power, 2=charging, 3=full, 4=discharging`。
  **必须含放电位**：曲线按 charge 分段着色，否则放电斜率被充电段污染。
- 仅电池供电时采样：mains 判定沿用 sleep 策略既有的引脚逻辑（插电时
  电压近恒值，无曲线价值，且省每周期 ADC 突发）。
- ADC 读失败返回 `mv=0`（调用方语义：没有有效样本）。

### shim
- `shim.cpp`：`extern "C" int rf_battery_sample(uint16_t* mv, uint8_t* pct,
  uint8_t* charge)`，返回 0=无传感器/读失败，1=成功。
- `shim.rs`：FFI 声明 + host 桩 `set_battery_sample()`（照 `rf_power_counters`
  / `rf_last_reset_reason` 的既有模式，供 cargo test 驱动）。

### Rust（策略）
`page_sync.rs` 拼 URL 处（现 :172-174）：
- `mv>0` 时追加 `&v=<mv>&p=<pct>&c=<charge>`；
- `mv=0` 不追加 —— 与 `rr` 同语义：缺键=没这回事，不是 0（旧固件兼容
  语义已定型于 `app.py:517-553`）。

## 服务端侧

### 新表 `battery_history`
```sql
CREATE TABLE IF NOT EXISTS battery_history (
    device_id TEXT NOT NULL,
    ts        INTEGER NOT NULL,   -- epoch 秒
    mv        INTEGER NOT NULL,   -- 2500..5000
    pct       INTEGER NOT NULL,   -- 0..100
    charge    INTEGER NOT NULL,   -- 0..4
    wakes INTEGER NOT NULL DEFAULT 0,
    awake_ms INTEGER NOT NULL DEFAULT 0,
    radio_ms INTEGER NOT NULL DEFAULT 0,
    http_gets INTEGER NOT NULL DEFAULT 0,
    refresh_submit_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (device_id, ts)
);
```
- 计数器列存**该次 GET 快照原值**：相邻两行差值 = 区间耗电速率的解释
  变量（awake/radio 增量大 ⇒ 掉电快），曲线解释力免费获得。
- 沿 `devices.py` 既有模式：`self._lock` 串行 + 幂等 `CREATE TABLE IF
  NOT EXISTS` 启动迁移。

### 解析点（`app.py` schedule GET 处理器，与 power 计数器同处）
- 四键 `v/p/c` 齐备且 `v>0` 才追加行；部分键或缺键 = 旧行为完全不动。
- 追加行时同时写入本次 GET 已解析的 power 计数器快照。
- PURGE：插入后顺手 `DELETE ... WHERE ts < now-90d AND device_id=?`
  （无独立清理任务）。
- 快照兼容：`devices.power_counters` JSON 增 `battery_mv/battery_pct/
  battery_charge` 三字段（前端状态栏即时显示走快照，历史走新表；
  旧固件 GET 不含键 ⇒ 快照不加字段）。

### 输入校验（恶意/坏值防御）
`mv ∈ [2500,5000]`、`pct ∈ [0,100]`、`charge ∈ [0,4]`，越界整点丢弃
（不落行、不报错）。

## 查询 API

`GET /api/devices/{device_id}/power-history?hours=N`
- `N` 默认 24，上限 2160（=90 天）。
- 鉴权：operator token，同现有 `/api/devices` 约定。
- 返回：`{points: [{ts, mv, pct, charge, wakes, awake_ms, radio_ms,
  http_gets, refresh_submit_ms}, ...]}`，按 ts 升序。
- 降采样：点数 >500 时按时间桶平均（桶宽=区间/500），桶内 charge 取
  多数位；桶内计数器列取桶末值（差值语义保持单调不减）。

## 前端

设备详情卡片加一条手绘 SVG 折线（零图表库依赖）：
- 默认窗口 24h，可切 7 天/30 天/90 天。
- 放电段（charge=4 或 0）黑色、充电段（charge=2/3）用面板调色板色
  （红/黄）。
- 悬浮（或点选）显示 mv/pct/相对时间（复用 `formatTime` 约定）。

## 错误处理

| 情形 | 行为 |
|---|---|
| 旧固件 GET（无 v/p/c）| 不追加历史行；快照不加电池字段 |
| ADC 读失败 / 无电池 | 固件不追加参数；服务端无新样本 |
| 插电（mains）| 固件不采样 ⇒ 曲线自然空档 = 插电期 |
| 越界值 | 服务端整点丢弃 |
| GET 本身失败 | 既有重试语义（整周期失败重试）覆盖，不新增机制 |

## 测试（TDD）

- **Rust**（`page_sync.rs` 测试，先红）：host 桩给样值后 URL 含
  `v=3980&p=76&c=4`；`mv=0`（桩返回失败）时 URL 无 v/p/c 三参数。
- **server pytest**（先红）：
  1. 四键齐备 GET → 表新增一行且计数器列正确；
  2. 缺键 / 部分键 → 不追加、旧行为不变（旧固件兼容阴性对照）；
  3. 越界 mv/pct/charge → 丢弃（3 条阴性对照）；
  4. 插入 91 天前旧点 + 新点 → PURGE 后只剩新点；
  5. `power-history` 端点：>500 点降采样桶数正确 + 未鉴权 401。
- **真机项**：曲线形态、放电斜率与计数器增量的对应 —— `待真机联调`。
