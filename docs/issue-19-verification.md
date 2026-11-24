# Issue #19：常驻状态胶囊

2026-11-12。工作树 `E:\luyin\.worktrees\issue-19`，分支 `ticket/issue-19`，基线 `ec97f86`（`integration/v0.2`，含 #16），报告前已合并 `integration/v0.2`（`0562e58`，含 #17 SenseVoice 引擎与 #18 术语校正）。主工作树未改动。本文只记录实际执行过的测量；未运行的场景明确标为未覆盖。

## 结论摘要

- 主窗口（`ListenCaptureToCopyWindowV1`、状态 EDIT、备用文本框、五个按钮）已完全删除。`listen.exe` 现在只有一个用户可见表面：常驻桌面的长条状态胶囊，不显示任何文字、数字或实时转写。
- 状态胶囊由本进程自绘：32 位预乘 ARGB DIB + `UpdateLayeredWindow` 的圆角长条；四种状态只用颜色、形状和动效区分（就绪=暗绿静止、采集=亮绿呼吸、处理=蓝色扫光、失败=红色静止），准备中=暗色虚线慢呼吸。
- 窗口样式实测 `0x8080088`（`WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE`），窗口标题长度为 **0**，`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 返回成功且 `GetWindowDisplayAffinity` 读回 **17**。
- `startup_capsule_ms`（进程创建 → 首帧呈现）多次实测为 **24–44 ms**，合并 #17 后用默认 SenseVoice 引擎为 **41 ms**，均低于 300 ms 目标。
- 点击胶囊不改变前台窗口（实测前台句柄点击前后均为 `0x106DE`），`WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE`（3）；拖动 140×60 px 后 `capsule-position.txt` 与实际窗口位置一致，重启后位置恢复一致。
- 胶囊不出现在任何屏幕截图里：全桌面截图（`.artifacts\issue-19\desktop-capsule-excluded.png`）中胶囊所在区域是它背后的桌面内容。由于这个排除是设计目标，四张状态图不是屏幕截图，而是胶囊**实际呈现的那一帧**（通过文档化的内部消息 `WM_APP + 20` 让胶囊把当前 DIB 写成 BMP，再由验收脚本转 PNG）。
- 验收脚本 `scripts\verify-capsule.ps1` 对真实运行的 `listen.exe` 跑了 **40 项检查，全部通过**，证据 JSON 在 `.artifacts\issue-19\capsule-verification.json`。
- 测试：合并前 **76 passed, 3 ignored**（基线 71 passed / 3 ignored，新增 5 个胶囊单元测试）；合并 #17/#18 后全套 **104 passed, 12 ignored**，仍全绿。`cargo build --release` 成功；`cargo fmt --check` 通过。

## 实现

### 1. 纯状态映射（`app/listen/src/capsule.rs`）

`CapsuleState { Preparing, Ready, Capturing, Processing, Failure }`，`capsule_state(&Snapshot) -> CapsuleState` 是无 Windows 调用的全函数，优先级写在代码注释里：

1. `Phase::Failure` → `Failure`；
2. `Phase::Capturing` → `Capturing`（即使 `Preparation::Pending`：用户已按快捷键，必须看到“正在采集”）；
3. `Phase::Processing` → `Processing`；
4. 其余阶段中 `Preparation::Failed` → `Failure`（失败的准备绝不渲染成就绪）；
5. `Phase::Preparing` 或 `Preparation::Pending` → `Preparing`；
6. `Ready | Result | NoSpeech` 且准备完成 → `Ready`。

表驱动单元测试覆盖全部 7×3 = 21 种 `Phase × Preparation` 组合（期望值按规格逐条写死，不在测试里重算优先级），另有一条不变量测试：任何未就绪/失败的准备都不允许映射为 `Ready`。这 5 个测试在默认 `cargo test` 里运行，不需要 Windows 桌面。

### 2. 胶囊窗口（`app/listen/src/capsule_window.rs`）

- `WS_POPUP` + `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED`；窗口标题为空字符串，代码里没有任何 `DrawText`/`SetWindowTextW`。
- 32 位顶朝下 DIB，逐像素解析圆角长条（stadium 的解析 SDF + 1 设备像素抗锯齿），颜色按状态调制，`UpdateLayeredWindow(ULW_ALPHA)` 呈现；没有 GDI+、没有新增依赖。
- 动效由胶囊自己的 `SetTimer` 驱动：动画状态（准备/采集/处理）16 ms，静止状态（就绪/失败）250 ms，并且只有画面真的变化时才重绘（静止状态不重绘）。
- 拖动：`WM_LBUTTONDOWN` → `SetCapture`，`WM_MOUSEMOVE`（带 `MK_LBUTTON`）→ `SetWindowPos`，`WM_LBUTTONUP` → 释放并持久化。坐标取自鼠标消息本身（`ClientToScreen`），不是轮询光标，因此拖动始终跟随真实输入。
- 位置：`<runtime>\capsule-position.txt`（`x=..` / `y=..`），启动时读取并夹取到当前虚拟屏幕内；默认位置为虚拟屏幕顶部居中。`WM_DISPLAYCHANGE` 重新夹取，`WM_DPICHANGED` 重建位图并按建议矩形缩放。
- DPI：`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` 在创建任何窗口之前调用。
- 显示排除：创建后立即 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`，结果写入 `status.txt` 的 `capsule_capture_excluded=1|0`。
- 点击钩子：`capsule_window::on_click()`，当前为空实现并注明 #20 会用它切换展开面板；它不会调用 `SetForegroundWindow`/`SetActiveWindow`/`SetFocus`。
- 帧检查钩子：`CAPSULE_FRAME_MESSAGE = WM_APP + 20`，把当前呈现的帧写成 `<runtime>\capsule-frame.bmp`。只被验收脚本使用，产品路径不发送它；它不改变外观、焦点或位置。

