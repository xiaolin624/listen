# Issue #17 验证：SenseVoice 常驻识别引擎

2026-11-12。分支 `ticket/issue-17`（工作树 `E:\luyin\.worktrees\issue-17`），
`integration/v0.2` 已合入 #16/#18/#22。机器：11th Gen Intel Core i5-11400H @ 2.70 GHz，
Windows 11 26200，x64；**CPU 支持 AVX2**，`std::arch::is_x86_feature_detected!("avx2")`
在全部真引擎测试中输出 `true`。

本文所有 SenseVoice 文本与延迟均来自 **sherpa-onnx v2.0.1 C API（常驻进程内）**。
旧 `E:\luyin\.artifacts\engine-bench\sensevoice-results.json` 是 GGUF CLI 路径（每个窗口
起一个进程），其文本（如 `Deep sea harnessness.`）仅用于说明为什么不能混用数字，
**不引用其任何延迟或准确率作为本轮结果**。

第二轮（本文件“三种解码方案对比”“长窗口重叠验证”两节）针对 orchestrator 的实测反例：
同一个 11.38 s 真实采集，整窗一次解码完全正确，而按 VAD 片段分别解码丢了“请/检”并在尾部
多出幻觉文字。结论是改成**整窗解码一次 + 用引擎 token 时间戳把文本归属回 VAD 片段**，
并用真实 41.75 s 回环采集验证 30 s 窗口重叠不重复。

## 结论与验收对照

| 验收项 | 证据 |
| --- | --- |
| 固定普通话 WAV 输出简体、带标点 | `mandarin.wav`：`这段语音只在本机处理，不会上传到云端，请保留完整的发言内容，由我检查以后手动发送。`；真实采集 11.38 s 同样一字不差（整窗解码） |
| 中英混合保留英文技术词、不差于记录的 whisper | 整窗原始输出：`我们用re实现音频采集通过waapp loop back获取声音模型在本机运行使用CPU和进行对照。`；经 #18 术语表校正后交付：`我们用 Rust 实现音频采集通过 WASAPI loopback 获取声音模型在本机运行使用 CPU和进行对照。`。CUDA 在这个上下文里原始输出缺失，术语表无法凭空补词，见“偏差与已知限制” |
| 10 秒语音停止→结果 1 秒级 | 整窗解码最终三次实测：378.1 / 396.5 / 399.0 ms（记录 372 / 382 / 394 ms），中位数 396.5 ms；同一进程内 `mandarin` 单次 325 ms。旧分片方案为 317.6 ms / 中位 665 ms；两者同一量级 |
| 静音、音乐、提示音不产生凭空文字 | Silero VAD 判为 0 段时**不调用识别器**；三个质量语料在 Application 边界均为 `Phase::NoSpeech`，`current-result.txt` 与上一份结果逐字节不变 |
| 引擎不可用时明确失败，不静默回退云端 | 缺失库目录 → 初始 `Phase::Failure`；模型哈希不符 → 准备阶段 `Phase::Failure`；二者都断言从未出现 `Ready`，失败文本明确写 “no cloud service” |
| 真引擎测试默认忽略、显式运行通过 | `tests/sense_voice_quality.rs` 9 个 `#[ignore]` 测试，显式运行 9/9 通过 |
| 既有失败重试/成功清理语义不变 | 合并 `integration/v0.2` 后完整套件 99 passed / 0 failed / 12 ignored，既有测试全部仍通过 |
| 30 s 窗口重叠不重复 | 真实 41.75 s WASAPI 回环采集经生产 `SegmentedAudioWriter` + 识别 worker + 合并路径，6 个测试句各出现 1 次；停止→结果 1.79 s（两个窗口） |

## 固定输入与 SHA-256

`check_ready()` 只做廉价检查（文件存在、C API DLL 哈希、AVX2），不加载模型；
`prepare()` 在后台线程按顺序校验 **全部五个输入** 后才加载。任一缺失或不符都返回
显式 `AppResult` 错误，不加载、不回退。

