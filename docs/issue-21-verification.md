# Issue #21：DeepSeek 文字校正与安全回填

2026-11-12。工作树 `E:\luyin\.worktrees\issue-21`，分支 `ticket/issue-21`，基线 `70bcf92`（`integration/v0.2`，含 #16–#20 与 #22 工具）。开工前 `git merge-base --is-ancestor integration/v0.2 HEAD` 退出码 0。报告前已合并 `integration/v0.2`（`32c890a`，含 #16 的准备期缓冲提交）到本分支（merge `ea42d51`），合并后全套测试 156 passed / 13 ignored，`cargo fmt --check` 通过。主工作树未改动。

## 结论摘要

- 新增应用层“文字进、文字出”端口 `RefineText`：`availability()` 不联网，只说明本机能否精修；`refine(text)` 只接收**一段转写文本**。`Application::with_refine` 接入，`Control::Refine` / `--control refine` 触发。
- 精修状态永远可观察：`Snapshot.refine`（`Unavailable(reason)` / `Available` / `Running` / `Applied` / `NotApplied(reason)` / `Failed(reason)`）和 `Snapshot.refined_text`（不能写回目标时保留精修文本）。精修不改变采集、处理、结果的既有语义，`Phase` 仍是 `Result`。
- 回填安全边界：交付时只在**确认追加成功**后记住“目标句柄 + 交付前内容 + 交付后内容 + 本次转写”。精修完成后，只有目标内容仍（忽略 CRLF 差异）等于交付后内容、且本次结果仍是被追加的那段转写时，才用精修文本替换；替换后必须读回一致才宣称 `Applied`。用户手改、目标不可读、写入未确认都**不写**，精修文本只进面板，本地结果原样保留。
- DeepSeek 客户端（`app/listen/src/deepseek.rs`）用 WinHTTP：`POST https://api.deepseek.com/chat/completions`，`deepseek-flash`，关闭 thinking，`temperature=0`，`stream=false`，`max_tokens=4096`；`finish_reason != "stop"`、非 200、空结果一律按失败处理。运行在 worker 线程，经 `poll()` 回主线程，UI 不阻塞；连接/发送/接收与总时长都有上限。
- key 只来自进程环境变量 `DEEPSEEK_API_KEY`：不写日志、不进 `status.txt`/`panel.txt`/证据文件/错误字符串；没有 key 时精修入口置灰（`精修（不可用）`），本地识别与交付完全不受影响。
- 面板接线：#20 的占位置灰按钮改为只在 `Refine::Available` 时可用，文案随状态变化（`精修` / `精修中…` / `精修（不可用）`）；`panel.txt` 与可见控件逐字符一致（#20 的 86 项桌面回归全过）。`status.txt` 增加 `refine=` 与 `refine_reason=`。
- 证据：新增 15 个假端口应用边界测试、11 个客户端纯函数单测、5 个面板单测、1 个 `#[ignore]` 真 API 测试。全套 **156 passed / 13 ignored**、格式检查通过、release 构建成功。真机运行（无 key）15 项检查全过，截图在 `.artifacts\issue-21\`。本机没有 key，因此**真实 API 往返没有运行**，见“未验证”。

## 1. 端口与状态

`app/listen/src/lib.rs`：

```rust
pub trait RefineText: Send + Sync {
    fn availability(&self) -> Result<(), String>;
    fn refine(&self, text: &str) -> AppResult<String>;
}

pub enum Refine {
    Unavailable(String),
    Available,
    Running,
    Applied,
    NotApplied(String),
    Failed(String),
}

