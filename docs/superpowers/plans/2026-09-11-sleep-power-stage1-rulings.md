# Stage 1 执行期间的裁决记录（sleep/power 重构）

这份文件是 subagent-driven 执行本计划时，控制者代表人类伙伴做出的全部裁决的**完整清单**。
每条格式：`裁决 — 理由 — 若判错的代价`。执行期间 8 个任务、每任务一轮定向重审，
共 53 条裁决；其中带 ⚠️ 的几条是修正我先前判错的（执行流程里的重审把它们挡下了）。

计划：`2026-09-11-sleep-power-stage1.md`　规格：`2026-09-11-sleep-power-redesign.md`


1. Ruling: 不建 worktree，在 `2bp` 分支原地执行 — 运行中的服务端（:9002）与设备烧录都绑定这棵树，且 `2bp` 是项目工作分支而非 main/master — 代价：若需并行验证会互相干扰；本项目既有工作方式一直是原地。

2. Ruling: F1 按"桩清零 48 字节"修 — 结构体 sizeof 是 48，调用方传入的缓冲不保证已清零 — 代价：无（桩更严格，只会更早暴露偏移错误）。携带进 T3 dispatch。

3. Ruling: F2 按"复用测试文件里既有的鉴权与画布 fixture 写法"解 — 计划文本已如此指示，实现者需先读该文件 — 代价：若既有文件用的是别的注入方式，实现者多读一遍文件。

4. Ruling: 上一批遗留的三个真机验证项（refresh 归因实验、按键复核、空白页提示）并入本计划 Task 8 的真机矩阵跟踪，不另立任务 — 矩阵覆盖同样的设备交互 — 代价：refresh 归因的结论要等 Task 8 才产出。

5. Ruling: T1-T4 无设备依赖，连续执行；T5-T8 需要一次烧录（冷启动 = 拔插 USB，比按 BOOT 便宜） — 计划的矩阵本就如此安排 — 代价：T5 起需要人工拔插一次线缆。

6. Ruling: F3 按"恢复 400 校验与原测试"修 — spec §3.2 的前提（"保存时未生效"）是错的：app.py 本来就有 `if duration_minutes < settings.canvas_min_page_duration_minutes: raise HTTPException(400, …)`，静默修正用户输入比明确拒绝更糟，且会推翻一个既有且已测的契约 — 代价：API 对过短时长仍返回 400；盘上若已有 0 分钟历史条目，由 schedule_position 的 max(…,1) 容忍。已同步修订 spec §3.2 与计划 Task 1 文本，并重新生成 brief。

7. Ruling: 秒级 vs 分钟级按"秒级"（即实现者所选）保留 — 字段名与用途都是秒；分钟量化永远返回 60 的倍数、最坏晚 59 秒翻页；秒级在 ≥60 秒时精确，<60 秒被固件侧 60 秒下限兜住，最坏同为晚 59 秒但平均更准。计划的断言 (0,1)/(1,1) 保持原样 — 代价：spec §3.1 的公式需改成秒级（已改）。

8. Ruling: F4（重言式 md5 测试）按"换成时间不变性测试"修，不用 golden digest — 原测试比较同一个纯函数对同一输入的两次调用，永远不可能失败（计划我写的，属于 plan-mandated 缺陷）；golden digest 也能挡，但它 pin 死一个魔法串、且不表达真正的不变式。换成的测试用 monkeypatch 时钟取两个相隔 10 分钟的快照，断言 md5 相等而位置不同——直接钉住"摘要不随时间推进而变"这个属性 — 代价：若将来有人有意把时间相关字段纳入摘要，这个测试会正确失败并需要显式更新。

9. Ruling: F5（只断言键存在）按"补值断言"修 — 空排期契约是 (0, None)，仅断言键存在对错误值/类型都放行，等于 app.py 的接线没有 HTTP 层覆盖 — 代价：无。