| 文件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib\sherpa-onnx-c-api.dll` | 4,294,656 | `0965A6E4C0254DA2BEFF489CF7A8AAFFAF7DC94F90B43B3E66448B8CAB4177EA` |
| 同目录 `onnxruntime.dll` | 17,799,168 | `7F66F939A881BAF4F46A2216496798EDF4A1429878B646D12674AA62F27D8A25` |
| `models\sense-voice\model.int8.onnx` | 239,233,841 | `C71F0CE00BEC95B07744E116345E33D8CBBE08CEF896382CF907BF4B51A2CD51` |
| `models\sense-voice\tokens.txt` | 315,894 | `F449EB28DC567533D7FA59BE34E2ABCA8784F771850C78A47FB731A31429A1DC` |
| `models\sense-voice\silero_vad.onnx` | 643,854 | `9E2449E1087496D8D4CABA907F23E0BD3F78D91FA552479BB9C23AC09CBB1FD6` |

哈希用 `Get-FileHash -Algorithm SHA256` 在本机核算，并与模块内常量逐字节一致。

## FFI ABI 验证（先于模块设计）

先写了一次性 `examples/sensevoice_probe.rs`（仅用于证明 ABI，证明后删除，模块内保留同样的
镜像结构）：

1. `LoadLibraryExW(..., LOAD_WITH_ALTERED_SEARCH_PATH)` 加载 C API DLL，使同目录
   `onnxruntime.dll` 可解析；加载前校验上面的 DLL SHA-256。
2. 全零 `SherpaOnnxOfflineRecognizerConfig`，只填 `feat_config`（16000/80）、
   `model_config.tokens`、`num_threads`、`provider="cpu"`、
   `sense_voice.model/language="auto"/use_itn=1`、`decoding_method="greedy_search"`。
3. `CreateOfflineStream` → `AcceptWaveformOffline` → `DecodeOfflineStream` →
   `GetOfflineStreamResult`，按 `DestroyResult` → `DestroyStream` → `DestroyRecognizer` 顺序释放。
4. 第一次运行即得到真实转写 `我们使用deep seek harness来做测试。`，`lang=<|zh|>`、
   `emotion=<|NEUTRAL|>`、`event=<|Speech|>`；识别器创建 1.65 s，3.83 s 音频解码 126 ms。

结构体按 `c-api.h` 声明顺序逐个镜像为 `#[repr(C)]`，并加编译期
`size_of` 断言；另一路用独立脚本 `E:\luyin\.artifacts\sensevoice-abi.py` 直接解析
`c-api.h`（只用头文件文本，不看 Rust 代码）计算 MSVC x64 布局，两边一致：

| 结构 | 字节 |
| --- | ---: |
| `SherpaOnnxFeatureConfig` | 8 |
| `SherpaOnnxOfflineWhisperModelConfig` | 48 |
| `SherpaOnnxOfflineSenseVoiceModelConfig` | 24 |
| `SherpaOnnxOfflineCanaryModelConfig` | 40 |
| `SherpaOnnxOfflineFunASRNanoModelConfig` | 88 |
| `SherpaOnnxOfflineQwen3ASRModelConfig` | 64 |
| `SherpaOnnxOfflineCohereTranscribeModelConfig` | 32 |
| `SherpaOnnxOfflineModelConfig` | 504 |
| `SherpaOnnxOfflineRecognizerConfig` | **624** |
| `SherpaOnnxOfflineRecognizerResult` | **128** |
| `SherpaOnnxSileroVadModelConfig` | 32 |
| `SherpaOnnxVadModelConfig` | **88** |
| `SherpaOnnxSpeechSegment` | 24 |

填充字段的偏移（相对 `SherpaOnnxOfflineRecognizerConfig` 基址，已由解析器与运行时行为双向确认）：
`feat_config` +0、`model_config` +8、其中 `tokens` +104、`num_threads` +112、`debug` +116、
`provider` +120、`model_type` +128、`sense_voice` +160（`model` +160、`language` +168、
`use_itn` +176）、`lm_config` +512、`decoding_method` +528、`max_active_paths` +536。

第二轮还用到结果结构里的 token 字段（本轮已在实际引擎上确认非空）：
`text` +0、`timestamps`（`float*`，每 token 起始秒）+8、`count`（`i32`）+16、
`tokens_arr`（`const char* const*`）+32、`lang` +48、`emotion` +56、`event` +64。

