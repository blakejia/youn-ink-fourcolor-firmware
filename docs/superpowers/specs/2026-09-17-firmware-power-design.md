# NOTE4C 固件省电设计（唤醒期为主）

日期：2026-09-17
范围：`firmware/`（ESP-IDF v6.0，target esp32s3，NOTE4C 四色墨水屏电池设备）

## 1. 目标与不变量

**目标**：在不影响功能的前提下降低整机能耗，重点是**唤醒窗口**（深睡本身已很扎实）。

**功能不变量（用户红线，逐条要在真机验证）**

1. 网页版「串口 / 固件刷写」工具照常可用 ⇒ **USB-Serial-JTAG 与 UART0 双控制台不得关闭**，日志保持 `INFO`，不做 light sleep。
2. 新内容的**可见延迟不得变差** ⇒ 不靠拉长轮询省电；`seconds_until_next_page` 的保真度不变。
3. 按键 / 配对 / 通知确认的手感不变 ⇒ 唤醒判定、去抖、确认窗口时序不动。
4. 首屏与页面切换的**画质不降** ⇒ 不做降质刷新。

**验收口径**：设备侧计数器 + 时长记账。**没有电流表，不声称实测 mA**，只报「每小时射频开秒数」「清醒占空比」「每次唤醒请求数」。

## 2. 现状取证（只读，逐条有出处）

### 已经做对的（不动）

| 机制 | 证据 |
|---|---|
| 深睡 duty-cycle，睡眠判定是真逻辑（非 stub），Rust 侧 12 条单测 | `main/rust/src/power.rs` |
| 唤醒源：timer + BOOT(`ext0`) + 充电检测(`ext1`) | `main/application.cc:1046-1047`、`:266-273` |
| RTC 保留「玻璃上当前是什么」⇒ 内容没变就跳刷 | `main/rust/shim.cpp`（`g_panel_rec`） |
| 只下载要画的位图 | `main/rust/src/page_sync.rs:160,190` |
| **EPD 电源轨每次刷新结束即切断**（0x07 深睡 + 断电），下次刷新前重新上电并 `EPD_Init` | `boards/zectrix-s3-epaper-4.2/custom_lcd_display.cc:900-923`、`:942-944`、`:675,685` |
| 音频轨（功放）空闲 15 s 关闭、busy 投票 | `main/audio/audio_service.cc` |

> 由此**撤销**一个候选优化：「空闲切断 EPD 电源轨」**已经是现状**，不是待办。

### 白扔的部分（本次要改）

| # | 现象 | 证据 |
|---|---|---|
| 1 | 一次唤醒固定发 **2 个 GET**：`/api/pages/schedule` + `/api/notifications/next`；后者无条件 | `main/application.cc:926,953,954`、`main/rust/src/notify.rs:237` |
| 2 | 射频**直到睡前才断** ⇒ 全刷 15–25 s 期间一直关联 AP | `main/application.cc` RunPowerCycle 顺序 + 睡前 `rf_rails_audio(0)` |
| 3 | `SetPowerSaveLevel` 全链路已实现（板级包装 → `WifiStation` → `esp_wifi_set_ps`）但**零调用者** | `boards/zectrix-s3-epaper-4.2/zectrix-s3-epaper-4.2.cc:199-211`、`components/78__esp-wifi-connect/src/wifi_station.cc:957-975` |
| 4 | `CONFIG_PM_ENABLE` 未开、无 `esp_pm_configure`、无 tickless idle ⇒ **CPU 长期 240 MHz** | `firmware/sdkconfig:2627`、`:2749-2751` |
| 5 | `PowerLedTask` **500 ms 永久循环**（GPIO3 状态灯：充电状态 + 活动脉冲，**用户可见**） | `boards/zectrix-s3-epaper-4.2/board_power_bsp.cc` |
| 6 | 每次 I2C 读写都强制拉高音频电源脚 GPIO42 | `boards/common/i2c_device.cc:47,57,79,89`、`boards/zectrix-s3-epaper-4.2/i2c_power_hook.cc` |
| 7 | 固件里 3 个 `/api/photos*` 路径是**死代码**（服务端也没有对应路由） | `page_sync.rs` / `app.py:191-546` 路由表对照 |

### DFS 安全性（已验证，不是假设）

