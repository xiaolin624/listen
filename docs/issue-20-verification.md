# Issue #20：展开结果面板

2026-11-12。工作树 `E:\luyin\.worktrees\issue-20`，分支 `ticket/issue-20`，基线 `99363ac`（`integration/v0.2`，含 #19 常驻状态胶囊）；报告前确认 `integration/v0.2` 尖端仍是 `99363ac`，`git merge integration/v0.2` 为 `Already up to date`。主工作树未改动。

## 结论摘要

- 单击状态胶囊展开/收起第二个窗口——展开结果面板；面板也可由托盘左键、托盘菜单「打开结果面板」、`--control show` 和 `--control panel` 打开。Esc、标题栏关闭按钮和面板内「关闭」都能收起。
- 面板显示两部分：上方是状态区（当前阶段、一句人话说明、实时采集进度、应用给出的完整 `detail`），下方是**完整当前转写文本**；两者都是只读多行 EDIT，可滚动、可选择、可 Ctrl+C，程序内没有任何改动这段文字的路径。
- 按钮：`复制完整结果`（只写剪贴板，不粘贴、不发送）、`重试本地识别`、`清理失败音频`、`精修（即将可用）`（明确置灰的占位，等 #21 接线）、`关闭`。重试/清理的可用性与托盘菜单共用 `panel::recovery_available`，两个表面不会不一致。
- 没有目标输入框时结果只出现在面板里并可复制；`Delivery::Fallback` 的结果和任何 `Phase::Failure` 会自动打开面板（每次事件只自动打开一次，用户关掉后不会每 100 ms 重开），成功追加从不自动打开，自动打开不抢焦点。
- 原详情窗口（`ListenCaptureToCopyWindowV1`、状态 EDIT、备用文本框、五个按钮）已不存在；运行中的进程只有三个顶层窗口：胶囊、结果面板、隐藏的属主窗口。代码与脚本里不再引用旧窗口类名或旧控件 ID。
- 自动化验收不再依赖窗口类名/控件 ID：`--control`、`status.txt`、`panel.txt`、`current-result.txt` 覆盖面板的全部可观察行为。#24 可直接按本文“自动化契约”驱动。
- 真实 `listen.exe` 桌面验收：`scripts\verify-panel.ps1` **86 项检查全部通过**；回归运行 #19 的 `scripts\verify-capsule.ps1` **40 项检查全部通过**。全套测试 **122 passed / 12 ignored**，`cargo fmt --check` 通过，release 构建成功。

## 实现

### 1. 面板状态模型（`app/listen/src/panel.rs`，纯逻辑，无 Windows 调用）

- `status_text(&Snapshot)`：标题 + 说明 +（采集中）实时进度行 + `detail` +（模型准备失败时）提示。阶段映射：`Preparing→正在准备本地识别模型`、`Ready→就绪`、`Capturing→正在采集（实时进度）`、`Processing→正在本机识别`、`Result+Appended→已完成：结果已追加到目标输入框`、`Result+Fallback→已完成：结果只在面板中`、`NoSpeech→没有检测到语音`、`Failure→失败`。`Phase::Result`/`NoSpeech` 之外的阶段复用 `capsule_state` 的优先级，胶囊与面板不会对“是否就绪”说不同的话。
- 采集进度行用 `Snapshot::progress.received_samples`、`recognized_segments`、`pending_segments` 现算，写在长 `detail` 之前，打开面板即可看到“已接收 N 秒”，不需要滚动。
- `result_text(&Snapshot)`：完整转写文本；为空时给占位符（第一次使用、只有失败时）。
- `text(&Snapshot)`：`status_text + 空行 + result_text`，即 `panel.txt` 的完整内容，也是面板两个只读框的拼接。
- `auto_open_key(&Snapshot) -> Option<String>`：`Failure` 与 `Result+Fallback` 各给出一个稳定 key（失败按 `detail`、回退结果按完整文本）。GUI 记住上次已处理的 key：同一事件不会反复打开，用户关掉后保持关闭；下一次新的失败/新的回退结果会重新打开；成功追加返回 `None`。
- `actions(&Snapshot)`：`copy = 有文本`；`retry`/`cleanup = recovery_available`；`refine` 在 #21 之前恒为 `false`。
- `recovery_available`：非采集/非处理/非准备中且有保留音频；托盘菜单与面板按钮共用。
- `position(capsule, work, width, height, gap)`：水平以胶囊为中心，优先放在胶囊下方；下方放不下时放上方；都放不下时夹到工作区底部。始终夹进胶囊所在显示器的工作区，不覆盖任务栏、不出屏。`size(scale)` 按 `dpi/96` 缩放（640×480 逻辑像素）。