## 引擎、VAD 与解码策略

- 识别：`SenseVoiceSmall q8`；sample_rate 16000、feature_dim 80、`language="auto"`、
  `use_itn=1`、CPU、`greedy_search`、`max_active_paths=4`、6 线程（`--threads`）。
- VAD：`silero_vad.onnx`，threshold 0.5、`min_silence_duration=0.6 s`、
  `min_speech_duration=0.25 s`、window 512、`max_speech_duration=10 s`，检测器常驻并在每个窗口前
  `Reset`，因此 `SpeechSegment.start` 就是窗口内相对采样下标。
- **解码规则（第二轮定稿）**：
  1. VAD 先跑一遍；片段数为 0 时直接返回空，**不创建 decode stream、不调用识别器**。
  2. 整个窗口只调用一次 `SherpaOnnxAcceptWaveformOffline` + `DecodeOfflineStream`，保留完整
     上下文（这是修复“请→经、检→点、尾部幻觉”的关键）。
  3. 用结果的 `timestamps`（每 token 起始秒）按顺序把 token 分回 VAD 片段：相邻片段的分界取
     两者静音间隙的中点，最后一个片段接收尾部标点。于是 `TimedText` 的 mid-window 时间是引擎
     真实 token 时间，而不是整窗伪中点；`streaming.rs` 的中点归属规则完全不用改。
  4. 没拿到 token 时间戳的构建（当前 pinned 构建不会）退回“每 VAD 片段单独解码”，同一引擎、
     同一门禁；这是显式的兼容路径，不是云端/whisper 回退。
- `transcribe()` 与 `transcribe_segment()` 使用同一次整窗解码的结果；后者按上述规则切 span。
  如果 token 文本拼回去与整窗 `text` 不一致（丢 token、无效 UTF-8），返回显式错误并保留音频。
- 非语音门禁三层：
  1. VAD 片段数为 0 → 返回空；
  2. 结果 `event` 不是 `<|Speech|>`（如 `<|BGM|>`）→ 整窗丢弃；
  3. 清理 `<|...|>` 控制标签后，空文本、纯标点、已知幻觉短词（`The.`、`.`、`Thank you.`、
     `字幕`、`Music` 等）→ 丢弃。
- 尾部幻觉门禁：最后一个 VAD 片段若文字密度超过 7 个“语音单位”/秒 + 1（中文 1 字 = 1 单位，
  拉丁字母 0.25），判为不合常理并整段丢弃。纯函数 `implausible_tail_text`，单测覆盖
  `登录用户的I`（保留期 400 ms，6 单位）被过滤而 `谢谢大家`、`The API uses Rust.` 不被过滤。

### VAD 参数说明

0.6 s 的 `min_silence_duration` 是第一轮为“每个片段单独解码”调的；第二轮改为整窗解码后，
它不再影响文字内容，只影响 **span 边界**（窗口仍按语音停顿切 span，`max_speech_duration=10 s`
避免单个 span 过长）。`VAD_DECODE_TAIL_SAMPLES`（20 ms）现在只用于无时间戳时的兼容分支。

## 三种解码方案对比

测量方法（可复现，脚本未入库）：

- `E:\luyin\.artifacts\sensevoice_modes.py`：ctypes 直接调用同一 pinned DLL/model/VAD，
  对 9 个固定 WAV 分别执行三种策略并计算 CER 与解码墙钟。
  - 方案 1（原实现）：每个 VAD 片段单独解码（后接 20 ms 尾部）。
  - 方案 2：整窗一次解码。
  - 方案 3：每个 VAD 片段解码，前面拼 0.5 s pre-roll，再用 token 时间戳把 pre-roll 部分裁掉。
- CER = 编辑距离 / 参考字符数；参考文本为固定源文本，比较时忽略空白与标点（因此标点差异不计入，
  这对三个方案是同一口径）。

### CER（越低越好）

