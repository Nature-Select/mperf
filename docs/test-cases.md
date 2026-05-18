# mperf 功能测试用例

人工功能测试清单。每次重构、加 feature、改 sampler 之后过一遍，确认没有破现有功能。

**约定**

- 平台标记：`[A]` = Android, `[I]` = iOS, `[A/I]` = 两端都适用。括号内是设备前置条件。
- 每条用例：**前置 → 操作 → 预期**。
- "回归点"标签 `[REGRESSION:bug-name]` 标注的是历史上踩过的坑，重构后必须验证。

跑测试的时机：

1. **每次 Rust 改动后**：crates / src-tauri 任何 .rs 改动都要重新 `Ctrl+C` 重启 `pnpm dev`，跑下面 §A / §C / §D-G 的快速冒烟。
2. **每次前端结构性改动后**：组件拆分、prop 重构、hooks 顺序变化，跑下面 §C / §I / §J 完整一遍。
3. **每次发版前**：从 §A 到 §L 全部跑一遍，至少 1 台 Android + 1 台 iOS。

---

## §A 设备发现与选择

左栏改成 PerfDog 风格:顶部下拉 + 下方三个 tab(设备 / 设置 / 关于)。同一台 iOS 同时插 USB 和配 Wi-Fi 会出**两条**(USB 一条 + Wi-Fi 一条),分别用于性能采集和(未来的)电量采集。

| ID  | 平台  | 前置                            | 操作                  | 预期                                                                                                   |
| --- | ----- | ------------------------------- | --------------------- | ------------------------------------------------------------------------------------------------------ |
| A1  | [A]   | adb 已授权                      | USB 插入 Android      | ~3s 内出现在下拉 Android 分组内,带 `USB`(青色)tag                                                  |
| A2  | [A]   | `adb connect <ip>:5555` 已连    | 启动 app              | 出现一条独立条目,带 `Wi-Fi`(橙色)tag                                                                |
| A3  | [I]   | 已配对                          | USB 插入 iOS          | iOS 分组下出现一条,带 `USB`(青色)tag,可用                                                          |
| A4  | [I]   | iOS USB + Wi-Fi 同时在          | 打开下拉              | 同 UDID 出**两条**,分别 `USB` + `Wi-Fi` tag,USB 排在前面;Wi-Fi 那条 disabled + `USB required` 灰色 tag |
| A5  | [I]   | iOS 仅 Wi-Fi 在线               | —                     | 下拉只出 Wi-Fi 那条,disabled + `USB required`,主面板顶部出橙色 Wi-Fi 警告横幅                       |
| A6  | [A/I] | 设备在下拉中已选                | 拔掉 USB              | 选中项消失;若有同 UDID 的 Wi-Fi 条目,selectedKey 变 null(那条不会自动 fallback,因为 transport 不同);录制中触发 watchdog 2 次 poll 后停止 [REGRESSION:watchdog-2-strike] |
| A7  | [A/I] | 下拉已加载                      | 点 refresh 图标       | spinner 转一下,立刻刷新                                                                              |
| A8  | [A]   | 同时插入 2 台 Android           | 打开下拉              | 两台都在 Android 分组下,可分别选中                                                                   |
| A9  | [A/I] | 录制中                          | 试图打开下拉换设备    | 下拉 disabled;弹 info "Stop the current session first."                                              |
| A10 | [A/I] | 任意状态                        | 鼠标移到 Sider 右边缘   | 出现 col-resize 光标 + 4px 浅灰色条                                                                  |
| A11 | [A/I] | 鼠标在 Sider 右边缘             | 按下并左右拖动          | Sider 宽度实时变化,范围 220–480px;松开后写入 localStorage (`mperf.sidebarWidth`);刷新页面后保持   |
| A12 | [A/I] | 拖到极小或极大                  | —                       | 自动 clamp 在 220 / 480,不会更窄/更宽                                                              |
| A13 | [A/I] | Sider 拖到任意宽度              | 看下方三个 tab           | 设备/设置/关于 **等宽平分**整个 Sider,无论窗口/Sider 宽度都跟着均分                                |
| A14 | [A/I] | 窗口窄(<1024)或 Sider 拖到 480 | 看右侧任意 chart 的 tile | tile 自动换行成 2-3 行,不挤压;每个 tile 最小 110px(`auto-fit minmax`),数值字号不截断          |
| A15 | [A/I] | DeviceSelector 区域            | 找刷新按钮              | **没有**(每 3s 自动 poll);PerfDog 也没有                                                            |
| A16 | [A/I] | 主面板顶部                     | 找设备名 / Start / Marker | 都**不在主面板**;Start / Marker 在左栏 AppSelector 下面,设备名在 DeviceSelector 里显示              |

## §B App 列表（左侧 sidebar 内）

| ID  | 平台  | 前置         | 操作                       | 预期                                                                  |
| --- | ----- | ------------ | -------------------------- | --------------------------------------------------------------------- |
| B1  | [A]   | 选中 Android | 打开左栏 App 下拉          | 显示第三方 launchable app，**不包含**系统 app；带 icon (LetterAvatar) |
| B2  | [I]   | 选中 iOS     | 打开下拉                   | 显示已安装 launchable app，不含系统 daemon                            |
| B3  | [A/I] | 下拉已展开   | 输入关键字                 | 按 label 和 bundle id 都能匹配（不区分大小写）                        |
| B4  | [A/I] | 未选 device  | App 下拉                   | disabled，placeholder "先选设备"                                      |
| B5  | [A/I] | 录制中       | App 下拉                   | disabled（不可中途切换 target app）                                   |

## §C 录制开始 / 停止