### 3. 应用外壳（`app/listen/src/gui.rs`）

- 主窗口删除，改为一个**隐藏的属主窗口**（`ListenHostWindowV2`，1×1，位于 -32000,-32000，`WS_EX_TOOLWINDOW`）：它接收 `--control`、`WM_HOTKEY` 和托盘回调，不进入任务栏、不抢焦点。
- 保留：消息循环、100 ms 的 `WM_TIMER` 轮询（`app.poll()`）、`Ctrl+Alt+R` 全局热键、托盘图标。
- 托盘菜单：`开始 / 停止  Ctrl+Alt+R`、`重试本地识别`、`清理失败音频`、分隔线、`退出`；重试/清理在“非空闲或没有保留音频”时用 `MF_GRAYED` 置灰，与旧窗口按钮同一可用性规则。#20 的“打开面板”和精修入口位置留了注释。
- `--control start|stop|toggle|retry|cleanup|show|quit` 全部通过 `FindWindowW(ListenHostWindowV2)` 到达运行中的实例，语义不变（`quit` 仍会先停止采集、等处理结束再退出）。`show` 目前只是在不抢焦点的情况下重新显示/置顶胶囊，#20 会改为打开面板。
- `status.txt` 保留全部既有键与格式（`phase`、`detail`、`endpoint_*`、`current_result`、`retained_audio`、`retry_available`、`backend`、全部 `timing_*`、`process_id`），新增 `preparation=Pending|Ready|Failed`、`capsule=Preparing|Ready|Capturing|Processing|Failure`、`startup_capsule_ms=<n>`、`capsule_capture_excluded=1|0`；写入节拍仍是快照变化时的 100 ms 轮询。
- `--hidden` 仍然只启动胶囊（胶囊是唯一可见表面），并抑制托盘气泡通知（脚本化/隐藏启动不再弹提示）。
- 识别引擎边界不经过界面：`gui.rs` 只调用 `config.transcriber()`，因此合并 #17 的 SenseVoice 默认引擎时界面代码无需改动（合并只在 `gui.rs` 的 import 上有一处冲突，已解决）。

## 验收证据