10. Ruling: 时间不变性测试的时长改为 (20, 10)、并断言精确位置值 — 我给的 verbatim 测试体用 duration_minutes=5 建第二页，而 round 1 刚恢复的 400 校验（下限 10）会在 setup 阶段就拒绝它，两者不可能同时成立；(20,10) 两页都合规，且 1_000_000 % 1800 = 1000（第 0 页）→ 1_000_600 % 1800 = 1600（跨过 1200 翻页点，第 1 页），断言精确值而非仅"不相等"，既自证又能在常量被改动时立刻失败 — 代价：测试与那两个时间常量、两个时长耦合；改动其一会需要同步更新断言（这是刻意的，测试本身就是钉住这些值）。

11. Ruling: R1 音频轨开关加到 Board 基类（虚函数 SetAudioRail(bool)，默认空实现），由板级 override — 计划让我在 zectrix-s3-epaper-4.2.h 加 GetPower()，但那个头文件不存在（板级类是 .cc 内联定义的），shim.cpp 无法命名具体板级类；基类虚函数与既有 GetDisplay() 的取得方式一致，shim 不必知道具体板 — 代价：Board 基类多一个虚函数（默认空实现，不影响其他板）。

12. Ruling: R2 在 rf_set_display 内部注册 refresh-idle 回调（幂等） — 没有别的 C++ 调用点可挂；rf_set_display 已被 application.cc:178 → page_sync_set_display → set_display → rf_set_display 这条链到达。若不注册，"跳过重绘"优化会静默失效——本项目已有 server_pairing_init 从未被调用的先例 — 代价：注册时机绑定在首次 set_display，若将来有人删掉那个调用点，回调也不会注册（因此要求实现者在报告里写明调用链）。

13. Ruling: R3 mark_pending（page_sync 任务）与 commit（显示刷新任务）跨任务共享 33 字节 md5，必须用一段短临界区串行化 — 明文 static 读写会撕裂（33 字节 memcpy 非原子） — 代价：每次绘屏请求/提交各进一次临界区（几字节拷贝，无阻塞）。

14. Ruling: F6 用 esp_sleep_get_wakeup_causes() 的位图 API 取代 esp_sleep_get_wakeup_cause() — 后者在 IDF v6.0 带 deprecation 属性（esp_sleep.h:725），计划我写的；位图的位号就是 esp_sleep_wakeup_cause_t 的值（sleep_modes.c:2337），rf_wakeup_cause 对外的 0/1/2/3 契约不变 — 代价：无（行为等价，警告消失）。已在 review 之前修掉，因此本轮 review 覆盖的是修后状态。

15. Ruling: F7（主机桩丢弃 displayed_index 且不写 magic）按"桩必须忠于 shim.cpp"修 — 桩是计划我给的文本，它让下游 host 测试变成虚构（读索引永远 0、以 magic 判定的读者永远判为无效） — 代价：无。

16. Ruling: F8（桩缺 null 检查）直接修（一行） — 桩的意义就是与 C 边界行为一致 — 代价：无。

17. Ruling: F9（commit 监听不区分是哪一次刷新完成）判为 deferred minor，不修 — 窗口是"第二次 mark_pending 落在飞行中的刷新期间"，它会自愈：任何一次绘屏请求最终都会触发刷新，而电源策略拒绝在刷新未完成时入睡，因此入睡时刻记录与玻璃必然一致；要彻底关掉需要给 custom_lcd_display.cc 加刷新起点钩子（第二个槽位），复杂度超过该窗口的收益 — 代价：飞行期间记录可能短暂领先，若刷新因故而未落地则需等下一次翻页纠正（入睡前不会）。已指向最终全分支 review 裁决。

18. Ruling: T4 的 page_sync_next_wake_s 返回类型改为 int32_t，用 -1 表示未知/空排期 — 计划原本是 uint32_t + "0 = 未知"，而 T7 会把它直接塞进 seconds_until_next_page（int32），0 会被 power::decide 当作"还剩 0 秒"并夹到 60 秒下限，空排期就永远睡不满 cap — 代价：多一个哨兵约定，已在 spec/计划的接口与说明处写明。

19. Ruling: R-A 记录门槛改为 magic AND valid（不只看 valid）— 设备只在两者同时置位时有效，T3 刚把主机桩改成与设备一致，用同一个门槛才能让 host 测试代表设备行为；全零记录（上电 RTC）必须读作"无已知内容" — 代价：多一次 4 字节比较。

