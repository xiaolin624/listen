# Issue #6：长时间后台分段转写与完整停止收尾

2026-11-11。分支 `implement/issue-6-long-sessions`（工作树 `E:\luyin\.worktrees\issue-6`），基线为集成分支最新提交 `4b9eb56`（含 #3 原生目标与 #9 识别质量/无语音行为）。

本记录只包含**实际执行过**的检查。真实 WASAPI → 目标/备用文本框的桌面验收不在本轮范围，见文末"仍未验证"。

## 业务边界

沿用已确认的唯一主要业务测试入口：`Application::dispatch`（Start / Stop / Toggle）、`poll`，以及可观察的 snapshot、结果文本、交付方式和临时音频生命周期。外部播放设备与本地模型是可替换依赖；分段、重叠归属、检查点、积压上限、收尾和结果交付都是生产代码。没有为内部函数另设测试入口。

长会话程序（`app/listen/examples/long_session.rs`）用**静默墙钟回放**替换外部播放设备：按真实时间把已知 PCM 写入与 WASAPI 相同的 `SegmentedAudioWriter`，经过同一 `Application::dispatch`、生产分段、真实本地 VAD/Whisper 和结果生命周期。它**不打开任何音频设备**，因此不能替代真实耳机路径验收。

## 本轮完成的工作

1. **接管未提交工作**。先把 #6 的未提交/未跟踪内容备份到 `.artifacts\issue-6-wip-backup\`，再以 WIP 提交 `10a706b` 存入分支历史，随后合并 `4b9eb56`。
2. **保留 #9 的无语音语义**。合并后空识别结果在**保存当前结果与写入目标之前**就返回 `delivery: None`，进入 `NoSpeech`，不覆盖旧结果、不追加空行；`audio.rs` 保留分段写入器，同时采用 #9 的收尾顺序（先 finalize 再传播错误）与"静音/空闲渲染设备是合法采集结果"的判定。
3. **接通真实 Whisper 时间戳**。本地适配器新增 `-ojf`（full JSON）路径；`whisper_timing` 按字节反序列化 token，把被拆开的 UTF-8 片段拼回完整码点，按 token id 过滤控制/时间戳 token，并要求拼出的文本与 segment 文本一致，否则按"结果不完整"失败并保留音频。
4. **修两个分段缺陷**：
   - 真机窗口无语音时，`recognize` 原本会清空上一窗口保留的尾部文本，导致已识别内容丢失；现在空窗口保留尾部。
   - 识别线程 panic 原本会让采集一直停留在 `Capturing`；现在捕获 panic 并立即报告失败、保留音频。
5. **时间戳与重叠归属的完整流程回归**（外部 CLI 夹具按 v1.8.2 的 JSON 协议输出，含被拆分的 UTF-8 字节）。
6. **长会话测量脚本**可在本机运行：修正了 PowerShell 5.1 下 `Start-Process` 重定向与重复环境变量冲突、`Measure-Object` 求和、以及失败时遗留测量进程的问题。

## TDD 证据

| 阶段 | 观察 |
| --- | --- |
| RED（时间戳） | 新增的 `local_cli_timestamp_output_preserves_split_chinese_bytes_and_merges_only_the_audio_overlap` 在接通 `-ojf` 之前无法通过：生产适配器只产出整窗文本，无法按 token 时间戳判断重叠归属。 |
| RED（静音抹除） | `silence_in_a_later_window_does_not_erase_the_already_recognized_trailing_word` 暴露空窗口清空尾部文本，最终结果丢失"收尾。"。 |
| RED（线程崩溃） | `a_crashed_background_recognizer_is_reported_without_waiting_for_more_audio` 观察到采集卡在 `Capturing`，失败状态永不出现。 |
| GREEN | 上述三项在修正后通过；重叠区按 token 中点归属，同一句话不会在相邻窗口重复追加。 |

复现的重复词回归 `equal_words_spoken_on_both_sides_of_a_boundary_remain_two_distinct_utterances` 确认：边界两侧真实说出两次的同一句话仍出现两次，没有被去重逻辑吞掉。

## 已执行的检查

```powershell
cd E:\luyin\.worktrees\issue-6
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test        # 15 + 8 通过，2 项需显式本地模型的用例被忽略
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat # 通过（cargo fmt --check）
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task BuildHarness
```

- `tests/capture_to_copy.rs`：15 项通过（含 #3/#9 的目标绑定、无语音、写入未确认回退）。
- `tests/long_sessions.rs`：8 项通过（含窗口重叠、边界重复词、静音不抹除、存储耗尽中断、积压中断、短尾收尾、外部 CLI 时间戳）。
- `tests/local_quality.rs`：2 项 `#[ignore]`，需要 `-Task Quality` 与已准备的语料，本轮未重复运行。

真实本地模型短 smoke（**静默回放，不播放声音，不操作桌面**）：

```powershell
.\app\listen\target\release\examples\long_session.exe `
  --input E:\luyin\.artifacts\quality-corpus\mandarin.wav `
  --runtime E:\luyin\.artifacts\issue-6-smoke `
  --engine E:\luyin\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe `
  --model E:\luyin\.tools\models\ggml-small.bin `
  --vad-model E:\luyin\.tools\models\ggml-silero-v5.1.2.bin `
  --seconds 70 --threads 6 --interval-seconds 10
```

