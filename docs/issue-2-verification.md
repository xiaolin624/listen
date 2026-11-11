# Issue #2 实现与验证记录

范围：[T1：快捷键采集耳机音频并得到可复制的本地转写](https://github.com/xiaolin624/listen/issues/2)。基线 `ba3c7d95b6ba231a147a6d9a11bd77de6d3b9867`。本记录不声称完成 #3–#11。

结论：**T1 验收通过**。已补齐不同播放来源、真实系统快捷键、菜单展开时停止、完整备用文本框与复制，以及强制禁网条件下的真实采集到复制流程。电脑控制服务故障后，用户明确指定改用 Windows 本地脚本；以下记录区分各轮验证的方法与结果。

## 自动行为验证

遵循 tdd 技能；测试前已确认唯一业务边界为一次完整的开始、停止、本地转写、备用文本及临时音频生命周期。正式 GUI、全局快捷键、应用内控制命令和测试均使用 `Application::dispatch(Control)`。测试只替换播放设备、本地引擎和快捷键注册结果，保留真实临时文件和异步应用控制。

命令在工作树 `E:\luyin\.worktrees\issue-2` 中执行，使用 `E:\luyin\.tools` 的缓存工具链和依赖。

| 纵向测试 | 实际 RED | 随后 GREEN |
| --- | --- | --- |
| 开始/停止，完整多行中英及 emoji 结果、成功清理 | `cargo test --offline --manifest-path app/listen/Cargo.toml --test capture_to_copy -- --nocapture`：5 秒后仍为 `Ready`，未开始采集 | 相同测试 1/1 通过 |
| 识别进程失败，保留可恢复 WAV | `cargo test --offline --locked --manifest-path app/listen/Cargo.toml --test capture_to_copy local_backend_failure -- --nocapture`：预期 1 个保留音频，实际 0 | 相同测试通过；完整套件 2/2 |
| 全局快捷键注册冲突 | 同一命令筛选 `failed_hotkey`：预期 `Failure`，实际 `Ready` | 相同测试通过 |
| 重复开始、处理中操作及重复停止 | 同一命令筛选 `busy_operations`：没有明确的 `Already capturing` 反馈 | 相同测试通过，结果未替换或重复 |
| 后端未准备好时启动前即明确失败 | `scripts/build-listen.ps1 -ToolsRoot E:/luyin/.tools -Task Test`：预期启动阶段 `Failure`，实际 `Ready` | 5/5 通过 |
| 采集线程中断后自动反馈并保留部分音频 | 同一脚本：5 秒后仍停留 `Capturing`，预期 `Failure` | 6/6 通过 |

另补充播放端无法打开的外部故障验证：调用同一 `Toggle` 入口，保留具体设备错误，不显示采集中或虚构音频文件。此测试在既有失败处理上直接通过，没有虚构新的 RED。最终 `scripts/build-listen.ps1 -ToolsRoot E:/luyin/.tools -Task Test` 为 **7/7 通过**；`-Task CheckFormat`、PowerShell 两个脚本的语法解析和 `git diff --check` 均通过。

手动 `cargo` 命令设置了与 `build-listen.ps1` 相同的 `CARGO_HOME`、`RUSTUP_HOME`、gnullvm linker 和 `RUSTFLAGS`；脚本已将这些设置固化，支持在工作树重跑。

## 构建与集成

正式 release 构建已通过：`scripts/build-listen.ps1 -ToolsRoot E:/luyin/.tools`。随后包含启动前检查及采集中断反馈的更新 release 也已构建通过。产物为 `app/listen/target/release/listen.exe` 和相邻 `libunwind.dll`，使用锁定的离线依赖。

审查修复工作树为 `E:\luyin\.worktrees\issue-2-review-fixes`，从根集成提交 `f8496cb85d2bff5c6110b9066ec4d469b505fe54` 开始。修复前后均运行同一业务套件，**7/7 通过**。修复后的 `-Task CheckFormat`、`-Task Build`、`build-listen.ps1` 和 `make-test-speech.ps1` 的语法解析及 `git diff --check` 均通过；release 构建耗时 29.95 秒。

## 根工作区真实集成记录（2026-11-11）

以下证据来自主流程在 `E:\luyin` 的实际运行，记录于 `E:\luyin\.artifacts\issue-2-integration\root-validation-notes.md`，对应初始集成提交 `f8496cb85d2bff5c6110b9066ec4d469b505fe54`，尚未包含后述菜单修复。根工作区也已通过 7/7 业务测试、格式检查及离线锁定依赖的 release 构建。

环境为 Windows 11 25H2（26200.9711），whisper.cpp v1.8.2 CPU、`zh`、6 线程；本地模型 SHA-256 为 `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`。

| 实际操作 | 观察结果与证据范围 |
| --- | --- |
| 用不存在的本地引擎路径启动正式进程，再执行 Start | 启动即为 `Failure`，明确指出缺失可执行文件；再次 Start 仍为失败，临时 WAV 数为 0。实际启动故障反馈通过。 |
| 正式采集开始后，使本任务专用的复制后端文件不可用，再停止 | 进入 `Failure` 并报告系统错误 2；保留可读的 8.660 秒、16 kHz 单声道 WAV（277164 字节）。原始工具和模型缓存未改变，测试副本已恢复。实际失败保留行为通过。 |
| 将 A、B 两份已知语音原文件分别交给本地引擎 | A 时长 8.080 秒，CLI 墙钟 1.929 秒；B 时长 8.445 秒，CLI 墙钟 1.923 秒。输出包含已知短句，有繁体及标点差异。这只是引擎检查，没有验证正式采集或桌面操作。 |
| 存在其他播放声音时，用正式应用采集第一份样本 | 状态为 `Capturing→Processing→Result`，采集 8.620 秒，停止到结果 3.7905 秒，成功路径剩余临时 WAV 数为 0；但结果缺少预期样本句，**不计为声音来源或识别质量验收通过**。 |

这轮采集的参考音频 RMS 为 0.0594，采集 RMS 为 0.2321，归一化互相关峰值为 0.2334，支持其他更响播放声音干扰的判断。这里不提交无关播放内容的转写。当时等待暂停声音的确认，没有把这轮尝试算作通过；用户随后已确认暂停，成功复测结果见后文。

上述初轮应用控制使用正式 `--control start/stop` 入口，没有注入桌面按键。当时 Computer Use 遇到应用访问拒绝，后又出现 `Computer Use native pipe is unavailable`、`os error 2`，重连及会话重置均未恢复。用户随后明确允许验证 Listen，并指定“用脚本验证”。本地脚本的桌面验证已完成，详见后文；没有继续把服务连接故障描述为用户尚未授权。

物理断网后的正式完整流程尚未运行。后续已补充系统强制禁网环境的实际检查，见下一节；整机物理断网不是检验离线能力的唯一方式。引擎单独运行和启动检查仍不能代替完整采集及桌面复制验收。

## 禁网隔离补充验证（2026-11-11）

使用已有 Windows 受限令牌沙箱，通过 `codex sandbox -P :workspace --sandbox-state-disable-network -C E:\luyin -- <测试程序>` 启动测试进程树，没有断开用户整机网络。测试子进程使用 `UseShellExecute = false` 创建本地程序，使 Listen、识别引擎和控制进程继承隔离环境。

- 同一外连探针连接 `github.com:443`：普通进程成功（192 ms），禁网进程明确返回 `SocketError.AccessDenied`（36 ms）。拒绝来自系统访问限制，不是把无法连接的目标或等待超时当成禁网成功。
- 正式 Listen 在隔离环境中完成本地后端和模型检查，发布 `phase=Ready`，随后通过正式 `--control quit` 正常退出；没有播放或采集音频。
- 同一隔离环境内，真实 whisper.cpp CPU 后端对原始 A、B 样本分别识别成功，退出码均为 0。A 为 2.025 秒，B 为 1.845 秒，均保留已知短句的完整发言，有繁体及标点差异。此处是识别进程墙钟时间，不是应用端到端延迟。
- 识别前后外连探针均返回 `AccessDenied`。证据保存在本机 `.artifacts/issue-2-integration/isolated-asr-evidence.json`，本次没有使用云端识别或下载模型。

这轮先补齐了本地引擎在强制禁网条件下的实际运行证据，也证实正式应用可以在该环境准备就绪。之后取得了实际采集流程的禁网证据，见下一节；两轮均没有以文件结果替代真实键盘输入、目视检查及复制按钮的证据。

## 已暂停其他声音后的真实采集复测（2026-11-11）

用户明确回复“已暂停，允许验证 Listen”后，使用正式 release 应用、真实 WASAPI 播放端采集和本地 CPU 模型，通过正式应用控制入口完成四次采集。Python `winsound` 与 PowerShell `System.Media.SoundPlayer` 分别播放原始已知语音，没有向应用注入测试音频或假识别结果，也没有选择或筛选声音来源应用。

| 环境 | 播放来源 | 采集音频长度 | 停止到结果 | 成功后临时 WAV |
| --- | --- | ---: | ---: | ---: |
| 普通进程 | Python，样本 A | 8.380 秒 | 2.294 秒 | 0 |
| 普通进程 | PowerShell，样本 B | 8.700 秒 | 2.202 秒 | 0 |
| 系统禁网进程树 | Python，样本 A | 8.330 秒 | 2.562 秒 | 0 |
| 系统禁网进程树 | PowerShell，样本 B | 8.700 秒 | 4.096 秒 | 0 |

两次应用启动均为 `Ready`；四次采集均由正式 Start/Stop 入口进入 `Capturing` 并得到 `Result`。逐字对照已知发言，普通与禁网环境的 A、B 结果分别一致：

- A 原文：“今天测试耳机音频。请保留这段发言，由用户检查并手动发送。”结果：“今天測試耳機音頻請保留這段發言由用戶檢查並手動發送”。
- B 原文：“第二个应用播放语音。本地识别无需选择会议软件，也不会打开麦克风。”结果：“第二個應用播放語音。本地識別無需選擇會議軟件,也不會打開麥克風。”

完整发言已保留，仍有繁体及标点差异；本次短样本不是普通话/混合语言质量或延迟 SLA 的承诺。每次成功后本次临时音频均清理，当前结果文件可继续读取。禁网流程前后的同一外连探针均明确返回 `SocketError.AccessDenied`（44 ms、23 ms）；应用、播放程序、控制进程与识别引擎均在该受限进程树内运行。

这补齐了不同应用播放来源、真实采集到本地结果及成功清理、以及系统强制禁网条件下同一流程的证据。本机可复核记录为 `.artifacts/issue-2-integration/normal-capture-evidence.json` 和 `isolated-capture-evidence.json`。这些流程通过正式控制命令驱动，明确记录 `desktop_key_injected=false`、`copy_button_clicked=false`，没有声称完成桌面按键、目视检查或复制验收。

## Windows 本地脚本桌面验收（2026-11-11）

用户指定使用脚本后，验证驱动只定位本次新启动的 Listen 进程及其原生控件。每次发送输入前检查窗口归属，避免把测试按键发给其他应用。正式应用和识别引擎未改动；本轮新增内容是本地验收脚本、记录及窗口截图。

普通桌面的两次完整流程均使用 Windows `SendInput` 发送真实 `Ctrl+Alt+R` 开始采集，每次组合键包含 6 个系统输入事件且全部被接受。样本播放后，通过已注册的 Shell 右键通知打开真实 `TrackPopupMenu`；使用 `GetGUIThreadInfo` 确认原生菜单处于展开状态，再分别测试快捷键停止和正式控制命令停止。后者在转入 `Processing` 时菜单仍展开，确认消息在原生菜单循环中得到处理。

结果阶段通过 Unicode `WM_GETTEXT` 读取备用文本框全文，与当前 UTF-8 结果文件逐字比较；再用原生 `BM_CLICK` 触发“复制完整结果”按钮，读取剪贴板全文并比较。没有直接把预期结果写进结果框或剪贴板来替代复制动作；复制前只设置已知哨兵值，避免把旧剪贴板误认为成功。

| 环境与样本 | 开始/停止方式 | 菜单展开后停止 | 文本框全文 | 复制全文 | 停止到结果 | 剩余临时 WAV |
| --- | --- | --- | --- | --- | ---: | ---: |
| 普通桌面 A | 系统快捷键 / 系统快捷键 | 通过 | 与结果文件一致 | 与结果文件一致 | 2.070 秒 | 0 |
| 普通桌面 B | 系统快捷键 / 正式停止命令 | 通过，处理中菜单仍展开 | 一致 | 一致 | 2.064 秒 | 0 |
| 禁网隔离桌面 A | 正式开始 / 停止命令 | 本轮未测菜单 | 一致 | 一致 | 2.215 秒 | 0 |
| 禁网隔离桌面 B | 正式开始 / 停止命令 | 本轮未测菜单 | 一致 | 一致 | 2.029 秒 | 0 |

禁网沙箱使用单独的 Windows 桌面，不能从普通桌面发送输入，也不能在其中操作普通桌面的光标。因此系统热键及菜单回归在普通用户桌面验证；禁网流程在同一隔离桌面通过正式应用控制入口驱动，并实际读取原生文本框、触发复制按钮和检查该桌面的剪贴板。Listen、本地识别、播放程序及验证进程都在受限进程树内；前后外连探针分别返回 `SocketError.AccessDenied`（64 ms、1 ms）。隔离条件保持有效，未放开网络或修改整机网络设置。

四次转写仍与前述 A、B 已知文本一致。检查了普通桌面及隔离桌面的四张 `PrintWindow` 截图：完整转写可见，按钮可用，没有文字截断；隔离桌面没有系统托盘服务时，应用也明确显示任务栏回退提示。没有发送消息、向其他输入框写入内容或执行转写文字。

本机脚本与证据位于 `.artifacts/issue-2-integration/`：

- `accept-native.ps1 -Context normal`、`ListenNativeQa.cs`；结果为 `normal-native-evidence.json`，截图为 `normal-native-hotkey.png` 和 `normal-native-control.png`。
- 在前述禁网沙箱中运行 `accept-isolated-copy.ps1`；结果为 `isolated-copy-evidence.json`，截图为 `isolated-copy-a.png` 和 `isolated-copy-b.png`。

驱动准备期间修正过前台焦点及 ANSI/Unicode 文本长度处理，修正后的完整流程重新运行通过；这些是验收驱动的问题，没有当作 Listen 的生产代码 RED 或验收通过证据。

## Standards 审查

已完成审查的原始计数：**0 项硬性违规，1 项低优先级判断**，名称为“possible Primitive Obsession”。问题是 `parse()` 通过 `Result<Result<Config, String>, String>` 和 `Ok(Err(control))` 表达正常控制操作，成功与失败语义混淆。

本次改为 `LaunchAction::{Run, Control, Help}`，外层 `Result` 表达实际解析或环境读取失败。控制操作和帮助均为明确的成功动作；帮助使用信息对话框，正常退出。此处为审查阶段的类型重构，没有新增独立解析器测试边界。

## Spec 审查

已完成审查的原始计数：**1 项 P2**，没有其他已确立的代码缺陷或范围扩大。该问题最初由静态分析确认：开始采集后打开托盘菜单，原先 `TrackPopupMenu` 在 `UI.with` 的可变借用内运行原生消息循环，重入的热键、`--control stop` 和定时器消息遇到 `try_borrow_mut` 失败后被跳过。

本次将原生菜单循环移到 `UI.with` 返回之后，打开菜单期间不持有 UI 状态借用，热键、控制命令和状态定时器继续经过原有处理入口。菜单选择在返回后只投递一次 `WM_COMMAND`，由正常命令入口取得状态；没有把所有重入消息重新排队。同步控件通知仍沿用原有借用保护。

修复先经过现有 7 个完整流程测试和静态核查，随后补齐了上述真实原生菜单回归，两种停止方式都通过。旧版本没有执行桌面 RED 复现，不能把验证驱动的准备故障算作产品 RED。验证仍从完整采集流程进入，没有为内部 GUI 函数建立独立业务测试入口。

## Issue #2 验收结论

| 验收主题 | 结果 |
| --- | --- |
| 后台启动、真实全局快捷键、必要状态 | 原生系统输入及完整流程通过；正常采集仅显示状态，没有持续字幕 |
| 不同应用的播放端混合音频、不主动开麦克风 | Python、PowerShell 已知语音均进入真实采集；采集实现仅打开 render endpoint 的 loopback |
| 真实本地结果、完整备用文本框、复制、离线运行 | 普通桌面及强制禁网流程通过，文本框与剪贴板全文一致 |
| 不自动发送、不执行转写内容 | 应用没有消息发送、大模型 API 或执行转写文字的路径；桌面驱动只操作 Listen |
| 当前结果保留、成功清理、失败保留 | 成功流程临时 WAV 全部清理；实际后端失效时已验证音频保留及明确错误 |
| 注册、设备与本地后端故障反馈 | 既有 7 项完整流程测试及实际后端缺失验证通过，没有虚构采集中状态 |
| 真实集成与可控故障使用同一业务入口 | 系统热键、原生控件与正式控制入口均进入同一应用流程；实际音频和本地模型已验证 |
| 没有固定分钟数限制、如实标注后续边界 | 已核对，长时间分段、目标回填及优化仍属于后续任务 |

本任务验收通过，满足关闭 Issue #2 的条件。没有将被其他声音污染的初轮采集、控制文件检查或单独引擎运行冒充桌面验收，也不代表父规格与 #3–#11 已完成。

## 已知边界

- 这是短语音完整流程。没有产品固定时长上限，但尚未完成持续分段、长时间压力或资源优化验证。
- 使用默认多媒体 render endpoint；没有承诺覆盖分离的通信设备、按应用独立路由或中途切换等全部情况。
- 停止后启动本地 CPU whisper.cpp CLI；不将已有 CUDA 实验推导为本版本性能结论。
- 只交付备用文本框；实际编辑区自动追加属于后续任务。应用没有发送或执行转写文字的路径。
- 失败音频保留并给出路径；完整重试/清理管理属于后续任务。

## 原生接口依据

- [Microsoft：WASAPI loopback](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)：render endpoint 的共享模式 loopback 获取播放设备混合声音。
- [Microsoft：RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)：检查注册失败，`MOD_NOREPEAT` 避免按键自动重复。
- [Microsoft：EM_SETLIMITTEXT](https://learn.microsoft.com/en-us/windows/win32/controls/em-setlimittext)：备用文本框显式设置容量，并在设置结果后核对完整 UTF-16 长度；完整当前结果另存 UTF-8 文件。
- [Microsoft：SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)：通过系统键盘输入流验证快捷键，并检查接受的事件数量及现有按键状态。
- [Microsoft：BM_CLICK](https://learn.microsoft.com/en-us/windows/win32/controls/bm-click)：模拟原生复制按钮点击，实际核对剪贴板全文。
- [Microsoft：PrintWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-printwindow)：获取本次 Listen 窗口的验证截图。
- 本地 whisper.cpp 与模型来源沿用 [官方资料核验](../.scratch/meeting-transcription/verification-sources.md)，完整运行参数在 `app/listen/src/whisper.rs`。
