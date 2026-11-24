# v0.2 双轴代码审查修复记录

2026-11-12。分支 `ticket/v0.2-review-fixes`（工作树 `E:\luyin\.worktrees\review-fixes`），基线 `integration/v0.2` 的 `017f9fa`。本文记录两轴审查（Standards：是否符合仓库文档化的代码标准；Spec：是否符合来源 issue/spec）发现的全部条目、处理结果、实测数字与本轮接受的技术债。

审查输入：`50-review-fixes.md`（汇总的 14 条）与 #15（规格）、#16、#17、#20、#21 的验收标准。基线测试：**156 passed / 0 failed / 13 ignored**。修复后：**163 passed / 0 failed / 14 ignored**，`-Task CheckFormat` 退出码 0，`-Task Build` 成功。

## 结论一览

| # | 轴 | 条目 | 结果 |
| ---: | --- | --- | --- |
| 1 | Standards | `verify-panel.ps1` 的验收仍依赖旧窗口类名/控件 ID，文档却声称已不依赖 | **已修复**：验收断言改为纯控制通道；窗口级检查拆成 `ui` 自检；唯一保留一条标注 `REMOVAL CHECK` 的删除断言；文档更正 |
| 2 | Standards | 根 `README.md` 与 `packaging/README.md` 仍描述 v0.1 | **已修复**：根 README 更新到 v0.2 现状并加署名与许可；包 README 如实描述当前 whisper 包，明确换引擎是 #23 |
| 3 | Standards | 用户可见文字仍说 “fallback text box” / “the window” | **已修复**：统一为 result panel 措辞；测试/脚本同步；行为不变 |
| 4 | Standards | `panel_window.rs` 的 `WM_DPICHANGED` 未检查 `lparam` | **已修复**：与 `capsule_window.rs` 相同的空指针保护 |
| 5 | Standards / Spec | 剪贴板复制失败被静默丢弃 | **已修复**：失败写入 `Notice::CopyFailed`，面板与 `status.txt` 明示“剪贴板未被替换”；真机诱导截图 |
| 6 | Standards | `run-listen.ps1` 永远强制 whisper | **已修复**：默认 SenseVoice，新增 `-Engine`（sensevoice/whisper/路径）；真机启动实测 backend 为 SenseVoice |
| 7 | Spec（ADR 0002） | `SetWindowDisplayAffinity` 失败只在 `capsule_capture_excluded=0` 里可见 | **已修复**：失败原因进入 `Notice::CaptureExcluded`，面板与 `status.txt` 明示；本机无法诱导失败，见“未诱导项” |
| 8 | Standards | `Win32_Networking_WinHttp` 是否投机引入 | **不适用**：#21 合并后 `deepseek.rs` 实际使用 21 个 WinHttp 符号 |
| 9 | Spec（#16 核心承诺） | 准备期停止会失败，缓冲音频未被识别 | **已在集成分支修复**（`cb71e91`），本分支未再改；补齐证据即可，见下文 |
| 10 | Spec（#15 性能目标） | “按下快捷键 <1 s 可采集”“回填 <300 ms”没有实测数字 | **已修复**：新增 `Timing::capture_start_ms`（进入结果与 `latency.jsonl`/`status.txt`）；新增回填预算测试；真机数字见下 |
| 11 | Spec（#17 AC1） | “明显优于 whisper”的中英混合没有真引擎断言 | **已修复**：新增默认忽略的真 whisper vs SenseVoice 对比测试，普通话断言不差、混合如实写明 whisper 更好；不再把术语表测试叫“不差于 whisper” |
| 12 | Spec（#15 Testing Decisions） | `timed_spans...` 断言引擎内部 span 时间戳 | **已修复**：改写为只断言交付文本（每个源短语恰好一次），删除内部时间戳断言 |
| 13 | Spec（#20） | `Phase::Failure` 自动打开面板属于范围蔓延 | **已修复**：只有 `Delivery::Fallback` 自动打开；失败由红胶囊提示、用户点开；测试与文档同步 |
| 14 | Spec（#15/#18） | 默认术语表穷举错误写法 | **已修复**：证明 `wa-sa-pi`、`waapp loop back` 可被正式写法召回后删除；保留拼音/汉字/中文邻接短别名并记录理由 |

## 逐项说明