pub enum Control { Start, Stop, Toggle, Retry, CleanupAudio, Refine }
```

- `Application::with_refine(Arc<dyn RefineText>)`，与 `with_targets` / `with_glossary` 同型；不调用它时 `Refine::Unavailable`，原因明确说“没有配置精修端口”。
- `Snapshot` 新增 `refine: Refine` 与 `refined_text: Option<String>`；`Refine::name()` 给 `status.txt` 稳定拼写，`reason()` 给用户可读原因。
- 可用性规则（唯一入口）：`Phase::Result` + 非空 `text` + `availability() == Ok(())`。因此：采集中、处理中、无完整结果、结果被 `NoSpeech` 保留但本次没有完整结果、失败后、刚启动时都不可精修，且每次拒绝都写清原因。
- 精修进行中拒绝 `Control::Start` / `Toggle`（进入采集会清掉交付记忆，和回填 worker 竞争）；重复 `Control::Refine` 拒绝且不会再调端口。
- 新采集开始、新的完整结果、`NoSpeech`、任何失败都会清空交付记忆与精修状态；新的完整结果重新计算 `Available` 或不可用原因。

## 2. 安全回填规则（逐分支）

`deliver()` 现在返回 `DeliveryOutcome { delivery, detail, memory }`；`DeliveryMemory` 只在 `Target::replace` 返回成功（即目标已读回完整替换）后产生：

```rust
struct DeliveryMemory {
    target: Box<dyn Target>,   // 存活句柄，后续精修可再次读取
    before: String,            // 交付前读到的完整目标内容
    after: String,             // 本次写下的完整内容
    appended: String,          // 本次追加的转写文本
}
```

`refine_delivery()` 在 worker 线程执行下列规则：

| 分支 | 判定 | 动作 | 状态 / 本地结果 |
| --- | --- | --- | --- |
| 目标未变 | `content()` 与 `after` 忽略 CRLF 后相等，且 `appended == 当前完整结果` | `replacement = before.is_empty() ? refined : before + "\n" + refined`；`replace(expected=读到的原始 content, replacement)`；再读回，忽略 CRLF 后必须等于 `replacement` | `Applied`；`current-result.txt`、`Snapshot.text`、面板当前结果都变为精修文本；不再保留 `refined_text` |
| 用户手改 | 目标内容不等于 `after` | **不写** | `NotApplied(原因)`；`Snapshot.text` / `current-result.txt` / 用户编辑全部不变；`refined_text = Some(refined)`，面板显示精修文本与“未写回”说明 |
| 目标不可读 | `content()` 返回错误（窗口已销毁等） | **不写**、不猜测 | `NotApplied(原因)`，精修文本留在面板、本地结果不变 |
| 写入未确认 | `replace` 报错，或写后读回与 `replacement` 不一致（截断/部分写入） | 不重试、不回滚、不再次追加 | `NotApplied(原因)`，不宣称写回，本地结果不变 |
| 没有目标（交付是 `Fallback`） | `DeliveryMemory` 为 `None` | 精修文本直接成为当前结果并写 `current-result.txt` | `Applied`；面板/快照更新；没有向任何控件写文字 |
| 精修请求失败 | 端口返回错误、非 200、`finish_reason != stop`、响应为空/非法 | 不写目标、不重新追加、不恢复旧文本 | `Failed(原因)`；`Snapshot.text`、`current-result.txt`、目标全部不变 |

补充约束：

- 比较使用 `normalize_newlines`（`\r\n` → `\n`）。`replace()` 的 `expected` 用**原样读到的内容**（编辑器可能用 CRLF），保持 `Target` 契约不变。
- 在替换前额外校验 `memory.appended == 当前结果文本`，确保只替换“这次交付的那段转写”，不会把别的结果写进目标。
- 精修不改变 `Phase`、不触发采集、不改变 `Delivery`。
- 精修凭据从不参与状态文本；`status.txt` 的 `refine_reason` 只含固定说明与 API 返回/环境变量名，绝不含 key 值。

## 3. DeepSeek 请求（实际发送的字节）

请求（`deepseek.rs`）：

```
POST https://api.deepseek.com/chat/completions
Content-Type: application/json
Authorization: Bearer <DEEPSEEK_API_KEY>
```

请求体（键顺序即序列化顺序）：

```json
{"model":"deepseek-flash","thinking":{"type":"disabled"},"temperature":0,"stream":false,"max_tokens":4096,"messages":[{"role":"system","content":"<下面的 system prompt>"},{"role":"user","content":"<当前完整转写文本>"}]}
```

system prompt 原文：

```text
You correct speech-to-text transcripts. The user message is one transcript produced by a local speech recognizer. Correct only obvious recognition mistakes: proper nouns, technical terms, homophones and punctuation. Keep the meaning, the wording, the speaker's language mixing (for example Chinese with English technical terms) and every number exactly as intended. Do not translate, summarize, expand, shorten or add commentary. Output only the corrected transcript text: no explanations, no quotes, no code fences. If nothing needs correcting, return the transcript unchanged.
```

响应处理：只接受 HTTP 200；取 `choices[0].message.content` 并去掉首尾空白；`choices[0].finish_reason` 必须存在且等于 `"stop"`（`length`、`content_filter`、`insufficient_system_resource`、`aborted`、缺失都按失败处理）；空文本按失败处理；错误响应取 `error.message`，非 JSON 错误体截断到 200 字符；响应体上限 1 MiB。

超时与线程：WinHTTP resolve 5 s / connect 5 s / send 10 s / receive 30 s，另有 60 s 总时长上限；`refine()` 在独立 worker 线程运行，结果经 `poll()` 用 `mpsc` 回到 UI，整个网络调用不占用 UI 线程。

## 4. 凭据与“只有文字离开本机”

key 来源只有 `std::env::var("DEEPSEEK_API_KEY")`：

- 去掉首尾空白，空白视为没有 key；没有 key 时 `availability()` 返回固定原因（只包含变量名），精修入口不可用，本地功能不受影响；
- key 只出现在 `Authorization: Bearer ...` 请求头里；不写日志、不写入 `status.txt` / `panel.txt` / `current-result.txt` / `latency.jsonl`，不进入任何错误字符串或证据文件；
- 本机运行环境没有设置 `DEEPSEEK_API_KEY`（验收时进程环境为空）；
- 启动后新增 key 需要重启进程才生效（没有运行时重新读取，避免让“可用性”随时间变化）。

离开本机的字节只有：HTTPS 头部（`Content-Type`、`Authorization`）和上面那段 JSON。**不发送**：音频、音频路径、runtime 目录、当前结果文件路径、端点名称/ID、进程 ID、机器名、术语表、任何其他转写文本。`RefineText` 的签名本身只暴露一个 `&str`，假端口测试断言它收到的字符串与当前转写逐字节相等。

## 5. 界面与自动化契约（在 #20 契约上追加）

- 面板 `精修` 按钮（内部控件 ID 204，只用于自检）：只有 `Refine::Available` 时可用；文案 `精修` / `精修中…` / `精修（不可用）`。点击只向属主窗口投递 `COMMAND_REFINE`，由应用层执行 `Control::Refine`；不在面板窗口过程里跑应用逻辑。
- `--control refine`：新增控制命令（id 10），与其它控制命令一样“已送达运行实例”即退出码 0；拒绝原因写入 `status.txt` 的 `detail`。
- `panel.txt`：状态区新增一行“精修…”说明（`refine_note`）：可用/进行中/已应用/未写回/失败/不可用各有明确句子；当精修结果不能写回目标时，完整结果之后追加：

  ```text
  【精修结果，未写回目标输入框；可手动复制】
  <refined text>
  ```

  `panel.txt` 仍严格等于面板两个只读控件的拼接（#20 回归逐字符检查通过）。
- `status.txt` 保留全部旧键，新增：
  - `refine=unavailable|available|running|applied|not_applied|failed`
  - `refine_reason=<单行原因；状态没有原因时为空>`（换行替换为空格）
- 托盘菜单**不新增**精修项：规格的用户故事 35 把精修入口放在展开面板，用户故事 11 的托盘菜单仍是退出/重试/清理/打开面板；#20 的 6 项托盘菜单回归保持不变。

## 6. 测试与证据

### 红 → 绿

- 红：先写 `tests/refine_backfill.rs` 并运行，编译失败 `error[E0432]: unresolved imports listen::Refine, listen::RefineText`（`.artifacts\issue-21\red-refine-suite.txt`）。
- 绿：实现端口/状态/回填后，同一套测试 **15 passed**；随后补齐客户端单测与面板单测。

### 假端口应用边界测试（`app/listen/tests/refine_backfill.rs`，15 个）

| 测试 | 证明的规则 |
| --- | --- |
| `a_successful_refine_replaces_the_appended_transcript_and_reads_it_back` | 目标未变 → 只替换追加段，读回确认，`current-result.txt` / 快照 / 面板一致 |
| `the_refine_port_receives_exactly_the_transcript_text_and_nothing_else` | 端口收到的就是当前转写原文，且一次调用 |
| `an_empty_original_draft_is_replaced_with_only_the_refined_text` | `before.is_empty()` 分支不带前导换行 |
| `line_endings_reported_by_the_target_do_not_prevent_a_safe_backfill` | CRLF 目标仍判定“未变”并安全回填 |
| `a_target_edited_by_the_user_is_never_overwritten_and_keeps_the_refined_text_in_the_panel` | 手改 → 目标字节级不变、零写入、精修文本进面板、本地结果不变 |
| `a_target_that_cannot_be_read_after_delivery_is_not_written_and_keeps_the_refined_text` | 目标不可读 → 不写、不猜、精修文本保留 |
| `a_failed_refine_keeps_the_local_result_and_never_appends_it_again` | API 失败（HTTP 401 fixture）→ 本地结果/目标/current-result 全不变，不重复追加 |
| `a_fallback_result_with_no_target_is_refined_into_the_panel_and_the_current_result` | 无目标 → 精修文本成为当前结果，不写任何控件 |
| `no_refine_port_refuses_explicitly_and_leaves_the_result_and_target_untouched` | 没接端口 → 明确拒绝，结果/目标不变 |
| `a_port_whose_availability_fails_refuses_with_its_own_reason` | 无 key 型可用性错误 → 拒绝原因透出，端口从未被调用 |
| `refine_is_unavailable_while_capturing_or_processing_and_never_calls_the_port` | 采集中/处理中都不可精修、端口零调用 |
| `starting_a_capture_is_refused_while_a_refine_is_in_flight` | 精修中拒绝 Start/Toggle，结束后再拒绝第二次精修 |
| `a_second_refine_request_while_running_is_refused_without_calling_the_port_again` | Running 中重复请求不重复调用 |
| `a_new_capture_clears_the_refine_state_and_a_new_result_offers_refine_again` | 新采集清状态，新结果重新可用 |
| `no_speech_keeps_the_previous_result_but_does_not_leave_refine_available` | `NoSpeech` 保留旧结果但不可精修 |

### 客户端纯函数单测（`src/deepseek.rs`，11 个）

请求体形状/参数/UTF-8/换行、只含一段文本、无路径与 endpoint 泄漏；正常响应；`length`/`content_filter`/`insufficient_system_resource`/`aborted` 与缺失 `finish_reason` 全部失败；非法 JSON；缺 `choices`；空修正；HTTP 401 带 vendor message；非 JSON 错误体截断；无 key 可用性错误且不发送；环境值去空白/空白视为无。

### 面板单测（`src/panel.rs` 新增 5 个，`panel_window.rs` 更新 1 个）

精修按钮只在 `Available` 时可用；按钮文案随状态；未写回时精修文本附在本地结果之后且状态区解释原因；已应用时不重复显示文本；不可用原因只在有结果时解释；控件 ID 204 映射到 `COMMAND_REFINE`。

### 全套结果

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test     -Manifest E:\luyin\.worktrees\issue-21\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-21\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task Build    -Manifest E:\luyin\.worktrees\issue-21\app\listen\Cargo.toml
```

