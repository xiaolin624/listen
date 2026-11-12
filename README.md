# Listen：耳机音频本地转写

Windows 后台工具：先点击目标编辑区，按 **Ctrl+Alt+R** 开始采集默认多媒体播放设备的声音，再按一次停止，在本机用 whisper.cpp 转写，将完整结果追加到原生或网页目标编辑区的最新草稿末尾。无法可靠写入时，在工具自己的**备用文本框**中查看和复制完整结果。工具不会自动发送消息，也不会把转写文本作为命令执行。

已完成 [Issue #2](https://github.com/xiaolin624/listen/issues/2) 的采集到复制流程，并实现 [Issue #3](https://github.com/xiaolin624/listen/issues/3) 的原生目标绑定和 [Issue #4](https://github.com/xiaolin624/listen/issues/4) 的通用桌面/网页编辑区追加；真实桌面与浏览器验收分别见 [T2 验证记录](docs/issue-3-verification.md) 和 [T3 验证记录](docs/issue-4-verification.md)。[Issue #9](https://github.com/xiaolin624/listen/issues/9) 的本地模型与语音检测方案见 [质量基线](docs/issue-9-verification.md)，[Issue #6](https://github.com/xiaolin624/listen/issues/6) 的长时间分段采集与收尾见 [长会话验证记录](docs/issue-6-verification.md)，[Issue #7](https://github.com/xiaolin624/listen/issues/7) 的失败重试见 [重试验证记录](docs/issue-7-verification.md)，[Issue #8](https://github.com/xiaolin624/listen/issues/8) 的播放路由变化与中断反馈见 [路由验证记录](docs/issue-8-verification.md)，[Issue #10](https://github.com/xiaolin624/listen/issues/10) 的进程内模型复用、`Preparing` 状态与延迟拆分见 [准备与延迟验证记录](docs/issue-10-verification.md)。普通采集期间只显示状态，不持续生成字幕。

## 本机准备与构建

已准备的共享缓存放在仓库根目录 `.tools/`：Rust stable gnullvm 工具链、whisper.cpp v1.8.2 CPU 程序及其相邻 DLL。默认识别使用多语言 `ggml-small.bin` 和 `ggml-silero-v5.1.2.bin` 语音检测模型；引擎同目录须包含 `vad-speech-segments.exe`。首次准备缺少的模型时运行下面的显式下载/哈希校验脚本，之后可以离线构建和识别。模型选择、固定语料、残留错字与实际验证范围见 [本地质量基线](docs/issue-9-verification.md)。正式工具默认使用 CPU；实验的 CUDA 耗时不代表本工具的端到端性能。

```powershell
.\scripts\prepare-quality-model.ps1
.\scripts\build-listen.ps1
.\scripts\run-listen.ps1
```

脚本从 Git 的共享元数据目录解析仓库根目录，因此在 `.worktrees/issue-3/` 中也会复用 `E:\luyin\.tools`，不会在工作树重复下载依赖。可以显式指定：

```powershell
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools
.\scripts\run-listen.ps1 -ToolsRoot E:\luyin\.tools -Runtime E:\luyin\.artifacts\listen
```

构建使用锁定且已缓存的依赖，可离线执行。输出为 `app\listen\target\release\listen.exe` 和相邻的 `libunwind.dll`，两者应放在同一目录。构建本身不下载模型或识别引擎。

## Windows 交付包

最终用户不需要开发脚本。用下面的命令生成可直接运行的目录：

```powershell
.\scripts\package-listen.ps1 -ToolsRoot E:\luyin\.tools -IncludeModels
```

产物是 `dist\Listen-0.1.0\`（版本取自 `app\listen\Cargo.toml`），包含 `listen.exe`、`libunwind.dll`、`engine\`（引擎及实际相邻 DLL）、可选的 `models\`、双击启动器 `启动 Listen.cmd`、`README.md`、`VERSION.txt` 和记录每个文件 SHA-256 的 `manifest.json`。不带 `-IncludeModels` 时只生成约 4.6 MiB 的包，模型准备说明在包内 README 中；两种形式都会先清空目标目录再生成，并调用 `scripts\verify-listen-package.ps1` 校验文件、哈希和启动状态。`dist\` 已被 Git 忽略，交付包本身不入库。

用户解压后双击 `启动 Listen.cmd` 即可；它用包内路径启动 `listen.exe`，不需要单独启动引擎或推理命令。包内验收和发布说明见 [整体验收记录](docs/issue-11-verification.md) 与 [发布说明](docs/issue-11-release-notes.md)。


## 使用

1. 在 Windows 声音设置中将实际使用的耳机设为默认多媒体播放设备。让会议或播放器通过这个设备播放声音。
2. 启动工具，确认 `Ready`。开始时会检查全局快捷键注册、本地引擎是否可启动和模型文件；失败会显示具体错误。
3. 点击希望接收文字的编辑区（原生控件或网页编辑区），再按 **Ctrl+Alt+R**。工具在显示反馈前通过 UI Automation 绑定实际焦点元素，状态变为 `Capturing`。工具使用 WASAPI render loopback 直接采集**默认多媒体渲染端点**上的混合声音，并在状态中写明该端点的角色、设备 ID 和名称；不要求选择声音来源应用，不打开麦克风。正常开始不会切换前台焦点；采集中切换窗口或输入框不改变本次目标。音响/耳机以外的设备或应用单独路由到非默认端点的声音**不在本次采集范围内**；默认通信端点上的声音也不在范围内，即使它与多媒体端点不同。
4. 再按 **Ctrl+Alt+R**，或在窗口中点“停止并转写”。`Processing` 表示正在本机处理，处理中再次操作会明确提示等待。
5. 采集中默认多媒体端点被切换、当前端点被移除或失活时，工具停止采集并立即进入 `Failure`，状态写明原端点/新端点和中断原因；已收到的音频或识别检查点保留，当前文本明确标注本次采集中断、不完整，不会冒充完整采集。工具不自动跟随到另一个设备，因为无法确认新设备是否就是用户正在听的耳机路径。
6. `Result` 后，工具读取原目标的最新草稿，在末尾追加一次完整转写；非空草稿和转写之间加一个换行。采集中新增的草稿仍保留，成功追加不会主动切换前台窗口或抢走当前焦点。由你检查并手动发送。
7. 无有效目标、目标失效或无法确认写入时，工具打开备用文本框，保留完整结果。可选择文字按 Ctrl+C，或点“复制完整结果”。若提示写入未确认，先检查原目标是否已有全部或部分内容，再决定复制粘贴；工具不自动重试。成功追加后也可从托盘打开 Listen 查看或复制本次完整转写。
8. `NoSpeech` 表示没有检测到有效语音：本次不追加任何文字或空行，保留上一次完整结果，清理本次临时音频。状态和托盘会明确提示，可以直接开始下一次采集。

当前自动追加支持 UIA `ControlType.Edit` 或 `Document` 且具备可写 Value 模式的编辑区，包括原生多行 Edit 控件、浏览器 `<textarea>` 和 `contenteditable` 富文本区；只读、禁用、密码框、权限不足、无 Value 模式或无法确认身份时进入备用文本框。`<input>` 与多行控件走同一 UIA 路径，但没有单独的真实桌面验收记录。浏览器 `contenteditable` 只能确认可访问值已写入并读回，不会派发 `input`/`change` 事件，原有富文本结构会被拍平，受控 React/Vue 编辑器可能在重绘时覆盖。

有些受控富文本编辑器（例如 DeepSeek Harness 的输入框）虽然暴露 Value 模式，但会静默丢弃写入。这类编辑区在 **目标窗口仍是前台、焦点仍是原目标** 时会改用降级写入：读出编辑区真实草稿后**追加**文本（不覆盖已有草稿），再用全选复制逐字校验。每个按键（`Ctrl+A/V/C` 与方向键）之前都会重新校验前台窗口和焦点元素；任一不符立即中止，不继续发送后续按键。写入后若目标已经被延迟写入或规范化成完整结果，会按已确认处理而不再粘贴；目标含 CRLF 草稿时也只追加一次转写，不会重复整段原草稿。降级写入需要短暂借用系统剪贴板（结束后恢复原文本）；如果剪贴板里是文件、图片等非文本内容或无法读取，**不会清空或覆盖它**，而是直接使用备用文本框并说明原因。目标不在前台、焦点变化或校验不通过时仍进入备用文本框，不重试、不盲目投递。工具自动判断能力，不要求选择应用类型。窗口按钮和托盘的开始/停止操作仍可用；在 Listen 自身窗口开始时使用备用文本框。

关闭窗口会隐藏到系统托盘；托盘左键打开窗口，右键提供显示、开始/停止和退出。托盘不可用时关闭窗口改为最小化。点窗口内“退出”可结束程序；若仍在采集或处理中，会完成本次收尾后退出。工具没有自动开机启动或安装系统服务。

只运行一个实例。快捷键被其他程序占用时会显示失败，释放 Ctrl+Alt+R 后退出并重新启动。按住快捷键不会产生键盘自动重复事件；重复停止不会重复生成当前结果。

## 路径与本地数据

默认运行目录是共享仓库根目录下的 `.artifacts\listen`，已被 Git 忽略。

| 内容 | 用途 |
| --- | --- |
| `current-result.txt` | 当前完整 UTF-8 转写文本；成功后覆盖，不建立正常会议历史 |
| `status.txt` | 当前阶段、说明、采集端点（角色/设备 ID/名称）、当前结果文件及失败保留音频路径；不包含转写正文 |
| 本次会话的临时目录 | 采集音频和本地后端诊断；成功后删除，失败时保留以便人工恢复 |

成功转写并保存当前结果后才清理本次临时目录；成功判定无语音也会清理本次目录，保留旧结果。语音检测或识别进程启动失败、返回失败、采集端点被切换/移除、音频无法写入或无法保存结果时，显示 `Failure`，保留已生成的音频或识别检查点并给出路径，同时把可恢复文本标为不完整。不会静默改用云端服务。`vad.log`、`vad.txt` 和 `whisper.log` 位于同一临时目录。

识别失败时，工具保留本次已采集音频，在窗口和托盘显示可理解的 `Failure`，并提供“重试本地识别”和“清理失败音频”按钮；`--control retry|cleanup` 走同一入口。重试只重新运行本地识别并投递给采集开始时绑定的原目标，不重放、不切换云端。**重试范围**：单文件采集、或流式采集中尚未成功消费任何识别窗口时，可以重试整段保留音频；如果长会话中已有窗口成功识别，这些窗口的音频在识别后按有界设计删除，后续失败只保留识别检查点和剩余窗口音频，不提供整段自动重试（状态会明确说明这一点，剩余音频路径仍列出供人工恢复）。成功后清理临时音频；清理后不会再用同一音频重试。失败音频目录在 `status.txt` 中列出，确认不再需要后退出工具再删除；重启后旧的失败目录仍保留，当前窗口的失败路径只描述当前进程已遇到的失败。[本地基准脚本](scripts/benchmark-local-asr.py) 只用于离线对照，不是重试入口。

本地模型和运行库准备好后，采集及识别流程不需要联网。应用没有云端识别、消息发送或大模型 API 调用路径。#9 在 T8 固定语料上做过系统防火墙强制禁网的真实采集/识别验收；打包产物的静态导入表（无网络 DLL/API）和运行期已加载模块（无网络模块）证据见 [整体验收记录](docs/issue-11-verification.md)。**包级系统防火墙复验已于 2026-11-12 完成**：三条临时出站封禁规则生效期间，从交付包本身跑通真实采集→识别→追加交付；规则已删除、残留 0 条。

## 显式配置与集成操作

```powershell
.\scripts\run-listen.ps1 -Engine E:\luyin\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe -Model E:\luyin\.tools\models\ggml-small.bin -VadModel E:\luyin\.tools\models\ggml-silero-v5.1.2.bin -Runtime E:\luyin\.artifacts\listen -Hidden
.\scripts\run-listen.ps1 -Control start
.\scripts\run-listen.ps1 -Control stop
.\scripts\run-listen.ps1 -Control show
.\scripts\run-listen.ps1 -Control quit
```

直接启动可执行文件还支持 `--engine <path> --model <path> --vad-model <path> --runtime <directory> --language <zh|auto|...> --threads <positive count> --hidden`。默认中文、6 个 CPU 线程，固定 5 beams / best of 5，温度 0 起、增量 0.2。语音检测只判定有无语音，识别使用原始完整音频。保留模型的繁简体和换行，不做摘要、同音纠正或语义改写。中英切换、专有名词、数字仍须检查。路径可包含空格。更换配置前先退出正在运行的实例。

`listen.exe --control start|stop|toggle|retry|cleanup|show|quit` 向正在运行的工具发送应用内控制事件。控制命令行失败时不会弹出阻塞式对话框，而是把原因写到 stderr 并以非零码退出，适合脚本使用。`start/stop/toggle` 与全局快捷键和窗口按钮调用同一个 `Application::dispatch` 入口，`retry/cleanup` 与失败状态按钮共用同一入口；实际集成仍使用正式 WASAPI 和本地引擎；没有注入测试音频、假识别结果或定时自动停止模式。`status.txt` 可用于等待阶段，`current-result.txt` 可用于比对完整文字；文件检查不能替代全局快捷键、可见文本框与复制操作的桌面验证。

## 测试与范围

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
# 已准备模型后，可生成并验证固定本地语料；不会播放声音
.\scripts\make-quality-corpus.ps1
.\scripts\verify-local-quality.ps1
```

行为测试只使用已确认的“一次开始→停止→转写→目标追加或备用结果/临时音频生命周期”边界。音频设备、本地模型和目标编辑区是可控外部依赖，正式热键与测试使用同一应用控制入口。确定的多行中英、数字和 emoji 结果用于检查原目标绑定、切窗、最新草稿、重复控制及错误回退。真实播放、模型和实际编辑区的集成证据与逻辑测试分开记录，见 [T1 验证记录](docs/issue-2-verification.md)、[T2 验证记录](docs/issue-3-verification.md) 和 [T3 验证记录](docs/issue-4-verification.md)。

没有加入固定分钟数上限，也没有按分钟数自动停止。采集按 30 秒窗口、2 秒重叠持续写临时 WAV，由一个本地识别线程在采集期间逐窗转写；每个窗口的文本先写入原子可恢复检查点，确认后才释放该窗口音频。等待识别的窗口队列有界（8 个待处理窗口，加正在运行的窗口和压力保留窗口），临时音频累计预算约 9.6 MB；超出预算会明确中断采集、保留已接收音频和检查点，不静默丢数据。停止时补识别最后一个不足整窗的尾段再合并为一份完整结果。重叠区按词的时间戳中点归属，只保留一次；不含语音的窗口不会生成文字，也不会抹掉相邻窗口已识别的尾部文字；有语音→完全无声→再有语音时，上一窗口保留的尾部会先提交而不是被后一窗口丢弃。普通 WAV 格式容量限制和磁盘写入失败仍会明确报错并保留已写音频。长时间真实采集的资源、收尾时间与边界证据见 [长会话验证记录](docs/issue-6-verification.md)；这不构成任何延迟 SLA 或时长上限承诺。

已建立普通话、中英技术词、数字、本机/本季、静音、无声乐旋律和提示音的固定本地质量基线；交付包内已用真实 WASAPI 重跑该语料，识别质量与基线一致。#9 在相同采集与识别源码上做过系统防火墙禁网的完整流程；包内 `listen.exe`/`whisper-cli.exe`/`vad-speech-segments.exe` 的 PE 导入表和运行期模块列表也没有网络 DLL。#11 的整体验收记录已更新：**打包产物本身已在系统级强制禁网下跑通完整流程**，临时规则随后删除。small 改善了部分遗漏，但同音、WASAPI 拼写及中英切换仍有明确错误，且比 base 消耗更多 CPU 时间。没有准确率或延迟数值承诺，详见 [T8 验证记录及重跑命令](docs/issue-9-verification.md) 与 [整体验收记录](docs/issue-11-verification.md)。

已交付并验证：原生目标与网页编辑区追加、失败重试/清理、默认多媒体端点身份与切换中断（#8）、进程内模型复用和 `Preparing` 状态（#10）、`--control` 与 Windows 交付包（#11）。尚未验证或交付：受控 React/Vue 富文本编辑区在长期使用中的稳定性（本机已用 DeepSeek Harness 输入框实测通过一次完整流程，见 T3 验证记录）、浏览器插件/扩展自己的编辑区、物理拔插/禁用耳机等设备变化、真实会议软件并发时的流畅度、真实会议录音下的分段边界准确率、GPU/CUDA 加速。原生自动追加的桌面验收与边界另见 T2 验证记录；浏览器普通编辑区与 `contenteditable` 的实际验收及平台限制见 T3 验证记录；长会话资源、积压与收尾见 #6 长会话验证记录；准备状态和延迟拆分见 #10 记录；包级强制禁网复验见 #11 记录。已有 loopback 和 UIA 实验不等于这些后续任务已经完成。