### 1. 面板验收改为控制通道（Standards）

**发现**：脚本文档写“验收不依赖窗口类名/控件 ID”，实际验收断言仍用 `GetDlgItem($panel, 211/212)`、`BM_CLICK`、`Get-Panel`/`Get-Capsule`；`docs/issue-20-verification.md` 也声称“自动化验收不再依赖窗口类名/控件 ID”，属于过度声明。

**修复**：`scripts/verify-panel.ps1` 重构为三组检查，并在 `panel-verification.json` 里给每项标 `kind`：

- `acceptance`（**43 项，全部通过**）：只用 `--control show|panel|copy|start|stop|quit`、`status.txt`、`panel.txt`、`current-result.txt`、剪贴板、进程退出码。包括：`panel_visible` 的开关、回退结果自动打开、`NoSpeech` 不自动打开、失败**不**自动打开且 `--control show` 后可读到原因、`--control copy` 的剪贴板结果、`timing_capture_start_ms` < 1 s。
- `ui`（**42 项，全部通过**）：窗口级自检——置顶/工具窗样式、同显示器与工作区、DPI、不移动胶囊、不抢焦点、`panel.txt` 与两个只读 EDIT 逐字符一致、按钮可用性、胶囊单击/Esc/托盘菜单、截图。明确标注“不是验收”。
- `removal`（1 项，通过）：唯一保留的旧窗口断言，`REMOVAL CHECK: the old ListenCaptureToCopyWindowV1 detail window no longer exists`。

**旧证据**：改前脚本第 499–500、553–581、692–708、717–723、759、779–782 行都直接读窗口/控件；现这些断言全部归入 `ui` 或删除。

**文档**：`docs/issue-20-verification.md` 重写了“自动化契约”段，逐组说明哪些是控制通道验收、哪些只是窗口自检、唯一旧窗口检查是删除断言；验收流程按新脚本重写（含失败不自动打开）。

### 2. README 同步（Standards）

