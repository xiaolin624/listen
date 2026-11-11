# Listen：耳机音频本地转写

Windows 后台工具：按 **Ctrl+Alt+R** 开始采集当前默认播放设备的声音，再按一次停止，在本机用 whisper.cpp 转写，在工具自己的**备用文本框**中查看和复制完整结果。工具不会自动发送消息，也不会把转写文本作为命令执行。

这是 [Issue #2](https://github.com/xiaolin624/listen/issues/2) 的最小完整流程。普通采集期间只显示状态，不持续生成字幕。自动回填目标输入框、长时间分段和性能优化属于后续任务。

## 本机准备与构建

已准备的共享缓存放在仓库根目录 `.tools/`：Rust stable gnullvm 工具链、whisper.cpp v1.8.2 CPU 程序及其相邻 DLL、多语言 `ggml-base.bin` 模型。模型和运行库的来源及先前基准见 [实验依据](.scratch/meeting-transcription/verification-sources.md)。正式工具默认使用 CPU；实验的 CUDA 耗时不代表本工具的端到端性能。

```powershell
.\scripts\build-listen.ps1
.\scripts\run-listen.ps1
```

脚本从 Git 的共享元数据目录解析仓库根目录，因此在 `.worktrees/issue-2/` 中也会复用 `E:\luyin\.tools`，不会在工作树重复下载依赖。可以显式指定：

```powershell
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools
.\scripts\run-listen.ps1 -ToolsRoot E:\luyin\.tools -Runtime E:\luyin\.artifacts\listen
```

构建使用锁定且已缓存的依赖，可离线执行。输出为 `app\listen\target\release\listen.exe` 和相邻的 `libunwind.dll`，两者应放在同一目录。构建本身不下载模型或识别引擎。

## 使用

1. 在 Windows 声音设置中将实际使用的耳机设为默认多媒体播放设备。让会议或播放器通过这个设备播放声音。
2. 启动工具，确认 `Ready`。开始时会检查全局快捷键注册、本地引擎是否可启动和模型文件；失败会显示具体错误。
3. 按 **Ctrl+Alt+R**。状态变为 `Capturing`，工具使用 WASAPI render loopback 直接采集这个播放设备上的混合声音，不要求选择声音来源应用，不打开麦克风。正常开始不会切换前台焦点。
4. 再按 **Ctrl+Alt+R**，或在窗口中点“停止并转写”。`Processing` 表示正在本机处理，处理中再次操作会明确提示等待。
5. 完成后打开备用文本框，显示 `Result`。可选择文字按 Ctrl+C，或点“复制完整结果”。由你手动粘贴、检查和发送。

关闭窗口会隐藏到系统托盘；托盘左键打开窗口，右键提供显示、开始/停止和退出。托盘不可用时关闭窗口改为最小化。点窗口内“退出”可结束程序；若仍在采集或处理中，会完成本次收尾后退出。工具没有自动开机启动或安装系统服务。

只运行一个实例。快捷键被其他程序占用时会显示失败，释放 Ctrl+Alt+R 后退出并重新启动。按住快捷键不会产生键盘自动重复事件；重复停止不会重复生成当前结果。

## 路径与本地数据

默认运行目录是共享仓库根目录下的 `.artifacts\listen`，已被 Git 忽略。

| 内容 | 用途 |
| --- | --- |
| `current-result.txt` | 当前完整 UTF-8 转写文本；成功后覆盖，不建立正常会议历史 |
| `status.txt` | 当前阶段、说明、当前结果文件及失败保留音频路径；不包含转写正文 |
| 本次会话的临时目录 | 采集音频和本地后端诊断；成功后删除，失败时保留以便人工恢复 |

成功转写并保存当前结果后才清理本次临时目录。识别进程启动失败、返回失败、播放设备中断、音频无法写入或无法保存结果时，显示 `Failure`，保留已生成的音频并给出路径。不会静默改用云端服务。后端错误的 `whisper.log` 位于同一临时目录。

失败音频暂由用户人工处理；本任务没有加入重试/清理管理界面。可用已有 [本地基准脚本](scripts/benchmark-local-asr.py) 对保留的 WAV 重试。确认不再需要后，退出工具再删除状态中列出的本次临时目录。重启后旧的失败目录仍保留；当前窗口的失败路径只描述当前进程已遇到的失败。

本地模型和运行库准备好后，采集及识别流程不需要联网。应用没有云端识别、消息发送或大模型 API 调用路径。真实断网验证结果应以 [Issue #2 验证记录](docs/issue-2-verification.md) 为准。

## 显式配置与集成操作

```powershell
.\scripts\run-listen.ps1 -Engine E:\luyin\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe -Model E:\luyin\.tools\models\ggml-base.bin -Runtime E:\luyin\.artifacts\listen -Hidden
.\scripts\run-listen.ps1 -Control start
.\scripts\run-listen.ps1 -Control stop
.\scripts\run-listen.ps1 -Control show
.\scripts\run-listen.ps1 -Control quit
```

直接启动可执行文件还支持 `--engine <path> --model <path> --runtime <directory> --language <zh|auto|...> --threads <positive count> --hidden`。默认中文、6 个 CPU 线程。路径可包含空格。更换配置前先退出正在运行的实例。

`listen.exe --control start|stop|toggle|show|quit` 向正在运行的工具发送应用内控制事件。`start/stop/toggle` 与全局快捷键和窗口按钮调用同一个 `Application::dispatch` 入口，实际集成仍使用正式 WASAPI 和本地引擎；没有注入测试音频、假识别结果或定时自动停止模式。`status.txt` 可用于等待阶段，`current-result.txt` 可用于比对完整文字；文件检查不能替代全局快捷键、可见文本框与复制操作的桌面验证。

## 测试与范围

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
```

行为测试只使用已确认的“一次开始→停止→转写→备用结果/临时音频生命周期”边界。音频设备和本地模型是可控外部依赖，正式热键与测试使用同一应用控制入口。真实播放和模型的集成证据与逻辑测试分开记录，见 [验证记录](docs/issue-2-verification.md)。

没有加入固定分钟数上限。当前短音频版本逐包将音频写到磁盘，停止后交给本地 CLI 一次性识别；尚未实现长时间持续分段识别、受控识别积压和长期压力验证。普通 WAV 文件也有格式容量限制，达到存储格式容量或磁盘写入失败会明确报错并保留已写音频。这一版本不承诺长时间采集资源有界或所有耳机路由均已验证。

尚未验证或交付：实际编辑区自动追加、完整设备路由/切换策略、静音和非语音识别质量、长时间分段边界、模型选择与中文规范化、预加载/加速策略及数值性能承诺。已有 loopback 和 UIA 实验不等于这些后续任务已经完成。
