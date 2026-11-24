# Listen：耳机音频转写（云端默认 + 显式本地备用）

Windows 后台工具：先点击目标编辑区，按 **Ctrl+Alt+R** 采集**默认多媒体播放设备**上的声音，再按一次停止；日常默认把这段连续音频送到**已通过验收的阿里云百炼北京识别**，把完整结果追加到开始时选定的目标编辑区。本地 **SenseVoiceSmall q8（sherpa-onnx）** 识别保留为**显式备用**，只有用 `--engine` 或包内“本地备用”启动器才会运行。没有有效目标或写入无法确认时，完整结果保留在**展开结果面板**中，可直接复制。工具不会自动发送消息，也不会把转写文本作为命令执行。

v0.2 的界面是**常驻状态胶囊**：置顶、不抢焦点、不进入任务栏，并用 `WDA_EXCLUDEFROMCAPTURE` 从屏幕共享与录屏中隐藏；胶囊只用颜色、形状和动效表达状态，点击展开**结果面板**（完整结果、复制、重试、清理、精修）。原详情窗口已删除，托盘仍可打开面板。更早版本的历史与验收记录保持不变：[#2 采集到复制](docs/issue-2-verification.md)、[#3 原生目标](docs/issue-3-verification.md)、[#4 通用编辑区](docs/issue-4-verification.md)、[#6 长会话](docs/issue-6-verification.md)、[#7 失败重试](docs/issue-7-verification.md)、[#8 路由中断](docs/issue-8-verification.md)、[#9 质量基线](docs/issue-9-verification.md)、[#10 准备与延迟](docs/issue-10-verification.md)、[#11 交付包](docs/issue-11-verification.md)。v0.2 的引擎、术语表、胶囊、面板与精修验收见 [#17](docs/issue-17-verification.md)、[#19](docs/issue-19-verification.md)、[#20](docs/issue-20-verification.md)、[#21](docs/issue-21-verification.md)；本轮代码审查的修复与实测数字见 [审查修复记录](docs/issue-review-fixes.md)，用户真实语音暴露的术语表加载与匹配修复见 [术语表真实语音修复记录](docs/issue-glossary-real-speech.md)。

## v0.2 的行为

- **引擎**：日常默认是阿里云百炼 Model Studio / `qwen-audio-3.1-asr-flash-message` / `cn-beijing`（单厂商），依据 2026-11-21 的 20 段真实音频验收（20/20 交付、最慢 844 ms、准确率 95.86% 对比本地基线 75.39%，见 [验收证据](docs/issue-40-acceptance-20261121.md)）。本地 SenseVoiceSmall q8 通过 sherpa-onnx C API 常驻应用进程（CPU，**要求 AVX2**）保留为**显式备用**：只有 `--engine sensevoice`（或 `--engine whisper` / `--engine <whisper-cli.exe>`）才运行；云端失败时绝不静默改用本地，本地结果也不会冒充云端结果。
- **准备期可采集**：模型在后台准备；准备期间按快捷键仍能立刻进入采集，音频先缓冲，模型就绪后继续识别并交付完整结果。准备中与就绪在状态上可区分，准备失败明确失败并保留本次音频。
- **非语音门禁**：Silero VAD 判为无语音时不调用识别器；音乐/提示音不会产生“The.”这类凭空文字。
- **术语表校正**：识别结果在交付前经过本地术语表，一个词一条、支持拼音/中文写法/近似匹配，不要求穷举错误写法；普通词不会被替换。文件在每次开始采集时重新读取。
- **状态胶囊**：置顶、不抢焦点、无任务栏/Alt+Tab 入口，共享屏幕时对他人不可见；可拖动并记住位置，支持多显示器和高 DPI。
- **结果面板**：单击胶囊展开/收起；面板显示当前阶段、实时采集进度、应用给出的完整说明和**完整当前转写文本**，并提供复制、重试本地识别、清理失败音频、精修、关闭。结果只在面板中（无目标回退）时会自动展开一次；失败不会自动抢屏，胶囊变红后由用户点开查看原因。
- **精修（可选）**：面板里手动触发，只把**文字**（不含音频）发送到 DeepSeek API，修完在目标内容未变时直接替换本次追加的文本；目标被手改或无法确认时，精修结果留在面板、不覆盖用户编辑。没有 key、断网或 API 失败时保留本地结果并明确说明。本地识别不依赖精修。
- **不自动发送**：没有任何 Enter、发送按钮或消息 API 路径；结果只追加文本或写入剪贴板。
- **多段采集**：每次开始/停止是一个独立分段，后一段不会覆盖前一段的文字、目标或费用。旧段等待联网、仍在处理或失败保留音频都不拒绝新段；目标输入框的追加按采集顺序（先旧后新），识别/保存仍优先处理刚停止的当前段。面板按采集序号保留每个待取结果，托盘可按段清理。
- **重启恢复**：退出不等待云端确认；下次启动从运行目录的 `segments.json`、`delivery.json`、保留音频和已保存文字恢复各段，只把仍可读的文字显示为待取结果（不重复识别、不释放唯一证据），未完成录音按采集顺序一次一段自动补处理。旧的窗口/输入框句柄不能跨进程使用，所以重启后的结果一律留在面板、绝不自动回填。
- **存储边界**：待处理音频有跨段总预算和磁盘余量底线（见下方选项），超过预算或低于底线时停止开始/继续采集，但保留已经收到的声音与全部旧段；保存文字的 4 MiB 独立上限溢出时按存储故障处理，也不写目标。

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
.\scripts\run-listen.ps1                  # 日常默认：云端（需要本机凭据与免费保护）
.\scripts\run-listen.ps1 -Engine sensevoice   # 显式选择本地 SenseVoice 备用
.\scripts\run-listen.ps1 -Engine whisper      # 显式使用 whisper.cpp 本地备用
.\scripts\run-listen.ps1 -CloudTrial ali      # 显式选择云端（与 -Engine 不能组合）
.\scripts\run-listen.ps1 -Control show        # 打开结果面板；还有 panel/copy/start/stop/retry/cleanup/refine/quit 等
.\scripts\run-listen.ps1 -Control delete-segment:2  # 清理面板中采集序号为 2 的待处理音频/待取结果
```

`run-listen.ps1 -Engine` 接受 `sensevoice`、`whisper` 或一个 `whisper-cli.exe` 路径；不传
`-Engine` 时和产品默认一样走云端，且 `-CloudTrial ali` 不能与任何 `-Engine` 组合（两个方向
都拒绝，进程不会在云端与本地之间自动切换）。云端和本地开发运行目录分开
（`.artifacts\listen` / `.artifacts\listen-local`），避免把一种引擎的待处理音频交给另一种。
脚本从 Git 共享元数据目录解析仓库根目录，工作树里复用同一份 `.tools`。构建输出为
`app\listen\target\release\listen.exe` 与相邻的 `libunwind.dll`；构建使用锁定且已缓存的依赖，
可离线执行。

直接启动 `listen.exe` 也支持 `--cloud-trial ali`（显式云端，默认即云端）、`--engine <sensevoice|whisper|whisper-cli.exe>`（显式本地）、`--runtime <directory>`、`--glossary <file>`、`--threads <count>`、`--hidden`，以及 SenseVoice 的 `--sherpa-lib`、`--sensevoice-model`、`--sensevoice-tokens`、`--sensevoice-vad`、`--sensevoice-language`。帮助文本见 `listen.exe --help`。没有可用凭据/工作空间/型号/地域时，默认启动显示可操作的失败原因；开始采集被拒绝，不会悄悄改成本地识别。

存储与清理选项：

- `--pending-audio-budget-mib <MiB>`：跨全部段的待处理音频预算，默认 1024（1 GiB）。统计每个 `session-*\` 里的全部普通文件（云端连续 PCM、本地重叠识别窗口、临时上传分片、修复/重试副本）以及根目录的 `.wav`/`.pcm`；已保存的文字与索引不占这份预算。
- `--pending-audio-warn-mib <MiB>`：接近预算的警告线，默认预算的五分之四；只提示，不阻止任何操作，且不会高于预算。
- `--free-disk-floor-mib <MiB>`：运行目录所在卷必须保留的空闲空间，默认 1024（1 GiB）。
- 达到或超过待处理音频预算（`storage_level=full`）或磁盘余量到达底线（`storage_level=disk_low`）时，不会开始新的采集；正在采集的段会被停止，已经收到的声音、旧段和待取结果都保留在本机，不会被覆盖或删除。清理是显式操作：托盘「清理待处理段」或 `--control delete-segment:<n>`，按面板显示的采集序号删除单个段的待处理音频和/或待取结果。
- `--control retry|cleanup` 仍只作用于最近一个失败捕获保留的音频；`delete-segment:<n>` 作用于指定段，其他段（包括仍在处理或等待联网的段）不受影响。

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

## 精修（可选）

在结果面板里点**精修**，应用才把当前完整转写文本发送到 DeepSeek API 校正；不发送音频，不发送目标输入框里的其它内容。Key 只从本机环境变量 `DEEPSEEK_API_KEY` 读取，没有 key 时按钮置灰、本地功能不受影响。回填沿用“先读回、再确认”的安全边界：只有目标内容仍等于交付后的完整内容时才替换本次追加的文本；用户改过则把精修文本留在面板。精修失败或断网时保留本地结果，不重复追加。

## 云端识别（日常默认）

默认把本段耳机音频及开始时固定的术语发给阿里云百炼北京 Message 型号；只使用这一个厂商，不常规双发，不在失败时跨厂商切换，也不用本地结果冒充云端结果。每次开始采集前，应用会现场只读核验免费保护（当前 CLI 身份下唯一默认业务空间、精确型号、额度 > 0、`autoStop=true`、到期日）并检查共享费用账本；核验或鉴权不通过就不发送音频，已收到的音频保留在本机。缺凭据、工作空间、型号或地域时，启动状态给出可操作提示，开始采集被拒绝。

费用上，**免费额度到 2026-12-21 为止，之后按厂商实际计费转为付费**。CNY 50 是首轮共享硬上限（所有已结算估计与未决预留之和不得超过；不会自动购买套餐或提高预算）；每月 CNY 100 只是提醒线，并在本月合计达到 90 元时就提前提醒，提醒不会停止采集、取消录音或提高额度。实际现金账单仍需你在阿里云控制台核对；验收只证明免费保护使额外现金扣费路径不成立。首次配置、数据范围、失败行为与未验证项见 [交付包云端指南](packaging/CLOUD-TRIAL.md)、[本轮交付说明](docs/issue-40-delivery.md) 和 [#40 验收证据](docs/issue-40-acceptance-20261121.md)。

显式本地备用：`.\scripts\run-listen.ps1 -Engine sensevoice`（或 `-Engine whisper`）。这一选择会取消云端默认，二者不能组合；`runtime\status.txt` 的 `recognition_mode=local` 与 `backend=` 会写明本机来源。

## Windows 交付包（现状）

`scripts\package-listen.ps1` 生成的 `dist\Listen-0.1.0\` 日常默认是**已通过验收的云端路径**：`启动 Listen.cmd` 使用 `runtime-cloud\` 和固定阿里云型号；`启动 Listen 本地备用.cmd` 是明确标注的本地 **SenseVoice q8（sherpa-onnx）** 备用，包内为 `engine\sherpa\` 运行库与 `models\sense-voice\` 的 q8 模型，不再包含 whisper.cpp 的模型或运行库。包内 README/CLOUD-TRIAL 说明首次凭据配置、`bl.cmd` 免费保护、费用与数据边界；`manifest.json` 记录 `default_mode=cloud`、固定厂商/型号/地域以及 `engine_source`/`engine_files`；`THIRD-PARTY-NOTICES.md` 与 `licenses\` 带 FunASR/SenseVoice、sherpa-onnx、ONNX Runtime 的署名与许可证原文。换包验收记录见 [docs/issue-23-verification.md](docs/issue-23-verification.md)。

## 路径与本地数据

开发启动器的云端默认运行目录是共享仓库根目录下的 `.artifacts\listen`；显式本地备用默认用
`.artifacts\listen-local`（都可 `--runtime` 改），两者都被 Git 忽略。交付包用
`runtime-cloud\` / `runtime\`，理由相同：云端连续 PCM 与本地识别窗口不能混用。

| 内容 | 用途 |
| --- | --- |
| `current-result.txt` | 当前完整 UTF-8 转写文本；成功后覆盖，不建立会议历史 |
| `segments.json` | 带版本的多段索引：采集序号、状态、文字文件名、交付方式、每段费用、捕获时的术语快照和 `current_sequence`；重启恢复与“哪一段是当前结果”的依据。版本不匹配或损坏时只报告降级，不静默重写 |
| `segment-NNNNNNNN.txt` | 某一段自己的完整文字（8 位采集序号）。结果待取、失败或崩溃窗口时保留；确认追加成功后随该段索引一起删除，不是“每段永久归档” |
| `delivery.json` | 带版本的交付记录，状态取值固定为 `pending|started|appended|fallback|uncertain`：写目标前先落盘 `started`、确认读回后记 `appended`，`started`/`uncertain` 永不自动重写。历史按条数与字节有界修剪，`pending`/`started`/`uncertain` 永不删除 |
| `playback.state` | 仅云端连续 PCM 段：`committed_bytes=` 与 `clean_stop=`，是异常退出后判断录音是否完整结束的结束标记 |
| `recovered.pcm` / `recovered.wav` | 仅在需要修复时生成在该段目录内（云端截断 PCM 的可读前缀 / 本地损坏识别窗口的重拼）；计入待处理音频预算，随完整文字保存后的音频清理释放 |
| `panel.txt` | 结果面板当前全文：状态块 + 空行 + 完整结果；面板隐藏时也更新，供自动化读取 |
| `status.txt` | 当前阶段、说明、`recognition_mode=cloud-trial\|local`、实际来源 `backend`、采集端点、`preparation`、`refine`、`notice`、`glossary`、`panel_visible`、`panel_error`、`capsule_capture_excluded`、`segments.json`/`delivery.json` 降级、等待/待取段数、`storage_level`/`storage_fault`、各段耗时、进程 ID；不含转写正文 |
| `latency.jsonl` | 每次采集/失败的准备、采集启动、收尾、识别、保存、交付耗时；不含音频 |
| 本次会话临时目录 | `session-*` 下是采集音频（云端连续 PCM + `pcm-format.txt`/`playback.state`，或本地重叠识别窗口）、临时上传分片与修复副本；完整文字保存后释放，失败时保留以便人工恢复 |

失败时保留本次已采集音频，并在状态和面板中提供“重试本地识别”和“清理失败音频”（`--control retry|cleanup` 走同一入口）。重试只重新运行本地识别并投递给采集开始时绑定的原目标，不重放、不切换云端。长会话中已有识别窗口成功消费后，不提供整段自动重试，状态会明确说明并列出剩余音频路径。失败段的保留音频不会拒绝下一次采集：新段照常绑定自己的目标并采集，旧段仍可在面板中重试或按采集序号显式删除（`--control delete-segment:<n>`）。成功识别后清理临时音频；清理后不会再用同一音频重试。

## 署名与许可

- **SenseVoiceSmall 模型权重**来自 FunASR / SenseVoice 项目，使用 **FunASR MODEL_LICENSE**。本机文件在 `.tools\models\sense-voice\`（`model.int8.onnx`、`tokens.txt`、`silero_vad.onnx`）。交付包把该署名与许可证原文放进 `THIRD-PARTY-NOTICES.md` 与 `licenses\`（ticket #23）。
- **sherpa-onnx** 与 **onnxruntime** 按其各自许可证分发；本机运行库在 `.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib\`（`sherpa-onnx-c-api.dll`、`onnxruntime.dll`）。交付包同时带上它们各自的许可证文本。
- whisper.cpp 及其模型仅在显式回退时使用，仍受其各自许可证约束；交付包自 #23 起不再包含它们。

## 测试与范围

云端规格 #26 的 20 段真实语料、参考原文及本地固定基线已在 #28 完成并获用户验收；内容 CER 复跑步骤见 [#28 录制与评测说明](docs/issue-28-real-corpus-baseline.md)。后续云端比较复用这批固定语料，旧十句保存文字和合成回归语料不算本次成绩。

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
# 真引擎测试默认忽略，显式运行时需要固定引擎与语料路径：
# LISTEN_SHERPA_LIB, LISTEN_SENSEVOICE_MODEL, LISTEN_SENSEVOICE_TOKENS,
# LISTEN_SENSEVOICE_VAD, LISTEN_QUALITY_CORPUS, LISTEN_WHISPER_CLI/MODEL/VAD
cargo test --test sense_voice_quality -- --ignored --nocapture
```

行为测试的主接缝是 `Application` 边界：用假播放设备、假识别器、假目标输入框和假精修端口驱动开始/停止/重试/清理/精修，只断言用户可见的状态、文本、交付结果和保留音频。真引擎测试默认 `#[ignore]`。桌面验收不再依赖旧窗口类名或控件 ID：`--control` + `runtime\status.txt` + `runtime\panel.txt` + `runtime\current-result.txt` 覆盖面板的可观察行为；`scripts\verify-panel.ps1` 把控制通道验收与窗口级自检分开报告，细节见 [issue #20 验证记录](docs/issue-20-verification.md)。

已建立的约束：只采集默认多媒体渲染端点，不打开麦克风；默认通信端点或单独路由到其它设备的声音不在范围内；播放设备切换/移除会中断采集并保留已收到内容；不建立会议历史；不自动发送。仍不受支持或未验证：分发安装器、多用户、自动更新、macOS/Linux、GPU/CUDA 加速、词级时间戳、会议摘要/翻译、术语表自动学习，以及任意第三方编辑器/浏览器扩展输入区的无条件可写。交付包引擎替换（#23）已完成，见 [docs/issue-23-verification.md](docs/issue-23-verification.md)；基于真实腾讯会议语料的启用闸门（#22）见 [docs/issue-22-verification.md](docs/issue-22-verification.md)。