- 根 `README.md`：重写为 v0.2 现状——SenseVoice q8/sherpa-onnx 常驻 CPU（AVX2 必需、后台准备、准备期可采集）、状态胶囊（置顶/不抢焦点/无任务栏/录屏隐藏）、结果面板（复制/重试/清理/精修/关闭、不自动发送）、本地术语表与编辑方式、可选精修（仅文字、无 key 不可用）、当前交付包仍是 whisper 且换引擎是 #23、数据文件、测试方式与范围；保留 v0.1 的历史链接（#2–#11）；新增**署名与许可**：SenseVoiceSmall 权重来自 FunASR/SenseVoice（FunASR MODEL_LICENSE），sherpa-onnx/onnxruntime 各自许可证，文件位置，交付包必须保留署名。
- `packaging/README.md`：改为描述当前包真实内容（whisper.cpp 0.1.0、`engine\`、可选 `models\`、`glossary.txt`、启动器），明确“本包仍是 whisper.cpp；SenseVoice 换包是 ticket #23，尚未解包”，并把面板/胶囊行为更新到当前二进制；示例术语表与裁剪后的 `packaging/glossary.txt` 一致。

### 3. 用户可见措辞（Standards）

改前（`lib.rs` / `paste.rs`）：

- `Closing the window keeps Listen in the tray.`
- `The complete result is in the fallback text box.`（`not_delivered`、`failed_write` 的 `fallback`）
- `or the fallback text box is used if it is no longer valid.`（两处保留音频说明）
- `it is shown in the fallback box and stored at ...`
- `Copy the complete result from the fallback text box instead.`（两处剪贴板降级）

改后统一为 `result panel` / `expanded result panel`（例如 `Closing the result panel keeps Listen running in the tray.`、`The complete result is in the expanded result panel.`）。同步更新了唯一断言旧措辞的脚本 `scripts/verify-generic-editor.ps1`（`*fallback text box*` → `*expanded result panel*`）。Rust 测试没有断言这些原文，行为未变，全套测试保持绿色。

### 4. DPI 消息空指针（Standards）

`panel_window.rs` 原为 `let suggested = *(lparam.0 as *const RECT);`；现与 `capsule_window.rs::handle_dpi_change` 一致：`lparam` 为空时只重排布局、不解引用。

### 5. 复制失败不再静默（Standards / Spec）

**发现**：`gui.rs` 的复制路径是 `let _ = paste::copy_to_clipboard(&text);`，用户点“复制完整结果”失败后没有任何反馈。

**修复**：

- 新增 `Notice`（`CopyFailed` / `CaptureExcluded`）与 `Snapshot::notice`，与 `detail` 分开保存，避免采集中的 100 ms 进度刷新把它抹掉。
- `Application::copy_result(impl FnOnce(&str) -> AppResult<()>)` 成为应用边界：空结果不动剪贴板；成功清除旧的复制失败提示；失败写入 `Could not copy the result: <reason>. The clipboard was not replaced; ...`。`gui.rs` 改调它，不再丢错误。
- 面板状态块和 `status.txt` 新增 `notice=` 展示这句话。

**测试**（`tests/ui_notices.rs`，4 个）：失败被报告且明示剪贴板未被替换；成功复制会清除旧的复制失败；无结果不碰剪贴板；胶囊无法隐藏的警告不会被一次成功复制清掉。`panel.rs` 增加 notice 渲染单测。

**真机证据**：用另一个窗口 `OpenClipboard(hwnd)` 占住剪贴板，再对运行中的实例执行 `--control copy`，`status.txt` 得到
`Could not copy the result: Cannot open the clipboard: 拒绝访问。 (0x80070005). The clipboard was not replaced; ...`，面板截图见下。

### 6. 开发启动器默认引擎（Standards）

**发现**：`scripts/run-listen.ps1` 无条件传 `--engine <whisper-cli.exe> --model ... --vad-model ...`，文档化的开发启动永远不跑新默认引擎。

**修复**：`-Engine` 默认 `sensevoice`（不传任何引擎参数），可选 `whisper` 或 whisper-cli 路径；只有 whisper 时才解析并传 `--model`/`--vad-model`/`--backend`；`--control` 行为与 `panel.txt` 提示保留。选择 SenseVoice 时若显式传模型参数会明确报错，避免“看起来生效”。

**证据**：真机运行 `run-listen.ps1 -Runtime ... -Hidden`，输出 `Engine: SenseVoice (default resident engine, sherpa-onnx)`，`status.txt` 为 `backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s); ...`；面板就绪 `detail` 里也带 `Backend: SenseVoice q8 ...`（截图）。

### 7. 胶囊隐藏失败可见（Spec，ADR 0002）

**发现**：`capsule_window.rs` 只保留 `SetWindowDisplayAffinity(...).is_ok()`，失败只在 `status.txt` 的 `capsule_capture_excluded=0` 里可见，而 ADR 承诺胶囊对所有屏幕共享隐身。

**修复**：保留平台错误原因（`capture_exclusion_error`），启动时由 `gui.rs` 转成 `Notice::CaptureExcluded`：`The status capsule could not be hidden from screen capture (<reason>), so screen sharing and recordings will show it. Capture and local recognition are unaffected.`；`status.txt` 保留 `capsule_capture_excluded=1|0` 键。该提示是会话级事实，成功复制不会清除它。

**测试**：`a_capsule_that_cannot_hide_from_screen_capture_is_reported_and_survives_a_copy`。

**未诱导项**：本机 Windows 11 上 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 成功（`capsule_capture_excluded=1`）。要让它失败需要关闭 DWM/合成、使用非顶层窗口或旧系统；产品运行路径无法在不改代码/不注入的情况下制造该条件，因此**没有**失败态真机截图，只有单元测试 + 成功态 `capsule_capture_excluded=1` 证据。这是如实记录，不把成功态当成失败态证据。

### 8. WinHttp（不适用）

`deepseek.rs` 在 #21 合并后实际使用 21 个 `Win32_Networking_WinHttp` 符号，不是投机依赖；无需改动。

### 9. 准备期停止（已在集成分支修复）

**发现**的原 bug（`Control::Stop` 立即转写、准备未完成时 SenseVoice 返回 “not prepared”）已在 `cb71e91`（#16 follow-up，`feat: wait for model preparation before recognizing buffered audio (#16)`）修复并被 `32c890a` 合入 `integration/v0.2`。当前 `lib.rs` 有：

- `ReadinessSignal`（`Mutex` + `Condvar`，120 s 上限）+ `PROCESSING_WHILE_PREPARING_DETAIL` / `PROCESSING_READY_DETAIL`；
- Stop 线程在非流式路径 `readiness.wait().and_then(|()| transcriber.transcribe(...))`，超时走既有“保留音频失败”路径；
- 流式 worker 同样等待 readiness（`transcribe_segment` 的 fake 在准备前会拒绝）。

`tests/preparation_capture.rs` 已有审查要求的两个场景：
`stop_before_preparation_finishes_waits_for_readiness_and_appends_the_result_once`、
`preparation_failure_after_stop_reports_failure_retains_audio_and_delivers_nothing`，
以及流式版本 `streaming_capture_stopped_before_preparation_finishes_records_the_windows_after_readiness`、
`streaming_preparation_failure_retains_the_buffered_windows_for_recovery`。
本分支**没有改动**这条路径；全套测试通过。

### 10. 性能目标实测（Spec，#15）

**实现**：`Timing::capture_start_ms` 从 `Control::Start`/热键分发进入 `Phase::Capturing` 计时（含目标绑定与启动设备），写进结果 `Timing`、`latency.jsonl` 和 `status.txt` 的 `timing_capture_start_ms=`，并随结果保留（不再被完成时的 Timing 覆盖）。回填沿用既有 `timing_delivery_ms`。

**应用边界测试**：`latency_and_preparation::backfill_to_the_bound_target_stays_inside_the_three_hundred_millisecond_budget` 用假设备 + 假目标断言 `delivery_ms <= 300`，同时断言 `capture_start_ms < 1000` 且文本只追加一次。

**真机数字**（共享桌面，私用 target dir，release）：

- 采集启动（`timing_capture_start_ms`）：非可编辑探针窗口 **43–55 ms**（`verify-panel.ps1` 两次）；真实 UIA 可写目标 **246 / 268 / 403 ms**（含 UIA 目标绑定的波动），均 < 1 s，**达标**。
- 回填（`timing_delivery_ms`，真实原生 EDIT 目标，逐字读回确认）：**274 / 280 / 294 ms**（中位 280），**达标但余量很小**；主要成本是 UIA 读回确认。测试机上偶尔出现目标未绑定（共享桌面前台被其它 agent 改动）或音频路由抖动（`NoSpeech`），这些是环境噪声，重试后的成功样本才计入上表。
- 停止→结果（同一轮真实采集）：429 / 434 / 439 ms（另有一次 129 ms 的 NoSpeech 路径），与 #17 记录的 0.4–0.5 s 量级一致。

未达标的项目：无。余量说明：回填 280 ms 距 300 ms 只有 20 ms，不应把 300 ms 当作可长期依赖的硬保障；真实目标、大文本或慢 UIA 提供程序会更容易超。

### 11. #17 AC1 的真引擎对比（Spec）

新增默认忽略测试 `sense_voice_is_not_worse_than_whisper_on_mandarin_and_the_mixed_outcome_is_stated`：同一条 WAV 分别走真实 whisper.cpp（`--engine whisper` 的代码路径）与真实 SenseVoice，CER 忽略大小写/标点。

实测（2026-11-12）：

| 语料 | SenseVoice CER | whisper CER | 结论 |
| --- | ---: | ---: | --- |
| `mandarin.wav`（10.8 s） | **0.000** | 0.378（繁体/半角标点） | 断言 SenseVoice 不差 |
| `mixed-technical.wav`（19.2 s） | 0.167 | **0.130** | whisper 在英文专名（`WASAPI loopback`）上更好；SenseVoice 还丢了 `CUDA` |

测试**如实断言**混合语料上 whisper 不差（`mixed.2 < mixed.1`）并打印两边原文；如果未来翻转，测试会失败并要求更新记录，而不是悄悄放宽。原来的 `mixed_chinese_english_keeps_technical_words_and_is_no_worse_than_whisper` 改名为 `the_shipped_glossary_repairs_the_production_chain_for_mixed_chinese_english`：它只证明“SenseVoice → 术语表 → 交付”这条生产链能被术语表修好，不再声称不差于 whisper。`docs/issue-17-verification.md` 顶部已加更正说明。

### 12. 内部 span 断言（Spec，#15 Testing Decisions）

删除 `timed_spans_follow_the_vad_utterances_instead_of_the_whole_window`（它断言 `start_ms`/`end_ms`/span 顺序），改为 `one_mandarin_window_delivers_every_source_phrase_exactly_once`：只断言生产 `transcribe` 交付的文本——四个源短语各出现一次、全角标点、简体、无控制标签。30 s 重叠的“长采集每个短语一次”仍由既有 `a_real_long_capture_keeps_every_phrase_once_across_the_thirty_second_overlap` 覆盖。真机运行该测试通过，输出整句正确。

### 13. 失败不再自动打开（Spec，#20）

`panel::auto_open_key` 原对 `Phase::Failure` 返回 `failure:<detail>`。现在只对 `Result+Fallback` 返回 key；失败时胶囊变红，用户点开面板读原因（#20 故事 10），不抢屏。更新了 `panel.rs` 的两个单测、`gui.rs`/`panel_window.rs` 注释，以及 `docs/issue-20-verification.md` 的验收流程（失败运行断言 `panel_visible=0`，再 `--control show` 得到 `panel_visible=1` 与完整原因）。`verify-panel.ps1` 的失败场景按新行为通过。

### 14. 默认术语表裁剪（Spec，#15/#18）

**证明**：`glossary::tests::the_shipped_canonical_forms_alone_catch_the_removed_variants` 先去掉 `packaging/glossary.txt` 的所有别名，再验证 `wa-sa-pi`（归一化为 `wasapi`，等于正式写法）与 `waapp loop back`（由 `WASAPI loopback` 近似匹配）仍被纠正。于是删除这两个别名。保留的别名都是算法无法推断的：

| 条目 | 保留的别名 | 理由 |
| --- | --- | --- |
| `DeepSeek Harness` | `shen du qiu suo`, `迪普西克` | 拼音/汉字写法，算法无汉字→拼音表，无法推断 |
| `WASAPI` | `waapp` | 单独 `waapp` 与 `wasapi` 首尾字符不同，短正式写法无法近似召回 |
| `CUDA` | `ku da`, `ku` | 正式写法 4 字符 < 近似匹配下限；中文邻接短别名是实测误识别 |
| `Rust` | `re` | 同上，实测“我们用re实现” |

负例测试（`ordinary_sentences_are_not_rewritten_into_terms`、`short_aliases_do_not_match_inside_english_words_or_re_prefixes`、`the_shipped_glossary_leaves_clean_chinese_and_ordinary_latin_text_alone`）未改动、仍通过。`packaging/glossary.txt` 头部注释与 `docs/issue-18-verification.md` 示例同步更新，说明哪些别名保留、为什么。

## 接受的技术债（本轮不改）

1. **`verify-capsule.ps1` / `verify-panel.ps1` / `verify-listen-package.ps1` 的 status/phase/wait 助手与 P/Invoke 探针重复**：三个脚本服务不同验收面（胶囊样式/面板契约/包完整性），抽公共库会把 #24 的稳定工具绑到私有实现，收益低于风险。
2. **`Phase`/`Preparation` 级联出现在 `capsule.rs`、`panel.rs`（标题+说明）、`gui.rs` 托盘文字、`lib.rs`**：四个表面回答不同问题（颜色 / 人话 / 托盘提示 / 状态文件），共用一个格式化器反而会丢掉各自的措辞与优先级。
3. **`LoadRequest` 重复 `SenseVoice` 字段、`paste::copy_to_clipboard` 是薄包装、集成测试重复 `Runtime` harness**：都是 ABI/应用边界上的适配器；泛化只会增加间接层，不减少领域知识。
4. **`packaging/*` 仍随包发布 whisper**：明确属于 ticket #23，且由 #22 的真实语料基准把关；本轮只在文档里如实说明，不做半成品替换。

## 验收与证据

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test        -Manifest E:\luyin\.worktrees\review-fixes\app\listen\Cargo.toml -TargetDir E:\luyin\.artifacts\cargo-target-review-fixes
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\review-fixes\app\listen\Cargo.toml -TargetDir E:\luyin\.artifacts\cargo-target-review-fixes
& E:\luyin\.artifacts\run-cargo.ps1 -Task Build       -Manifest E:\luyin\.worktrees\review-fixes\app\listen\Cargo.toml -TargetDir E:\luyin\.artifacts\cargo-target-review-fixes
& E:\luyin\.worktrees\review-fixes\scripts\verify-panel.ps1 -Exe E:\luyin\.artifacts\cargo-target-review-fixes\release\listen.exe -Runtime E:\luyin\.artifacts\issue-review-fixes\panel-runtime -Evidence E:\luyin\.artifacts\issue-review-fixes
```

- 测试：**163 passed / 0 failed / 14 ignored**（基线 156/0/13；新增 7 个测试，新增 1 个默认忽略的真引擎对比）。
- 格式：`cargo fmt --check` 退出码 0。构建：release 成功。
- `verify-panel.ps1`：**PASS**——验收 43/43、窗口级自检 42/42、删除检查 1/1，证据 `E:\luyin\.artifacts\issue-review-fixes\panel-verification.json`。
- 真引擎：`sense_voice_quality` 的对比测试与改写后的普通话可观察测试均显式运行通过；输出见 `E:\luyin\.artifacts\review-fixes-comparison.log`。

### 截图与真机证据（`E:\luyin\.artifacts\issue-review-fixes\`）

| 文件 | 内容 |
| --- | --- |
| `panel-ready.png` | 就绪面板（`verify-panel.ps1`，控制通道 + UI 自检） |
| `panel-capturing.png` | 采集中的实时进度 |
| `panel-fallback.png` | 无目标回退结果，`detail` 已改为 “expanded result panel” |
| `panel-failure.png` | 失败在用户点开后的原因（失败不再自动打开） |
| `tray-menu.png` | 托盘菜单（含“打开结果面板”） |
| `panel-launcher-sensevoice.png` | `run-listen.ps1` 默认启动后的面板，`detail` 显示 `Backend: SenseVoice q8 ...` |
| `launcher-output.txt` | 启动器输出：`Engine: SenseVoice (default resident engine, sherpa-onnx)` |
| `panel-copy-failure.png` | 真机剪贴板失败：面板显示 “Could not copy the result: ... The clipboard was not replaced ...” |
| `copy-failure-evidence.json` | 上述失败对应的 `status.txt notice=` 原文 |
| `real-target-appended.png` | 真实原生 EDIT 目标收到追加结果（回填计时样本） |
| `backfill-evidence.json` | 真实目标的 `timing_delivery_ms` / `timing_capture_start_ms` / 目标文本 |
| `panel-verification.json` | `verify-panel.ps1` 的三组检查与截图清单 |

## 偏差、风险与未覆盖

- **真机数字来自共享桌面**：本机同时有其它 agent 的构建/UI 自动化，音频路由偶尔捕获不到语音（`NoSpeech`）或前台目标绑定失败；`verify-panel.ps1` 对无语音回退采集最多重试 3 次，回填测量只统计成功样本。单用户机器不会遇到这些噪声，但 280 ms 的回填余量本身很薄。
- **显示亲和失败态未真机诱导**：理由见第 7 条；成功态 `capsule_capture_excluded=1` 与单元测试是现有证据。
- **失败详情有重复词**：`sensevoice.rs` 的缺失模型消息当前是 “The local SenseVoice SenseVoice model is missing”，不在本次审查条目内，未改；已记录为后续清理项。
- **`cargo clippy` 未运行**：工具链未安装 clippy 组件（离线环境无法补装），与 #20 记录一致。
- **真引擎测试默认忽略**：`#17` 的对比测试需要 `LISTEN_WHISPER_*` 与引擎/语料路径，已在本机显式运行；CI/无模型环境仍只跑快速套件。

## 提交与合并

- 分支：`ticket/v0.2-review-fixes`，从 `integration/v0.2` `017f9fa` 创建。
- 提交（按主题）：
  - `5bbb400` `fix: close the v0.2 review findings for the panel, latency and Windows hazards (#15, #17, #18, #20)` —— 第 3、4、5、6、7、10、11、12、13、14 条的代码、测试与脚本。
  - `docs: update the v0.2 product docs and record the review fixes (#15, #20, #23)` —— 第 1 条脚本对应的文档更正、第 2 条 README、第 9 条证据说明、`docs/issue-17/18/20` 更正与本文。
- 交付前 `git merge integration/v0.2` 干净完成，合并后全套测试仍为 163 passed / 0 failed / 14 ignored。
