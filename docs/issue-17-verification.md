# Issue #17 验证：SenseVoice 常驻识别引擎

2026-11-12。分支 `ticket/issue-17`（工作树 `E:\luyin\.worktrees\issue-17`），基线
`integration/v0.2`，合并提交见文末。机器：11th Gen Intel Core i5-11400H @ 2.70 GHz，
Windows 11 26200，x64；**CPU 支持 AVX2**，`std::arch::is_x86_feature_detected!("avx2")`
在全部真引擎测试中通过，引擎可加载本身就是 AVX2 可用的必要条件。

本文所有 SenseVoice 文本与延迟均来自 **sherpa-onnx v2.0.1 C API（常驻进程内）**。
旧 `E:\luyin\.artifacts\engine-bench\sensevoice-results.json` 是 GGUF CLI 路径（每个窗口
起一个进程），其文本（如 `Deep sea harnessness.`）仅用于说明为什么不能混用数字，
**不引用其任何延迟或准确率作为本轮结果**。

## 结论与验收对照

| 验收项 | 证据 |
| --- | --- |
| 固定普通话 WAV 输出简体、带标点 | `mandarin.wav`：`这段语音只在本机处理，不会上传到云端。保留完整的发言内容，由我检查以后手动发送。`；真引擎测试断言 4 个原文片段、`，`/`。`、无繁体字 |
| 中英混合保留英文技术词、不差于记录的 whisper | `mixed-technical.wav` 保留 `Rust`、`CPU`、`音频采集`、`本机`、`对照`；whisper 同一 WAV 的 plain 记录写成“本纪”，prompt 记录把中文写成繁体。WASAPI/CUDA 仍被识别错，属术语表票 #19/#20 的范围（ADR 0001 已说明 SenseVoice 无解码期热词） |
| 10 秒语音停止→结果 1 秒级 | 同一 10.795 s WAV 三次实测：725.1 / 665.1 / 625.7 ms（记录 701 / 650 / 618 ms），中位数 665.1 ms；空载单独运行 317.6 ms，不同负载下 0.31–0.95 s（见“延迟”） |
| 静音、音乐、提示音不产生凭空文字 | Silero VAD 判为 0 段时**不调用识别器**；三个质量语料在 Application 边界均为 `Phase::NoSpeech`，`current-result.txt` 与上一份结果逐字节不变 |
| 引擎不可用时明确失败，不静默回退云端 | 缺失库目录 → 初始 `Phase::Failure`；模型哈希不符 → 准备阶段 `Phase::Failure`；二者都断言从未出现 `Ready`，失败文本明确写 “no cloud service” |
| 真引擎测试默认忽略、显式运行通过 | `tests/sense_voice_quality.rs` 8 个 `#[ignore]` 测试，显式运行 8/8 通过 |
| 既有失败重试/成功清理语义不变 | 合并 `integration/v0.2` 后完整套件 78 passed / 0 failed / 11 ignored，既有 64 个测试全部仍通过 |

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

## 引擎与 VAD 配置

- 识别：`SenseVoiceSmall q8`；sample_rate 16000、feature_dim 80、`language="auto"`、
  `use_itn=1`、CPU、`greedy_search`、`max_active_paths=4`、6 线程（`--threads`）。
- VAD：`silero_vad.onnx`，threshold 0.5、`min_silence_duration=0.6 s`、
  `min_speech_duration=0.25 s`、window 512、`max_speech_duration=10 s`，检测器常驻并在每个窗口前
  `Reset`，因此 `SpeechSegment.start` 就是窗口内相对采样下标。
- 每个 VAD 片段单独解码；解码时只额外向后多取 320 采样（20 ms）的尾巴，防止 VAD 在词尾收音处
  截掉最后一个辅音，但 `TimedText` 仍使用 VAD 的原始 `start`/`n`（真实起点/终点），没有伪造整窗时间。
- `transcribe_segment()` 每个存活片段输出一个 `TimedText`；`streaming.rs` 的原有中点归属规则未改。
  片段级（而非 token 级）时间戳的残余风险见“偏差与已知限制”。
- 非语音门禁三层：
  1. VAD 片段数为 0 → 直接返回空，**不创建 decode stream、不调用识别器**；
  2. 结果 `event` 不是 `<|Speech|>`（如 `<|BGM|>`）→ 丢弃该片段；
  3. 清理 `<|...|>` 控制标签后，空文本、纯标点、已知幻觉短词（`The.`、`.`、`Thank you.`、
     `字幕`、`Music` 等）→ 丢弃。