20. Ruling: R-C 现有测试按"逐条清单化 keep/rewrite/delete"处理，不接受默默删除 — 现有测试描述的是被移除的轮询/本地轮换机制，部分会编译不过；哪些仍描述新设计（解析/策略/位图缓存/md5 快路径）保留、轮换那条改写成"挂起时不绘屏"、plan_tick 那 4 条连同 plan_tick/Tick/TICK_MS/POLL_MS 删除 — 代价：实现者要先做一次测试盘点（也正好给 review 一个可核对的清单）。

21. Ruling: R-D sync_once 返回 bool（Rust 与 C 双侧），并保留一个静态的"上次是否成功"供 page_sync_sync_ok() 用（T7 要读）；page_sync_start() 保留名字与签名但不再建任务 — 代价：application.cc:178 的调用点语义变了（不再起轮询），T6/T7 会跟进。

22. Ruling: R-E 位图下载/缓存路径复用不重写 — 本任务只改"何时画"，不改"怎么取" — 代价：无。

23. Ruling: F10+F11 合成一条契约——空页提示必须是"被记录的状态"而不是一次不可记录的绘制：用 displayed_index == -1 作为提示标记（负载复用 EMPTY_PAGE.md5 的 32 个零字节；读取方只认 index），show_empty_hint() 在请求刷新后 mark，paint_if_changed() 在 count==0 时读记录比对标记、不匹配就画提示并 mark — 理由：否则记录里留着上一张画板页的 md5（F10），且空排期下 start() 声称拥有屏幕却永远不画、UI 被抑制、UP/DOWN 落进空的 next()/prev()（F11）— 代价：多一个哨兵约定（index=-1），已在两处写注释；T6/T7 被要求"引导与唤醒路径无条件调用 paint_if_changed()"，因此本任务不加调用点。

24. Ruling: F12 对齐问题照改 — [0u8;48] 只保证 1 字节对齐，而设备侧用结构体赋值（*out = g_panel_rec）写 4 字节成员，是真实的 UB 边界 — 代价：无（一行包装类型）。

25. Ruling: F13 重写那条"断言不可能失败"的失败同步测试 — 计划我写的文本：空表下"sync 返回 false 且刷新数为 0"本来就恒成立。改为先成功同步 0xa1、再让调度返回 500，断言表仍是 0xa1 且刷新数未增长 — 代价：无。

26. Ruling: F14 补上 magic 那一半的门槛测试与 prev() 覆盖 — R-A 的 magic 检查必须保留（全垃圾的 RTC 记录可能 valid==1 而 md5 是垃圾，只有 magic 能让冷启动安全），但现状没有任何测试能区分两半；给主机桩加一个仅测试用的 stage_panel_record 直接写记录，并加"magic 错、valid=1 必须重绘"的用例 — 代价：主机桩多一个测试专用入口。

27. Ruling: T5/T6/T7 的真机验收步骤统一延后到 Stage 1 矩阵一次跑完 — 设备当前深睡、USB 未枚举，而刷机需要一次物理重新插拔（我无法代劳）；每个任务的代码验收仍要求 build clean，设备观测集中在最后一次刷机 — 代价：这七个任务的设备侧结论要等到矩阵那一轮才产出（与 T8 本就要做的事重叠，不额外增加工作量）。

28. Ruling: 计划里的 CreateDisplay 是我写错的名字，实际是 InitializeLcdDisplay（zectrix-s3-epaper-4.2.cc:334，只被 Initialize() 第 73 行调用） — 实现者按实际名称加默认参数 bool bring_up_panel = true，既有调用点不变 — 代价：无（已在计划与 T6 brief 更正）。

29. Ruling: 真机验收的判据不能依赖 INFO 标记 — custom_lcd_display.cc 内部 #undef 了 ESP_LOGI（第 45-46 行），所以 "EPD bring-up" 这类标记根本不会输出；矩阵改以 `CustomLcdDisplay: EPD busy wait` 的 5/10/15 秒告警序列 + 画面是否出现为准 — 代价：矩阵判据少一个标记，多依赖已有告警序列（该序列本身就是 20 秒开销的直接证据）。