单元测试 17 个（`panel.rs`）＋1 个（`panel_window.rs` 的命令映射），覆盖：追加/回退区分、无语音时保留上一次结果、采集实时进度、失败原因与保留音频路径、准备失败不显示成就绪、空结果占位、自动打开的触发与去重、按钮可用性、托盘/面板可用性一致、DPI 缩放、上/下/夹取/多显示器/所有胶囊位置都在工作区内。

### 2. 面板窗口（`app/listen/src/panel_window.rs`）

- 第二个顶层窗口，类名 `ListenResultPanelV1`（实现私有；验收不使用它）。`WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN`，扩展样式 `WS_EX_TOOLWINDOW | WS_EX_TOPMOST`：不进入任务栏/Alt+Tab，和胶囊一样置顶，但**没有** `WS_EX_NOACTIVATE`，所以用户点它时可以获得焦点去选择、Ctrl+C。
- 打开：`ShowWindow(SW_SHOWNOACTIVATE)` + `SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE|SWP_NOMOVE|SWP_NOSIZE)`。轮询更新、失败/回退自动打开都走这条路径，不会抢焦点。只有在用户自己点击面板时才会激活。
- 两个只读 `EDIT`：`ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_VSCROLL`，`EM_SETLIMITTEXT` 提到 `0x7ffffffe`。`Panel::apply` 只在文本真正变化时 `SetWindowTextW`，并在替换前后保存/恢复选择区与首个可见行，所以采集进度每 100 ms 更新也不会把用户正在读/选的位置弹回顶部。
- 按钮行按 DPI 缩放布局；字体用 `Microsoft YaHei UI`（`CreateFontW`，失败时退回系统字体），`WM_DPICHANGED` 时重建字体并按建议矩形重排。
- 位置在每次显示前重算：胶囊所在显示器的工作区、同显示器、工作区内夹取；`WM_SIZE`/`WM_DPICHANGED` 只做子控件布局。
- 关闭：`WM_CLOSE`（标题栏 X / Alt+F4）、`IDCANCEL`（Esc，经消息循环里的 `IsDialogMessageW`）、面板内「关闭」按钮，全部改为隐藏窗口而不是销毁；会话内再次打开无需重建，位置/文本复用。窗口由 GUI 持有，退出时随 `Drop` 销毁。
- 面板按钮只向隐藏的属主窗口 `PostMessage(COMMAND_MESSAGE)`，应用在属主窗口里执行复制/重试/清理/关闭；不在面板自己的窗口过程里跑应用逻辑。`精修` 按钮占位、置灰，`command_for_control(ID_REFINE)` 故意返回 `None`，等 #21。
- 面板截图来自真实屏幕（面板不排除录屏；只有胶囊用 `WDA_EXCLUDEFROMCAPTURE`），因此验收能截到面板本身。

### 3. 应用外壳（`app/listen/src/gui.rs`）

- 胶囊点击：`capsule_window::on_click()` 现在分发给 GUI 注册的 `toggle_panel_on_click`（线程内函数指针，胶囊窗口不依赖应用状态）。单击切换面板，拖动仍然只移动胶囊。
- 托盘菜单新增第一项「打开结果面板」（`ID_SHOW`）；托盘左键也打开面板。重试/清理的置灰规则改用共享的 `panel::recovery_available`。开始/停止、退出保持原样。
- `--control` 新增：`panel`（切换，id 8）、`copy`（复制完整当前结果，id 9）；`show`（id 4）语义改为“现在打开面板”。`--control copy` 在无结果时不动剪贴板。
- 每次轮询（100 ms，快照变化时）：
  - 用同一份快照生成 `panel.txt`，内容变化才写；面板未打开时也写，所以自动化无需打开面板即可读取。
  - `status.txt` 保留全部既有键，新增 `panel_visible=1|0` 与 `panel_error=<创建失败原因，通常为空>`。
  - 自动打开：`panel::auto_open_key` 与上次不同且非空时 `open_panel()`（创建/定位/填充/`ShowWindow` 均不激活）。
  - 把快照推给已存在的面板（文本 + 按钮可用性）。