| WAV | 时长 | VAD 段 | 方案 1 分片 | 方案 2 整窗 | 方案 3 pre-roll |
| --- | ---: | ---: | ---: | ---: | ---: |
| `captured-mandarin.wav`（真实采集） | 11.38 s | 2 | 0.027 | **0.000** | 0.054 |
| `mandarin.wav` | 10.79 s | 2 | 0.027 | **0.000** | 0.054 |
| `mixed-technical.wav` | 19.21 s | 9 | 0.185 | **0.167** | 0.241 |
| `zh-term.wav` | 3.83 s | 1 | 0.304 | **0.000** | 0.087 |
| `zh-term-degraded.wav` | 3.83 s | 1 | 0.435 | 0.435 | 0.435 |
| `en-sentence.wav` | 3.77 s | 1 | 0.143 | **0.119** | 0.143 |
| `en-sentence-degraded.wav` | 3.77 s | 1 | 0.143 | 0.143 | 0.190 |
| `en-term.wav` | 1.91 s | 1 | **0.133** | 0.467 | 0.400 |
| `en-term-degraded.wav` | 1.91 s | 1 | **0.133** | 0.400 | 0.400 |

结论：中文/混合的 5 个样本上方案 2 全部最好或持平（captured/mandarin/zh-term 都是 0）；
方案 3 因 pre-roll 让开头字被裁掉（`这段`→`段`、`We use`→`use`），整体更差。
纯英文专名 probe（`en-term`）方案 1 更好，但这是术语表 #18 负责的专名范围，orchestrator 已明确
不围绕它调引擎。方案 2 的 `DeepSek harnessness` 与 whisper 的 `DeepSeek harness` 差距同样
由术语表覆盖。

关键的“真实采集”文本对比（`captured-mandarin.wav`，11.38 s）：

| 方案 | 文本 |
| --- | --- |
| 原始 1 | `…不会上传到云端。保留完整的发言内容…`（丢“请”） |
| 原始 2 | `这段语音只在本机处理，不会上传到云端，请保留完整的发言内容，由我检查以后手动发送。` |
| 原始 3 | `段语音只在本机处理…`（丢“这”） |

### 解码耗时（引擎本身，同一进程/同一模型）

| WAV | 方案 1 | 方案 2 | 方案 3 |
| --- | ---: | ---: | ---: |
| `captured-mandarin.wav` | 242.9 ms | 264.9 ms | 239.4 ms |
| `mandarin.wav` | 274.3 ms | 266.1 ms | 269.5 ms |
| `mixed-technical.wav` | 428.2 ms | 549.4 ms | 603.3 ms |
| `en-sentence.wav` | 101.6 ms | 120.9 ms | 104.8 ms |

整窗解码在 10–11 s 音频上与分片几乎相同（差 20 ms 内）；在 19 s、9 段的混合音频上整窗慢
约 120 ms，但仍在秒内。分片方案 3 因为每段都多解 0.5 s，19 s 样本反而最慢（603 ms）。

### 停止→结果（Application 边界，`mandarin.wav` 10.795 s）

- 方案 1（旧实现，commit `34b48de`）：空载单次 317.6 ms；并发构建下中位 665.1 ms。
- 方案 2（本轮实现）：单次 325 ms；最终三次 378.1 / 396.5 / 399.0 ms（记录 372 / 382 / 394），
  中位 396.5 ms。
- 方案 3：没有单独跑 Application 路径；其引擎解码在 10.8 s 样本上是 269.5 ms（方案 1 为
  274.3 ms），按同一 VAD/IO 开销推算仍在 0.3–0.4 s 量级；19 s 样本上反而比方案 1 慢约 175 ms。
  未把推算值写成实测值。

## 长窗口重叠验证（真实 41.75 s 采集）

要求是“用真实 >30 s 采集证明，不靠推理”。做法：

1. 用 System.Speech 生成 41.17 s 的 6 句已知 TTS（每句之间 0.9 s 静音），句首分别是
   `第一段测试语音` … `第六段测试语音`；第 4 句落在 30 s 窗口边界附近。
2. `capture_only.exe` 以真实 WASAPI 回环录制默认多媒体渲染端点（耳机 WH-1000XM4），
   同时 `System.Media.SoundPlayer` 播放该 WAV；得到 `captured-long-tts.wav`：
   **41.75 s，16 kHz 单声道，真实回环采集**。