| ID  | 平台  | 前置                     | 操作                  | 预期                                                                                                                     |
| --- | ----- | ------------------------ | --------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| C0  | [A/I] | 选了 device，**未**选 app | hover Start          | 按钮 disabled，tooltip "Pick a target app first — recording is always scoped to one app."；Memory 区显示占位 "请先在上方选择目标 app" |
| C1  | [A/I] | 选好 device + 选好 app   | 点 Start              | Start 替换成 Stop（红），出现 Marker 按钮，成功提示 banner 3.5s 自动消失                                                 |
| C2  | [A/I] | 录制中                   | 点 Stop               | Stop 替换成 Start，**不应**出现 "ended automatically" 警告 [REGRESSION:session-ended-on-user-stop]                       |
| C3  | [A/I] | 已在录制 session A       | 点 Start 开 session B | A 自动结束并写入 DB，B 立刻开始；无残留                                                                                  |
| C4  | [A]   | 录制中                   | 拔 USB                | ≤1.5s 内（一个 list_devices poll 周期）弹 warning banner 并自动停止；history 里能看到这条 session 已 finalize。后端 sampler 报 DeviceDisconnected 后 ~50ms 也会通过 EVENT_SESSION_ENDED 兜底（先到先 user_stopping 抢断，另一条沉默）。Toast 应在 3.5s 后自动消失（auto: true） [REGRESSION:scheduler-no-event-on-natural-exit] |
| C4a | [A]   | 录制中,设备已变 `state=offline`(拔线后 adb 短暂不删条目) | 等 ~3s    | watchdog 通过 `!current.usable` 路径触发（2-strike × 1.5s）;Android 的 `usable` 必须随 state 翻 false [REGRESSION:android-usable-hardcoded-true] |
| C5  | [I]   | 录制中                   | 拔 USB                | 同 C4。iOS 拔 USB 后 usbmuxd 可能保留一个 network 条目，**必须**也被检测到 [REGRESSION:watchdog-iOS-network-fallback]    |
| C5a | [I]   | 录制中,sysmontap 静默停推样本(比如锁屏 / suspend 长时间)                     | 等 12s    | sample-heartbeat watchdog 触发(`No samples received for Ns — stopping the session`),自动停止 |
| C6  | [I]   | Wi-Fi-only iOS 被选中 + 选了 app | 试图点 Start          | 按钮 disabled，hover 显示"USB required"提示（usable 检查优先级高于选 app 检查）                                          |
| C7  | [REMOVED 2026-05-16] iOS "不选 app 也能录系统级指标" 行为已废除，现在两端都强制选 app。被 C0 替代。                                                                                                                |
| C8  | [A/I] | 录制中                   | 关掉 app 重启         | 重启后 history 那条 session 已被 finalize（不是 "in progress")                                                           |
| C9  | [A/I] | 录制中,先点 Stop 等一拍后再次拔线 | —      | toast 只出现一次("Stop" 路径不弹 toast),不会重复出现"ended automatically"(`user_stopping` flag 抑制后端两条独立路径)           |
| C10 | [A/I] | 录制完一段(自动结束 / 手动 Stop 都行) | 检查 DB | session 行的 `wall_end_ms` 是 sampler 真正停掉的时刻,不是 cleanup 完成的时刻(差 1-300ms);两条 finalize 路径(writer auto + Session::stop)`finish_session` 是幂等的 [REGRESSION:finish-session-overwrite] |

## §D CPU chart

| ID  | 平台  | 前置                        | 操作              | 预期                                                                                     |
| --- | ----- | --------------------------- | ----------------- | ---------------------------------------------------------------------------------------- |
| D1  | [A]   | 录制中（必然已选 app）      | —                 | 蓝线（Total）+ 红线（App）同坐标轴，0-100%，1Hz                                          |
| D2  | [REMOVED 2026-05-16] Android 自动跟随前台 app 已废除；改为强制显式选 app（PerfDog 风格，跟 iOS 对齐）。                                                                                              |
| D3  | [I]   | 录制中（必然已选 app）      | —                 | Total + App 都有值；App 的"占总 CPU 百分比"语义与 Android 一致（4 核机 100% 单核 ≈ 25%） |
| D4  | [REMOVED 2026-05-16] iOS "不选 target" 路径已不可达（Start 按钮 disabled）。                                                                                                                      |
| D5  | [A/I] | 录制中                      | hover chart       | 出现 tooltip 显示当前 t / App / Total 数值                                               |
| D6  | [A/I] | 录制中                      | 点击 chart 任意点 | tooltip 钉住，竖线指示 x；Esc 或再点取消                                                 |
| D7  | [A/I] | —                           | "samples" 计数    | 与录制秒数大致相等（1Hz）                                                                |
| D8  | [A]   | 选了 target，录制中         | adb shell 杀掉该 app | App 红线**立刻掉到 0** 并保持 0%（不能停在死前的值），Total 蓝线继续；录制不停；若再启动 app，App 线 1-2 个 tick 后从新值续画 [REGRESSION:app-cpu-frozen-after-kill] |
| D9  | [I]   | 选了 target，录制中         | 从手机上手动杀掉 app（任务栏上滑）或 Xcode kill | App 红线在**第 3 个连续看不到 target 的 tick**(≈3s)后掉到 0 并保持(不是立刻,因为 sysmontap 偶发推送 partial procs 帧时不能误判);重启 app 后续画。**正常运行时不应出现单 tick 闪 0** [REGRESSION:ios-missing-strikes-zero-flood] |

## §E 每核 CPU (PerCore)

| ID  | 平台  | 前置            | 操作 | 预期                                                                      |
| --- | ----- | --------------- | ---- | ------------------------------------------------------------------------- |
| E1  | [A/I] | 录制开始 ~1s 后 | —    | 出现 N 个 core 的 tile，N = 设备核数                                      |
| E2  | [A/I] | 录制中          | —    | 每个 core 单独颜色（SERIES_PALETTE），值 0-100%，1Hz                      |
| E3  | [I]   | —               | —    | 每核值就是 `CPU_TotalLoad`，无需 / cpu_count（iOS 端已是 per-core 0-100） |

## §F 内存 chart

