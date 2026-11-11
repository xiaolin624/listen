# Issue #7 实现与验证记录：识别失败后重试及临时音频生命周期

范围：[T6：识别失败后重试及临时音频生命周期](https://github.com/xiaolin624/listen/issues/7)。工作树 `E:\luyin\.worktrees\issue-7`，分支 `implement/issue-7-retry`，开始基线为集成分支 `integration/issue-3-native-target` 的 `76125dc`（含 #3 原生目标、#4 粘贴降级、#5 写入未确认诊断、#6 分段长会话）。实现提交 `462f638`。未推送、未关闭 issue。

一次本地识别失败后，应用保留本次采集音频，进入可理解的失败状态，并提供明确的“重试本地识别”和“清理失败音频”操作。重试只重新运行本地识别，并沿既有交付逻辑投递给采集开始时绑定的原目标；没有有效原目标时进入备用文本框，不读取重试时的当前焦点。重试成功清理对应临时音频；重试失败继续保留同一份音频。用户清理后，后续重试不会调用识别或产生结果。重试不重放发言、不静默改用云端、不写会议历史。

## 业务边界

沿用已确认的单一主要测试边界：`Application::dispatch(Control)`（Start / Stop / Toggle / Retry / CleanupAudio）、`poll`、`Snapshot`（阶段、文本、交付方式、`retained_audio`、`retry_available`）以及完整流程中的临时音频生命周期。外部播放设备、本地转写器和目标编辑区是可替换依赖；重试、音频保留/清理、原目标保持和交付复用均为生产代码。没有为内部函数、线程数或临时文件命名新增测试入口。

重试与首次识别走同一份生产交付函数 `deliver`：保存当前结果、读取原目标最新草稿、一次写入并核对。重试不会再次选择目标，也不会在写入未确认后自动再追加一次。

## RED → GREEN

新增测试按纵向切片加入，只在核心切片上观测到真实 RED。后续切片建立在同一重试状态机上，首次运行即通过；这里如实记录，不虚构 RED。

| 新增行为测试 | 实际 RED / 首次结果 | GREEN |
| --- | --- | --- |
| `failed_recognition_offers_retry_on_the_retained_audio_and_cleans_it_after_success`（失败后提供重试，用同一音频恢复并清理） | 先观测到编译 RED：`no field retry_available on type Snapshot`、`no variant Retry found for enum Control`。补最小 API 后观测到行为 RED：`Snapshot { phase: Failure, detail: "whisper.cpp process could not start: missing runtime DLL. Audio retained for local recovery: ...\audio.wav", text: "", retained_audio: [...], delivery: None, retry_available: false }` | 1 passed。断言重试音频路径与首次失败完全一致、两次识别调用、成功后 `retry_available=false`、音频文件与 session 目录均删除、当前结果文件一致 |
| `a_retry_that_fails_again_keeps_the_same_audio_retryable_until_it_succeeds`（连续失败后恢复） | 首次直接通过（同一状态机已覆盖），未虚构 RED | 通过。第二次失败 detail 含 `Local retry failed`，`retained_audio` 仍是同一路径；处理中再次 Retry 只提示 `Still processing`；第三次成功 |
| `retry_waits_for_the_local_condition_to_be_repaired_and_then_recovers_the_same_audio`（本地条件修复后重试） | 首次运行前有一次编译失败（缺少 `AtomicBool` 导入），不是行为 RED；修正导入后首次直接通过 | 通过。`check_ready` 失败时重试报 `Local model is missing` 且识别调用数不增加、音频保留；修复后同一音频成功 |
| `retry_delivers_to_the_target_bound_at_capture_start_not_to_later_focus`（重试仍投给原目标） | 首次直接通过 | 通过。重试向采集开始时的 0 号编辑框追加恰好一次，停止/重试时焦点的 1 号框逐字不变；`replace_calls == 1`；待处理失败音频期间 Start 被拒绝且状态说明原因 |
| `retry_without_a_valid_original_target_uses_the_fallback_not_the_retry_time_focus`（无有效原目标） | 首次直接通过 | 通过。结果进入备用文本框（`Delivery::Fallback`），detail 含 `No supported editable target`，两个编辑框都未被写入，`replace_calls == 0` |
| `cleaning_failed_audio_updates_the_state_and_prevents_invalid_retries`（明确清理） | 首次直接通过 | 通过。清理后 `retry_available=false`、`retained_audio` 为空、音频文件删除、状态说明 `deleted at your request`；之后 Retry 不增加模型调用、保持 `Phase::Failure`、提示 `No retained audio` |
| `unconfirmed_retry_write_keeps_one_fallback_and_later_controls_never_append_it_again`（写入未确认不重复追加） | 首次直接通过 | 通过。重试写未确认时 result 为 `Delivery::Fallback`、目标只有一份追加、`replace_calls == 1`；随后 Retry/Stop 不再追加、不再识别 |
| `streaming_failure_retries_the_whole_retained_capture_without_repeating_the_overlap`（分段采集的整体重试） | 首次直接通过（合并逻辑与重试状态机同批实现，未虚构 RED） | 通过。两个保留窗口重新拼合后的整段时长是 2000 samples，而不是含重复重叠的 2320；重试成功清理全部临时音频 |

回归：`tests/capture_to_copy.rs` 原有目标绑定、无目标、只读/禁用、截断、并发编辑、未确认写入、保存失败等测试全部保持通过；`tests/long_sessions.rs` 原 8 项全部通过（新增后 9 项）。

## 验收条件对应证据

| 验收条件 | 实现与证据 |
| --- | --- |
| 模型/后端失败保留本次音频，显示可理解的失败状态和本地重试 | 识别失败时，未消费任何窗口的完整采集（单文件或流式全部窗口）被登记为可重试；`Snapshot.retry_available=true`，`retained_audio` 列出实际 WAV；detail 保留底层错误并说明“重试识别同一音频，无需重放；原目标保留，失效则用备用文本框”。GUI 失败时自动显示，启用“重试本地识别”；托盘失败提示也说明音频已保留。自动测试 1、真实场景 1 均观测到。 |
| 修复本地可用条件后重试使用同一份音频；不要求重放，不静默用云端 | `Retry` 只取已保留的 `CapturedAudio`，先再次 `check_ready`，再调用同一 `LocalTranscriber::transcribe`。生产代码没有任何云端/备用识别器。自动测试 1 断言两次 `transcribe` 的输入路径完全相同；测试 3 断言模型不可用时不会调用识别；真实场景用假引擎失败两次，替换为真实 whisper.cpp 后重试得到完整结果，全程未重新播放。 |
| 重试仍对应原采集目标；无有效原目标时用备用文本框，不投给重试时焦点 | 采集开始时绑定的 `Box<dyn Target>` 与音频一起保存在失败状态里，重试直接把该对象交给既有 `deliver`。自动测试 4 在重试时让另一个编辑框获得焦点，结果仍只写原目标；测试 5 在开始时无目标、重试前把焦点切到有效输入框，结果仍进备用文本框且不写任何控件。真实场景 1 在另一个原生编辑框有焦点时重试，原目标追加一次、其他框不变。 |
| 重试失败仍保留可重试音频；成功清理临时音频，当前结果仍可查看复制 | 重试失败把同一 `RetainedCapture` 放回状态，`retry_available` 保持 true；测试 2 和真实场景 1 的失败重试验证了同一路径音频仍在。成功后 `remove_dir_all` 删除该 session，清空 `retained_audio`，`Snapshot.text` 与 `current-result.txt` 保存完整文本，GUI 的“复制完整结果”仍启用（真实截图可见）。 |
| 用户明确清理失败音频后状态更新，不再执行无效重试 | `Control::CleanupAudio`（GUI“清理失败音频”/托盘/`--control cleanup`）删除保留 session，置 `retry_available=false`，detail 说明已按用户要求删除、不会重试、当前结果不变。测试 6 与真实场景 2 均验证随后 `Retry` 不调用模型、不改变当前结果、目标不变。 |
| 不自动生成会议历史/长期归档/云端副本；失败重试数据不与其它采集混淆 | 除既有单份 `current-result.txt` 外不写任何历史/归档文件，没有任何网络客户端。失败音频、原目标和描述保存在独立的 `RetainedCapture` 中；待处理期间新的 `Start` 被明确拒绝（测试 4），因此不可能与其他采集的目标、音频或当前结果混合。真实验收后当前结果仍是唯一一份，临时 WAV 计数为 0。 |
| 写入未确认时不因重试重复追加；覆盖首次失败、连续失败、恢复成功和用户清理 | 只有识别失败才产生可重试状态；写入未确认属于 `Phase::Result`，不提供重试且不会自动重写。重试成功后的写入若未确认，也只保留备用结果并清除重试状态。测试 7 验证重试写未确认后进一步 Retry/Stop 不再追加，`replace_calls == 1`；真实场景 1 的原目标文本恰为“原草稿 + 换行 + 转写”一份。首次失败、连续失败、恢复成功、清理四条路径分别由测试 1、测试 2/3、测试 1/2/3/4、测试 6 以及真实场景 1/2 覆盖。 |

## 真实 Windows 桌面验收

2026-11-12（Asia/Shanghai），被测 release 由提交 `462f638` 的源码构建：`E:\luyin\.worktrees\issue-7\app\listen\target\release\listen.exe`。脚本只启动和操作本次创建的 Listen 进程与本次创建的原生 EDIT 测试窗口，退出时两者正常关闭。

```powershell
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File `
  E:\luyin\.artifacts\issue-7-acceptance\accept-retry.ps1 `
  -Binary E:\luyin\.worktrees\issue-7\app\listen\target\release\listen.exe
```

环境：Windows 11、默认多媒体播放设备、真实 WASAPI loopback、whisper.cpp v1.8.2 CPU、`ggml-small.bin`、Silero `ggml-silero-v5.1.2.bin`、中文模式、6 线程。目标为真实焦点选定的原生 Win32 多行 EDIT（未向 Listen 传入句柄）。故障用自建的 `whisper-cli.exe` 假引擎制造：`--help` 返回 0 使启动就绪检查通过，真正识别时退出码 2；同目录保留真实的 `vad-speech-segments.exe`。修复条件是把真实 `whisper-cli.exe` 覆盖回同一路径，不重启应用。

| 真实路径 | 观察结果 |
| --- | --- |
| 真实点击原目标、系统快捷键开始、播放已知 8.08 秒语音、切到另一窗口的原生编辑框后停止（假引擎） | `Phase::Failure`，detail 为 `Local whisper.cpp recognition failed (exit code: 2)` 并含保留音频路径；`retry_available=true`；保留 1 个 WAV；`current-result.txt` 未写入；原目标和当前焦点框文字均未改变。截图 `retry-failure.png` 目视检查到状态文字、“重试本地识别”和“清理失败音频”按钮可用、“复制完整结果”禁用。 |
| 焦点仍在另一个原生编辑框时点击“重试本地识别”（假引擎尚未修复） | 再次 `Phase::Failure`，detail 为 `Local retry failed: Local whisper.cpp recognition failed (exit code: 2)`；同一路径 WAV 仍保留，`retry_available=true`；原目标仍未被写入。 |
| 覆盖为真实 whisper-cli 后，焦点停在另一个编辑框时再次点击“重试本地识别” | 不需要重新播放，约数秒后 `Phase::Result`。转写为 `今天測試耳機音頻。請保留這段發言,由用戶檢查並手動發送。`（繁体/标点差异与 #3/#9 记录一致）；原目标恰为 `原始草稿：Rust 123。\r\n第二行保留。\r\n` + 整段转写，只追加一次；备用文本框与 `current-result.txt` 逐字一致；另一个编辑框和邻框逐字不变；临时 WAV 计数 0；`retry_available=false`。截图 `retry-result.png` 可见完整结果与可用的“复制完整结果”。 |
| 再次用假引擎完成一次真实失败，点击“清理失败音频” | 清理前 `retry_available=true`、保留 1 个 WAV；清理后 detail 为 `The temporarily retained audio from the failed capture was deleted at your request...`，`retry_available=false`，运行时目录下 WAV 为 0；当前结果与原目标不变。 |
| 清理后执行 `--control retry` | 状态仍为 `Failure`，detail 为 `No retained audio is available for a local retry...`，`retry_available=false`，没有新增 WAV，当前结果文件与原目标逐字不变。截图 `cleaned-failure.png` 可见该状态与仍可复制的当前结果。 |

两次开始/停止分别由系统接受 6 个按键事件；测试窗口的发送计数和 Enter 计数均为 0。证据 `.artifacts\issue-7-acceptance\retry-evidence.json` 包含两个场景的完整状态、转写、原目标全文和陈旧焦点框未被改写的布尔结果；同目录截图均已目视检查。驱动源码为 `accept-retry.ps1`、`ListenNativeQa.cs`、`NativeTargetFixture.cs` 和 `fake-engine.rs`；音频、可执行文件、运行时目录和截图不作为产品代码或会议历史提交。

## 构建与静态检查

```powershell
cd E:\luyin\.worktrees\issue-7
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test        # 27 + 9 通过，3 项需显式本地模型/操作者的用例被忽略
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat # 通过
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Build       # 离线锁定 release 构建通过
git diff --check                                                        # 通过
```

最终 `tests/capture_to_copy.rs` 为 **27 passed / 1 ignored**，`tests/long_sessions.rs` 为 **9 passed**，`tests/local_quality.rs` 的 2 项按原约定忽略（需要显式模型与语料）。从干净状态开始的首次 release 构建耗时 39.75 秒；提交前最终增量构建 5.22 秒。产物为本工作树 `app\listen\target\release\listen.exe` 及相邻 `libunwind.dll`。自动行为测试不连接桌面，也不运行真实 WASAPI 或识别引擎；桌面证据来自上面的独立验收。

## 已知限制与未覆盖

- **重试要求完整保留音频**：单文件采集、或流式采集中尚未成功消费任何识别窗口（`recognized_segments == 0`，包含所有“一开始就坏”的后端失败）可整体重试。若长会话中已有窗口成功识别，其音频按 #6 的有界设计立即删除；后续失败只保留 checkpoint 部分文本，不提供“用不完整音频假装整体重试”的入口。真实长会话中途失败后的整体重试未验证。
- **待处理失败音频会阻止新采集**：为满足“不与其他采集混淆”，`Start` 在可重试失败未解决时被拒绝，界面提示先重试或清理。这是有意选择的单任务恢复流程，不是后台队列。
- **清理只针对可重试的失败识别音频**：设备断开、存储耗尽等非识别失败的部分音频沿用 #2/#6 的恢复语义（列出路径并保留），不在本轮新增“清理”入口内。
- **清理后阶段仍为 `Phase::Failure`**：只更新说明、`retry_available=false` 并删除音频，不重置为 `Ready`；当前结果保持可见可复制。这是为了避免把刚才失败的采集伪装成一次新结果。
- **重试与写入未确认**：重试成功后的写入若无法确认，走既有备用文本框语义，不自动再追加；重试状态在该结果产生后即结束。该分支由确定性测试覆盖，未在真实桌面上制造 UIA 写入失败。
- **分段重试的拼合**：依赖每个 session 的 `capture-manifest.txt`（窗口/重叠样本数），自动测试用确定性分段回放验证了重叠只去掉一次；真实长会话的失败重试未在桌面跑过。
- 识别质量（繁体、标点、术语）属于 #9，本任务不改变模型或解码参数；真实场景的转写差异沿用已有记录。