- 合并 `integration/v0.2` 后：**156 passed / 0 failed / 13 ignored**（`.artifacts\issue-21\green-full-suite-postmerge.txt`），格式检查退出码 0，release 构建成功。
- 合并前本分支：153 passed / 13 ignored；基线 `70bcf92` 为 122 passed / 12 ignored。旧测试只改了 #20 明确要求接线的两处占位断言（`panel::actions.refine` 恒 `false`、`command_for_control(ID_REFINE) → None`），其余断言未改。
- `#[ignore]` 的 13 个测试都需要真引擎/真模型/真网络；其中新增的 1 个是真 DeepSeek API 测试（见下）。

### 真机运行（无 key，release 二进制）

`.artifacts\issue-21\verify-refine-disabled.ps1`（证据脚本，位于 gitignore 的 artifacts 目录）驱动真实 `listen.exe`：启动 → Ready → 打开面板 → 真实采集一段播放音频 → `--control refine`。**15 项检查全过**：

- `status.txt`：`refine=unavailable`，`refine_reason` 含 `DEEPSEEK_API_KEY`；
- 面板 `精修` 按钮 `IsWindowEnabled=false`，文案 `精修（不可用）`；
- 真实采集得到 `Result`（`current-result.txt` 非空）；`--control refine` 被拒绝，`detail` 明确写 “Refine is unavailable: No DeepSeek API key is configured…”；
- `panel.txt` 含 “精修不可用” 与 key 原因，且与可见控件一致；
- 拒绝后 `current-result.txt` 逐字符不变；应用可 `--control quit` 干净退出。