| ID  | 平台 | 前置                        | 操作           | 预期                                                                                                 |
| --- | ---- | --------------------------- | -------------- | ---------------------------------------------------------------------------------------------------- |
| F1  | [A]  | 录制中（必然已选 app）      | —              | 折线只画 App PSS 紫线;system 内存以独立 tile 形式显示在 stats 行;chartSub 文字 "App PSS · MB"             |
| F1a | [A]  | 录制中,屏幕锁屏过 / app 切后台过 | —      | App PSS 折线**不应**出现"偏偏一个点零"的闪跳;sysmontap 偶发丢 procs 帧时 sampler 跳过该 tick(uPlot 不画新点),app 真死才在 3 个 tick 后才走到 0 [REGRESSION:ios-missing-strikes-zero-flood — 见 §D9 也同上] |
| F2  | [REMOVED 2026-05-16] Android 不再 auto-detect；选 app 是必填。                                                                                                                          |
| F3  | [I]  | 录制中（必然已选 app）      | —              | App (physFootprint) 单独紫线;chartSub 文字 "App PSS · MB"。**不显示** system tile(iOS sysmontap 的 system 数值与 Xcode 报告不一致,暂时隐藏)。与 Xcode "Memory" 列对得上(±5MB 内) |
| F3a | [REMOVED 2026-05-16, late] iOS system tile 已隐藏;system-mem-gb-rounding 已迁到 Android(用 SystemMemTile 直接渲染 formatBytes 字符串避免 toFixed 二次格式化)。                |
| F4  | [A/I]| 未选 target（任何平台）     | —              | Memory 区显示占位 "请先在上方选择目标 app"，不渲染 chart；同时 Start 按钮 disabled                   |

## §G FPS chart

| ID  | 平台 | 前置                         | 操作                  | 预期                                                                                        |
| --- | ---- | ---------------------------- | --------------------- | ------------------------------------------------------------------------------------------- |
| G1  | [A]  | 录制中，前台是带 UI 的 app   | —                     | FPS 线有值，1Hz                                                                             |
| G2  | [A]  | 录制游戏（SurfaceView 渲染） | —                     | FPS 取 gfxinfo 和 SurfaceFlinger 两路里的 max，不应卡 0                                     |
| G3  | [A]  | 录制游戏 ~30s                | —                     | SmallJank / Jank / BigJank 计数随时间累计；Stutter % 出值                                   |
| G4  | [A]  | 录制中                       | 点开 FpsAdvancedPanel | 显示 avg / 1%Low / MedRange / Drop(/h) / ≥30/45/55 %                                        |
| G4a | [A]  | 跑稳定 ~60fps 场景 30s       | 看 FpsAdvancedPanel   | `FPS ≥ 55` 接近 100%，颜色应为**绿色**（不是红色） [REGRESSION:fps-advanced-color-inverted] |
| G4b | [A]  | 跑稳定 ~60fps 场景 30s       | 看 FPS / Jank tile    | stutter 接近 0%，颜色应为**绿色**（不是红色）[REGRESSION:stutter-color-inverted]            |
| G4c | [A]  | 跑卡顿场景（很多 jank）      | 看 stutter tile       | stutter > 20%，颜色应为**深红**（不是绿色）                                                 |
| G5  | [I]  | —                            | —                     | FPS 不渲染（iOS jank 尚未实现，是已知项）                                                   |
| G6  | [A]   | Android 录制中,切换到别的 app(home/Settings/别的 app) | —          | FPS 折线**降到 0**(`dumpsys gfxinfo <pkg>` 按目标 pkg 查,目标不在前台 → frame counter 不增长 → FPS=0;app 真被杀 → "No process found" → 直接发 0);subtitle `Frame rate + Jank · fps`(无 scope 注解 — Android 默认就是 per-app) |
| G7  | [I]   | iOS 录制中,切换到别的 app(home/Settings/别的 app)     | —          | FPS 折线**继续有值**(DTX CoreAnimationFramesPerSecond 是 CoreAnimation tree 帧率,跟 target pkg 无关,屏幕显示什么就报什么);subtitle `Frame rate · fps · 屏幕级`(scope 注解只在 iOS FPS 出现,因为反直觉) |

## §H GPU chart

| ID  | 平台 | 前置                                    | 操作     | 预期                                                                      |
| --- | ---- | --------------------------------------- | -------- | ------------------------------------------------------------------------- |
| H1  | [A]  | Adreno 设备（高通），/sys 可读          | 录制     | Device 单值线                                                             |
| H2  | [A]  | Mali 设备（联发科 / 三星），/sys 可读   | 录制     | Device 单值线                                                             |
| H3  | [A]  | OEM 锁了 /sys（Vivo / 华为 / 小米常见） | 录制 ≥5s | chart 空，日志 warn "stopping GPU emission"，后续 sample 不再尝试读       |
| H4  | [I]  | 录制中                                  | —        | Tiler / Renderer / Device **三条线**同时出现，y 轴对齐                    |
| H5  | [I]  | 切换不同 GPU 负载场景                   | —        | 三条线值跟随场景变化（不能恒为同一个值，那是 GraphicsClient bypass 失效） |
| H6  | [A/I] | 录制中,切换到别的 GPU 重负载 app(如游戏/视频) | —    | GPU chart 值**跟着变**(整块 GPU 利用率,不区分 app);subtitle `Device + Renderer + Tiler · %`(无 scope 注解 — GPU 是设备级是行业常识,不需提示);Android `/sys/class/kgsl/.../gpubusy` / iOS DTX `services.graphics.opengl` 都是 device-wide 信号  |

## §I 温度 chart

| ID  | 平台 | 前置                    | 操作     | 预期                                                              |
| --- | ---- | ----------------------- | -------- | ----------------------------------------------------------------- |
| I1  | [A]  | thermal_zone 可读的设备 | 录制     | CTemp (橙) + BTemp (青) 两条线，2s 间隔                           |
| I2  | [A]  | OEM 锁了 thermal_zone   | 录制 ≥6s | CTemp 缺失（auto-stop after 3 polls），BTemp 仍有（dumpsys 通用） |
| I3  | [I]  | —                       | —        | 温度 chart **不渲染**（App.tsx 平台 gate）                        |

## §J Marker