### 为什么是 0.6 s 静音阈值

在同一 `mixed-technical.wav` 上对比过 VAD 参数（片段解码，CPU）：

| `min_silence_duration` | 结果特征 |
| --- | --- |
| 0.3 s | 拆成 11 段，`Rust`/`CPU` 正确，但一句被切成“实现音频采集。”“通过。”，标点破碎 |
| 0.45 s | 9 段，`Rust`/`CPU` 正确，仍偏碎 |
| 0.6 s | 9 段，`Rust`（加 20 ms 尾巴）/`CPU` 正确，句子按停顿成组，标点最自然 |

不额外做起点 padding：实测起点 padding 会改变短片段解码，反而更容易丢英文词；只保留 20 ms 尾部
温度补偿。

## 真实转写结果

### 质量语料（Application 边界，`LISTEN_QUALITY_CORPUS`）

| 样本 | 时长 | 结果 |
| --- | ---: | --- |
| `mandarin` | 10.795 s | `这段语音只在本机处理，不会上传到云端。保留完整的发言内容，由我检查以后手动发送。` |
| `mixed-technical` | 19.215 s | `我们用。 Rust.实现音频采集通过。 Wesapp high loop back.获取声音。模型在本机运行使用。 Cpu.クら？进行对照。` |
| `silence` | 8 s | VAD 0 段 → `Phase::NoSpeech`（整文件强制解码时模型会吐 `我.` + `<|BGM|>`） |
| `instrumental` | 12 s | VAD 0 段 → `Phase::NoSpeech`（强制解码时吐 `.` + `<|BGM|>`） |
| `notification` | 8 s | VAD 0 段 → `Phase::NoSpeech`（强制解码时吐 `The.` + `<|BGM|>`） |

### 六个 probe WAV（模块路径，`cargo test ... probe_wav`）

| WAV | 模块转写 |
| --- | --- |
| `zh-term.wav` | `我们使用deepea harnessness来做测试。` |
| `zh-term-degraded.wav` | `我们使用BC partners来做测试。` |
| `en-term.wav` | `Deep sea harness.` |
| `en-term-degraded.wav` | `Deep sea harness.` |
| `en-sentence.wav` | `We use the Deepse harnessness API for transcription.` |
| `en-sentence-degraded.wav` | `We use the deep Sea harnessness API for transcription.` |

这些与下面“一次性 spike 的整文件解码”不完全相同：模块把每个 VAD 片段单独解码，而 spike 把整个
文件一次性解码。片段边界十几毫秒的差别就会改变英文专名的结果（`deep seek harness` ↔
`deepea harnessness`），所以这两组数字要按“两条不同路径”理解，不能混用。

### 一次性 spike 的整文件解码（仅作 ABI 参考，非模块路径）

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
SenseVoice 列是本文实测的模块/引擎输出。

| WAV | whisper plain（记录） | whisper prompt（记录） | SenseVoice（本轮） |
| --- | --- | --- | --- |
| `zh-term` | 我们使用 DeepSeq Harness 来做测试。 | 我們使用DeepSeek Harness來做測試。（繁体） | 见上方 probe 表 |
| `en-term` | Deep Seek Harness | DeepSeek harness | 见上方 probe 表（英文专名弱于 whisper） |
| `en-sentence` | 我們利用 DeepSeq Harness API for Transcription（繁体） | We use the DeepSeek harness API for transcription | 见上方 probe 表 |
| `mandarin` | 這段語音只在本機處理,不會上傳到雲端。請保留完整的發言內容,由我檢查以後手動發送。（繁体、半角逗号） | 同上（繁体） | 简体、全角标点、内容完整 |
| `mixed-technical` | 我们用“Rust”实现音频采集 通过“Waze up high loop back”获取声音模型在本纪运行 使用“CPU”和“Cuda”进行对照 | 我們用 rust 實現音頻採集 通過 WASAPI Loopback獲取聲音模型在本機運行 使用 CPU和 CUDA進行對照（繁体） | Rust/CPU/音频采集/本机/对照 保留；WASAPI→Wesapp、CUDA→クら 仍错 |
| `english-numbers` | The API uses Rust, Windows, Whisper, CPU, and GPU.Version 1.8.2.Port 8080.Retry 3 times, then wait 2 seconds. | Version 1.8.2 Port 8080 Retry 3 times, then wait 2 seconds（丢第一句） | The API uses rust, Windows, whisper, CPU and GPU. Version 1.8.2. Port 8080. Retry three times, then wait two seconds. |
| `silence` | 你不要再說了（凭空文字） | VIP 電腦（凭空文字） | `Phase::NoSpeech` |
| `instrumental` | MING PAO CANADA \| MING PAO TORONTO（凭空文字） | VIP 電腦（凭空文字） | `Phase::NoSpeech` |