- 消息循环在面板可见时对每条消息调用 `IsDialogMessageW(panel, ...)`：Esc 关闭面板、Tab 在只读文本框与按钮之间移动；不属于面板的消息继续原样 `TranslateMessage`/`DispatchMessage`。
- 退出：面板窗口随 `Ui` 销毁；隐藏属主窗口销毁时其 owned window（面板）一并销毁。
- 顺带修复 #19 暴露的一个缺陷：`Capsule::reveal()` 的 `SetWindowPos` 少了 `SWP_NOMOVE`，会把胶囊移到 (0,0)。现在打开面板不会再移动胶囊（验收里有一条检查）。

### 4. 剪贴板（`app/listen/src/paste.rs`）

新增 `pub fn copy_to_clipboard(text: &str) -> AppResult<()>`：复用现有 `write_text`，只写剪贴板；不粘贴、不发键盘、不改任何既有粘贴/键盘行为。`复制完整结果`、`--control copy` 都只经过它。

## 自动化契约（#24 可依赖）

面板不改变任何旧的文件语义；新增两个可观察面：

| 入口/文件 | 语义 |
| --- | --- |
| `--control show` | 立即打开面板（幂等；不抢焦点） |
| `--control panel` | 切换面板显示；退出码 0 只表示命令已送达运行实例 |
| `--control copy` | 把当前完整结果写入剪贴板；无结果时保持剪贴板不变；不粘贴/不发送 |
| `runtime\panel.txt` | 面板当前全文：状态块 + 一个空行 + 完整结果（与面板两个只读控件拼接一致；控件内部以 CRLF 存储换行，比对时把 CRLF 归一为 LF）；面板隐藏时也更新 |
| `runtime\status.txt` | 保留全部既有键；新增 `panel_visible=1\|0` 和 `panel_error=`（面板窗口创建失败时的原因，正常为空） |
| `runtime\current-result.txt` | 语义不变：只保存完整当前转写文本 |

面板可见性判定：轮询 `status.txt` 的 `panel_visible=`，需要等“文本”时轮询 `panel.txt`；需要“结果本身”时用 `current-result.txt`。**不要**用窗口类名 `ListenResultPanelV1`、控件 ID 或 UIA 来驱动验收——`scripts\verify-panel.ps1` 只在读回“可见文字是否等于 panel.txt”、按真实按钮、发 Esc 这些自检动作中使用私有类名/控件 ID，属于实现自检，不属于契约。

## 验收证据

