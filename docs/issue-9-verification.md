# Issue #9：本地识别质量基线与非语音处理

2026-11-11。默认方案改为 **whisper.cpp v1.8.2 CPU、多语言 small F16、中文解码、Silero v5.1.2 语音存在检测**。已用固定 WAV 对照 base/small、解码参数，并验证应用的无语音及英文数字交付路径。普通话样本保留了内容，small 改善了英文数字和部分术语遗漏；同音错字、语言切换错误仍存在，不能将本记录理解为真实会议准确率保证。

本轮没有播放音频、操作桌面或改变设备。完整的真实 WASAPI → 目标/备用文本框质量验收、实际耳机路径及强制禁网复验仍待串行桌面验收；下文提供可直接运行的驱动。原先 #2/#3 的桌面结果不替代本轮模型变更后的验收。

## 固定配置与来源

环境沿用本机 Windows 11、i5-11400H、16 GiB；本轮使用 CPU，6 线程。模型由显式准备脚本获取并核对官方 LFS SHA-256；正式程序及构建不联网下载，不存在云端识别回退。

| 文件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `ggml-base.bin`，对照 | 147951465 | `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe` |
| `ggml-small.bin`，默认 | 487601967 | `1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b` |
| `ggml-silero-v5.1.2.bin`，默认 VAD | 885098 | `29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf` |