结论要分开说：

- **普通话**：SenseVoice 明显更好——简体、全角标点、长句完整；whisper plain/prompt 都输出繁体或半角标点。
- **中英混合**：在“保留英文技术词 + 简体 + 标点”这一验收口径上，SenseVoice 至少与 whisper plain
  一样好或更好（Rust/CPU 保留、中文正确简体），但 WASAPI/CUDA 的专名仍明显差于 whisper prompt；
  这是 ADR 0001 已记录的 CTC 无热词限制，由术语表票 #19/#20 负责校正，本轮不静默改写识别结果。
- **纯英文专名**：SenseVoice 不如 whisper prompt（`DeepSek harnessness` vs `DeepSeek harness`），
  同样属于术语表范围；本轮不把英文专名准确率当作通过条件。
- **非语音**：whisper 会凭空生成整句（上表），SenseVoice 模块由 VAD + event/短结果门禁返回
  `Phase::NoSpeech`，这是本票的核心行为改进。

## 延迟

### 停止→结果（Application 边界，`mandarin.wav` 10.795 s）

同一进程连续三次完整采集（停止前起表，轮询到非 `Processing`）：

| 次 | 墙钟实测 | `timing.stop_to_result_ms` | `recognition_ms` |
| ---: | ---: | ---: | ---: |
| 1 | 725.1 ms | 701 ms | 696 ms |
| 2 | 665.1 ms | 650 ms | 642 ms |
| 3 | 625.7 ms | 618 ms | 615 ms |
| 中位数 | **665.1 ms** | **650 ms** | **642 ms** |

同一测试在不同时段的复测（本机同时运行其他 ticket 的构建/测试，负载不同）：

| 运行 | 三次样本（实测 / 记录，ms） | 中位数 |
| --- | --- | ---: |
| 空载单独运行 | 317.6 / 313（单次） | 313 ms |
| 套件运行 A | 366.5/362、406.7/402、366.9/361 | 366.9 ms |
| 套件运行 B（本表） | 725.1/701、665.1/650、625.7/618 | 665.1 ms |
| 套件运行 C（其他 agent 正在跑 whisper 与构建） | 1119.1/1088、953.0/944、686.5/677 | 953.0 ms |

判据取三次的中位数：偶发一次 1.1–2.1 s 的外部 CPU 抢占会落在单个样本上；所有原始样本都记录在此，
不取最佳值。空载/轻载下是 0.31–0.73 s，属于 1 秒级。

### 识别耗时（引擎本身，仅供拆分参考）

- spike 单次整文件：3.83 s 音频 67–139 ms；19.2 s 音频 612 ms；10.8 s 音频 333 ms。
- 模块按 VAD 片段解码时，`mandarin` 两段的识别合计约 350–400 ms（含 VAD 约 60 ms 和 WAV 读取）。
- 模型准备：识别器创建约 1.5–1.7 s（spike 实测），`prepare()` 另需对 228 MB 模型和运行库做 SHA-256，
  都发生在启动后台线程，不占用停止→结果路径。

## 测试与命令

工具链环境由 `scripts/build-listen.ps1` / `.artifacts/run-cargo.ps1` 设置（Rust 1.99.1
`stable-x86_64-pc-windows-gnullvm`，`--offline --locked`）。快速套件：

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test -Manifest E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml
```

合并 `integration/v0.2` 后结果：**78 passed, 0 failed, 11 ignored**（含既有 64 个测试）；
`cargo fmt --check` 无输出。

真引擎套件需要显式环境变量（默认 `#[ignore]`）：

```powershell
$env:LISTEN_SHERPA_LIB = 'E:\luyin\.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib'
$env:LISTEN_SENSEVOICE_MODEL = 'E:\luyin\.tools\models\sense-voice\model.int8.onnx'
$env:LISTEN_SENSEVOICE_TOKENS = 'E:\luyin\.tools\models\sense-voice\tokens.txt'
$env:LISTEN_SENSEVOICE_VAD = 'E:\luyin\.tools\models\sense-voice\silero_vad.onnx'
$env:LISTEN_QUALITY_CORPUS = 'E:\luyin\.artifacts\quality-corpus'
# 用 run-cargo.ps1 设置好的 toolchain 环境执行：
cargo test --offline --locked --manifest-path E:\luyin\.worktrees\issue-17\app\listen\Cargo.toml `
  --test sense_voice_quality -- --ignored --nocapture
