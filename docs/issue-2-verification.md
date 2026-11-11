# Issue #2 实现与验证记录

范围：[T1：快捷键采集耳机音频并得到可复制的本地转写](https://github.com/xiaolin624/listen/issues/2)。基线 `ba3c7d95b6ba231a147a6d9a11bd77de6d3b9867`。本记录不声称完成 #3–#11。

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

最后一次采集的参考音频 RMS 为 0.0594，采集 RMS 为 0.2321，归一化互相关峰值为 0.2334，支持其他更响播放声音干扰的判断。这里不提交无关播放内容的转写。主流程已请求用户暂停其他声音，回复仍未收到，未继续播放或采集；干净环境下的不同应用来源验收尚未完成。

上述应用控制使用正式 `--control start/stop` 入口，没有注入桌面按键。Computer Use 明确拒绝访问新建 Listen 应用、Windows Media Player 和 Chrome；没有通过其他自动化方式绕过拒绝。因此实际全局快捷键、完整备用文本框的目视检查和复制按钮仍待验证，不能用状态文件或结果文件替代这些证据。

物理断网后的正式完整流程尚未运行。后续已补充系统强制禁网环境的实际检查，见下一节；整机物理断网不是检验离线能力的唯一方式。引擎单独运行和启动检查仍不能代替完整采集及桌面复制验收。

## 禁网隔离补充验证（2026-11-11）

使用已有 Windows 受限令牌沙箱，通过 `codex sandbox -P :workspace --sandbox-state-disable-network -C E:\luyin -- <测试程序>` 启动测试进程树，没有断开用户整机网络。测试子进程使用 `UseShellExecute = false` 创建本地程序，使 Listen、识别引擎和控制进程继承隔离环境。

- 同一外连探针连接 `github.com:443`：普通进程成功（192 ms），禁网进程明确返回 `SocketError.AccessDenied`（36 ms）。拒绝来自系统访问限制，不是把无法连接的目标或等待超时当成禁网成功。
- 正式 Listen 在隔离环境中完成本地后端和模型检查，发布 `phase=Ready`，随后通过正式 `--control quit` 正常退出；没有播放或采集音频。
- 同一隔离环境内，真实 whisper.cpp CPU 后端对原始 A、B 样本分别识别成功，退出码均为 0。A 为 2.025 秒，B 为 1.845 秒，均保留已知短句的完整发言，有繁体及标点差异。此处是识别进程墙钟时间，不是应用端到端延迟。
- 识别前后外连探针均返回 `AccessDenied`。证据保存在本机 `.artifacts/issue-2-integration/isolated-asr-evidence.json`，本次没有使用云端识别或下载模型。

这补齐了本地引擎在强制禁网条件下的实际运行证据，也证实正式应用可以在该环境准备就绪。由于其他声音暂停及 Listen 桌面控制授权尚待确认，仍未将其记作真实播放采集、快捷键、完整备用文本框及复制的整条离线验收通过。

## Standards 审查

已完成审查的原始计数：**0 项硬性违规，1 项低优先级判断**，名称为“possible Primitive Obsession”。问题是 `parse()` 通过 `Result<Result<Config, String>, String>` 和 `Ok(Err(control))` 表达正常控制操作，成功与失败语义混淆。

本次改为 `LaunchAction::{Run, Control, Help}`，外层 `Result` 表达实际解析或环境读取失败。控制操作和帮助均为明确的成功动作；帮助使用信息对话框，正常退出。此处为审查阶段的类型重构，没有新增独立解析器测试边界。

## Spec 审查

已完成审查的原始计数：**1 项 P2**，没有其他已确立的代码缺陷或范围扩大。该问题由静态分析确认，未在桌面复现：开始采集后打开托盘菜单，原先 `TrackPopupMenu` 在 `UI.with` 的可变借用内运行原生消息循环，重入的热键、`--control stop` 和定时器消息遇到 `try_borrow_mut` 失败后被跳过。

本次将原生菜单循环移到 `UI.with` 返回之后，打开菜单期间不持有 UI 状态借用，热键、控制命令和状态定时器继续经过原有处理入口。菜单选择在返回后只投递一次 `WM_COMMAND`，由正常命令入口取得状态；没有把所有重入消息重新排队。同步控件通知仍沿用原有借用保护。

修复经过现有 7 个完整流程测试和静态核查，但这些测试不会打开原生菜单，**本次没有声称取得菜单问题的自动 RED 或桌面回归通过证据**。仍使用已批准的完整业务边界，未建立内部 GUI 测试入口。

## 仍需完成的验收

- 用户确认其他播放声音已暂停后，用不同应用播放已知语音，重新完成正式开始、停止、本地识别和结果检查，核对耳机音频来源范围。
- 在获准的桌面环境实际用 `Ctrl+Alt+R` 开始和停止；目视检查必要状态和备用文本框的完整结果，并操作复制按钮核对复制内容。
- 单独执行菜单回归：采集中打开托盘菜单，保持菜单打开，分别用 `Ctrl+Alt+R` 和正式 `--control stop` 停止；观察停止及处理中/结果状态仍被处理。另检查关闭或选择菜单后，显示、切换采集和退出命令仍仅执行一次。
- 在已验证的系统禁网隔离环境中，完成一次真实播放采集、识别及复制流程；需要保留禁网检查与完整流程的对应证据。

这些证据完成前，Issue #2 保持开放。这里没有将现有替身测试、引擎单独测量或受其他声音污染的采集记为完整桌面验收通过。

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
- 本地 whisper.cpp 与模型来源沿用 [官方资料核验](../.scratch/meeting-transcription/verification-sources.md)，完整运行参数在 `app/listen/src/whisper.rs`。
