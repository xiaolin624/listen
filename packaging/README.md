# Listen 0.1.0：耳机音频本地转写

Listen 是 Windows 后台工具。先点击目标编辑区，按 **Ctrl+Alt+R** 开始采集**默认多媒体播放设备**上的声音，再按一次停止；它在本机用 whisper.cpp 转写，把完整结果追加到开始时选定的目标编辑区。无法可靠写入时，完整结果保留在 Listen 自己的**备用文本框**中。工具不会自动发送消息，也不会执行转写文字。

本目录是可直接运行的交付包，不需要先运行仓库里的开发脚本，也不需要单独启动识别命令。`启动 Listen.cmd` 只负责用包内路径启动 `listen.exe`。

## 快速启动

1. 把整个目录解压到一个**可写**位置（例如 `D:\Tools\Listen-0.1.0`）。不要直接放在 `C:\Program Files` 下，因为运行时要在本目录写 `runtime\`。
2. 如果 `models\` 为空，先按下面“模型准备”放入两个模型文件。包含模型的包可以跳过这一步。
3. 双击 **`启动 Listen.cmd`**。等待状态显示 `Ready`。
4. 在 Windows 声音设置中确认实际使用的耳机是**默认多媒体播放设备**；让会议或播放器通过这个设备播放声音。
5. 点击要用接收文字的编辑区（原生控件或网页编辑区），按 **Ctrl+Alt+R** 开始。状态变为 `Capturing`。
6. 再按 **Ctrl+Alt+R**，或在 Listen 窗口中点“停止并转写”。状态依次为 `Processing`、`Result`（或 `NoSpeech`）。
7. 检查原编辑区里的结果。由你手动发送；Listen 不发送 Enter，也不点发送按钮。

关闭窗口会隐藏到系统托盘；托盘左键打开窗口，右键提供显示、开始/停止和退出。只运行一个实例。

## 模型准备（不包含模型时）

把下面两个文件放入本目录的 `models\`，文件名必须保持一致：

| 文件 | 放置位置 | 字节数 | SHA-256 |
| --- | --- | ---: | --- |
| `ggml-small.bin` | `models\ggml-small.bin` | 487601967 | `1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b` |
| `ggml-silero-v5.1.2.bin` | `models\ggml-silero-v5.1.2.bin` | 885098 | `29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf` |

来源：

- Whisper 多语言 small（F16）：<https://huggingface.co/ggerganov/whisper.cpp/tree/5359861c739e955e79d9a303bcbc70fb988958b1>
- Silero VAD v5.1.2：<https://huggingface.co/ggml-org/whisper-vad/tree/9ffd54a1e1ee413ddf265af9913beaf518d1639b>

下载后可用 PowerShell 校验：

```powershell
Get-FileHash .\models\ggml-small.bin -Algorithm SHA256
Get-FileHash .\models\ggml-silero-v5.1.2.bin -Algorithm SHA256
```

两个模型都准备好后重新启动 Listen。模型只从本目录读取，不需要联网；识别全程在本机 CPU 上完成，没有云端识别、API 调用或自动更新。

## 缺少文件时会怎样

- **缺少 `listen.exe`、`libunwind.dll` 或启动器找不到它们**：`启动 Listen.cmd` 会显示缺失的文件名并暂停，不会静默退出。`libunwind.dll` 是 gnullvm 运行库，必须和 `listen.exe` 放在同一目录。
- **缺少 `engine\whisper-cli.exe`、`engine\vad-speech-segments.exe` 或引擎相邻 DLL**：启动器会提示引擎不完整；如果直接运行 `listen.exe`，启动状态为 `Failure`，并写明缺失的路径或“backend is unavailable; check its adjacent DLLs”。
- **缺少模型、模型为空或不是 ggml 格式**：启动状态为 `Failure`，写明 `Local model is unavailable (...)` 或 `Local model is not a whisper.cpp ggml model`。补齐模型后重新启动即可。
- **默认多媒体播放设备不可用**：开始采集时进入 `Failure`，写明无法选择默认多媒体渲染端点；不会打开麦克风，也不会自动改用其他设备。
- 包内 `engine\` 已放入引擎实际需要的 `whisper.dll`、`ggml.dll`、`ggml-cpu.dll`、`ggml-base.dll`、`vad-speech-segments.exe` 以及 Microsoft VC++ 运行库 DLL；`SDL2.dll`、`whisper-server.exe` 等无关程序没有打包。

## 采集范围与行为

- **只采集 Windows 默认多媒体渲染端点**（`eRender` + `eMultimedia`）上的混合声音，不打开麦克风。默认通信端点或单独路由到其他设备/应用的声音不在采集范围内，即使耳机也在播放。
- 开始采集时状态会写出实际端点的角色、设备 ID 和友好名称。不要求选择应用类型或声音来源应用。
- 采集期间默认多媒体端点被切换、移除或失活时，工具立即停止并进入 `Failure`，写明原端点/新端点和中断原因；已收到的音频或识别检查点保留，并明确标为不完整，不冒充完整采集。工具不会自动跟随到新设备。
- 不建立会议历史：成功转写只覆盖 `runtime\current-result.txt`，失败时保留本次临时音频并在状态中给出路径。`status.txt` 不包含转写正文。
- **不会自动发送**：没有 Enter、发送按钮、消息 API 或执行转写文本的代码路径。原生/网页写入、剪贴板降级都只追加文本。
- 写入无法确认时，完整结果进入备用文本框；你可以按 Ctrl+C 或点“复制完整结果”。工具不自动重试，也不回滚原目标。

## 平台限制（请先读）

- 自动追加要求目标通过 UI Automation 暴露**可写的 Value 模式**，且焦点在开始时能绑定到该编辑区。只读、禁用、密码框、没有 Value 模式的控件或无法确认身份的目标会进入备用文本框。
- 浏览器 `contenteditable` 的 `ValuePattern.SetValue` **不会派发 `input`/`change` 事件**，原有富文本结构会被拍平。React/Vue 等受控编辑器可能在下一次重绘时覆盖写入；Listen 只能确认写入当时可访问值已写入并读回。
- 有些受控编辑器（例如部分聊天输入框）暴露 Value 模式但会静默丢弃写入。这类编辑区只有在**停止时目标窗口仍是前台、焦点仍是原目标**时才会走粘贴降级：读出真实草稿、只追加、用全选复制校验。降级会短暂借用系统剪贴板（结束后恢复文本；原来不是文本时只能清空），只发送 `Ctrl+A/V/C` 和方向键，**从不发送 Enter**。目标不在前台或校验不通过时仍进入备用文本框。
- **Codex 桌面端等没有专门适配**：如果它不通过标准可写 Value 模式暴露输入区，Listen 不会猜测控件或注入发送，结果会进入备用文本框。没有按应用白名单/黑名单分流。
- UIA 读取、检查和写入不是跨进程原子事务；在最后检查和写入之间发生的并发编辑仍可能造成“可能已部分写入”的提示。失败后只做诊断读回，不自动重试、不回滚。
- 目标有长度上限时会截断。Listen 会检测稳定前缀，明确报告“只保存了 N/M 个字符，不是完整交付”，并在备用文本框保留完整结果；不会把截断报告成成功。
- CPU 识别；本版本没有交付 CUDA/GPU 加速。识别会保留模型输出的繁体/简体、标点、英文和数字，不做摘要、同音纠正或语义改写；中英切换、专有名词、同音字和标点仍可能有错。
- 测试中的 10 分钟/60 分钟只是验证样例，**不是产品时长上限**，也不是延迟或准确率 SLA。
- 尚未覆盖：所有第三方编辑器/浏览器扩展输入区、物理拔插耳机、蓝牙免提/立体声切换、会议软件独占模式、其他 Windows 版本、真实会议录音的分段边界质量。不要把它理解为“任意控件无条件可写”或“任意会议逐字零错误”。

## 本地数据

| 路径 | 用途 |
| --- | --- |
| `runtime\current-result.txt` | 当前完整 UTF-8 转写；成功后覆盖，不建立会议历史 |
| `runtime\status.txt` | 当前阶段、说明、采集端点、当前结果与失败保留音频路径；不含转写正文 |
| `runtime\latency.jsonl` | 每次采集的准备/收尾/识别/保存/交付耗时；不含音频 |
| `runtime\` 下本次会话临时目录 | 采集音频与本地后端日志；成功后删除，失败时保留以便人工处理 |

确认不再需要失败音频后，退出 Listen，再删除 `status.txt` 中列出的本次临时目录。重启不会自动清理旧的失败目录。

## 版本与验证

- 版本：0.1.0
- 源码：<https://github.com/xiaolin624/listen>
- 交付包由 `scripts/package-listen.ps1` 生成；`manifest.json` 记录每个文件的大小和 SHA-256、源码提交和是否包含模型。
- 端到端验证、延迟、长会话和已知限制见仓库 `docs/issue-11-verification.md` 与 `docs/issue-11-release-notes.md`。
- 本机断网封禁复验由主会话补做；包内三个二进制没有网络导入，运行期只加载本机模块。