- `esp_pm_config_t = {max_freq_mhz, min_freq_mhz, light_sleep_enable}` ⇒「只开 DFS」= `light_sleep_enable=false`：`components/esp_pm/include/esp_pm.h:22-26`。
- SPI master 驱动自带 `ESP_PM_CPU_FREQ_MAX` + `ESP_PM_APB_FREQ_MAX` 锁：`components/esp_driver_spi/src/gpspi/spi_common.c:883-886`；I2S 组件同样引用 PM 锁（`esp_driver_i2s/i2s_common.c` 等）⇒ **EPD SPI 与音频时序无需我们加锁**。
- **未验证**：Octal PSRAM(80 MHz) 与 `min_freq_mhz=80` 的兼容性 ⇒ 列入实现期的构建期/运行期检查（见 §5）。

## 3. 设计

### P1 唤醒窗口（最大杠杆）

**1.1 刷屏前先断射频**（用户选定：下完就断）
数据全部下完后、进入 `paint_if_changed` 之前，显式 `esp_wifi_disconnect()` + `esp_wifi_stop()`。全刷 15–25 s 全程无射频。刷屏后若还需回告服务器（如 ack），顺延到下一次唤醒处理 ⇒ 对延迟无影响（红线 2 满足），因为此刻该拿的数据已拿全。
风险与对策：**未来**若出现「刷屏过程中必须联网」的功能，这条会成为限制 ⇒ 在该路径留显式注释，并把「刷屏期间不发起网络请求」写进不变量。（现有 ack 发生在用户确认后的那次唤醒里，不受影响；此句不代表对现状的额外断言。）

**1.2 唤醒期射频省电模式**
在连上之后、发第一个请求之前，调用已存在但无人调用的链路把射频切到 `WIFI_PS_MAX_MODEM`（`Board::SetPowerSaveLevel` → `WifiStation::SetPowerSaveLevel` → `esp_wifi_set_ps`）。身份切换不引入新 API。
代价：单次请求增加几十~几百 ms ⇒ 如实计入「唤醒窗口毫秒数」计数器，真机确认未越过红线 2。
回滚：把调用点去掉即回到今天的行为。

**1.3 通知查询条件化（少一次往返，而不是多一次）**
服务端在**已有**的 `/api/pages/schedule` 响应中增加一个布尔字段（`notify_pending`，与 `schedule_md5`、`seconds_until_next_page`、`screen_active` 并列，见 `server/youn_server/app.py:506-540`）。设备仅当该字段为真时才执行 `GET /api/notifications/next`。
对延迟无影响：通知本来也只在该次唤醒才被查到；只是把「空手而归的那次请求」省掉。
服务端改动约 3 行 + pytest 用例（通知队列非空/为空两种）。

### P2 CPU 功耗（配置级）

`sdkconfig.defaults` + `sdkconfig`：`CONFIG_PM_ENABLE=y`；运行期 `esp_pm_configure({max: 240, min: 80, light_sleep_enable: false})`。
不开 light sleep ⇒ USB-Serial-JTAG 控制台存活（红线 1）。驱动自带 PM 锁 ⇒ 无需改驱动代码。
上限保持 240 MHz ⇒ 音频/刷屏/解码性能不退化。

### P3 常驻小功耗（配置级）

**3.1 LED 事件驱动**：`PowerLedTask` 的 500 ms 常循环改为「状态变化时刷新 + 活动脉冲用定时器」，**保持用户可见语义不变**（充电状态、活动指示、工厂覆盖分支）。判据：改后状态灯在充电/放电/活动三种情形下的观感与今天一致（真机对照）。
**3.2 音频电源脚**：注意这是**两个不同的脚** —— 音频电源脚 `Audio_PWR_PIN`(GPIO42) 由 `BoardI2cForcePowerOn` 在**每次 I2C 读写**时拉高；功放脚(GPIO46) 只由 `SetAudioRail` 控制且现有调用者都是**关**（§2 表中「音频轨空闲 15 s 关闭」说的是它）。本项只改前者：不再每次 I2C 都重复拉高，改为「音频会话期间保持供电 + 会话结束释放」，I2C 访问前仍保证供电（否则寄存器读写会失败）。功能影响应为零。
**3.3 日志**：**保持 `INFO` 不动**（用户选定），控制台配置也不变 ⇒ 本项**不改代码**，仅作为红线登记。

### P4 明确不做