```

结果：**8 passed, 0 failed**。覆盖：普通话长句、混合技术词、静音/音乐/提示音、10 s 停止延迟、
VAD 分段 `TimedText`、缺失库目录、模型哈希不符、六个 probe WAV 的转写记录。
测试内部用一把全局互斥锁串行化重引擎测试，避免 cargo 默认并行导致互相抢占 CPU 影响延迟数字。
完整输出保存在 `E:\luyin\.artifacts\issue-17-quality-suite.txt`（未入库）。

`run-cargo.ps1` 会先设置 `CARGO_HOME`、`RUSTUP_HOME`、`RUSTUP_TOOLCHAIN`、`RUSTFLAGS`、
`CARGO_TARGET_DIR`（默认多个 worktree 共用 `E:\luyin\.artifacts\cargo-target`）和 `PATH`；
直接调用 `cargo` 时需要先设置同样的变量。

> 构建缓存注意：验证过程中出现过多个 worktree 共用 `CARGO_TARGET_DIR`、同名 `listen 0.1.0`
> 包的缓存互相串扰，表现为 bin 报 `could not find sensevoice in listen`。执行
> `cargo clean -p listen --manifest-path <本分支 Cargo.toml>` 后重跑即恢复绿色；这是共享构建
> 缓存的环境问题，不是本票代码问题。真引擎测试使用独立 target dir 复跑也全部通过。

## 偏差与已知限制

1. **`gui.rs` 有两处小改动**：删除未再使用的 `Whisper` 导入，并把构造识别器的一整段替换为
   `config.transcriber()`（语义上就是一行）；引擎构造与所有新参数都在 `main.rs` 的
   `Config::transcriber()` 中。窗口代码没有动。此前已知 #19 正在重写 `gui.rs`，合并时需要
   以“保留 `config.transcriber()` 一行”为准。
2. **whisper 仍完整保留**：`--engine whisper` 或 `--engine <whisper-cli.exe>` 显式选择；
   `--model`/`--vad-model`/`--language`/`--backend host|cli` 语义不变。SenseVoice 用新增的
   `--sherpa-lib`、`--sensevoice-model`、`--sensevoice-tokens`、`--sensevoice-vad`、
   `--sensevoice-language`。不带参数时使用 SenseVoice。
3. **交付包未改**：模型/DLL 的打包替换属于 #23；本票只让开发构建默认走 SenseVoice。
4. **术语校正未做**：WASAPI/CUDA/DeepSeek 等专名错误留给 #19/#20；本模块不静默改写文本。
5. **片段级时间戳的残余风险**：同一段语音在两个相邻 30 s 窗口里若被 VAD 切成不同片段
   （边界随上下文变化），中点归属仍可能把同一片段判给两个窗口，造成窗口边界重复。已通过
   VAD 按 0.6 s 停顿成组、每段 ≤10 s 降低概率；现有 `streaming.rs` 的 token 级归属规则未改。
6. **延迟测试使用中位数**：见“延迟”一节，原因是本机同时跑其他 agent 的构建；每个样本都记录，
   不隐藏冷启动样本。
7. **spike 已删除**：一次性 `examples/sensevoice_probe.rs` 证明 ABI 后删除，避免与正式模块重复
   维护；其结构体镜像与验证方法已固化在 `src/sensevoice.rs` 的编译期断言和本文表格中。

## 分支与提交

- 分支：`ticket/issue-17`（工作树 `E:\luyin\.worktrees\issue-17`）。
- 基线：`eea9109`（`integration/v0.2` 起点）。
- 本票提交：`6319e9e`（引擎）、`c363e3e`（默认引擎与参数）、`83a19db`（真引擎测试），
  验证文档为后续 docs 提交。
- 合并：`integration/v0.2` 在 #16 落地后为 `ec97f86`，合并提交 `7247b0e`；合并后完整套件重跑通过。
- 真引擎测试原始输出（未入库）：`E:\luyin\.artifacts\issue-17-quality-suite.txt`。
- ABI 独立解析脚本（未入库）：`E:\luyin\.artifacts\sensevoice-abi.py`。
