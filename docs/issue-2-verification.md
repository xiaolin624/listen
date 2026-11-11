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

真实播放、本地模型、可见窗口、全局快捷键、复制及断网验证由负责集成的主流程补充，当前文档仍将这些列为**待验证**。自动外部替身测试不代表这些项目已经通过；也不把文件中有结果当作已经验证桌面快捷键或复制按钮。

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