30. Ruling: 修 last_sample_tick 锚点（一行） — R1 说"只搬列出的步骤"是我措辞过窄：该锚点本来就属于 bring-up 那一刻，放回去能让冷启动刷新时序与今天逐位一致；审阅者也确认当前的遗漏只会让首刷提前、不会卡住 — 代价：无。

31. Ruling: bring-up 与刷新任务之间未串行化的窗口判为 parked，不实现 — 它在仓库实际编译的配置下不可达（HAVE_LVGL 未定义、缓冲区分配即为白、窗口内无人绘制，刷新循环首轮走零差异路径就退出），只在 LVGL 构建下才打开，而本仓库没有这种构建 — 代价：若将来真启用 LVGL 构建，启动早期可能出现 SPI 并发（EPD_RecvData 会在序列中途释放并重加 SPI 设备）；已指向最终全分支 review 裁决。

32. Ruling: 矩阵增加"顺序检查"（finding 3） — EPD busy wait 那组告警单独不足以证明是 BringUpPanel 跑的（刷新循环的 FULL 路径会产出同一条轨迹），因此矩阵除告警序列外还要看它是否出现在首次刷新/WiFi 活动之前、以及之后画面是否为空白（面板本该是白底） — 代价：矩阵多一个判据。

33. Ruling: 两条启动路径都要跑同一个一次性周期（sync → notify → paint_if_changed → policy），quiet 只影响"是否建 UI/音频、面板是否 bring-up、启动后进不进交互宽限" — T4 review 指出轮询任务被删后交互路径也没人画首屏了；计划原文把这条只写在 quiet 侧，属缺口 — 代价：交互启动多一次同步+可能一次绘屏（与既有行为等价）。

34. Ruling: quiet 只在"已配网且已配对"时生效，否则强制回落交互路径并记日志 — 配网页/配对页依赖 rawdraw_ui_manager_ 渲染，quiet 下用户看不到 AP 名/密码/配网地址，会静默卡在无界面的配网流程里 — 代价：新设备首次上电总是走交互路径（本就如此），只有已配对设备享受省电路径。

35. Ruling: 执行顺序改为先 T7 后 T6（编号不变） — T6 的文本引用 T7 的 ServicePowerPolicy()/RunPowerCycle()（两条启动路径共用同一周期），按原顺序会撞前向引用；T7 不依赖 T6（它只依赖 T2/T3/T4 的产物），因此对调是安全的 — 代价：T6 的"quiet 路径无 EPD busy wait"这条真机观测要等 T6 落地后才能跑，而它本来就已统一延后到矩阵。

36. Ruling: R1 引入 RunPowerCycle() 作为两条启动路径共享的一次性周期（sync → notify → paint → policy），放在 Application 上供 T6 调用 — 否则交互路径与 quiet 路径会各写一份顺序 — 代价：多一个公开方法。

37. Ruling: R2 paint_if_changed() 无条件调用（含空排期） — 空排期时它就是"画并记录空页提示"的那一步，也正是 T4 review 确立的"画板声称拥有屏幕"的依据 — 代价：无。

38. Ruling: R3 定时器改毫秒粒度并更名；NVS 旧键 sync:sync_interval 不再驱动休眠，新键 power:idle_grace_min(3) / power:max_sleep_min(60) — 旧键保留不读，避免迁移逻辑 — 代价：旧键残留（无害）。

39. Ruling: R4 按键活动重置宽限（成员变量而非 static） — 现状按键完全不重置休眠计时，是真实缺陷 — 代价：无。

40. Ruling: R5 EnterManualSleep 保留原语义（用户明确要求立即睡），但必须说明它是否遵循音频轨拆卸顺序 — 代价：无。

41. Ruling: R6 本任务先加板级 IsPowerPresent() 转发 charge_status_.Get().power_present；ext1 唤醒电平必须以实测为准（CHARGE_DETECT_CHARGING_LEVEL 只说明"充电为低"，不能推出未插电电平），无法从代码确定时写成具名常量 + 注释并列入矩阵待验 — 代价：矩阵多一项待确认。