### 命令与结果

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test     -Manifest E:\luyin\.worktrees\issue-20\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task Build    -Manifest E:\luyin\.worktrees\issue-20\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-20\app\listen\Cargo.toml
& E:\luyin\.worktrees\issue-20\scripts\verify-panel.ps1
& E:\luyin\.worktrees\issue-20\scripts\verify-capsule.ps1
```

- 测试：lib 38 + bin 23（胶囊 5 + 面板 18）+ 27 + 4 + 6 + 4 + 11 + 2 + 7 = **122 passed, 0 failed, 12 ignored**；新增 18 个面板测试，旧测试无改动。
- 格式：`cargo fmt --check` 退出码 0。
- 构建：`Finished release profile ... in 9.1s`，该 release `listen.exe` 用于下面的桌面验收。
- `verify-panel.ps1`：**Result: PASS (86 checks, 0 failures)**，证据 `.artifacts\issue-20\panel-verification.json`。
- `verify-capsule.ps1`（#19 回归）：**Result: PASS (40 checks, 0 failures)**，证据 `.artifacts\issue-19\capsule-verification.json`。

### 截图

| 证据 | 内容 |
| --- | --- |
| `.artifacts\issue-20\panel-ready.png` | 就绪态面板：状态说明、完整 `detail`、占位结果、复制禁用、精修置灰 |
| `.artifacts\issue-20\panel-capturing.png` | 采集中：`正在采集（实时进度）`、`已接收 1.5 秒音频；识别窗口已完成 0 个，待处理 0 个。`、设备 `detail` |
| `.artifacts\issue-20\panel-fallback.png` | 无目标回退结果：`已完成：结果只在面板中`、原因、完整结果文本、复制可用 |
| `.artifacts\issue-20\panel-failure.png` | 模型缺失失败：`失败`、完整失败原因（含缺失模型路径）、复制/重试/清理均禁用 |
| `.artifacts\issue-20\tray-menu.png` | 托盘菜单：`打开结果面板` 在第一位，开始/停止、重试（灰）、清理（灰）、分隔线、退出 |

面板尺寸 800×600（125% DPI 下的 640×480 逻辑像素），窗口矩形 `560,91–1360,691`，位于胶囊 `822,48 275×33` 下方 10 设备像素（8 逻辑像素 × 1.25）；同一显示器工作区内。

### 真实链路验收流程（`verify-panel.ps1` 实际执行）

1. 新运行时目录启动 `listen.exe`（默认 SenseVoice），等待 `Ready`。检查 `panel.txt` 已存在、`panel_visible=0`、全部旧 `status.txt` 键仍在、`panel_error=` 为空、面板窗口在请求前不可见、没有旧类名窗口。
2. `--control show`：面板出现；`panel_visible=1`；`WS_EX_TOOLWINDOW|WS_EX_TOPMOST`、与胶囊同显示器、完全位于工作区、DPI 与胶囊一致、胶囊没有被移动；前台窗口不变（记录句柄 67294，`took_focus=false`）；`panel.txt` 与两个只读控件拼接逐字符相等；复制按钮禁用、精修按钮禁用。
3. `--control copy`（无结果）：剪贴板哨兵值保持不变。
4. `--control panel` 收起、再展开；胶囊单击收起、再展开；Esc 收起；托盘左键展开。
5. 托盘右键菜单：菜单窗口出现；菜单句柄可读；6 项（含分隔线）；第一项文本等于「打开结果面板」；重试/清理此时置灰；`WM_CANCELMODE` 关闭；随后按菜单会发出的 `WM_COMMAND(ID_SHOW=104)` 打开面板。
6. 采集：聚焦一个无编辑能力的探针窗口（保证本次采集不绑定任何可写目标，避免在共享桌面上误写别的应用），`--control start`，播放 `zh-term.wav` 到默认多媒体端点；打开面板并等待 1.5 s：`panel.txt` 含采集 `detail` 与实时接收秒数（本次 1.5 s）。随后收起面板，证明回退结果是**自己**打开面板的。
7. `--control stop`：轮询到 `Processing` 时 `panel.txt` 含 `Processing locally`；结束后 `Phase=Result`、`detail` 含 `fallback text box`、`panel_visible=1`、面板可见、前台不是 Listen；`current-result.txt` 非空；`panel.txt` 含完整结果与原因，且与可见文本一致；结果控件文本与 `current-result.txt` 逐字符相等；复制按钮启用、重试/清理禁用。
8. `--control copy` 与面板内真实 `复制完整结果` 按钮（`BM_CLICK`）都把完整结果放进剪贴板（与 `current-result.txt` 相等）。
9. 静音采集：收起面板后 `start→2 s→stop`，得到 `NoSpeech`；`current-result.txt` 不变；`panel_visible=0`（不自动打开）；`panel.txt` 含 `No valid speech detected` 且仍保留上一次结果。
10. `--control quit` 干净退出；无残留进程。
11. 失败自动打开：新运行时目录、`--sensevoice-model <missing>` 启动；`Phase=Failure`、`preparation=Failed`；`panel_visible=1` 且面板可见（面板此前从未创建过，证明首次自动创建路径）；`panel.txt` 含完整失败原因，与可见文本一致；复制/重试/清理禁用、精修禁用；自动打开不抢焦点；`--control quit` 干净退出。

### 与胶囊的关系

`--control show` 不再只是“把胶囊置顶”，而是打开面板；托盘左键同样打开面板。`verify-capsule.ps1` 的胶囊自检（样式、共享隐身、拖动位置、不抢焦点、托盘菜单打开/关闭）在合并后重跑仍 40/40 通过；胶囊点击现在会切换面板，但点击本身仍不改变前台窗口、不移动胶囊。

## 旧窗口依赖清理

- 源码：`git grep ListenCaptureToCopyWindowV1` 只命中历史文档（ADR 0002、issue-19 验证记录），代码中 0 处。旧控件 ID（101–106）已不存在；`gui.rs` 的常量全部重新定义为隐藏属主窗口/托盘/控制命令使用的 ID，且不再创建按钮/EDIT。
- 运行中的进程只有：`ListenStatusCapsuleV1`（唯一自绘窗口）、`ListenResultPanelV1`（新面板）、`ListenHostWindowV2`（隐藏属主）；`verify-panel.ps1` 用枚举窗口断言旧类名窗口不存在。
- 脚本盘点：
  - `scripts\run-listen.ps1`：原来只接受 `start|stop|toggle|retry|cleanup|show|quit`；现补充 `panel`、`copy`，启动提示改为“点胶囊或用 --control show 打开面板”，并输出 `panel.txt` 路径。不读任何窗口控件。
  - `scripts\capture-quality-sample.ps1`：只读 `status.txt`/`current-result.txt`，不涉及窗口控件，未改。
  - `scripts\measure-long-session.ps1`：驱动 `examples\long_session.exe`（应用边界），与 GUI 无关，未改。
  - `scripts\verify-listen-package.ps1`：只读 `status.txt` 并调用 `--control quit`，不涉及窗口控件，未改。
  - `scripts\verify-capsule.ps1`：本就已迁到 `--control`/`status.txt`；仍在自检里用扩展样式找胶囊（#19 的私有查找），#24 不得使用。回归通过。
  - `scripts\verify-generic-editor.ps1`/`test-focused-editor.ps1`：针对外部编辑器的 UIA 验收，与 Listen 窗口无关，未改。
- `status.txt` 的写节拍、`current-result.txt`、`latency.jsonl`、所有旧键与格式未变。

## 留给 #21（精修）的部分

- 面板上已有置灰的「精修（即将可用）」按钮（控件 ID 204）。#21 接线时：在 `panel::actions` 用快照里的精修可用性替换恒 `false`；让 `command_for_control(ID_REFINE)` 返回一个新命令；在 `gui::panel_command` 里分发到精修端口；把按钮文案改成可用态。
- 精修结果回填与“目标被手改不覆盖”的安全边界在应用层（#21），面板只负责在不可追加时展示结果；`panel.txt` 会自动跟着快照走，不需要新增文件。
- 单测 `only_the_documented_controls_produce_owner_commands` 明确断言 `ID_REFINE → None`，接线时需同步更新。

## 偏差与风险

- **面板常驻置顶**：需求要求“靠近胶囊、别被其他窗口盖住”，因此面板与胶囊一样 `WS_EX_TOPMOST`；用户关掉即消失。若后续觉得挡视线，可改普通 z-order，#20 的行为检查不受影响。
- **自动打开用“事件 key”去重**：同一失败原因（相同 `detail`）不会在用户关闭后立刻重开；新的失败/新的回退结果会重开。这是刻意选择，避免每次轮询抢屏。
- **共享桌面导致的前台检查抖动**：本机同时有其它 agent 在做 UI 自动化，验收中偶发前台窗口被外部改变；脚本对“打开面板不抢焦点”做最多 6 次短重试，最终证据运行第一次即稳定（记录前台 `0x106DE` 一类句柄）。真实单机不会遇到。
- **桌面追加路径未单独截图**：无目标/回退路径已真机验证；“成功追加不自动打开面板”由 `panel::auto_open_key` 的单元测试覆盖（`Appended → None`），真实追加路径留给 #24 的全量回归（那边本来就要覆盖目标追加）。
- **真实识别内容质量不属 #20**：本次证据里的转写文本来自当前 SenseVoice + 术语表链路，个别词有误（例如 `deep harness`），属 #17/#18/#22 的质量范围；面板只保证“原样显示/复制应用给出的完整结果”。
- **新旧实例共存**：`--control` 仍按窗口类名找实例（#19 行为），如果同时运行另一份 Listen 构建，控制命令可能落到那个实例。验收脚本要求本可执行文件的实例为唯一实例；没有改这个既有协议。
- **根 `README.md` 与 `packaging\README.md` 仍描述 v0.1 主窗口**：本次只更新了代码/脚本与 `--control` 帮助文本，包与 README 属 #23 的交付物；`packaging\README.md` 未改，避免与 #23 冲突。
- 未运行 `cargo clippy`：该工具链未安装 clippy 组件（离线环境无法补装）。