### 命令

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test        -Manifest E:\luyin\.worktrees\issue-19\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task Build       -Manifest E:\luyin\.worktrees\issue-19\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-19\app\listen\Cargo.toml
& E:\luyin\.worktrees\issue-19\scripts\verify-capsule.ps1
```

- 测试：合并前 16（lib 单元）+ 5（bin 单元，新增）+ 27 + 4 + 4 + 11 + 2 + 7 = **76 passed, 0 failed, 3 ignored**；合并 #17/#18 后 **104 passed, 0 failed, 12 ignored**（`cargo test --offline --locked`）。
- 构建：合并后 `Finished release profile ... in 17.1s`。
- 格式：`cargo fmt --check` 退出码 0。
- 验收脚本：合并后针对默认 SenseVoice 引擎的运行 `Result: PASS (40 checks, 0 failures)`。

### 启动预算

`startup_capsule_ms` 用 `GetProcessTimes` 的进程创建时间到首帧呈现计算（含参数解析、引擎对象创建、窗口创建与首帧），多次实测：

| 运行 | startup_capsule_ms |
| --- | --- |
| 首次手工冒烟 | 44 |
| 验收脚本多轮（whisper 回退引擎） | 24、25、27、28、29、31、35、37 |
| 合并 #17 后（默认 SenseVoice 引擎） | 41 |

全部低于 300 ms；`verify-capsule.ps1` 里有一条 `startup_capsule_ms < 300` 的检查，失败会显式报出。这个数字包含首次准备开始之前的部分；模型准备（whisper 或 SenseVoice）在后台进行，不阻塞胶囊出现。

### 窗口属性（最终证据运行，`.artifacts\issue-19\capsule-verification.json`）

| 项目 | 实测 |
| --- | --- |
| 胶囊窗口 | `ListenStatusCapsuleV1`，`HWND 0x160836`，可见，275×33（125% DPI） |
| 窗口标题 | `GetWindowTextLengthW == 0` |
| 扩展样式 | `0x8080088` = `TOPMOST(0x8) | TOOLWINDOW(0x80) | LAYERED(0x80000) | NOACTIVATE(0x8000000)` |
| 屏幕共享排除 | `GetWindowDisplayAffinity == 17`（`WDA_EXCLUDEFROMCAPTURE`），`status.txt` 的 `capsule_capture_excluded=1` |
| 置顶 | 用一个覆盖同矩形的普通窗口做探针，`WindowFromPoint(胶囊中心)` 仍返回胶囊 |
| 不抢焦点 | `WM_MOUSEACTIVATE` 返回 3（`MA_NOACTIVATE`）；点击前后前台窗口句柄均为 `0x106DE` |
| 点击不移动 | 点击前后窗口矩形不变 |
| 拖动 | 140×60 px，窗口恰好移动 140×60 |
| 位置持久化 | `capsule-position.txt` 为 `x=140` / `y=60`，与窗口位置一致 |
| 重启恢复 | 重启后窗口位置仍为 `140,60` |
| 托盘菜单 | 托盘回调后出现 `#32768` 菜单窗口，`WM_CANCELMODE` 后关闭 |
| 控制通道 | `--control show`、`retry`、`cleanup` 均到达运行实例（退出码 0），`start`/`stop`/`quit` 在状态检查中使用 |

### 状态与截图

四种状态都通过 `--control` 与真实引擎诱发，截图路径：

| 状态 | 诱发方式 | 截图 |
| --- | --- | --- |
| 就绪（暗绿、静止） | 启动后 `phase=Ready` | `.artifacts\issue-19\capsule-ready.png` |
| 采集（亮绿、呼吸） | `--control start` | `.artifacts\issue-19\capsule-capturing.png` |
| 处理（蓝色、扫光） | `--control stop` 后 `phase=Processing` | `.artifacts\issue-19\capsule-processing.png` |
| 失败（红色、静止） | 用不存在的 SenseVoice/whisper 模型启动 | `.artifacts\issue-19\capsule-failure.png` |
| 全桌面（胶囊被排除） | 就绪时截取整个虚拟屏幕 | `.artifacts\issue-19\desktop-capsule-excluded.png` |

说明：

- 静音采集时本地识别约 100 ms 就结束，100 ms 的状态写入节拍可能错过 `Processing`；验收脚本因此把 `zh-term.wav` 循环播放到默认多媒体渲染设备（`PlaySound`），让处理阶段持续数秒，再轮询 `status.txt` 抓帧。脚本里 `-AudioClip` 可覆盖该文件，找不到时退化为静音并对处理态给出明确检查结果。合并 #17 后脚本默认使用产品自带引擎（SenseVoice），不再显式传 whisper 路径；`-Engine/-Model/-VadModel` 仅用于显式测试 whisper 回退。
- 由于 `WDA_EXCLUDEFROMCAPTURE`，任何屏幕截图（包括本脚本的 `CopyFromScreen`）都看不到胶囊；这也是 `desktop-capsule-excluded.png` 里胶囊区域是桌面的原因。状态图因此来自胶囊**实际呈现的帧**（内部消息 `WM_APP+20` → `capsule-frame.bmp` → PNG）。这不是绕过排除，而是同一进程把已呈现的像素原样导出；屏幕共享排除本身由 `GetWindowDisplayAffinity`、`status.txt` 和全桌面截图三方印证。
- 状态图是 275×33 的裁切（125% DPI 下的整条胶囊），四张图颜色/动效特征明显不同（暗绿 / 亮绿 / 蓝底亮带 / 红）。