证据：`.artifacts\issue-21\refine-disabled-verification.json`、`refine-disabled.png`（Result 态：完整结果 + 不可用说明 + 置灰的 `精修（不可用）` 按钮）、`refine-disabled-ready.png`（Ready 态同样置灰）。截图来自真实屏幕（面板不排除录屏）。

### 回归

- `scripts\verify-panel.ps1`（#20 桌面面板验收）在合并后的 release 二进制上 **86 checks, 0 failures**（`.artifacts\issue-21\panel-regression\panel-verification.json`）：面板契约、`panel.txt` 与可见文本逐字符一致、按钮可用性、托盘菜单 6 项、失败/回退自动打开等全部保持。
- 真 API 测试的“无 key 跳过”路径已显式运行：`--ignored` 输出 skip 提示，1 passed、退出码 0（`.artifacts\issue-21\real-refine-skip.txt`）。
- 全套旧测试（采集、追加、重试、设备、长会话、准备期、术语表）除 #20 明确要求接线的两处占位断言外无改动、全部通过。

## 7. 未验证（本机没有 key）

本机未配置 `DEEPSEEK_API_KEY`，因此**没有运行**真实 API 往返测试，以下只能在有 key 的机器上验证：

- 真实 HTTPS/TLS/代理链路（WinHTTP 是否能在目标网络建立连接）；
- 真实模型对具体句子的校正质量（这是模型能力，不是代码契约）；
- 真实 401/429/5xx、超时、断网在端到端链路上的表现（解析与失败分支已由固定 fixture 单测覆盖）；
- 真实请求头的 key 在系统跟踪工具中的呈现（代码层面 key 只进入 `Authorization`，没有任何日志路径）。