3. 忽略测试用一个只替换“设备采样推送”的 fixture，把该 WAV 送入生产
   `SegmentedAudioWriter`（默认 30 s 窗口 / 2 s 重叠）、生产识别 worker、生产
   `streaming.rs` 中点合并和真实 SenseVoice 引擎；设备层之外没有任何替身。
4. `Application` 的结果（142 字）：

```text
第一段测试语音用于检查长窗口边界。第二段测试语音包含英文技数词rus和CPU。第三段测试语音是一段较长的普通话内容，请保留完整的发言内容。第四段测试语音将跨过30秒的窗口边界，这句话只能出现一次。
第五段测试语音用于确认重叠区域不会重复文字。第六段测试语音是结尾，由我检查以后手动发送。
```

6 个句首短语在结果中各出现 **1 次**（测试断言 `matches(...).count() == 1`），
包括跨 30 s 边界的第 4 句；没有出现整窗单 span 方案会造成的边界句重复。
停止→结果 **1.79 s**（两个窗口：首个 30 s + 最后 11.75 s，`timing.stop_to_result_ms=1787`）。

> 第一次尝试用的是“mandarin + english-numbers + multiline”拼接播放，但耳机回环在约 11 s 后
> 混入了机器上其他应用的音频，采集内容无法作为证据，已丢弃；改由已知 TTS 自生成音频并在
> 采集后先用整窗解码核对内容，再用于上述测试。

## 真实转写结果

### 质量语料（Application 边界，`LISTEN_QUALITY_CORPUS`）

| 样本 | 时长 | 结果 |
| --- | ---: | --- |
| `mandarin` | 10.795 s | `这段语音只在本机处理，不会上传到云端，请保留完整的发言内容，由我检查以后手动发送。` |
| `mixed-technical`（原始引擎） | 19.215 s | `我们用re实现音频采集通过waapp loop back获取声音模型在本机运行使用CPU和进行对照。` |
| `mixed-technical`（经 #18 术语表） | 19.215 s | `我们用 Rust 实现音频采集通过 WASAPI loopback 获取声音模型在本机运行使用 CPU和进行对照。` |
| `silence` | 8 s | VAD 0 段 → `Phase::NoSpeech`（整文件强制解码时模型会吐 `我.` + `<|BGM|>`） |
| `instrumental` | 12 s | VAD 0 段 → `Phase::NoSpeech`（强制解码时吐 `.` + `<|BGM|>`） |
| `notification` | 8 s | VAD 0 段 → `Phase::NoSpeech`（强制解码时吐 `The.` + `<|BGM|>`） |

### 六个 probe WAV（模块路径，`cargo test ... probe_wav`，整窗解码）

| WAV | 模块转写 |
| --- | --- |
| `zh-term.wav` | `我们使用deep seek harness来做测试。` |
| `zh-term-degraded.wav` | `我们使用VC harnessness来做测试。` |
| `en-term.wav` | `Deepea harnessness.` |
| `en-term-degraded.wav` | `Deep sea harnessness.` |
| `en-sentence.wav` | `We use the DeepSek harnessness API for transcription.` |
| `en-sentence-degraded.wav` | `We use the deepse harnessness API for transcription.` |

这些与“一次性 spike 的整文件解码”现在是同一路径（整窗），因此基本一致；差异只来自 spike
当时用 4 线程、现在 6 线程以及容器差异。

### 一次性 spike 的整文件解码（仅作 ABI 参考）

| WAV | 整文件文本 | `lang` / `event` | 解码 |
| --- | --- | --- | ---: |
| `zh-term.wav` | `我们使用deep seek harness来做测试。` | `<|zh|>` / `<|Speech|>` | 112.7 ms |
| `zh-term-degraded.wav` | `我们使用VC harnessness来做测试。` | `<|zh|>` / `<|Speech|>` | 139.4 ms |
| `en-term.wav` | `Deepea harnessness.` | `<|en|>` / `<|Speech|>` | 67.7 ms |
| `en-term-degraded.wav` | `Deep sea harnessness.` | `<|en|>` / `<|Speech|>` | 84.5 ms |
| `en-sentence.wav` | `We use the DeepSek harnessness API for transcription.` | `<|en|>` / `<|Speech|>` | 132.5 ms |
| `en-sentence-degraded.wav` | `We use the deepse harnessness API for transcription.` | `<|en|>` / `<|Speech|>` | 118.7 ms |