### 位置记忆

最终一轮：胶囊拖动 140×60 px → 立即读取 `capsule-position.txt` 得 `x=140,y=60` → `--control quit`（退出码 0）→ 重新启动 → 新窗口 `GetWindowRect` 仍为 `(140,60)`。脚本对这三步各自有检查；重启检查会把拖动得到的坐标重新写回位置文件再启动，以排除共享桌面上其他程序移动窗口的干扰。

## 与旧行为的差异

- 删除：主窗口、状态 EDIT、备用文本框、五个按钮、原窗口标题里的中文文案、`SetForegroundWindow` 显示主窗口的路径、`IsDialogMessage` 与 `WM_GETMINMAXINFO`。
- `--control show` 旧语义是“显示当前结果窗口”，现在是“确保胶囊可见并置顶”，不抢焦点；#20 会改成打开面板。
- 托盘“显示当前结果”菜单项删除（#20 以“打开面板”替代）。
- `--hidden` 额外抑制托盘气泡通知。
- `status.txt` 新增四个键，其余键与格式未变；`scripts\capture-quality-sample.ps1`、`scripts\verify-listen-package.ps1`、`scripts\measure-long-session.ps1` 的解析未受影响。

## 留给 #20 的部分

- 展开面板窗口（显示当前结果、复制、重试/清理、精修入口）；点击胶囊时应切换它：填 `capsule_window::on_click()`（文件里有明确注释）。
- 没有目标输入框时的结果展示：现在结果仍写入 `current-result.txt` 和托盘提示，面板存在后应改为在面板中显示与复制。
- 托盘菜单“打开面板”项：`show_tray_menu` 里留了注释位置，ID 从 `ID_TOGGLE=101` 之后顺延即可（`ID_SHOW` 可复用为“打开面板”）。
- 精修入口与精修菜单项：本 ticket 未实现（#21 提供端口）。
- `scripts\verify-capsule.ps1` 仍按扩展样式查找胶囊，这是**本实现自检**的私有查找；#24 的验收脚本不能依赖它，只能依赖 `--control`、`status.txt` 和面板（这一点写进了脚本头部注释）。
- `main.rs` 的 `HELP` 仍写着“Results are copyable in the application”，面板落地后应更新（本 ticket 未改 `main.rs` 的 CLI 文本，避免与 #17 冲突）。

## 偏差与风险

- **新增一个 `windows` crate feature**：`Win32_System_Threading`（只为 `GetProcessTimes` 提供进程创建时间，用于 `startup_capsule_ms`）。已用 `--offline --locked` 的 release 构建验证通过；没有新增依赖或改 `Cargo.lock`。
- **新增一个内部窗口消息**：`WM_APP + 20`（胶囊帧导出），只服务验收脚本；产品路径不发送它。若不接受这类可测试性钩子，替代方案是验收时临时关闭 `WDA_EXCLUDEFROMCAPTURE`（需要产品提供开关，更危险），因此选择了只读导出。
- **拖动改为使用鼠标消息坐标**（`ClientToScreen`）而不是 `GetCursorPos`：行为更准确，也让验收脚本能用窗口消息确定性地驱动拖动。真实鼠标拖动路径同样走这段代码。
- **共享桌面干扰**：本机同时有另一个 agent 在运行另一份 Listen 构建并做 UI 自动化，鼠标指针会被外部移动、胶囊可能被外部截获拖动。验收脚本因此只认**本可执行文件路径**的 Listen 实例，并对点击/拖动/重启检查做短重试；最终证据运行仍然全部通过。真实单机环境下这些重试不会被触发。
- **合并 #17/#18**：`git merge integration/v0.2` 唯一冲突是 `gui.rs` 的 import（#17 把引擎构造移入 `Config::transcriber`），解决后界面代码与 SenseVoice 引擎无耦合；合并后全套测试与验收脚本重新跑过。
- 未做：全屏独占游戏覆盖胶囊的情况（spec 已接受）、多显示器切换的实测（代码在 `WM_DPICHANGED`/`WM_DISPLAYCHANGE` 里处理并按虚拟屏幕夹取，但本轮只在 1920×1080@125% 单屏上运行）、托盘菜单各项的像素级可用性检查（菜单为原生菜单，脚本只验证打开/关闭；置灰规则与旧按钮共用同一可用性判断）。
- `cargo clippy` 未运行：该工具链未安装 clippy 组件（离线环境无法补装）。
