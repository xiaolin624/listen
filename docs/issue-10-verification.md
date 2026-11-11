# Issue #10：硬件加速、准备状态与端到端延迟优化

2026-11-12。工作树 `E:\luyin\.worktrees\issue-10`，分支 `implement/issue-10-latency`，基线 `b006dd1`（集成分支，含 #3–#9）。主工作树未改动。本文只记录实际执行过的测量；未运行的场景明确标为未覆盖。

## 结论摘要

- 采用**进程内常驻模型**：`listen.exe` 直接加载同一个 `whisper.dll` 和 `ggml-small.bin`，每个窗口创建新的 `whisper_state` 后解码。模型只加载一次，没有 `whisper-server`，没有 localhost 连接，也没有云端回退；识别结果与现有 `whisper-cli.exe` 路径**逐字一致**。
- 真实采集（受控原生 EDIT 目标、固定 WAV、默认渲染设备）上，6 个语音样本的停止到交付为 **6863–7470 ms（中位 7294 ms）**，CLI 路径为 **7685–8758 ms（中位 8115 ms）**；降低量 468–1417 ms，平均 924 ms。差异来自每窗 `whisper-cli` 进程启动与模型加载被消除，而不是改模型、改解码参数或降低识别质量。
- 官方 `whisper-server.exe` 已评估并**拒绝采用**：第一个请求与 CLI 逐字一致，但同一进程的后续请求结果改变；即使每次请求都发送 `no_context=true`，`multiline` 仍会丢句、`mixed-technical` 标点改变。它不满足“逐字一致”，因此没有进入正式路径。证据见 `.artifacts\issue-10\` 下的服务端探测记录。
- 首次准备是一个显式的 `Preparing` 阶段（实测 570–712 ms）。准备期间 `Start` 被拒绝，界面/`status.txt` 显示“ Preparing the local recognition model…”，准备完成后才进入 `Ready`；不存在“模型还没加载却显示就绪”的状态。
- GPU 未使用：本机有 RTX 3050 Laptop 4 GiB、驱动 546.33，但没有 CUDA toolkit，`.tools` 中没有 `ggml-cuda.dll`/`cudart`。常驻模型显式 `use_gpu=false`，whisper 日志为 `no GPU found`。本文不声称 GPU 加速已实现。
- 停止到结果的拆分已经落到 `status.txt` 和 `latency.jsonl`：准备、收尾、积压等待、最后窗口推理、保存、目标交付分别记录。
- 10 分钟（600 秒）持续处理测量见下文；**没有**把 10 分钟冒充 60 分钟。60 分钟的真实回放基线仍引用 #6 的记录，用于对照而不是本轮新测。
- 没有新增固定延迟 SLA，没有用“精度换速度”的配置；窗口大小、重叠、模型、语言、线程数、VAD 阈值全部沿用 #9/#6 的已确认配置。

## 实现

### 1. 常驻模型（`app/listen/src/whisper_host.rs`）

- 从 `whisper-cli.exe` 同目录加载 `whisper.dll`；只使用本机 DLL 与模型文件，不建立 socket、不启动服务进程、不导入网络 API。
- `whisper_init_from_file_with_params_no_state` 加载模型一次；每次识别调用 `whisper_init_state` 创建**新的 decode state**，调用 `whisper_full_with_state`，然后 `whisper_free_state`。这避免了 `whisper-server` 复用同一 state 带来的请求间污染。
- 参数与现有 CLI 完全一致：`beam_size=5`、`best_of=5`、`temperature=0`、`temperature_inc=0.2`、`no_speech_thold=0.6`、`entropy_thold=2.4`、`logprob_thold=-1`、`no_timestamps=true`、`no_context=true`、CPU 6 线程、`zh`。流式窗口额外使用 token 级时间戳（等价于 CLI 的 `-ojf`），并复用 `whisper_timing::build` 做 UTF-8 片段拼合与重叠归属。
- `whisper_full_params` 按 v1.8.2 头文件逐字段镜像；编译期断言 64 位下大小为 304 字节，运行期在加载 DLL 后校验 `whisper_full_default_params` 的已知默认值（strategy、`n_max_text_ctx=16384`、`beam_size=5`、`thold_pt≈0.01`、`temperature_inc=0.2`、`grammar_penalty=100`）。不一致时拒绝使用常驻模型并回退 CLI，而不是猜测 ABI。
- 为避免 DLL 在进程退出阶段被卸载后访问已卸载代码，`ModelHost` 在 `whisper_free` 后**不调用 `FreeLibrary`**；模块由操作系统在进程结束时回收。这是有意的生命周期取舍，不影响识别结果。

### 2. 回退路径（`whisper.rs`）

- `Whisper::prepare()` 在启动时后台加载常驻模型；成功时返回“模型已驻留、每窗新 state”的状态说明。
- 加载失败（DLL 缺失、导出缺失、布局不匹配、模型损坏）时**不阻止采集**：`check_ready()` 已验证 CLI/VAD/模型文件可用，`prepare()` 返回明确的 CLI 回退说明，界面与 `status.txt` 显示 `Backend: … CLI …`，每个窗口走原 `whisper-cli.exe` 路径，音频不丢失。
- 常驻模型在运行中失败时，当前窗口与后续窗口改用 CLI；`backend_note` 把失败原因写入状态，不静默切换，不把失败结果冒充成功。
- `--backend cli` 可用于复现优化前路径；默认 `host`。

### 3. 准备/就绪/采集/处理状态

- 新增 `Phase::Preparing`。`Application::new` 只在 `check_ready()` 通过后调用 `prepare()`；有准备任务时进入 `Preparing`，不进入 `Ready`。
- `Preparing` 期间 `Start`/`Toggle`/`Retry`/`CleanupAudio` 都返回“仍在准备”的明确说明，不启动采集、不显示已采集。
- 准备完成才进入 `Ready`，并把准备耗时写入 `timing.preparation_ms`。首次准备与后续使用在 `status.txt`/`latency.jsonl` 中可区分。
- GUI 的按钮、托盘提示、`status.txt` 都显示准备状态；准备完成后显示实际 backend。

### 4. 停止到结果拆分（`Timing`）

`Snapshot.timing`、`status.txt` 和运行目录的 `latency.jsonl` 记录：

| 字段 | 含义 |
| --- | --- |
| `preparation_ms` | 首次模型准备（启动到 Ready） |
| `stop_to_result_ms` | 从 `dispatch(Stop)` 到结果保存/交付完成（不含 GUI 下次刷新，也不含外部 `--control stop` 进程启动） |
| `finalize_ms` | 停止采集、finalize 最后一个 WAV 的时间 |
| `recognition_ms` | 流式路径中等待识别线程结束的时间（积压 + 最后窗口）；非流式为整次识别 |
| `last_window_wait_ms` | 最后窗口在队列中等待的时间（积压） |
| `last_window_inference_ms` | 最后窗口的实际本地解码时间 |
| `save_ms` | 写 `current-result.txt` |
| `delivery_ms` | 目标追加/回退文本框交付 |
| `recognition_windows` | 本次会话识别的窗口数 |

`latency.jsonl` 每完成一次采集追加一行，包含 phase、backend、delivery、文本长度和上述字段。它不替代音频/目标验收，只用于拆分。

## 识别质量对照（#9 固定语料）

真实设备采集、同一 `ggml-small.bin`、Silero、CPU 6 线程、相同 WAV。`host` 与 `cli` 使用相同的受控原生 EDIT 目标，逐字比较写入目标的新增文本：

| 样本 | host 输出 | cli 输出 | 逐字一致 |
| --- | --- | --- | --- |
| mandarin | `這段語音只在本機處理,不會上傳到雲端。請保留完整的發言內容,由我檢查以後手動發送。` | 同左 | 是 |
| mixed-technical | `我们用"Rust"实现音频采集 通过"Waze up high loop back"获取声音模型在本纪运行 使用"CPU"和"Cuda"进行对照` | 同左 | 是 |
| english-numbers | `The API uses Rust, Windows, Whisper, CPU, and GPU.Version 1.8.2.Port 8080.Retry 3 times, then wait 2 seconds.` | 同左 | 是 |
| homophones | `请确认本机的音频设置。本机的项目已经完成。不要把本机写成本机。` | 同左 | 是 |
| multiline | `第一段,保留原來的草稿,不要自動發送。第二段,覆蓋 API 回應。第三段,端口是 8080,重視三次,等待兩秒。` | 同左 | 是 |
| silence / instrumental / notification | 空（`NoSpeech`） | 空（`NoSpeech`） | 是 |

原始对照：`.artifacts\issue-10\quality-comparison.json`、`live-latency\live-host.json`、`live-latency\live-cli.json`。模型/引擎/DLL 哈希也在该 JSON 中。识别文本保留了 #9 已记录的同音/术语错误，没有用润色或提示词掩盖。

另做了一次 **96 秒、4 个流式窗口**的 host/CLI 对照（`long_session.exe`，同一 `loop-96s.wav`、同一模型/参数）：两边最终 `current-result.txt` 均为 203 字，**逐字相同**，`recognized_segments=4`、峰值积压 1。它覆盖了跨窗口重叠归属，而不只是单窗口。证据：`.artifacts\issue-10\loop96\comparison.json`。

### 为什么没有采用 `whisper-server.exe`

在相同 DLL/模型/参数下，向 `whisper-server` 的 `/inference` 依次发送同一批 WAV：

- 每个**新启动** server 的第一个请求与 CLI 逐字一致；
- 同一 server 的第二个请求开始出现差异。`multiline` 的第二个请求把“第二段,覆蓋 API 回應”变成英文或整句丢失；`mixed-technical` 的标点从空格变成逗号；
- 显式发送 `no_context=true` 并清空上下文后仍复现。原因是 server 复用同一个 decode state，与“每窗独立进程”的 CLI 语义不同。
- `/load` 可以重置，但会重新加载模型，等于放弃模型复用；因此没有把 server 放进正式路径。

该评估证明“server 可用/更快”不足以采用；必须同时满足逐字一致。原始探测在 `.artifacts\issue-10\`，正式实现不包含 HTTP 客户端或 loopback 连接。

## RED → GREEN

新增 `app/listen/tests/latency_and_preparation.rs`，仍只经过 `Application::dispatch`/`poll` 业务边界，外部依赖用受控 fake。为了不只给“最终通过”，下面记录实际观察到的失败；准备/回退的 RED 是在保留新 API 的前提下临时把行为回退到旧实现后运行的，不是事后编造的数字。

1. **准备状态 RED**：临时让 `Application::new` 忽略 `prepare()`（仍保持 API 与编译），运行同一测试，实际失败：
   ```text
   thread 'preparation_is_visible_and_start_is_refused_until_the_model_is_ready' panicked:
   assertion `left == right` failed: Snapshot { phase: Ready, ... }
     left: Ready
    right: Preparing

   thread 'a_failed_preparation_is_reported_and_never_claims_ready' panicked:
   expected Failure, observed Snapshot { phase: Ready, ... }
   ```
   恢复 `prepare()` 的 `Preparing` 分支与 `Start` 守卫后 GREEN。证据：`.artifacts\issue-10\red-preparation.txt`。
2. **计时 RED（实际运行期失败）**：`stop_to_result_reports_finalize_inference_save_and_delivery_separately` 首次运行失败，`recognition_ms: Some(0)`，因为非流式路径把 `recognition_ms` 设在识别之前；测试输出为
   ```text
   Timing { preparation_ms: None, stop_to_result_ms: Some(61), finalize_ms: Some(0),
            recognition_ms: Some(0), last_window_inference_ms: Some(60), ... }
   ```
   修正为在非流式 `transcribe` 完成后覆盖 `recognition_ms`，测试 GREEN。
3. **后端回退 RED**：临时让 `ready_detail` 忽略 backend note，运行测试，实际失败：
   ```text
   thread 'an_explicit_backend_fallback_is_visible_in_ready_and_in_the_snapshot' panicked:
   the ready detail must name the fallback: Snapshot { phase: Ready, detail: "Ready. ...",
     backend: Some("CLI fallback: the resident model is unavailable; each window reloads it.") }

   thread 'preparation_is_visible_and_start_is_refused_until_the_model_is_ready' panicked:
   Snapshot { phase: Ready, detail: "Ready. ...", timing: Timing { preparation_ms: Some(0), ... } }
   ```
   恢复 `ready_detail` 后 GREEN。证据：`.artifacts\issue-10\red-fallback.txt`。
4. **GREEN 全量**：普通 `-Task Test` 为 **44 passed / 1 ignored**（`capture_to_copy` 27、`device_routing` 4、`latency_and_preparation` 4、`long_sessions` 9；桌面编辑器测试按原约定 ignored）；显式本地模型 `-Task Quality` 为 **2 passed**。合计 **46 passed / 1 ignored**。

## 真实采集延迟

驱动：`.artifacts\issue-10\live-latency.ps1`。它创建一个任务自有的原生 Win32 多行 EDIT（不是向 Listen 传句柄），把焦点放在该 EDIT 上；用 `System.Media.SoundPlayer` 在默认渲染设备播放 #9 的固定 WAV；用正式 `--control start/stop` 驱动 Listen。`stop_to_result_ms` 来自应用内部（Stop dispatch 到保存+交付完成），外部 stopwatch 仅作旁证；`--control stop` 每次会新起一个控制进程，约 0.9 秒，热键路径没有这部分。

同一台机器、同一构建、同一目标、同一 WAV：

| 样本 | host 停止→结果 (ms) | cli 停止→结果 (ms) | 降低 (ms) | host 最后窗口推理 (ms) | cli 最后窗口推理 (ms) | 文本一致 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| mandarin（首次采集） | 7246 | 7714 | 468 | 7228 | 7698 | 是 |
| mandarin（后续采集） | 7368 | 8287 | 919 | 7342 | 8267 | 是 |
| mixed-technical | 7470 | 8481 | 1011 | 7450 | 8460 | 是 |
| english-numbers | 7032 | 7685 | 653 | 7017 | 7669 | 是 |
| homophones | 6863 | 7942 | 1079 | 6802 | 7924 | 是 |
| multiline | 7341 | 8758 | 1417 | 7310 | 8735 | 是 |
| silence | 128 | 162 | 34 | 120 | 155 | 是 |
| instrumental | 135 | 138 | 3 | 130 | 131 | 是 |
| notification | 115 | 114 | -1 | 109 | 104 | 是 |

- host 首次采集前的一次性准备为 **596 ms**（`preparation_ms`），发生在 `Ready` 之前，不计入上表的停止→结果。
- 6 个语音样本的降低量为 468–1417 ms，平均 924 ms。降低来自 CLI 每窗的进程启动和模型加载；`last_window_inference` 的差值就是这部分开销。
- 交付本身很快（目标追加 7–14 ms）；`finalize` 为 10–29 ms。本次不存在积压（`last_window_wait_ms=0`，`recognition_windows=1`），所以表中没有积压项；积压只在 30 秒窗口/受压场景出现。
- 非语音样本的 host/cli 差异在噪声内（115–162 ms），因为两者都只运行 VAD 后返回空结果；这是预期行为。
- 外部 stopwatch 对语音样本比应用计时多约 0.12–0.24 秒（控制进程启动与 50–100 ms 状态轮询）；对非语音样本多约 0.95–1.01 秒。因此本文以应用内部计时作为“停止操作到结果出现”的主数字，并保留外部值作对照。
- 另一次**非受控目标**的最终构建 smoke（焦点是浏览器编辑器，不是任务自有 EDIT）得到停止→结果 11105 ms，其中识别 7621 ms、交付 3476 ms。它说明复杂目标/大文本的交付可能成为最大项；该次不计入上面的受控对照表，也不能把 3476 ms 当成所有目标的固定交付成本。

30 秒窗口（静默墙钟回放，`long_session.exe`，无桌面交付）：

| 指标 | host | cli | 差值 |
| --- | ---: | ---: | ---: |
| 停止→结果 | 9050 ms | 9975 ms | -925 ms |
| 最后窗口推理 | 9040 ms | 9980 ms | -940 ms |
| 准备 | 573 ms | 无 | — |
| 文本长度 | 109 字 | 109 字 | 一致 |

这说明窗口越长，单窗模型加载占比越小，但 30 秒窗口仍有约 0.9 秒的稳定节省。

96 秒、4 窗口的流式对照（同一 `loop-96s.wav`）：

| 指标 | host | cli | 差值 |
| --- | ---: | ---: | ---: |
| 识别窗口 | 4 | 4 | — |
| 停止→结果 | 5847 ms | 6605 ms | -758 ms |
| 最后窗口推理 | 5840 ms | 6598 ms | -758 ms |
| 峰值积压 | 1 | 1 | — |
| 最终文本 | 203 字 | 203 字 | **逐字一致** |

多窗口下节省仍约 0.76 秒（11.5%），与 30 秒单窗同一量级；跨窗口重叠归属没有因为换后端而改变。

## 首次与后续使用

- host：首次采集 7246 ms，后续采集 7368 ms；首次没有额外的 per-window 模型加载，只有启动前 596 ms 的准备。后续差异属于同机测量噪声。
- cli：首次采集 7714 ms，后续采集 8287 ms；每个窗口都新起进程并加载模型，因此后续不会变快。
- 因此“首次使用”的主要代价是准备（0.6 秒，且准备期间不能采集）；“后续使用”的主要代价是最后窗口推理（约 6.8–7.5 秒，11 秒样本）。“准备完成后首次识别”没有出现额外的冷启动惩罚。

## 长会话（600 秒）

`measure-long-session.ps1 -InputWav ...loop-96s.wav -Seconds 600 -Backend host`，2026-11-12，`E:\luyin\.artifacts\issue-10\long-host-600\`。输入仍是 #6 的 96.04 秒固定样本循环，SHA-256 `9F71068965D8EC09351CBEFACD77B463CE72980484A1216EA39970AB94EA1E3F`；small 模型/Silero 哈希与 #9 一致；600 秒按墙钟回放，**不是** 60 分钟。

| 观测 | 结果 |
| --- | --- |
| 最终阶段 | `Result`（`terminal: true`） |
| 采集时长 | 600.0 秒（请求 600 秒，无截断、无自动停止） |
| 识别窗口 | 22 |
| 当前结果 | 1433 字（本 harness 没有选目标，走备用文本框） |
| 准备 | 757 ms（启动到 Ready） |
| 停止到结果 | **6797 ms**（harness 外部记 6813 ms） |
| 收尾 `finalize_ms` | 0 ms |
| 识别 `recognition_ms` | 6795 ms |
| 最后窗口排队等待 / 推理 | 0 ms / 6791 ms |
| 保存 / 交付 | 0 ms / 0 ms（无目标） |
| 峰值积压窗口 | **1** |
| 结束 spool | 0 字节 |
| 失败与保留音频 | 无；`retained_audio` 为空 |

资源（`resources.ndjson`，每 3 秒采样本进程及直接子进程，共 187 个样本）：

| 指标 | 峰值 | 平均 |
| --- | ---: | ---: |
| 进程组合工作集 | **831 MB** | 574 MB |
| 进程组合私有字节 | **1336 MB** | 703 MB |
| `long_session` 自身累计 CPU | 982 秒 / 600 秒（约 1.64 核） | — |
| 直接子进程 | `conhost`、`vad-speech-segments`；**无 `whisper-cli`** | — |
| GPU 全局显存（非本进程归因） | 713 MiB / 4096 MiB | 687–713 MiB |
| GPU 全局利用率（其他桌面应用） | 29% | 15–29% |

- 内存不是单调增长：每次窗口会临时分配约 780 MB 的 decode state，识别后释放；观测到私有字节在约 485 MB 与约 1270 MB 之间周期性变化。模型常驻本身约 500 MB，因此**平均内存高于 CLI 路径**（#6 的 60 分钟 CLI 回放平均工作集 183 MB、私有 299 MB，峰值 789 MB/1268 MB；其采样会漏掉短命的 `whisper-cli` 子进程，峰值是下界）。常驻模型的取舍是：用约 0.5 GB 常驻内存换取每窗不再加载模型；峰值与 CLI 同量级。
- 22 个窗口、峰值积压 1、spool 峰值来自窗口 WAV；没有完整音频堆内存，`buffered_samples` 恒定在 2 秒重叠上限 32,000。
- 本机 GPU 未被 whisper 使用；上表的 GPU 数字是 `nvidia-smi` 的全设备读数，不能当作 Listen 的显存占用。whisper 日志为 `no GPU found`，常驻模型显式 `use_gpu=false`。
- 与 #6 的 60 分钟 CLI 收尾 9.55 秒不是同一时长，不能直接当成“60 分钟已改善”；本轮的同一 30 秒窗口对照是 9.975 秒 → 9.050 秒，600 秒会话收尾 6.797 秒（最后窗口更短）。
- “同时开会”的流畅度：**没有真实会议软件**。另做了一次 180 秒的受控并发代理：真实采集、真实 WASAPI loopback、默认渲染设备播放 96 秒固定音频循环，同时运行 4 个 CPU 忙等进程（每个约 181 秒 CPU/180 秒），目标仍是任务自有原生 EDIT。结果为 `Result`，停止→结果 3385 ms（`last_window_wait_ms=3085`、最后窗口推理 263 ms；最后窗口在 Stop 前已开始，等待时间是积压，不是变快）；180 秒内采样到 1644 次状态刷新，刷新间隔中位 124.1 ms、最大 158.8 ms，Listen 工作集 510 MB，GPU 全设备 722 MiB。这只说明**共享 CPU/音频负载下 GUI 状态刷新和收尾没有长时间卡死**，不能替代真实会议软件（视频解码、网络、会议 UI、共享屏幕等）的流畅度验收。证据：`.artifacts\issue-10\concurrent-proxy\concurrent-proxy.json`。

## GPU 不可用时的处理

- 硬件：NVIDIA GeForce RTX 3050 Laptop GPU，4096 MiB，驱动 546.33。
- 本机没有 CUDA toolkit（无 `nvcc`），`.tools\whisper-v1.8.2-cpu\bin\Release` 没有 `ggml-cuda.dll`/`cudart`；whisper 初始化日志为 `whisper_backend_init_gpu: no GPU found`，实际 backend 为 CPU。
- 常驻模型显式 `use_gpu=false`，与现有 `-ng` 一致；不以“有 GPU”声称加速。
- 如果 DLL/模型布局不匹配或常驻模型不可用，回退 CLI 并显示原因；不会静默切云，也不会丢失音频。

## 离线与网络

- 正式实现没有引入 `whisper-server`，因此 `listen.exe` **没有 loopback 连接**，也没有外部网络回退。
- 新构建的 `listen.exe` PE 导入表只有 `KERNEL32`、`ntdll`、`user32`、`gdi32`、`ole32`、`oleaut32`、`propsys`、`combase`、`shell32`、`libunwind` 和 CRT；没有 `ws2_32`/`winhttp`/`wininet`/`urlmon`/`dnsapi`/`mswsock`，也没有 socket/connect/http 导入（`.artifacts\issue-10\pe-imports.txt`）。
- 运行期加载的 `whisper.dll`/`ggml*.dll` 来自本机 `.tools`；没有服务端进程。

## 已知限制与未覆盖

- **10 分钟不是 60 分钟**：本轮按任务要求做 600 秒；60 分钟的真实时长结论仍来自 #6 的 60 分钟回放，不替代真实耳机 60 分钟。
- **“同时开会”**：没有实际加入会议软件。10 分钟长会话在后台桌面、无其他交互负载下测得；另有 180 秒并发 CPU/音频代理（见上），但不是会议软件的视频/网络/UI/共享屏幕负载，不能声称真实会议下流畅度已验证。
- **GPU**：没有 CUDA 工具链，未编译/运行 GPU 版本；GPU 路径完全未验证。
- **常驻模型 ABI**：结构体/导出按 v1.8.2 头文件镜像，并在运行期校验默认值；如果替换了不同版本/不同构建的 `whisper.dll`，可能拒绝并回退 CLI。没有对所有 whisper.cpp 构建做兼容性测试。
- **DLL 生命周期**：退出时不 `FreeLibrary`，以避免已观察到的访问冲突；模块由 OS 回收。若未来需要在运行中反复加载/卸载，需要单独验证。
- **回退路径**：启动时 DLL 缺失/布局不符会显示 CLI 回退（`prepare` 失败 → `Ready` + 明确 fallback note，有 fake 测试）；运行中 `whisper_full` 失败 → 当前窗口改走 CLI 的恢复**只有代码路径**，没有做真实故障注入；没有在真实设备上故意制造该故障。
- **目标交付**：真实采集使用任务自有原生 EDIT；没有覆盖所有编辑器/浏览器输入框。`delivery_ms` 随目标实现变化，不能当成固定 SLA。
- **未覆盖**：物理拔插设备、会议软件独占模式、蓝牙免提/立体声切换、其他 Windows 版本、真实会议录音的分段边界质量。

## 复现命令

```powershell
cd E:\luyin\.worktrees\issue-10
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat
.\scripts\verify-local-quality.ps1 -ToolsRoot E:\luyin\.tools
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Build
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task BuildHarness

# 真实采集（需默认渲染设备；控制原生 EDIT 目标；会播放声音）
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File `
  E:\luyin\.artifacts\issue-10\live-latency.ps1 -Backend host
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File `
  E:\luyin\.artifacts\issue-10\live-latency.ps1 -Backend cli

# 10 分钟持续处理；分数按实际 600 秒，不冒充 60 分钟
.\scripts\measure-long-session.ps1 -InputWav E:\luyin\.artifacts\issue-6-smoke\loop-96s.wav `
  -OutputDirectory E:\luyin\.artifacts\issue-10\long-host-600 `
  -ToolsRoot E:\luyin\.tools -Seconds 600 -Backend host
```

原始证据在 `.artifacts\issue-10\`：`live-latency\live-host.json`、`live-latency\live-cli.json`、`quality-comparison.json`、`long-host-600\`、`window30\`、`pe-imports.txt`。这些目录和测量脚本不入库。