42. Ruling: R7 设备矩阵统一延后（与 T5 同一处理）。

43. Ruling: F15+F16 一起修——把失败计数落到 RTC（shim.cpp 加 RTC_DATA_ATTR g_fail_streak + rf_fail_streak_get/set，shim.rs 加声明与主机桩并纳入 host::lock 重置），且更新必须发生在策略读取之后（成功写 0，失败写 min(streak+1, 8)） — 每次深睡都是一次重启，RAM 里的计数永远归零，实测退避会退化成固定重试（服务器不可达时约 720 次唤醒/天而非 24 次），而"先自增再读"又会让首步变成 120 秒、跳过 spec 写明的 60 秒 — 代价：多一个小 RTC API；RTC 变量在上电复位后丢失（首次启动按 streak=0 处理，正确）。

44. ⚠️ 已被后续更正推翻：，并且不加上下拉、改为把引脚电平打进休眠日志 — charge_status.cc 的约定是"检测脚为高 ⇒ power_present"，而策略把 power_present 当 mains，因此本固件里"插入=高、拔掉=低"；按原实现（ANY_LOW）等于"拔掉就唤醒"，睡下立刻被唤醒，随后 mains=1 导致永不再睡 —— 这是会卡死电池续航的自相矛盾。选择 ANY_HIGH 让 ext1 与 power_present 两个读者由构造保持一致；不加上下拉是因为电平由充电 IC 的电路定义，我无法从这里确认内部上拉会不会与其打架；改为在休眠行打印 pin2 电平，让矩阵第一轮就能证实或推翻 — 代价：若硬件实际是"插入=低"，需要改这个常量并在必要时反转 charge_status 的判读，由矩阵定案。

45. Ruling: F18 由我在 T8 重写矩阵表（只用以存在的标记） — 报告里那张表点名了尚未打印的 Boot path / wake cause（属 T6）和被编译掉的 EPD bring-up，以及不存在的 USB disconnect 字符串 — 代价：无（T8 本就要写矩阵）。

46. ⚠️ 修正：（CHARGE_DETECT_PLUG_PULLS_LOW = 1） — 我上一轮说"charge_status 的约定是插入=高"，那是读了局部变量名 detect_high；实际代码是 gpio_get_level(detect_gpio_) == CHARGE_DETECT_CHARGING_LEVEL，而该常量为 0（"low=charging"），所以 插入=低、拔掉=高。按我上一轮改成 ANY_HIGH，等于在电池上每次入睡时该电平条件已满足，ext1 立刻触发 → 启动/WiFi/同步循环，反而是更严重的回归。已亲自复核源码确认 — 代价：一轮往返（这次是重审替我挡下的），以及矩阵判据需要按正确极性写（电池 pin2=1 mains=0，插 USB pin2=0 mains=1）。

47. Ruling: 失败计数只在"真的尝试过同步"后推进（用一次性的 RAM 标志：RunPowerCycle 在 page_sync_sync_once() 前置位，策略消费并清除） — 否则 mains/宽限的每次重装（60s/30s/15s）都会各自推进计数，形成只增不减的棘轮，使拔电后的第一次休眠直接跳到 cap — 代价：多一个按次消费的 RAM 标志（本来就不需要跨休眠保留）。

48.    并按"周期内快照结果 + 周期期间停掉重装定时器"修 — 重审原文只说"计数可能被定时器侧消费导致阶梯失真"，但同一交错还有一个更严重的面：定时器在周期中途触发时，page_sync_sync_ok() 仍是上一轮的假值、面板还不忙、音频空闲，decide 会返回 sleep，于是设备在"画完任何东西之前"就关音频、断 WiFi、深睡，之后每次唤醒重复 —— 表现为永远画不完一屏，而不是数错一格 — 代价：周期内多一次 esp_timer_stop（由周期末的 ServicePowerPolicy 重新装载，无空窗）。