| ID  | 平台  | 前置                            | 操作                                           | 预期                                                               |
| --- | ----- | ------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------ |
| J1  | [A/I] | 录制中                          | 点 "Marker" 按钮                               | 每个 Live chart 上立刻出现一条竖虚线（slate-gray）                 |
| J2  | [A/I] | 录制中                          | Cmd+Shift+M（Mac）或 Ctrl+Shift+M（Win/Linux） | 同 J1                                                              |
| J3  | [A/I] | 已加 N 个 marker                | —                                              | 按钮显示 "Marker (N)"                                              |
| J4  | [A/I] | 录制中                          | 点击某条 marker 竖线                           | 弹出 popover，含 label 输入框 + Delete/Cancel/Save                 |
| J5  | [A/I] | popover 打开                    | 输入 label，点 Save                            | 该 marker 旁出现 label chip；popover 关闭                          |
| J6  | [A/I] | popover 打开                    | Esc 或 Cancel                                  | popover 关闭，无变更                                               |
| J7  | [A/I] | popover 打开                    | 点 Delete                                      | marker 从所有 chart 上消失                                         |
| J8  | [A/I] | 录制中                          | 在某 chart 上按住 marker 拖动                  | 拖动过程中**每个**chart 上的同 marker 同步移动；mouseup 后写回后端 |
| J9  | [A/I] | session 开始**之后**加的 marker | 立即点击它                                     | popover 应打开 [REGRESSION:marker-stale-closure]                   |
| J10 | [A/I] | 录制中                          | 点 Stop                                        | markers 留在 chart 上不消失（直到切走 / 重开 session）             |

## §K History tab

| ID  | 平台  | 前置                                                                                   | 操作                     | 预期                                                                                                            |
| --- | ----- | -------------------------------------------------------------------------------------- | ------------------------ | --------------------------------------------------------------------------------------------------------------- |
| K1  | [A/I] | 至少录过一条 session                                                                   | 切到 History tab         | 列表按 wall_start_ms desc 排序                                                                                  |
| K2  | [A/I] | 列表已加载                                                                             | 点某条 session           | 右侧渲染 6 类 Static chart（CPU/PerCore/FPS/GPU/Memory/Temp，按数据有无）                                       |
| K3  | [A/I] | 录制中的 session 在列表里                                                              | 试图删它                 | UI 阻止（按钮 disabled 或选项不可见）                                                                           |
| K4  | [A/I] | 非活跃 session                                                                         | 点删除 → 确认            | 该 session + 所有 samples + markers 一起删除                                                                    |
| K4a | [A/I] | 勾选 ≥3 条 session 中其中一条对应的 DB 行已经被外部破坏 / 文件权限错 / 模拟一个 reject | 点确认删                 | 成功的取消勾选并消失，失败的保留勾选状态便于重试，modal 关闭，列表刷新 [REGRESSION:bulk-delete-partial-failure] |
| K5  | [A/I] | History 模式下展开 session 含 markers                                                  | —                        | 每个 Static chart 上都有 marker 竖线 + label chip                                                               |
| K6  | [A/I] | History StaticCpuChart 上有 marker                                                     | 拖动它                   | 拖完后 ts_us 改变（再次进 History 验证位置已变）                                                                |
| K7  | [A/I] | History StaticCpuChart 上有 marker                                                     | 点击它 → Delete          | 该 marker 在所有 Static chart 上消失 [REGRESSION:static-cpu-missing-delete-handler]                             |
| K8  | [A/I] | History StaticCpuChart 上有 marker                                                     | 点击它 → 改 label → Save | label chip 文字变化 [REGRESSION:static-cpu-missing-labelEdit-handler]                                           |
| K9  | [A/I] | 任一 Static chart                                                                      | hover                    | tooltip 显示当时刻的值                                                                                          |
| K10 | [A/I] | 任一 Static chart                                                                      | 点击                     | pin tooltip 钉住；Esc 或再点取消                                                                                |

## §L 应用生命周期 / 日志

| ID  | 平台  | 前置                                                   | 操作                                                              | 预期                                                                                           |
| --- | ----- | ------------------------------------------------------ | ----------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| L1  | [A/I] | 正在录制                                               | 强杀进程（kill -9 / 拔电源 / 任务管理器结束）                     | 下次启动时启动 hook 把那条 session finalize（wall_end_ms 写入），不再显示"录制中"              |
| L1a | [A/I] | 用旧版本（schema v3）跑一次产生 DB，然后用当前版本启动 | —                                                                 | migrations 自动 apply 到 v4，新增 `markers` 表，无报错 [REGRESSION:schema-migration-atomicity] |
| L2  | [A/I] | app 运行过一次                                         | 检查日志目录                                                      | macOS `~/Library/Logs/app.mperf/` 有 `mperf.log.YYYY-MM-DD`；按天滚动                   |
| L3  | [A/I] | 首次启动前撤掉 `pnpm fetch:adb` 拉下来的 adb           | 启动                                                              | 日志 warn "no bundled adb found"，但还能跑（系统 adb 在 PATH 上的话）                          |
| L4  | [A/I] | 故意触发 panic（改代码塞 panic!）                      | 启动                                                              | `$TMPDIR/mperf-panic.log` 里能看到 panic 内容                                            |
| L5  | [A/I] | 录制结束                                               | 检查 macOS `~/Library/Application Support/app.mperf/data.db` | 文件存在，sqlite3 能打开，里面有 sessions / samples_wide / samples_long / markers 四张表       |

## §M UI / UX 杂项