## 与 whisper.cpp 记录的对照

whisper 数值全部来自本机已记录的 `E:\luyin\.artifacts\engine-bench\whisper-results.json`
（v1.8.2 CPU small，`-l zh`，plain 与 prompt 两种模式；今天没有重跑 whisper）。
SenseVoice 列是本文实测的整窗输出（混合列同时给出术语表校正后结果）。

| WAV | whisper plain（记录） | whisper prompt（记录） | SenseVoice（本轮） |
| --- | --- | --- | --- |
| `zh-term` | 我们使用 DeepSeq Harness 来做测试。 | 我們使用DeepSeek Harness來做測試。（繁体） | `我们使用deep seek harness来做测试。`（术语表可校正） |
| `en-term` | Deep Seek Harness | DeepSeek harness | `Deepea harnessness.`（英文专名弱于 whisper） |
| `en-sentence` | 我們利用 DeepSeq Harness API for Transcription（繁体） | We use the DeepSeek harness API for transcription | `We use the DeepSek harnessness API for transcription.` |
| `mandarin` | 這段語音只在本機處理,不會上傳到雲端。請保留完整的發言內容,由我檢查以後手動發送。（繁体、半角逗号） | 同上（繁体） | 简体、全角标点、内容完整 |
| `mixed-technical` | 我们用“Rust”实现音频采集 通过“Waze up high loop back”获取声音模型在本纪运行 使用“CPU”和“Cuda”进行对照 | 我們用 rust 實現音頻採集 通過 WASAPI Loopback獲取聲音模型在本機運行 使用 CPU和 CUDA進行對照（繁体） | 原始：`…re…waapp loop back…CPU和进行对照`；校正后：`…Rust…WASAPI loopback…CPU和进行对照` |
| `english-numbers` | The API uses Rust, Windows, Whisper, CPU, and GPU.Version 1.8.2.Port 8080.Retry 3 times, then wait 2 seconds. | Version 1.8.2 Port 8080 Retry 3 times, then wait 2 seconds（丢第一句） | The API uses rust, Windows, whisper, CPU and GPU. Version 1.8.2. Port 8080. Retry three times, then wait two seconds. |
| `silence` | 你不要再說了（凭空文字） | VIP 電腦（凭空文字） | `Phase::NoSpeech` |
| `instrumental` | MING PAO CANADA \| MING PAO TORONTO（凭空文字） | VIP 電腦（凭空文字） | `Phase::NoSpeech` |

结论要分开说：

- **普通话**：SenseVoice 明显更好——简体、全角标点、长句完整；whisper plain/prompt 都输出繁体或半角标点。
  第二轮真实采集上整窗解码与源文本一致，修复了分片解码丢“请/检”的问题。
- **中英混合**：整窗原始输出保留 `CPU/音频采集/本机/对照`，`re`、`waapp loop back` 由 #18
  术语表校正；`CUDA` 在这个 19 s 上下文里整窗输出缺失，术语表无法补词。这是本轮仍存在的
  限制，记录在“偏差与已知限制”，不在引擎里做特判。
- **纯英文专名**：SenseVoice 不如 whisper prompt（`DeepSek harnessness` vs `DeepSeek harness`），
  属于术语表范围；本轮不把英文专名准确率当作通过条件。
- **非语音**：whisper 会凭空生成整句（上表），SenseVoice 模块由 VAD + event/短结果门禁返回
  `Phase::NoSpeech`，这是本票的核心行为改进。

## 延迟

### 停止→结果（Application 边界，`mandarin.wav` 10.795 s，整窗解码）

同一进程连续三次完整采集（停止前起表，轮询到非 `Processing`）：