结果：70.0 秒已知音频、3 个窗口（2 个采集中识别 + 1 个尾段），最终 315 字，`Phase::Result`，停止到结果 7.11 秒，`retained_audio` 为空，积压峰值 1，spool 归零。说明 `-ojf` 解析、UTF-8 片段拼合和重叠归属在真实模型输出上可用。

同一段 70 秒音频用单次整段识别（`-otxt`）作对照，**同样在重复句处出现漏句**，说明这些漏句来自模型在重复音频上的行为，而不是分段合并丢字；但这段语料本身不适合判断真实会议的分段边界质量。

## 60 分钟长时测量

## 60 分钟长时测量

```powershell
.\scripts\measure-long-session.ps1 -InputWav E:\luyin\.artifacts\issue-6-smoke\loop-96s.wav `
  -OutputDirectory E:\luyin\.artifacts\issue-6-measure -ToolsRoot E:\luyin\.tools -Seconds 3600
```

输入是把 #9 的 8 个固定样本按 `mandarin → mixed-technical → english-numbers → homophones → multiline → silence → instrumental → notification` 拼成的 96.04 秒循环，按墙钟速度回放到 3600.0 秒（57,600,000 samples）；`loop-96s.wav` SHA-256 `9F71068965D8EC09351CBEFACD77B463CE72980484A1216EA39970AB94EA1E3F`。small 模型、Silero VAD 与引擎哈希与 #9 记录一致（`docs/issue-9-verification.md`），CPU、6 线程、`-ng`。原始记录在 `.artifacts\issue-6-measure\`（`flow.ndjson`、`resources.ndjson`、`metadata.json`、`exit.json`）。

| 观测 | 结果 |
| --- | --- |
| 最终阶段 | `Result`（`terminal: true`） |
| 采集时长 | 3600.0 秒（请求 3600 秒，无截断、无自动停止） |
| 识别窗口 | 129 |
| 当前结果 | 7348 字，走备用文本框（本程序没有选目标） |
| 停止到结果 | **9.55 秒** |
| 失败与保留音频 | 无；`retained_audio` 为空 |
| 结束 spool | 0 字节 |

资源（每 3 秒采样本进程与直接子进程，共 1123 个样本，`resources.ndjson`）：

| 指标 | 峰值 | 平均 |
| --- | ---: | ---: |
| 进程组合工作集 | 789 MB | 183 MB |
| 进程组合私有字节 | 1268 MB | 299 MB |
| 长会话程序自身工作集 | 5.2 MB | — |
| whisper-cli 工作集 / 私有字节 | 774 MB / 1265 MB | — |
| 识别积压窗口 `peak_pending_segments` | **1** | — |
| 临时音频 `spool_bytes` | **1,408,088 B**（预算 9,600,440 B） | — |
| 内存中的 PCM `buffered_samples` | **32,000 samples**（2 秒重叠缓冲上限） | — |
| 长会话程序累计 CPU | 24.3 秒 / 60 分钟 | — |
| 独显显存（全局设备用量，非本进程） | 855 MiB | — |

结论与边界：

- 内存占用几乎全部来自每个窗口新起的 `whisper-cli` 进程（加载 small 模型），长会话程序本身稳定在约 5 MB；**没有随总时长增长的完整音频缓冲**。
- 临时音频峰值 1.4 MB，远低于 9.6 MB 预算；队列峰值 1，说明本机识别速度足以跟上 30 秒窗口，积压上限和压力中断只在受压场景（单元测试）中触发。
- 每个窗口都重新启动 `whisper-cli` 并重新加载模型；模型复用/预热不在本任务范围，属于 #10。
- 本机测量期间同机还执行了 #4 的编译与浏览器验收（约 15 分钟），且 `whisper-cli` 进程存活时间短于 3 秒采样间隔（1123 个样本中只有 298 个抓到它），所以峰值是下界性质的上报，**这些数字不作为延迟 SLA**。
- 60 分钟只是规格约定的验证样例，不是产品允许的最长采集时间。
- 文本内容不是本测量的目标；结果里仍能看到 #9 已记录的同音与术语错误（例如 `本级`/`本纪`）。

## 仍未验证（不能当作已完成）

- **真实 WASAPI 采集路径**：本轮所有长会话证据都使用静默回放替换播放设备，未打开真实耳机/默认渲染设备。合并后仍须在真实设备上做一次短采集验收。
- **真实会议录音下的分段边界质量**：固定语料是合成语音且重复播放时模型自身会漏句；跨段词句的边界正确率没有在真实会议音频上评估过。
- **同时开会的流畅度**：测量只记录本进程与直接子进程的资源；没有同时在开会/播放视频的条件下主观评价流畅度。
- **GPU**：默认 `-ng`（CPU），本轮没有 GPU 加速对比，这属于 #10。
- **60 分钟不是产品上限**：它只是规格约定的长时验证样例。