| ID  | 平台  | 前置                      | 操作                               | 预期                                                           |
| --- | ----- | ------------------------- | ---------------------------------- | -------------------------------------------------------------- |
| M1  | [A/I] | Live tab 录制中           | 切到 History tab 再切回            | Live 上原来的数据还在（chart 不重建，状态不丢）                |
| M2  | [A/I] | 窗口最大化 / 改尺寸       | 拖窗口边                           | 所有 chart 自适应宽度                                          |
| M3  | [A/I] | success/info banner 出现  | 等待 3.5s                          | 自动消失                                                       |
| M4  | [A/I] | error/warning banner 出现 | 等待                               | **不**自动消失，需点 Dismiss                                   |
| M5  | [A/I] | 已选中 device             | 录制中再去下拉切**别的**设备       | 弹 info banner "Stop the current session first."，不切换选中项 |
| M6  | [A/I] | Live tab 在用任一 chart   | 拖窗口缩小再放大                   | **所有 Live + Static chart 实时跟着重画**，画布宽度跟容器同步；用 ResizeObserver 监听 hostRef，比 window.resize 更稳 [REGRESSION:live-chart-not-responsive] |
| M7  | [A/I] | Live tab 录制中           | 拖左侧 Sider 改宽度                | 右侧 chart 同步变宽变窄（这条以前只有窗口 resize 时才行）                                                       |
| M8  | [A/I] | App 启动后                | 看顶部 tabs 下方                   | **只**有一条横向分割线(Header borderBottom);Arco 自带的 tab 基线已隐藏 [REGRESSION:two-lines-under-top-tabs]    |
| M9  | [A/I] | 应用窗口启动后立即看 Sider 下三个 tab(设备/设置/关于) | —             | **首次渲染就等宽平分**整个 Sider 宽度;以前需要拖 sidebar 一下才能看起来均分 [REGRESSION:sidebar-tabs-flex-noop]   |
| M10 | [A/I] | 拖 Sider 拖到一半,把焦点切到别的窗口(或锁屏) | —    | 切回来 Sider 不应卡在 dragging 状态(`pointercancel` 已处理) [REGRESSION:sidebar-drag-stuck-on-pointercancel]   |
| M11 | [A/I] | 任意 warning 类 toast 弹出 | 等 3.5s                              | **自动消失**(watchdog disconnect / EVENT_SESSION_ENDED / heartbeat 都有 `auto: true`);error 类仍需手动 Dismiss   |
| M12 | [A/I] | CPU chart 在最上面、其右上角有"Total + App · %" sub-text | 点 Start | toast 在屏幕**上方中央**(top: 60, translateX(-50%)),盖在 chart card header 的中间留白区(`.chartHeader` 是 `justify-content: space-between`,中间是 padding),不覆盖左侧 title 也不覆盖右侧 chartSub [REGRESSION:toast-overlaps-chart-subtext]   |
| M13 | [A/I] | 录制中 backend 触发 EVENT_SESSION_ENDED 时,用户在 History tab | —    | 切回 Live tab 应该仍能看到 toast(如未过 3.5s);或者根据 `auto: true` 自动消失也算正常。**关键**: 切 tab 后 toast 不应该被 `display: none` 祖先吞掉(`createPortal` 渲染到 body) [REGRESSION:toast-swallowed-by-display-none-ancestor]   |

## §P 日志面板（log terminal）

新功能（2026-05-16）。chart 区域底部新增 `日志` 复选框 + 可调高度的日志终端面板。

| ID  | 平台  | 前置 | 操作 | 预期 |
| --- | ----- | ---- | ---- | ---- |
| P1  | [A/I] | 任意 | 看 chart 区底部 28px 的 toolbar | 有 `日志` checkbox。未选设备 / 设备 unusable 时 checkbox disabled |
| P2  | [A]   | 选了 Android 设备 + 选了 app | 勾选 `日志` | 下方滑出黑底等宽字体的终端面板，默认高 240px；toolbar 显示 `logcat · pid=<pid>（已 filter）` |
| P3  | [A]   | 选了 Android 但**未**选 app | 勾选 `日志` | 终端打开，toolbar 显示 `logcat · 全部 pid`（不带 pid filter） |
| P4  | [I]   | 选了 iOS USB 设备 + 选了 app | 勾选 `日志` | 终端打开;backend log 出现 `starting iOS os_trace_relay` + `OsTraceStream::start returned`;app 在前台做点操作(navigate / tap)能看到 Flutter print / Swift Logger 输出。**不是** syslog_relay(死路);用 `com.apple.os_trace_relay`(Console.app 同源),能看到 os_log/NSLog 一切现代 logging API |
| P4a | [I]   | 选了 iOS USB 设备 **未选** app | 勾选 `日志` | 终端打开;无 filter → 整机所有 process 的 os_trace 流入;能看到 SpringBoard / WebKit / 系统 daemon 各种 image_name |
| P4b | [I]   | 选的 app 在设备上**没安装**(bundle id 不存在) | 勾选 `日志` | backend `bundle→exec resolve failed` warn log;stream 退化为整机模式(fallback),不会卡死 |
| P4c | [I]   | 选了 iOS USB + app(任意 native modern app,如微信/EVE/Mail) | 勾选 `日志`,操作 app | 看到的日志包括 os_log 输出(以前 syslog_relay 看不到的)。**关键回归点**:之前 syslog_relay 模式下整个 P4 是空的(只有 kernel),换 os_trace_relay 后能正常出 app 日志 [REGRESSION:ios-syslog-empty-on-modern-apps] |
| P5  | [A/I] | 日志开了 | 鼠标移到面板顶部边缘 4px | 出现 row-resize 光标；按下拖动改高度，松开后写入 localStorage `mperf.logHeight` |
| P6  | [A/I] | 日志开了 | 输入框输入关键字 | 列表只剩匹配的行（tag / 内容 / process 任一含关键字，不区分大小写）|
| P7  | [A/I] | 日志开了 | 切 `≥ Warn` 下拉 | 只剩 W/E/F 级别 |
| P8  | [A/I] | 日志开了，已滚到底 | —— 新日志到达 | 自动滚到底部 |
| P9  | [A/I] | 日志开了，手动往上滚 | —— 新日志到达 | **不自动滚** （工具栏右侧出现 `· 已停滚` 标识）；滚回底部恢复 auto-scroll |
| P10 | [A/I] | 日志开了 | 点 `暂停接收` | buffer 不再增长；新日志被丢弃；状态显示 `已暂停 · paused` |
| P11 | [A/I] | 日志开了 | 点 `清空 buffer` | 终端清空；计数变 0/0 |
| P12 | [A/I] | 日志开了 | 取消 `日志` checkbox | 终端面板收起，backend 收到 `stop_log_stream`（后端日志 `log stream task exited cleanly`）|
| P13 | [A]   | 日志开了，target app 跑着 | 杀掉 target app | 日志流静默（pidof filter 死锁定在旧 pid）；用户需要 toggle 关再开重新 resolve pid。**这是已知行为，不是 bug** |
| P14 | [A/I] | 录制中 + 日志开了 | 拔线 | 终端流停止（adb logcat exits / syslog_relay EOF），后端日志 `EOF — device likely disconnected`；toggle 仍为开但不再有新行 |
| P15 | [A/I] | 没录制 | 勾选 `日志` | 日志正常出值（log terminal 跟 recording 解耦）|
| P16 | [A/I] | 日志面板高度调到极小 / 极大 | —— | clamp 在 120 / 600，不会更窄 / 更宽 |
| P17 | [A/I] | 日志开了，每秒 ≥ 100 行的高频日志 | —— | UI 不卡（1000 行 ring buffer 限制；filter 是 useMemo 缓存的）|