| 次 | 墙钟实测 | `timing.stop_to_result_ms` | `recognition_ms` |
| ---: | ---: | ---: | ---: |
| 1 | 378.1 ms | 372 ms | 363 ms |
| 2 | 396.5 ms | 382 ms | 381 ms |
| 3 | 399.0 ms | 394 ms | 392 ms |
| 中位数 | **396.5 ms** | **382 ms** | **381 ms** |

同进程内 `mandarin` 验收测试单次 325 ms；长采集（41.75 s，两个窗口）停止→结果 1.79 s。
旧分片实现（commit `34b48de`）在同一机器上曾测到 317.6 ms（空载单次）/ 665.1 ms（并发构建
中位）。两种实现都属 1 秒级；整窗略慢 20–80 ms，换来正确的长句上下文。

判据取三次的中位数：偶发一次 1.1–2.1 s 的外部 CPU 抢占会落在单个样本上；所有原始样本都
记录在此，不取最佳值。空载/轻载下是 0.31–0.40 s。

### 识别耗时（引擎本身，仅供拆分参考）

- 整窗：10.8 s 音频约 266–325 ms；11.38 s 采集约 265 ms；19.2 s 音频约 549 ms；3.8 s 音频约 90–121 ms。
- VAD：10–19 s 窗口约 60–100 ms。
- 模型准备：识别器创建约 1.5–1.7 s（spike 实测），`prepare()` 另需对 228 MB 模型和运行库做
  SHA-256，都发生在启动后台线程，不占用停止→结果路径。

## 测试与命令

工具链环境由 `scripts/build-listen.ps1` / `.artifacts/run-cargo.ps1` 设置（Rust 1.99.1
`stable-x86_64-pc-windows-gnullvm`，`--offline --locked`）。快速套件：

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test -Manifest E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml
```

合并 `integration/v0.2` 后结果：**99 passed, 0 failed, 12 ignored**（含 #16/#18/#22 的测试与
既有 64 个测试）；`cargo fmt --check` 无输出。

真引擎套件需要显式环境变量（默认 `#[ignore]`）：

```powershell
$env:LISTEN_SHERPA_LIB = 'E:\luyin\.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib'
$env:LISTEN_SENSEVOICE_MODEL = 'E:\luyin\.tools\models\sense-voice\model.int8.onnx'
$env:LISTEN_SENSEVOICE_TOKENS = 'E:\luyin\.tools\models\sense-voice\tokens.txt'
$env:LISTEN_SENSEVOICE_VAD = 'E:\luyin\.tools\models\sense-voice\silero_vad.onnx'
$env:LISTEN_QUALITY_CORPUS = 'E:\luyin\.artifacts\quality-corpus'
# 可选：真实 >30 s 回环采集；不设置时该测试打印 skipped，其余 8 个照常运行
$env:LISTEN_LONG_CAPTURE = 'E:\luyin\.artifacts\smoke3\captured-long-tts.wav'
# 用 run-cargo.ps1 设置好的 toolchain 环境执行：
cargo test --offline --locked --manifest-path E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml `
  --test sense_voice_quality -- --ignored --nocapture
