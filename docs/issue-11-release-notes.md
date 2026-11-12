# Listen 0.1.0：Windows 可运行交付包发布说明

2026-11-12。本文对应任务 [Issue #11](https://github.com/xiaolin624/listen/issues/11)，是父规格 [Issue #1](https://github.com/xiaolin624/listen/issues/1) 的最终交付说明。完整的实测记录见 [整体验收记录](issue-11-verification.md)；各分任务细节见 `docs/issue-2` 至 `docs/issue-10` 的验证记录。

## 交付范围

Listen 是 Windows 后台工具：用户先选定目标编辑区，按 **Ctrl+Alt+R** 采集 Windows **默认多媒体播放设备**上的声音，再按一次停止；工具在本机用 whisper.cpp 转写，把完整结果追加到开始时选定的目标编辑区。无法可靠写入时，结果保留在工具自己的**备用文本框**中，由用户复制。**工具不会自动发送消息，也不会执行转写文本。**

本版本交付一个可直接运行的目录 `dist\Listen-0.1.0\`：

| 内容 | 说明 |
| --- | --- |
| `listen.exe` | 正式应用；必须通过 `启动 Listen.cmd` 或显式 `--engine/--model/--vad-model` 参数运行（直接双击 `listen.exe` 不会自动找到包内引擎/模型） |
| `libunwind.dll` | gnullvm 运行库，必须与 `listen.exe` 同目录 |
| `engine\` | whisper.cpp v1.8.2 CPU 引擎：`whisper-cli.exe`、`whisper.dll`、`ggml.dll`、`ggml-cpu.dll`、`ggml-base.dll`、`vad-speech-segments.exe`，以及实际需要的 VC++ 运行库 DLL |
| `models\` | 可选：`ggml-small.bin` 与 `ggml-silero-v5.1.2.bin`。带 `-IncludeModels` 构建时一并打包；否则按 `README.md` 放入 |
| `启动 Listen.cmd` | 双击启动，使用包内引擎、模型和本地 `runtime\`；缺少运行库/引擎/模型时给出明确提示 |
| `README.md` | 启动、模型准备、缺失反馈、平台限制和使用范围 |
| `manifest.json`、`VERSION.txt` | 每个文件的大小与 SHA-256、源码提交、是否包含模型 |

`listen.exe` 版本 0.1.0，源提交 `32cf8d12aad45f20b538636740050babc1e1617d`（打包时工作树干净）。包含模型时目录约 470 MiB；不包含模型时约 4.6 MiB，另需约 466 MiB 的两个模型文件。

## 本版本验证过的能力

以下能力在**同一个包内 `listen.exe`** 上、用正式入口（真实全局快捷键、`--control start/stop/retry/cleanup`、包内 `启动 Listen.cmd`）完成集成验收，而不是分别停留在实验程序或独立推理命令。**性能/长会话数字来自另一条测量入口**：600 秒与资源受压由同一源码的 `examples\long_session.exe` harness（替换播放设备、不开音频设备）+ 包内 engine/model 测得，不是包内 `listen.exe` 的界面流程，详见下文。

- 真实 WASAPI 默认多媒体 render 端点采集；真实耳机端点 `耳机 (WH-1000XM4)`；不打开麦克风；不要求选择应用或声音来源。
- 真实本地 whisper.cpp small + Silero，CPU 6 线程；`Preparing → Ready → Capturing → Processing → Result` 状态序列。
- 真实焦点选定的原生 Windows 多行 EDIT：原文和采集中新增编辑保留，末尾完整追加一次；重复停止不再追加；邻居和另一窗口逐字不变。
- Chrome `<textarea>` 与 `contenteditable` 网页编辑区：真实焦点、真实音频、完整追加；Enter/Submit 计数为 0；普通 textarea 的 `input` 事件正常，`contenteditable` 的 `input` 计数为 0（平台限制）。
- 目标失效、只读、容量截断和写入未确认：不向替代/非目标控件写入，完整结果进入备用文本框；状态明确写出原因，不自动重试、不回滚、不发送。
- 识别失败：保留本次 WAV，显示可理解的 `Failure` 和“重试本地识别”/“清理失败音频”；修复本地引擎后用同一份音频重试成功，不需要重放；成功后临时音频清理；清理后无效重试不再调用识别。**重试范围**：只有“尚未成功消费任何识别窗口”的采集可以整体重试；长会话中已有窗口成功识别后失败时，只保留检查点文本和剩余窗口音频，不提供整段自动重试，状态会明确说明。
- 设备中断：采集中真实切换默认多媒体端点到扬声器，约 250 ms 轮询内中断，状态写明原/新端点和“incomplete”，保留识别检查点，不覆盖上一次完整结果，不跟随新设备；验收后已还原并读回原端点。
- 无自动历史：成功采集只保留一份 `runtime\current-result.txt`、`status.txt` 和 `latency.jsonl`；没有日期归档、会议历史或网络副本。
- 离线能力：包内三个二进制没有 `ws2_32`/`winhttp`/`wininet`/`urlmon`/`dnsapi`/`mswsock` 导入；真实采集期间 `listen.exe` 加载的模块中没有网络模块。**2026-11-12 用提权防火墙把这三个二进制临时出站封禁后，从交付包本身跑通了完整流程**（`Ready` 1.347 s、停止→结果 7.138 s、目标追加正确），规则随即删除（残留 0 条）；详见 [整体验收记录](issue-11-verification.md)。

## 性能与长会话实测摘要

以下数字来自本版本实际运行；完整表格、测试条件和原始证据见 [整体验收记录](issue-11-verification.md)。

- 首次启动到 `Ready`：约 **0.62 秒**模型准备（进程启动/轮询后约 1.1 秒），不计入停止→结果。**最终审查修复后**，就绪检查增加一次真实 CLI 模型加载验证，默认 host 路径实测准备 **1453 ms**（外部观察 1711 ms）；上表其余延迟数字来自修复前构建，识别路径未变，但引用当前构建的完整延迟时应重新测量。
- 同包真实采集首次/后续停止→结果：**6674 ms / 6652 ms**（外部 stopwatch 6.80 / 6.76 秒），8.3 秒固定语音，原生 EDIT 交付。
- 备选与失败路径停止→结果：只读 6681 ms、容量截断 6727 ms、写入未确认回退 8676 ms（含 2 秒 UIA 确认窗口）；这些是观测值，不是 SLA。
- 600 秒长会话：由同源码 harness `examples\long_session.exe`（替换播放设备，不开音频设备、不操作桌面）+ **包内** engine/model 测得，不是包内 `listen.exe`；`Result`，识别 22 个窗口；停止→结果 **6921 ms**；收尾 `finalize` **5 ms**；峰值积压 **1**；结束 spool **0 字节**；无失败/保留音频。
- 资源（600 秒，3 秒采样 186 个样本）：进程组合工作集峰值 **795.1 MB**、平均 **547.5 MB**；私有字节峰值 **1276.2 MB**、平均 **666.6 MB**；GPU 未被 whisper 使用。
- 资源受压场景：使用现有测量脚本主动限制 spool 预算并延迟识别，观察到 `Failure`，保留音频/检查点，停止→结果 16.1 秒；详细数据见验收记录。

600 秒/10 分钟只是验证样例；**不是产品允许的最长采集时间**，也不是延迟或准确率上限承诺。

## 真实识别质量摘要

固定语料在本包上重跑（真实 WASAPI、small + Silero、CPU 6 线程）：

| 样本 | 结果 | 主要观察 |
| --- | --- | --- |
| mandarin | `Result`，41 字 | 内容保留；繁体、标点变化 |
| mixed-technical | `Result`，72 字 | Rust/CPU/CUDA 保留；`WASAPI loopback` → `Waze up high loop back`，`本机` → `本纪` |
| english-numbers | `Result`，109 字 | 数字和术语保留；句间空格/标点被合并 |
| homophones | `Result`，31 字 | “本季”仍被写成“本机”，同音问题未解决 |
| multiline | `Result`，56 字 | 英文句被译写成“第二段,覆蓋 API 回應”；`重试` → `重視` |
| silence / instrumental / notification | `NoSpeech` | 不追加、不覆盖旧结果、临时音频清理 |

这些结果与 [Issue #9 质量基线](issue-9-verification.md) 一致；本版本没有用提示词、词表或润色掩盖错误。**不宣称任意会议逐字零错误。**

## 尚存平台限制（必须阅读）

- 自动追加要求目标通过 UI Automation 暴露**可写的 Value 模式**；只读、禁用、密码框、无 Value 模式或身份无法确认时进入备用文本框。不能写“任意控件无条件可写”。
- 浏览器 `contenteditable` 的 `ValuePattern.SetValue` 不派发 `input`/`change`，原有富文本结构会被拍平；React/Vue 等受控编辑器可能在重绘时覆盖。实测 `input` 事件计数为 0。
- 受控编辑器（如 DeepSeek Harness 输入框）的粘贴降级只在**停止时目标窗口仍是前台、焦点仍是原目标**时生效，并且每个按键前都会重新校验；它短暂借用系统剪贴板，结束后恢复原文本。**如果剪贴板里是文件/图片等非文本内容或无法安全读取，降级不会运行、也不会清空剪贴板**，完整结果直接进入备用文本框并说明原因。降级只发送 `Ctrl+A/V/C` 和方向键，**从不发送 Enter**。这一正向路径沿用 [Issue #4](issue-4-verification.md) 与 [Issue #5](issue-5-verification.md) 的真实 Harness 验收；本任务包内验证了“目标不在前台时不发按键、完整进入备用框”的负向门禁。最终审查后另行重跑了 Harness 正向路径，见验收记录。
- **Codex 桌面端等没有专门适配**：若它不以标准可写 Value 模式暴露输入区，结果进入备用文本框；没有按应用白名单/黑名单分流。
- UIA 读取、检查与写入不是跨进程原子事务；最后检查与写入之间的并发编辑仍可能产生“可能已部分写入”的提示，极端情况下可能覆盖最后几毫秒的并发编辑。失败后不自动重试、不回滚。
- 目标容量截断会明确报告“只保存了 N/M 字符，不是完整交付”，完整结果保留在备用框。
- CPU 识别；本版本没有交付 CUDA/GPU 加速。繁体/简体、标点、同音字、中英切换、专有名词和数字仍需人工检查。
- 只采集默认多媒体 render 端点；默认通信端点、其他设备或 Windows“每应用声音输出”单独路由的声音不在范围内。默认端点切换/移除时中断而不跟随。
- 尚未覆盖：所有第三方编辑器/浏览器扩展、物理拔插耳机、蓝牙免提/立体声切换、会议软件独占模式、其他 Windows 版本、真实会议录音的分段边界质量、真实会议软件同时运行时的流畅度。
- 600 秒/60 分钟是验证样例，不是产品时长上限；没有延迟或准确率 SLA。

## 离线与网络

模型和引擎准备好后，采集和识别不需要联网。正式代码没有云端识别、消息发送、大模型 API 或自动更新路径。发布包不在运行时下载模型。

- `listen.exe`、`whisper-cli.exe`、`vad-speech-segments.exe` 的 PE 导入表没有网络 DLL/API（记录在 `.artifacts\issue-11\offline\pe-imports.txt`）。
- 真实采集期间 `listen.exe` 的已加载模块中没有 `ws2_32`、`winhttp`、`wininet`、`urlmon`、`dnsapi`、`mswsock`（记录在 `.artifacts\issue-11\offline\module-evidence.json`）。
- **包级强制禁网复验已完成**（2026-11-12 11:02）：三条 `Outbound`/`Block`/`Program` 规则生效期间，从 `dist\Listen-0.1.0` 启动并完成真实采集→识别→追加交付；随后删除规则并读回 `cleanup_complete: true`、残留 0 条。证据见 [整体验收记录](issue-11-verification.md) 的“包内二进制的系统级强制禁网复验”。

## 复现

```powershell
cd E:\luyin\.worktrees\issue-11
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat
.\scripts\package-listen.ps1 -ToolsRoot E:\luyin\.tools -IncludeModels
# 产物：dist\Listen-0.1.0\启动 Listen.cmd
```

包内验收驱动和原始证据在 `.artifacts\issue-11\`（Git 忽略）：`accept-core`、`accept-fallbacks`、`accept-retry`、`accept-web`、`accept-device`、`accept-quality`、`accept-offline`、`long\600`、`pressure` 及对应 `*-evidence.json`。这些脚本只操作本任务启动的 Listen、本任务创建的测试窗口/Chrome 配置和已知端点；退出时关闭并还原。

## 不宣称的内容

- 不宣称任意 Windows 控件、浏览器扩展或受控富文本编辑器都能无条件写入。
- 不宣称任意会议、口音、噪声、多人重叠或专有名词逐字零错误。
- 不宣称 GPU 加速、真实会议软件并发流畅度、真实耳机 60 分钟连续采集已验收。
- 不宣称包级强制禁网复验等同于物理断网、长时间浸泡或覆盖所有系统配置；本轮是短样本单场景实测。

## 最终整体审查后的修复（2026-11-12）

本版本在最终整体审查后修复了以下交付风险；详细 RED/GREEN 和真实证据见 [整体验收记录](issue-11-verification.md) 的“最终整体审查后的修复与回归”。

- **粘贴降级（SP-01/SP-02）**：先归一化原草稿行尾再计算新增文本；粘贴前重新读取真实草稿，若已被延迟写入/规范化成完整目标则按已确认处理，绝不重复粘贴整段原草稿或转写。真实 Harness 回归使用已知多行草稿，最终输入框中草稿出现 1 次、转写出现 1 次。
- **按键焦点（SP-06）**：每个按键前重新校验前台窗口和焦点运行时 ID，中途离开立即中止。真实受控 EDIT 在 `Ctrl+A/C` 后失去焦点时，工具没有发送 `Ctrl+V`，完整结果进入备用文本框。
- **剪贴板（SP-07）**：非文本剪贴板或读取失败时不再清空；直接回退备用文本框并说明。文本剪贴板在降级后逐字恢复。
- **流式尾部（SP-03）**：有语音→完全无声→再有语音时，上一窗口的尾部文字会先提交，不再被后一窗口无条件清掉；新增确定性回归。
- **重试范围（SP-04）**：明确收窄并写入运行时状态、README 和验证记录：只有“尚未成功消费任何识别窗口”的采集可整段自动重试；长会话中已有窗口成功后的失败只保留检查点文本和剩余音频路径供人工恢复。
- **就绪（SP-05/S-01）**：准备阶段先由子进程真实加载模型；模型损坏时进入 `Failure`，不显示虚假 `Ready`。常驻 host 在调用任何 `whisper.dll` 导出前按固定 SHA-256 校验构建，并用按引用接口校验 40 余个默认字段。
- **控制/打包（SP-12/S-07）**：`--control` 失败只写 stderr/退出码，不弹阻塞对话框；打包脚本只删除带 Listen manifest/标记的目录。
- **文档（SP-09/SP-10/SP-14）**：README 已移除“没有重试/清理界面”“设备路由/预加载未交付”等过时说法；600 秒/资源数字明确标注为 `examples\long_session.exe` harness + 包内引擎/模型的测量，不是包内 `listen.exe` 的界面流程；发布说明的包级强制禁网状态已按 2026-11-12 的实测更新。

仍未完成：真实会议软件并发流畅度（SP-11/T9 AC5）、用包内 `listen.exe` 的 600 秒应用入口测量（SP-09/T10 AC7）。包级强制禁网复验（SP-08/T10 AC2）已于 2026-11-12 补做通过。