## §N 侧边栏 Tabs（设备 / 设置 / 关于）

| ID  | 平台  | 前置                       | 操作                                  | 预期                                                                                  |
| --- | ----- | -------------------------- | ------------------------------------- | ------------------------------------------------------------------------------------- |
| N1  | [A/I] | 选了设备                   | 看"设备"tab                          | 顶部第一行 "Connection" 显示 USB 或 Wi-Fi（iOS Wi-Fi 时带 "· 不可采样"），下面 DeviceInfoPanel 一行一对 label:value（单列布局，窄栏友好） |
| N2  | [A/I] | 未选设备                   | 看"设备"tab                          | 显示"请在上方选择设备"占位                                                          |
| N3  | [A/I] | 任意状态                   | 点"设置"tab                          | 显示诊断信息列表：app 版本 / OS / schema 版本 / idevice / adb / adb 路径 / data 目录 / log 目录 |
| N4  | [A/I] | 设置 tab 打开              | 点 data dir 或 log dir 右侧 📁         | 系统文件管理器打开对应目录（mac=Finder, win=Explorer, linux=xdg-open）              |
| N5  | [A/I] | 设置 tab 打开              | 点任意行右侧 📋                       | 内容复制到剪贴板（注意：可能要权限，第一次失败别慌）                                |
| N6  | [A/I] | 任意状态                   | 点"关于"tab                          | 显示 app 名 + 版本 + Apache-2.0 + 技术栈列表                                          |
| N7  | [A/I] | 设备 tab 内容很长          | tab 内滚动                           | 内容在 tab 内 scrollY，不顶动右侧主面板布局                                          |

---

## 回归 bug 历史（必跑）

下列回归点在过去出过 bug，每次重构必须验证：