```

结果：**9 passed, 0 failed**。覆盖：普通话长句、混合技术词（经术语表）、静音/音乐/提示音、
10 s 停止延迟、整窗 token 时间戳 span、缺失库目录、模型哈希不符、六个 probe WAV 的转写记录、
真实 41.75 s 采集的 30 s 重叠去重。
测试内部用一把全局互斥锁串行化重引擎测试，避免 cargo 默认并行导致互相抢占 CPU 影响延迟数字。
完整输出保存在 `E:\luyin\.artifacts\issue-17-quality-suite-v2.txt`（未入库）。

`run-cargo.ps1` 会先设置 `CARGO_HOME`、`RUSTUP_HOME`、`RUSTUP_TOOLCHAIN`、`RUSTFLAGS`、
`CARGO_TARGET_DIR`（默认多个 worktree 共用 `E:\luyin\.artifacts\cargo-target`）和 `PATH`；
直接调用 `cargo` 时需要先设置同样的变量。

> 构建缓存注意：验证过程中出现过多个 worktree 共用 `CARGO_TARGET_DIR`、同名 `listen 0.1.0`
> 包的缓存互相串扰，表现为 bin 报 `could not find sensevoice in listen`。执行
> `cargo clean -p listen --manifest-path <本分支 Cargo.toml>` 后重跑即恢复绿色；这是共享构建
> 缓存的环境问题，不是本票代码问题。真引擎测试使用独立 target dir 复跑也全部通过。

## 偏差与已知限制

1. **第二轮解码策略改动**：从“每 VAD 片段单独解码”改为“整窗解码一次 + token 时间戳归属”。
   理由和三方案数据见上文；`transcribe_segment` 的 span 时间来自引擎 token 时间戳，不是
   伪造整窗中点。没有 token 时间戳的构建退回分片解码。
2. **尾部幻觉门禁是启发式**：按“文字密度 vs VAD 片段时长”丢弃最后一个片段。它能挡住
   `登录用户的I` 这类短噪声段，但阈值（7 单位/s + 1）是经验值；长片段里附加少量幻觉不会触发。
   整窗解码本身已消除了 orchestrator 实测的尾部幻觉，这个门禁是第二道防线。
3. **混合语料 CUDA 缺失**：19.2 s 上下文的整窗输出没有 CUDA，术语表无法补词；本轮不为此特判。
   若后续真实会议里这是高频问题，需要重新评估上下文长度/分片策略或热词引擎（超出本票）。
4. **长窗口测试的边界**：真实采集是 41.75 s，生产窗口写入/识别/合并路径都是真的；只有“设备
   采样推送”由 fixture 一次性写入，不是实时 WASAPI feed。断言的是合并语义，不是采集时序。
   第一次“拼接语料”的采集因回环混入机器其他音频而作废，未作为证据。
5. **`gui.rs` 有两处小改动**：删除未再使用的 `Whisper` 导入，并把构造识别器的一整段替换为
   `config.transcriber()`（语义上就是一行）；引擎构造与所有新参数都在 `main.rs` 的
   `Config::transcriber()` 中。窗口代码没有动。
6. **whisper 仍完整保留**：`--engine whisper` 或 `--engine <whisper-cli.exe>` 显式选择；
   `--model`/`--vad-model`/`--language`/`--backend host|cli` 语义不变。SenseVoice 用新增的
   `--sherpa-lib`、`--sensevoice-model`、`--sensevoice-tokens`、`--sensevoice-vad`、
   `--sensevoice-language`。不带参数时使用 SenseVoice。
7. **交付包未改**：模型/DLL 的打包替换属于 #23；本票只让开发构建默认走 SenseVoice。
8. **延迟测试使用中位数**：见“延迟”一节，原因是本机同时跑其他 agent 的构建；每个样本都记录。
9. **spike 已删除**：一次性 `examples/sensevoice_probe.rs` 证明 ABI 后删除；其结构体镜像与
   验证方法已固化在 `src/sensevoice.rs` 的编译期断言和本文表格中。

## 分支与提交

- 分支：`ticket/issue-17`（工作树 `E:\luyin\.worktrees\issue-17`）。
- 基线：`eea9109`（`integration/v0.2` 起点）。
- 第一轮提交：`6319e9e`（引擎）、`c363e3e`（默认引擎与参数）、`83a19db`（真引擎测试）、
  `b0737a2`（VAD 边界修正）、`3388754`（probe 记录）、`34b48de`（第一轮验证文档）。
- 合并：`integration/v0.2` 后续为 `ec97f86`（#16）和 `cd87b70`（#18/#22）。
- 第二轮提交：整窗解码 + token 时间戳 span + 尾部门禁、长采集测试、本验证文档更新
  （提交哈希见 `git log`）。
- 真实采集/复现脚本（未入库）：`E:\luyin\.artifacts\smoke3\`（`captured-long-tts.wav`）、
  `E:\luyin\.artifacts\sensevoice_modes.py`、`E:\luyin\.artifacts\sensevoice_groups.py`。
- 真引擎测试原始输出（未入库）：`E:\luyin\.artifacts\issue-17-quality-suite-v2.txt`。
- ABI 独立解析脚本（未入库）：`E:\luyin\.artifacts\sensevoice-abi.py`。