运行方式（有 key 时）：

```powershell
$env:DEEPSEEK_API_KEY = 'sk-...'
& E:\luyin\.artifacts\issue-21\run-real-refine.ps1
```

没有 key 时同一脚本会打印 `DEEPSEEK_API_KEY is not set; skipping the real DeepSeek refine test.` 并通过（本机实测：1 passed、退出码 0，`.artifacts\issue-21\real-refine-skip.txt`），所以“已验证/未验证”是显式的：自动化证据是假端口套件，真 API 只覆盖请求形状与解析。

注意：共享构建 helper 的 `-Task Raw` 会把 `--manifest-path` 追加到 `--` 之后，libtest 会报 `Unrecognized option: 'manifest-path'`，所以上面的脚本按 helper 的同一套 toolchain 环境直接调用 cargo。等价命令：

```powershell
& E:\luyin\.tools\cargo\bin\cargo.exe test --offline --locked `
    --manifest-path E:\luyin\.worktrees\issue-21\app\listen\Cargo.toml `
    --test refine_deepseek_api -- --ignored --nocapture
```

（需要 helper 里那套 `CARGO_HOME` / `RUSTUP_TOOLCHAIN` / `RUSTFLAGS` / `CARGO_TARGET_DIR` 环境；`run-real-refine.ps1` 已按该方式设置。）

## 8. 偏差与风险

- **每个结果只精修一次**：`Applied` / `NotApplied` / `Failed` 之后入口保持不可用，直到下一次完整结果。失败不提供“重试精修”，因为重试需要重新读取目标，收益小且会扩大状态机；规格未要求。
- **`NotApplied` 不只表示“用户手改”**：目标不可读、写入未确认也用 `NotApplied`，原因逐字说明；`Failed` 专指精修请求失败。两者都保证本地结果不动、不重复追加。
- **额外的 `status.txt` 键**：除要求的 `refine=` 外增加 `refine_reason=`，让“没有 key / 失败原因”在文件契约里可解释；旧键与写节拍未变。
- **托盘不加重试精修**：规格把精修入口放在展开面板（用户故事 35），托盘保持 #20 的 6 项；#20 回归通过。如果后续希望托盘也能触发，只需加一个菜单项映射到 `Control::Refine`。
- **精修结果不自动打开面板**：用户是从已打开的面板点按钮；`--control refine` 的自动化可读 `panel.txt`。成功追加本来就不该抢屏，#20 的自动打开规则未改。
- **`Refine::Available` 只在 `Phase::Result`**：`NoSpeech` 会保留上一次完整结果，按规格“只有上一次完整结果才可精修”的严格读法，本次没有完整结果时不可精修；状态与原因都可见。
- **共享桌面上的真实采集偶发 NoSpeech**：本机同时有其他音频/代理在跑，第一次真机采集被 VAD 判为无语音，重试后成功（最终截图即成功那次的真实结果）。#20 的面板回归也遇到一次同样的抖动，重跑后 86/86；这不是本改动的逻辑问题。
- **没有运行 `cargo clippy`**：该工具链未安装 clippy 组件（离线环境无法补装），与 #20 相同。
- **`--control refine` 的退出码语义沿用 #20**：命令送达运行实例即 0；被拒绝时的原因在 `status.txt` / 面板可见，不通过退出码区分。

## 9. 与 #20 文档的关系

`docs\issue-20-verification.md` 末尾“留给 #21”清单中的所有项均已接线：`panel::actions.refine` 改为真实可用性、`command_for_control(ID_REFINE)` 返回 `COMMAND_REFINE`、`gui::panel_command` 分发 `Control::Refine`、按钮文案改为可用态；#20 的窗口类名/控件 ID 仍只用于自检，不是自动化契约。#21 新增的契约见本文第 5 节。