- **REGRESSION:marker-stale-closure** (J9) — `MarkerOverlay.tsx` 的 useEffect 空依赖闭包导致 session 期间新加的 marker 点不开 popover。修复：`markersRef` 镜像。
- **REGRESSION:session-ended-on-user-stop** (C2) — 用户点 Stop 后 UI pump 任务在 `RecvError::Closed` 时无脑 emit `EVENT_SESSION_ENDED`，弹误导提示。修复：`Arc<AtomicBool> user_stopping`。
- **REGRESSION:static-cpu-missing-delete-handler** / **labelEdit-handler** (K7, K8) — HistoryView 给 StaticCpuChart 漏传了 4 个 marker callback 中的 2 个。修复：`MarkerControls` 单 prop 打包。
- **REGRESSION:watchdog-2-strike** (C4, C5) — `LiveView` watchdog 的"2 次连续 poll 才停"机制其实不工作：sync effect 把 `selected.usable` 同步成 false 后，watchdog 条件 `selected.usable && !current.usable` 恒为 false，strike 计数清零。修复：条件改成 `!current.usable`，deps 改成 `selected?.id`。
- **REGRESSION:fps-advanced-color-inverted** (G4a) — FpsAdvancedPanel 用 `pctColor`（CPU 语义，高=红）给"高占比=好"指标着色（MedRange / ≥30/45/55%），结果越流畅颜色越红。修复：换成 `fpsColor`。
- **REGRESSION:schema-migration-atomicity** (L1a) — `schema::run_migrations` 之前 `unwrap_or(0)` 把 DB 错误静默成"重跑全部 migration"，且 migration 的 DDL + version 写入不在事务里。崩溃恢复或 DB 损坏后启动会试图二次 CREATE TABLE → 直接挂。修复：错误用 `?` 上抛，每个 migration 包在 `c.transaction()` 里。
- **REGRESSION:stutter-color-inverted** (G4b, G4c) — `StaticFpsChart` 的 stutter tile 走默认 fallback 用了 `fpsColor`（高=绿），而 stutter 是"低=好"语义，导致 0% stutter 显示深红，50% 显示浅绿。`LiveFpsChart` 同样问题（虽然 fallback 没启用，但显示为中性灰也丢信息）。修复：新增 `stutterColor()` (低=绿)，两边都显式传 `valueColor`。
- **REGRESSION:system-mem-gb-rounding** (F3a) — `LiveMemoryChart` 的 system tile 把 `formatBytes(b)` 拆成数值 + 单位再过 `StatTile` 的 `toFixed(0)`，导致 GB 量级被二次格式化丢精度（1.5 GB → "2 GB"）。修复：用专门的 `SystemMemTile` 组件直接渲染 `formatBytes` 字符串，不走 StatTile 的数字格式化。
- **REGRESSION:app-cpu-frozen-after-kill** (D8 / D9) — explicit-target 模式下 app 被杀后 Android (`crates/android/src/cpu.rs`) 的 `pidof` / iOS (`crates/ios/src/cpu.rs`) 的 sysmontap processes dict 都查不到目标 PID，原本两端都是静默不 emit `CpuAppPct`，前端 uPlot 没新点，App 红线末端 y 值就一直挂着，Total 继续走，看着像 app 还在跑。修复：两端都在"目标 app 不存在"的 tick 显式 emit `CpuAppPct = 0.0`（app 不存在 → CPU 占用就是 0%，语义对，两端语义对齐）。iOS 上严格要求"这一 tick 确实推了 procs 但找不到 target"才报 0——sysmontap 不是每个 tick 都推 procs，没推的 tick 不能瞎报。重启后下一个有效 tick 自然续画。Mem 不归 0 故意保留——"app 不存在"时内存语义未定义，0 MB tile 会误导。
- **REGRESSION:live-chart-not-responsive** (M6, M7) — Live 图表完全不跟随窗口 / sidebar 缩放,History 却正常。第一轮误诊为 `window.resize` 事件不可靠 → 改 `ResizeObserver`(在 `apps/desktop/src/lib/chartResize.ts`),12 个图表全切;但根本症状不变。**真正的根因**是 flexbox `min-width: auto`:uPlot 创建时把 root div 写死像素宽,这个宽度沿着 chartHost → chartCard → Content(flex 子项)冒泡成 min-content;Arco `Layout.Content` 没显式给 `min-width: 0`,所以 Content 永远不肯缩。容器不缩 → RO 当然不触发 → 图表不重画。HistoryView 用 CSS Grid (`grid-template-columns: 320px 1fr`),grid 子项默认 `min-width: 0`,所以一直没事。最终修复:`Content style={{ minWidth: 0 }}` + `.chartHost { min-width: 0; overflow: hidden }`。两层加在一起才彻底放开。ResizeObserver 那部分保留,跟拖 sidebar 触发 Layout 内部重排时仍然需要——它没错,只是不够。
- **REGRESSION:bulk-delete-partial-failure** (K4a) — `HistoryView.handleConfirmDelete` 用 `Promise.all`，一条失败时其它已成功的请求结果被丢，UI state 全部不更新（modal 不关、checked 全保留、列表不刷新），出现状态与后端实际不一致。修复：`Promise.allSettled` 分拣成功/失败，已成功的取消勾选，失败的保留以便重试。
- **REGRESSION:scheduler-no-event-on-natural-exit** (C4, C5) — Backend scheduler task 自然退出(sampler 报 DeviceDisconnected 等)时 broadcast 不会关闭(因为 `SchedulerHandle` 拿了 `live_tx` clone 用作 `subscribe()`),导致 UI pump 的 `RecvError::Closed` 分支永远不进 → `EVENT_SESSION_ENDED` 完全不发。前端只能等 list_devices watchdog 1-3s 后才知道。修复:`SchedulerHandle` 加 `exit_notify: Arc<Notify>`,scheduler task 退出时 `notify_one()`,`session.rs::spawn_exit_watcher_task` 监听并在 `user_stopping=false` 时主动 emit `EVENT_SESSION_ENDED`。Toast 现在拔线后 ~50ms 就到。
- **REGRESSION:session-leaks-after-natural-exit** (C4, C10, history-inprogress) — 上一条修复只解决了"前端通知"的半边,backend 那边 `Session` 仍在 `AppState.session` 里持有 `SchedulerHandle` → live_tx clone 还活 → broadcast 不关 → writer task 永远 await → `finish_session` 不调用 → DB 行 `wall_end_ms = NULL` → History 一直显示 in-progress。之前没暴露是因为前端 device-list watchdog 1-3s 后会调 `stop_session`,顺手把 backend session 也清掉了;exit_notify 修后 frontend 立即把 recording 翻 false,watchdog 不再触发,backend 没人收尾。修复:`spawn_exit_watcher_task` 在 emit 之后,锁 `state.session`、检查 `db_id` 匹配后取出 session,跑 `Session::stop`。`db_id` 检查负责处理并发 race(用户在 watcher 来之前点了 Start 开新 session,新 session db_id 不同 → watcher 无动作)。
- **REGRESSION:android-usable-hardcoded-true** (C4a) — `crates/android/src/devices.rs` 原本无条件 `usable: true`,即使 adb 报 `state=offline`(拔线后 adb 通常会保留条目几十秒到几分钟)前端 watchdog 也看不到 disconnect。修复:`usable = (state == "device")`,offline/unauthorized 等都标 false,推动 watchdog 的 `!current.usable` 路径触发。
- **REGRESSION:ios-missing-strikes-zero-flood** (D9, F1a) — iOS sysmontap 偶发推送 procs 帧丢 target exec(idevice 0.1.61 残留 bug,锁屏/suspend 时常见)。最初版本:每次 miss 都 emit `CpuAppPct=0 + MemAppPssBytes=0`,导致图表"偏偏一个点零"。第一轮修复:加 missing_strikes 计数,连续 3 个 miss 才 emit,**且只在 threshold-crossing tick 时 emit**(`==` 判断),之后不再 emit。**这条修复有 bug**:uPlot 的 x 轴随实时前进,但 y 没新数据点折线就停在那一个 0 点上,app 真死时 chart 看起来像消失而非"持续显示 0"。第二轮修复(当前):还是 3-strike 阈值,但 threshold 之后**每个 tick 都 emit 0**(`>=` 判断),跟 Android 行为对齐(`dumpsys meminfo "No process found" → 0` 每 tick 都发,Android `pidof` 找不到 → CpuAppPct=0 也每 tick 都发)。Counter 在 target 重新出现时 reset。**别再退回 `==` 优化**——chart 需要 0 的持续流来画 flatline。
- **REGRESSION:finish-session-overwrite** (C10) — `storage::finish_session` 没幂等性保护:writer auto-finalize 和 `Session::stop` 都会调用,第二次的 UPDATE 覆盖第一次的 timestamp(差 1-300ms),导致 `wall_end_ms` 是 cleanup 时刻而不是采样真正停掉的时刻。修复:`UPDATE ... WHERE wall_end_ms IS NULL`,第二次调用 no-op。
- **REGRESSION:schema-no-downgrade-guard** (新加 L1b 类) — `run_migrations` 之前对 `cur > HEAD` 静默不处理:用户运行新版本写了 schema_version=5,然后回退到旧版本(HEAD=4),旧版本启动不报错但后续 query 撞到不存在的列时才挂。修复:启动时检测到 `cur > HEAD` 直接 `SqliteFailure` 报错,提示用户升级或清空 data.db。
- **REGRESSION:two-lines-under-top-tabs** (M8) — Arco Tabs 内置 `.arco-tabs-header-nav::before` 一条横线,加上 `Layout.Header` 自己的 `borderBottom`,在 48px 高的 Header 里两条线相距约 8px,看起来像双下划线。修复:`App.module.scss` 用 `:global` + 顶级 className(`topTabs`) 隐藏 `::before`,只 affect 顶部 Tabs,侧边栏 Tabs 保持原状。
- **REGRESSION:sidebar-tabs-flex-noop** (M9) — 之前 `SidebarTabs.module.scss` 给 `.arco-tabs-header-title` 设了 `flex: 1`,但 Arco 的 `.arco-tabs-header` 默认 `display: inline-block`,所以 `flex: 1` 是无效声明,导致三个 tab 不平分。拖动 sidebar 偶尔看起来平分是 Arco 的 ResizeObserver 重算 header 时刚好把 inline-block 撑宽。修复:加 `.arco-tabs-header { display: flex; width: 100% }`,首次渲染就平分。
- **REGRESSION:sidebar-drag-stuck-on-pointercancel** (M10) — `useResizableSidebar` 只处理 `pointerup`,如果 OS 抢走 pointer(失焦、touch 被系统手势拦截) `pointercancel` 触发但 `pointerup` 永远不来,`dragging=true` 卡住。修复:加 `onPointerCancel` 走相同的 finish 路径;`releasePointerCapture` 用 `hasPointerCapture` guard 避免对未捕获的 pointer 调用 release 异常。
- **REGRESSION:toast-overlaps-chart-subtext** (M12) — NoticeBanner 原本 `top: 60 right: 24`,正好压在第一个 chart card 的 `.chartSub`(如 CPU 的 "Total + App · %") 上。修复:改 `bottom: 24 right: 24`(业界 toast 标准位置),不影响任何 chart header。同时给 watchdog / EVENT_SESSION_ENDED / heartbeat 三种 warning toast 都加了 `auto: true`(3.5s 自动消失),减少长期占用屏幕。
- **REGRESSION:get-diagnostics-adb-path-mismatch** (内部一致性) — `get_diagnostics` 读两次 `MPERF_ADB_PATH` 用不同 fallback,导致 Settings tab 显示的 adb 路径可能与实际 exec 的 binary 不一致(显示 "adb (system PATH)" 但实际跑 `adb`)。修复:读一次复用。
- **REGRESSION:ios-syslog-empty-on-modern-apps** (P4, P4c) — 最初 iOS 日志接 `com.apple.syslog_relay`(lockdown 旧 asl/syslog 通道),前端日志面板一片空。原因:iOS 10+ 现代 app(微信、Flutter app、所有用 os_log 的)都不再写 asl,只写 unified logging,syslog_relay 在 iOS 17+ 上几乎只剩 kernel 几行。诊断 log 显示 socket 是活的但 30s 只收到一行 `process=kernel`。修复:切到 `com.apple.os_trace_relay`(`Console.app` 同源 + idevice 0.1.61 已带),`crates/ios/src/os_trace.rs` 是新实现,旧 syslog.rs 保留 `#[allow(dead_code)]` 以备 kernel-only 诊断或回退。schema/log.rs 同步加了 `subcategory` 字段把 os_log 的 subsystem/category 都展示出来。前端 LogLineRow 改成两端共用 fixed-width 列布局(level / process / pid / tag / msg),不再因 platform 字段缺失而歪。
- **shell-injection-defense (no test ID, internal hardening)** — `is_safe_pkg_name` 之前只在 `cpu.rs` 内部用。`memory.rs` 的 `dumpsys meminfo {pkg}`、`fps.rs` 的 `dumpsys gfxinfo {pkg}` 和 SurfaceFlinger layer 名插值都缺校验。实际利用门槛极高（Android 包名 grammar 限制 + 用户从 dropdown 选），但已统一兜底。函数移到 `adb.rs` 作 `pub(crate)`，三处插值点都加校验，layer 名额外拒绝 `"`/`$`/`` ` ``/`\` 字符。
- **double tracing init** — macOS 启动 crash（在 `tao` `did_finish_launching` 里 panic 然后 abort）。修复：tracing 只在 setup() 里初始化一次。
- **iOS Wi-Fi-only 显示** — 老版本会把 Network 项当成可用项让用户点 Start 然后报错。修复：`devices.rs` USB/Network 去重 + `usable=false` + UI 灰显。

---

## 数据正确性抽检（值得手动核对）

CLAUDE.md 里多次提到"待验证"的项，这些是 perfdog 数值层面的正确性，需要用对照工具：

| 检查项                         | 平台 | 对照工具                                                               | 接受范围             |
| ------------------------------ | ---- | ---------------------------------------------------------------------- | -------------------- |
| App CPU                        | [A]  | `top -n 1 -p <pid>`                                                    | ±5%                  |
| App CPU                        | [I]  | Xcode Debug Navigator → CPU                                            | ±5%                  |
| 每核 CPU                       | [I]  | Mac 上 Activity Monitor → CPU usage                                    | ±10%（采样时机不同） |
| System mem used                | [I]  | Xcode → Memory gauge "Used"                                            | ±50 MB               |
| App memory (PSS/physFootprint) | [A]  | `dumpsys meminfo <pkg>` 里 "TOTAL PSS"                                 | ±10 MB               |
| App memory                     | [I]  | Xcode Debug Navigator → Memory                                         | ±5 MB                |
| FPS                            | [A]  | 游戏内置 FPS counter（如 Genshin / 王者）                              | ±2 fps               |
| GPU Device %                   | [A]  | OEM 自带性能面板（如 GameSpace）                                       | 趋势一致             |
| CTemp                          | [A]  | 第三方温度 app 或 termux + `cat /sys/class/thermal/thermal_zone*/temp` | ±2°C                 |
| BTemp                          | [A]  | 系统设置 → 电池信息                                                    | ±1°C                 |

---

## 维护说明

- 加新 feature → 在对应 §X 节末追加用例。
- 修 bug → 在 §M 回归列表加一行，引用对应 ID。
- 删 feature → 用例标 `[REMOVED 2026-MM-DD]` 但**不要删**（保留历史断点）。
- 这份文件**不是**单元测试规范；自动化测试看 `crates/*/tests/` 里的 cargo test。