small 来自 [Whisper 模型仓库固定修订](https://huggingface.co/ggerganov/whisper.cpp/tree/5359861c739e955e79d9a303bcbc70fb988958b1)，VAD 来自 [ggml-org 固定修订](https://huggingface.co/ggml-org/whisper-vad/tree/9ffd54a1e1ee413ddf265af9913beaf518d1639b)。与 v1.8.2 的 [ASR 下载脚本](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/models/download-ggml-model.sh)、[VAD 下载脚本](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/models/download-vad-model.sh) 指向同一模型来源。缓存中的 base 和引擎没有重复下载。

CPU `whisper-cli.exe` SHA-256：`dc1648751878e91acb5cfaa69bd85cff64ede44497a108844ad9d4f106cb14e8`；同目录 `vad-speech-segments.exe` SHA-256：`92ddb93ad372210519ec02184037f4e4e986c6a88a1a0d0f78524becf58c56cf`。程序与相邻 DLL 必须成套保留。

实际流程先运行同包 `vad-speech-segments.exe`，阈值 0.5，默认最短语音 250 ms、最短静音 100 ms、padding 30 ms、overlap 0.1 秒。它只判断当前音频窗口是否有语音，不删除或拼接样本。存在语音时，把**原始完整 WAV**交给 Whisper：

```text
-l zh -t 6 -ng -bs 5 -bo 5 -tp 0 -tpi 0.2 -nth 0.6 -lpt -1 -et 2.4 -nt -otxt
```

无初始提示词、翻译开关、摘要、同音替换或语义润色。默认保留模型给出的繁简体、标点、英文、数字和换行；本轮没有自动繁转简。这样“本季→本机”等错误仍能被用户看见和纠正，不用文字改写掩盖识别错误。自动断句不保证恢复原稿的排版换行，交付层保留识别结果中已有的换行。

VAD 命令非零退出、结果不符合版本化格式、模型/程序不可用时均报告失败并保留音频，不把异常当作无语音。v1.8.2 独立 VAD CLI 的 `-vspd/-vsd` 参数解析有重复分支，因此适配器使用上述时长默认值，只显式传递受支持的参数，依据见 [官方 speech.cpp](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/examples/vad-speech-segments/speech.cpp)。Whisper 的无语音条件同时依赖模型无语音分数和文本 log probability；只调 `-nth` 不能取代独立检测，依据见 [官方解码实现](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/src/whisper.cpp#L6912-L6940)。

## 固定样本与人工对照

源文本和生成方式在 [corpus.json](../quality/corpus.json) 与 [生成脚本](../scripts/make-quality-corpus.ps1)。语音由已安装的 Microsoft Huihui Desktop（zh-CN）和 Microsoft Zira Desktop（en-US）以速率 0 离线合成；混合样本在中英文片段间切换声音。所有样本均为 16 kHz 单声道 16-bit PCM。音乐是固定音符与谐波合成的无声乐旋律，提示音是固定频率衰减铃声，没有外部版权音频或真实会议录音。

| 样本 | 长度 | small 默认结果的人工对照；完整原文/输出另存 JSON |
| --- | ---: | --- |
| `mandarin` | 10.795 s | “这段语音只在本机处理……由我检查以后手动发送”内容保留；繁体及标点变化 |
| `mixed-technical` | 19.215 s | Rust、CPU、CUDA 保留；WASAPI loopback 成为 `Waze up high loop back`，“本机”成为“本纪”；base 原来遗漏整段 WASAPI loopback，并把本机写成“本金” |
| `english-numbers` | 14.010 s | API、Rust、Windows、Whisper、CPU、GPU、1.8.2、Port 8080、重试 3 次/等待 2 秒保留；base 将 `Port 8080` 写成 `48080` |
| `homophones` | 8.695 s | 原文两处“本机”保留，两处“本季”也被写作“本机”，small 没有修好这一问题 |
| `multiline` | 15.325 s | 第一、三段内容大体保留，`重试`成为`重視`；英文 `Second line: review the API response` 被错误译写成“第二段,覆蓋 API 回應”；base 原来完全遗漏英文句。属于未解决的语言切换错误，不能算正确转写 |
| `silence` | 8.000 s | VAD 0 段，无文字；未加 VAD 的 base 生成“你不要再想我了” |
| `instrumental` | 12.000 s | VAD 0 段，无文字；未加 VAD 的 base 生成字幕署名 |
| `notification` | 8.000 s | VAD 0 段，无文字；未加 VAD 的 base 生成字幕署名 |

[固定基线 JSON](../quality/baseline-2026-11-11.json) 保存音频哈希、完整已知文本、base/small 原始输出、实际参数和单次耗时。完整本机原始日志在 `.artifacts\quality-issue-9\`；WAV 与带哈希的清单位于 `.artifacts\quality-corpus\`。同一台机器的语音引擎版本变化或其他 Windows 机器可能生成不同字节；重新生成后以新清单哈希为准，不能把不同音频当作同一加速对照。

选择 small 是为了保住英文数字和更多技术发言内容，并非证明它对任意会议都更好。在同一批语音 WAV 上，每次启动新 CPU 进程、含模型载入与检测，base 约 2.1–2.2 秒，small 约 6–9 秒；这些是少量质量对照的观测，没有控制整机其他负载，不是最终停止到交付 SLA。后续加速应固定这些音频/模型/配置，并同时检查转写内容。

另外实际比较了关闭温度回退/抑制非语音 token、严格 no-speech 阈值、auto 语言、greedy、禁用上下文、简体提示词和技术词提示：

- `-nf/-sns`、更严格 `-nth/-lpt` 仍对静音、音乐和提示音输出文字，且部分设置增加术语遗漏；greedy 的一组混合语音还产生非法 UTF-8，应用会作为失败保留音频。
- `--vad` 内建裁剪能过滤三个非语音样本，但拼接有效区域后有句首/词语遗漏，故默认只作外部存在判断，原样解码音频。
- auto 修好 base 的纯英文 Port 8080，但没有改善本批混合语音，small 上也未修好语言切换；增加识别成本，未作为默认。
- small 的 ASCII 技术词提示改善 WASAPI 与英文保留，却把所有本机/本季改成“本级”。中文简体提示的 Windows CLI 参数编码没有单独核对，其结果不作为模型优劣依据。未将针对本语料的词表设为通用会议默认，也未用预期整句作提示来伪造准确结果。

## 已执行的应用验证

测试只经过已确认的 `Application::dispatch` 开始→停止→输出边界；系统音频、外部本地引擎、目标编辑区是边界依赖。

1. 正常业务测试 15 项通过，新增空白识别结果保持旧结果、无旧结果仍为空、可再次采集、绑定目标及旁边输入框均不变。合并 #3 后先复现了空白结果被写盘/追加的失败，再把空结果判断移到保存和目标输出之前。
2. 显式本地模型测试：真实 Whisper+Silero，设备边界用已知 WAV 替代，三类非语音均进入 `NoSpeech`、旧结果和文件不变、临时音频清理。旧版本的全零 WAV 确实生成一句不存在的发言，RED 已复现。另复现了合法零帧 WAV 被 VAD CLI 拒绝的失败，现显式判为无语音；成功初始化后的空闲播放设备和静音不会再由采集适配器的音量门槛误报为设备故障，实际设备/读包/写盘错误仍报告失败。
3. 英文技术词和数字的同一应用流程：base 因遗漏 `Port 8080` 失败，默认 small 通过全部来源文本关键词与数值检查。该测试只覆盖这一确定的回归，不能代替上面的逐句人工对照。

```powershell
# 仅首次获取缺少的模型时联网；已有模型只校验，不重复下载
.\scripts\prepare-quality-model.ps1
.\scripts\make-quality-corpus.ps1
.\scripts\build-listen.ps1 -Task Test
.\scripts\verify-local-quality.ps1
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\build-listen.ps1
```

模型已准备后，测试、WAV 生成和识别均为本地操作；`verify-local-quality.ps1` 使用 cargo 的 `--offline --locked`。本轮没有实施网络隔离探针，强制禁网验收尚待重跑。

重复逐句对照（不播放、仅原始引擎）：

```powershell
python scripts/compare-local-quality.py --engine E:\luyin\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe --model E:\luyin\.tools\models\ggml-small.bin --vad-model E:\luyin\.tools\models\ggml-silero-v5.1.2.bin --gate --manifest E:\luyin\.artifacts\quality-corpus\manifest.json --output E:\luyin\.artifacts\quality-issue-9\repeat-small --profiles selected
# 将 --model 改为 ggml-base.bin、--output 改为 repeat-base，即同配置模型对照
```

## 真实采集验收命令（本轮未运行）

下面驱动会**实际播放声音**，通过已运行 Listen 的正式 start/stop 入口采集。它不选择目标、不改设备、不读取其他应用正文、不发送消息。先按本机验收安排准备好耳机路径、安静播放环境、已知的原生测试目标；只运行一个本工作树构建的 Listen。需要追加验证时，由验收者点击原生目标后再运行样本命令；开始前的控制命令不会抢焦点。

```powershell
.\scripts\run-listen.ps1 -ToolsRoot E:\luyin\.tools -Runtime E:\luyin\.artifacts\quality-live -Hidden
# 默认：small、Silero、CPU、zh、6 线程；也可显式传 -Model 和 -VadModel
.\scripts\capture-quality-sample.ps1 -Sample mandarin -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample mixed-technical -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample english-numbers -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample homophones -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample multiline -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample silence -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample instrumental -Runtime E:\luyin\.artifacts\quality-live
.\scripts\capture-quality-sample.ps1 -Sample notification -Runtime E:\luyin\.artifacts\quality-live
```

记录写到 `.artifacts\quality-capture\<sample>.json`，含原文、输出、WAV 哈希、最终状态与停止后耗时。脚本核对运行目录的进程 ID 和当前工作树程序路径；不操作已在采集/处理中的会话。非语音必须 `NoSpeech` 且旧结果不变；失败保留音频，不自动清理失败数据。每份证据明确写出没有验证桌面热键、目标/剪贴板读回。仍须另行检查原目标和备用文本框全文、原草稿、未自动发送，再在系统强制禁网条件下重复；静态文件或单独 CLI 成功不能代替这些步骤。

VAD 不是无误的语音证明：很短、很轻的发言可能被拒绝，复杂音乐/游戏音效可能误判。当前固定音乐/提示音样本没有覆盖全部非语音，更没有覆盖真实会议的噪声、口音、多人重叠与所有录音分段边界。这些限制和已知错字应随验收结果继续保留。

## 强制禁网复验（2026-11-12）

模型换成 small + Silero 之后补做的系统级禁网验收。本机没有第 #2 轮那个独立禁网桌面沙箱，本次改用**临时 Windows 防火墙出站封禁**：

- 由一次提权进程为 `listen.exe`、`whisper-cli.exe`、`vad-speech-segments.exe` 各建一条 `Direction=Outbound / Action=Block / Profile=Any / Enabled=True` 的规则（规则状态记录在 `.artifacts\offline-firewall\rules-ready.json`）；
- 在封禁生效期间跑完整流程：默认渲染设备上真实播放 8 个固定样本、真实 WASAPI loopback、真实 small + Silero、目标为真实桌面应用（DeepSeek Harness 输入框，由真实焦点选定）；
- 跑完后由同一提权进程删除规则，并记录 `cleanup_complete: true`、`still_present: []`（`.artifacts\offline-firewall\rules-removed.json`），随后查询确认没有残留规则。

结果（`.artifacts\quality-offline-harness\harness-evidence.json`）：

| 样本 | 阶段 | 交付 |
| --- | --- | --- |
| mandarin / mixed-technical / english-numbers / homophones / multiline | `Result` | 逐字追加到目标输入框 |
| silence / instrumental / notification | `NoSpeech` | 不写目标、旧结果与目标不变 |

8 个样本全部符合预期，说明封禁条件下采集、本地识别和交付都正常，不存在云端回退。

补充的静态与运行期证据：

- `listen.exe`、`whisper-cli.exe`、`vad-speech-segments.exe` 的二进制里**没有**任何 `ws2_32/winhttp/wininet/urlmon/dnsapi/mswsock` 名称，即没有网络导入。
- 在另一次真实采集流程中轮询三个进程实际加载的模块，共 56 个，**没有一个是网络模块**（`.artifacts\quality-offline\module-evidence.json`）。

限制：本次封禁作用在这三个二进制上，不是物理断网；但两个二进制层面的事实（无网络导入、运行期无网络模块加载）说明正式路径本来就没有联网能力。防火墙规则已全部删除，机器网络设置未被修改。