49. Ruling: T6 必须让定时器回调不再"每次 tick 都跑整个周期"，而是一个小调度器：到点（服务端 poll 间隔）或尚未跑过 → RunPowerCycle()，否则 → ServicePowerPolicy() — 否则 mains 常醒时每 60 秒重跑一次同步+拉通知（1440 次/天，正是本次重构要消除的），而反过来若周期只在启动时跑一次，常醒设备（插电）的画板将永远不再轮换。 — 代价：多一个判断分支；两个原因是可解释的（"数据该更新了" vs "条件变了吗"）。

50. Ruling: T6 的 Connected 处理器不要把 RunPowerCycle 直接放在事件回调里跑 — 它含 HTTP 同步与可能 15-25 秒的绘屏，会阻塞 WiFi 事件任务；应改为装一个立即/短延时定时器，让周期在 esp_timer 任务里跑（这也顺带消掉上面那条 watch-item：不再存在"周期中途才装上"的那次装填） — 代价：启动到首次绘屏多一次很小的定时器延迟。

51. Ruling: 三条一起修，按"quiet 只是起始状态、可被提升" — F20 插电后策略永久保持清醒而 quiet 单向门导致只能拔插恢复（窗口不小：显示通知的 quiet 唤醒最多清醒 5 分钟，正是用户可能插电的时刻）；F21 UP+DOWN 组合键在无 UI 下起 AP，且 AP 模式阻止休眠 → 空白屏耗到没电；F22 WiFi 连不上时定时器从未装填 → quiet 唤醒在电池上无限期清醒。修法：主循环消费一个提升请求（来源：mains 保持清醒分支、无 UI 时的按键活动、请求配网），补建 UI + BringUpPanel + 状态栏，幂等；quiet 的 Initialize 末尾装 30 秒兜底定时器（连上后被 3 秒装填覆盖，调度器的"本 boot 未跑过"判断防止多跑一次周期）。已同步修订 spec §4.1b 与计划的 Task 6 文本 — 代价：多一个跨任务标志与一次兜底装填；提升发生在主循环，因此按键后 UI 出现有约一秒延迟。

52. Ruling: F22 更深一层——光装兜底定时器不够：策略会跑，但 SleepManager::CanSleepNow() 里的 lifecycle 门（SyncIdle）在没 WiFi 时永远打不开，于是 busy=1、decide 以 15 秒节拍保持清醒。改 CanEnterSleepMode()：SyncIdle 允许；WifiConnecting 且"本 boot 已尝试过同步且失败"也允许；ApProvision 与配对流程（PairStart/WaitCode/ClaimPolling）仍排除。理由：已试过且失败的周期本身就是"下次再试"的占空比语义；配网/配对需要用户与屏幕。副作用（有意）：交互启动但连不上网时，宽限过后也会入睡——这正是电池设备应有的行为，按键会重置宽限所以使用时不会被打断 — 代价：生命周期门从"单一状态"变成"状态+一次已失败的尝试"，需要在注释里把理由写清楚，否则后人会以为这是漏判。已同步修订 spec §4.1b。

53. Ruling: 五条一起修（一次 fix wave） — F23 空页提示必须只在同步成功后才画（失败时 count==0 是"排期未知"而非"没有页"，现在会在上一张好页上重刷提示，多花一次 15-25 秒全屏刷新，spec §13 要求保持原图退避）；F24 提升到交互时必须先把手交还给 UI（page_sync_stop_display + 作废面板记录），否则 Init 清帧缓冲后 RenderAll 因画板持有而早退、TriggerRefresh 无此守卫地把白屏刷上去并永久留下；F25 要在提升后重新注册面板记录提交回调（UI 的 SetOnRefreshIdle 是替换语义，会把 shim 先前链上的回调抹掉）；F26 充电极性只保留一个真相源（由 CHARGE_DETECT_CHARGING_LEVEL 推导）；F27 删掉轮询循环遗留的 FFI（rf_now_us / rf_delay_ms / rf_task_stack_free） — 代价：一次 fix wave + 一次定向重审；F23 的三条用例是主机可测的，其余靠构建与后续设备矩阵。