EPD 轨空闲切断（现状已做）；light sleep；事件驱动唤醒重排；刷新路径重构；关闭任一控制台；死代码 `/api/photos*` 清理（与省电无关，另议）。

## 4. 数据流（改后的一次唤醒）

```
wake(timer/BOOT/充电)
  → 上电 + 连接 Wi-Fi
  → SetPowerSaveLevel(MAX_MODEM)                     [1.2]
  → GET /api/pages/schedule  → {md5, seconds_until_next_page, notify_pending, ...}
  → if notify_pending: GET /api/notifications/next   [1.3]
  → 按需 GET 位图（仅 md5 变化时）
  → esp_wifi_disconnect + esp_wifi_stop              [1.1]
  → paint_if_changed（全刷 15–25 s，期间无射频）
  → 上报计数器（随心跳）
  → 决策（power.rs）→ 深睡（timer + ext0 + ext1）
```

## 5. 验收与测试

**计数器（固件新增，随心跳上报；服务端落库/展示）**：唤醒次数、清醒毫秒、射频开秒数、每次唤醒的 HTTP 请求数、刷屏毫秒、放电阶段统计。
**对比指标**：每小时射频开秒数（主要）、清醒占空比、每次唤醒请求数（2 → 1~2，无变化时为 1）。
**基线**：改动前后各取一段真实运行数据（同一设备、同一页面集），按上述指标对比 ⇒ 报告写成「时长记账」，不写 mA。

**回归**：
1. 服务器 pytest 全绿（含 1.3 新增用例）。
2. `firmware/` 构建（`rm -rf build && idf.py build`，主会话执行）零 `undefined reference`。
3. 设备真机矩阵：首次刷屏、内容变化时刷屏、内容不变跳刷、按键（BOOT 长按进设置、UP/DOWN）、配对流程、通知确认（GPIO39/18/0）、网页串口工具、网页刷写工具、画质对照。
4. 构建期/运行期确认：Octal PSRAM 与 `min_freq=80` 兼容（§2 未验证项）；开启 PM 后音频无爆音、EPD 无花屏。

**TDD 适用面**：Rust 决策层（`power.rs` 风格）与服务器 pytest 走 TDD；固件其余部分**没有自动化测试框架**，如实以计数器 + 真机矩阵作为证据，不假装有单测。

## 6. 风险与回滚

| 风险 | 对策 |
|---|---|
| 开 DFS 后音频爆音 / EPD 花屏（PSRAM/时序） | 先只改配置并真机长跑；异常则回退 `CONFIG_PM_ENABLE` |
| MAX_MODEM 让唤醒窗口变长，抵消收益 | 计数器对比「唤醒窗口毫秒数」；不划算就撤 1.2 |
| 1.1 断射频破坏未来「刷屏中上报」功能 | 注释 + 不变量文档化 |
| LED 改事件驱动改变观感 | 真机三态对照，不一致就不合入 |

## 7. 变更记录（实现期裁定）

**§3.2 P3.2「音频电源脚幂等化」—— 已评估，决定不做（2026-09-17，用户认可）**

原设计：`BoardI2cForcePowerOn` 不再在每次 I2C 读写时重复拉高 `Audio_PWR_PIN`(GPIO42)，改为「音频会话期间保持供电」。

评估结论：**该改法在当前引脚配置下不安全，放弃**。依据（逐字取自 IDF v6.0 头文件）：

```c
/* components/esp_driver_gpio/include/driver/gpio.h:136 */
/* @warning If the pad is not configured for input (or input and output) the returned value is always 0. */
```

该脚按输出模式配置 ⇒ `gpio_get_level()` **读回恒 0** ⇒ 「电平已是目标值就跳过」的守卫会**漏掉必要的拉高** ⇒ 音频 codec 静默上电失败（故障不报错、只是没声音）。

将来若要重做，只有两条路，且都必须重验音频：①用**软件影子变量**记住上次写入的电平；②把该脚改为 `GPIO_MODE_INPUT_OUTPUT` 后再读回。**不建议顺手做。**

**其余实现期更正**（详见计划文件末尾的 Ruling 块）：DFS 只能以 `min=80 / light_sleep=false` 落地（保住 USB-Serial-JTAG 控制台）；`notify_pending` 字段缺失时默认 **true**（混合部署行为与改前逐字一致）；每任务必须过增量 `idf.py build` 编译门禁。
