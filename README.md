# Listen：耳机音频本地转写

Windows 后台工具：先点击目标编辑区，按 **Ctrl+Alt+R** 采集**默认多媒体播放设备**上的声音，再按一次停止；它在本机用 **SenseVoiceSmall q8（sherpa-onnx，常驻进程内）** 转写，把完整结果追加到开始时选定的目标编辑区。没有有效目标或写入无法确认时，完整结果保留在**展开结果面板**中，可直接复制。工具不会自动发送消息，也不会把转写文本作为命令执行。

v0.2 的界面是**常驻状态胶囊**：置顶、不抢焦点、不进入任务栏，并用 `WDA_EXCLUDEFROMCAPTURE` 从屏幕共享与录屏中隐藏；胶囊只用颜色、形状和动效表达状态，点击展开**结果面板**（完整结果、复制、重试、清理、精修）。原详情窗口已删除，托盘仍可打开面板。更早版本的历史与验收记录保持不变：[#2 采集到复制](docs/issue-2-verification.md)、[#3 原生目标](docs/issue-3-verification.md)、[#4 通用编辑区](docs/issue-4-verification.md)、[#6 长会话](docs/issue-6-verification.md)、[#7 失败重试](docs/issue-7-verification.md)、[#8 路由中断](docs/issue-8-verification.md)、[#9 质量基线](docs/issue-9-verification.md)、[#10 准备与延迟](docs/issue-10-verification.md)、[#11 交付包](docs/issue-11-verification.md)。v0.2 的引擎、术语表、胶囊、面板与精修验收见 [#17](docs/issue-17-verification.md)、[#19](docs/issue-19-verification.md)、[#20](docs/issue-20-verification.md)、[#21](docs/issue-21-verification.md)；本轮代码审查的修复与实测数字见 [审查修复记录](docs/issue-review-fixes.md)，用户真实语音暴露的术语表加载与匹配修复见 [术语表真实语音修复记录](docs/issue-glossary-real-speech.md)。

## v0.2 的行为

- **引擎**：默认 SenseVoiceSmall q8，通过 sherpa-onnx C API 常驻应用进程，CPU 推理，**要求 AVX2**。模型固定 SHA-256 后加载；whisper.cpp 代码路径保留，但只有显式 `--engine whisper` 或 `--engine <whisper-cli.exe>` 才使用，绝不静默回退到云端或 whisper。
- **准备期可采集**：模型在后台准备；准备期间按快捷键仍能立刻进入采集，音频先缓冲，模型就绪后继续识别并交付完整结果。准备中与就绪在状态上可区分，准备失败明确失败并保留本次音频。
- **非语音门禁**：Silero VAD 判为无语音时不调用识别器；音乐/提示音不会产生“The.”这类凭空文字。
- **术语表校正**：识别结果在交付前经过本地术语表，一个词一条、支持拼音/中文写法/近似匹配，不要求穷举错误写法；普通词不会被替换。文件在每次开始采集时重新读取。
- **状态胶囊**：置顶、不抢焦点、无任务栏/Alt+Tab 入口，共享屏幕时对他人不可见；可拖动并记住位置，支持多显示器和高 DPI。
- **结果面板**：单击胶囊展开/收起；面板显示当前阶段、实时采集进度、应用给出的完整说明和**完整当前转写文本**，并提供复制、重试本地识别、清理失败音频、精修、关闭。结果只在面板中（无目标回退）时会自动展开一次；失败不会自动抢屏，胶囊变红后由用户点开查看原因。
- **精修（可选）**：面板里手动触发，只把**文字**（不含音频）发送到 DeepSeek API，修完在目标内容未变时直接替换本次追加的文本；目标被手改或无法确认时，精修结果留在面板、不覆盖用户编辑。没有 key、断网或 API 失败时保留本地结果并明确说明。本地识别不依赖精修。
- **不自动发送**：没有任何 Enter、发送按钮或消息 API 路径；结果只追加文本或写入剪贴板。

## 本机准备与构建

共享缓存放在仓库根目录 `.tools/`：

```text
.tools/
  sherpa-onnx/sherpa-onnx-v2.0.1-win-x64-shared-MT-Release/lib/
    sherpa-onnx-c-api.dll
    onnxruntime.dll
  models/sense-voice/
    model.int8.onnx
    tokens.txt
    silero_vad.onnx
  whisper-v1.8.2-cpu/bin/Release/whisper-cli.exe   # 仅显式回退时使用
  models/ggml-small.bin
  models/ggml-silero-v5.1.2.bin
```

首次缺少模型时运行 `.\scripts\prepare-quality-model.ps1` 下载并校验固定模型；之后可离线构建和识别。构建与启动：

```powershell
.\scripts\build-listen.ps1
.\scripts\run-listen.ps1                  # 默认 SenseVoice，不传任何引擎参数
.\scripts\run-listen.ps1 -Engine whisper  # 显式使用 whisper.cpp 回退
.\scripts\run-listen.ps1 -Control show    # 打开结果面板；还有 panel/copy/start/stop/quit 等
```

`run-listen.ps1 -Engine` 接受 `sensevoice`（默认）、`whisper` 或一个 `whisper-cli.exe` 路径；只有选择 whisper 时才传 `--model`/`--vad-model`/`--backend`。脚本从 Git 共享元数据目录解析仓库根目录，工作树里复用同一份 `.tools`。构建输出为 `app\listen\target\release\listen.exe` 与相邻的 `libunwind.dll`；构建使用锁定且已缓存的依赖，可离线执行。

直接启动 `listen.exe` 也支持 `--engine <sensevoice|whisper|whisper-cli.exe>`、`--runtime <directory>`、`--glossary <file>`、`--threads <count>`、`--hidden`，以及 SenseVoice 的 `--sherpa-lib`、`--sensevoice-model`、`--sensevoice-tokens`、`--sensevoice-vad`、`--sensevoice-language`。帮助文本见 `listen.exe --help`。

## 术语表（专名校正）

默认路径是 `<runtime>\glossary.txt`；如果它不存在，再找 `listen.exe` 旁边的 `glossary.txt`（复制出去的构建因此也能用上随包术语表）。开发启动器 `run-listen.ps1` 默认传入仓库里的 `packaging\glossary.txt`，可用 `-Glossary` 换文件；`listen.exe --glossary <file>` 仍然优先。`status.txt` 的 `glossary=` 行写明本次实际使用的文件，没有文件时为 `none`。`#` 开头是注释，空行忽略；正式写法在前，可选别名（拼音、中文写法或顽固误写）放在 `|` 后，用逗号分隔：

```text
DeepSeek Harness | shen du qiu suo, 迪普西克
WASAPI | waapp
WASAPI loopback
CUDA | ku da, ku
Rust | re
```

匹配是近似的：忽略大小写、空格和连字符，多词正式写法还会用有辨识度的前导词召回（例如 `deep seek harness`、`deepsek` 都会得到完整的 `DeepSeek Harness`），所以一个词通常不需要列出所有误识别写法。`re`、`ku` 这类一到两个字母的别名只在紧邻中文时生效，避免误改英文单词。文件在**每次开始采集时重新读取**，改完保存后下一次采集即生效；文件缺失时不做校正，文件格式错误时状态会说明原因，本次文本仍会交付。

## 精修（可选，唯一出网路径）

在结果面板里点**精修**，应用才把当前完整转写文本发送到 DeepSeek API 校正；不发送音频，不发送目标输入框里的其它内容。Key 只从本机环境变量 `DEEPSEEK_API_KEY` 读取，没有 key 时按钮置灰、本地功能不受影响。回填沿用“先读回、再确认”的安全边界：只有目标内容仍等于交付后的完整内容时才替换本次追加的文本；用户改过则把精修文本留在面板。精修失败或断网时保留本地结果，不重复追加。

## Windows 交付包（现状）

当前 `scripts\package-listen.ps1` 生成的仍是一个 **whisper.cpp 版本**的目录（`dist\Listen-0.1.0\`，包含 `engine\whisper-cli.exe`、`models\ggml-*.bin`），包内 README 也仍是 v0.1 说明。把交付包换成 SenseVoice 运行库与 q8 模型、移除 whisper、加入本页署名与许可证，是 ticket **#23**，由 #22 的真实语料基准把关；本文不声称 SenseVoice 已在交付包内。开发构建与真机验证都以 SenseVoice 为默认引擎。

## 路径与本地数据

默认运行目录是共享仓库根目录下的 `.artifacts\listen`（`--runtime` 可改），已被 Git 忽略。

| 内容 | 用途 |
| --- | --- |
| `current-result.txt` | 当前完整 UTF-8 转写文本；成功后覆盖，不建立会议历史 |
| `panel.txt` | 结果面板当前全文：状态块 + 空行 + 完整结果；面板隐藏时也更新，供自动化读取 |
| `status.txt` | 当前阶段、说明、采集端点、`preparation`、`refine`、`notice`、`glossary`、`panel_visible`、`panel_error`、`capsule_capture_excluded`、各段耗时、进程 ID；不含转写正文 |
| `latency.jsonl` | 每次采集/失败的准备、采集启动、收尾、识别、保存、交付耗时；不含音频 |
| 本次会话临时目录 | 采集音频与本地诊断；成功后删除，失败时保留以便人工恢复 |

失败时保留本次已采集音频，并在状态和面板中提供“重试本地识别”和“清理失败音频”（`--control retry|cleanup` 走同一入口）。重试只重新运行本地识别并投递给采集开始时绑定的原目标，不重放、不切换云端。长会话中已有识别窗口成功消费后，不提供整段自动重试，状态会明确说明并列出剩余音频路径。成功识别后清理临时音频；清理后不会再用同一音频重试。

## 署名与许可

- **SenseVoiceSmall 模型权重**来自 FunASR / SenseVoice 项目，使用 **FunASR MODEL_LICENSE**。本机文件在 `.tools\models\sense-voice\`（`model.int8.onnx`、`tokens.txt`、`silero_vad.onnx`）。发布或交付时必须保留该署名与许可证说明（ticket #23 的交付包要求）。
- **sherpa-onnx** 与 **onnxruntime** 按其各自许可证分发；本机运行库在 `.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib\`（`sherpa-onnx-c-api.dll`、`onnxruntime.dll`）。交付包必须同时带上它们各自的许可证文本。
- whisper.cpp 及其模型仅在显式回退时使用，仍受其各自许可证约束。

## 测试与范围

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
# 真引擎测试默认忽略，显式运行时需要固定引擎与语料路径：
# LISTEN_SHERPA_LIB, LISTEN_SENSEVOICE_MODEL, LISTEN_SENSEVOICE_TOKENS,
# LISTEN_SENSEVOICE_VAD, LISTEN_QUALITY_CORPUS, LISTEN_WHISPER_CLI/MODEL/VAD
cargo test --test sense_voice_quality -- --ignored --nocapture
```

行为测试的主接缝是 `Application` 边界：用假播放设备、假识别器、假目标输入框和假精修端口驱动开始/停止/重试/清理/精修，只断言用户可见的状态、文本、交付结果和保留音频。真引擎测试默认 `#[ignore]`。桌面验收不再依赖旧窗口类名或控件 ID：`--control` + `runtime\status.txt` + `runtime\panel.txt` + `runtime\current-result.txt` 覆盖面板的可观察行为；`scripts\verify-panel.ps1` 把控制通道验收与窗口级自检分开报告，细节见 [issue #20 验证记录](docs/issue-20-verification.md)。

已建立的约束：只采集默认多媒体渲染端点，不打开麦克风；默认通信端点或单独路由到其它设备的声音不在范围内；播放设备切换/移除会中断采集并保留已收到内容；不建立会议历史；不自动发送。仍不受支持或未验证：分发安装器、多用户、自动更新、macOS/Linux、GPU/CUDA 加速、词级时间戳、会议摘要/翻译、术语表自动学习，以及任意第三方编辑器/浏览器扩展输入区的无条件可写。交付包引擎替换（#23）与基于真实腾讯会议语料的启用闸门（#22）仍在进行中。
