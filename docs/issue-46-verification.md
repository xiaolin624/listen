# #46 重构验证：段状态转移单点与更窄的段 seam（W4）

分支 `implement/w4-transition-point`，基线 `0665d96`（= `integration/refactor-20261124` 尖端，W1–W3 已合入）。
票：GitHub issue #46；计划：`docs/refactor-program-2026-11-24.md` 第 4 波；证据：`%TEMP%\listen-arch-20261124\core.md` 的两个候选。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 一次转移是一个值 | 通过 | `src/transition.rs::Transition { sequence, state, session, terminal, stopped, finalizing, cause, record, preparation_changed }`；`Cause` 有 9 个变体，各自拥有句子规则（`Cause::lines`）与快照读数规则（`standing`） |
| 唯一应用点 | 通过 | `Application::apply`（`lib.rs`）是唯一派生 `snapshot.phase` / `snapshot.recognition` / `snapshot.detail` / `segments_dirty` / 记录与持久化的地方；规则本身是纯函数 `transition::decide` |
| 「只有最新段驱动快照」只在一处判断 | 通过 | `index + 1 == self.segments.len()` **12 → 1**，唯一一处是 `Application::is_newest`；`apply` 用它算出 `primary` 并作为**入参**交给 `decide`（`Context`），调用者拿 `Applied { primary }` 的返回值，不再自行重算 |
| 转移表覆盖每类转移 | 通过 | `tests/segment_transition_table.rs`：12 行（session event / delivery outcome / storage stop / fail / retry / start refusal），每行断言 `(phase, recognition, detail 类别, delivery, segments.json 的 state)` |
| `snapshot.detail` 收敛 | 通过（实测） | `snapshot.detail =` **47 → 1**；`snapshot.detail.push*` **10 → 4**；四条都只在 `Application::say` 里。`facts.detail =` **12 → 3**、`facts.detail.push*` **10 → 1** |
| `-Task Test` | 通过 | **332 passed / 0 failed / 16 ignored**（基线 322/0/16，+10 = 8 个 `transition` 单测 + 2 个转移表测试） |
| `-Task CheckFormat` | 通过 | 无输出（exit 0） |
| `status.txt` 29 键 | 通过 | `gui.rs`、`panel.rs`、`capsule.rs`、`main.rs` **0 diff**；只改了 `src/lib.rs` + 新增两个文件 |
| 桌面验收脚本不改松 | 通过 | `scripts\*.ps1` **0 diff** |

## 转移表（characterization，重构前后都绿）

`tests/segment_transition_table.rs::every_segment_transition_leaves_the_documented_phase_recognition_sentence_and_record`
用表驱动跑 12 行；每行是一次全新的 `Application`（脚本化识别适配器 + 受控编辑器目标，**没有音频设备**），
断言列 = `phase` / `recognition` / detail 类别 / `delivery` / `segments.json` 里该段的 `state`（`None` = 记录已删）。

| # | 转移 | 触发 | phase | recognition | detail 类别 | 落盘 |
| ---: | --- | --- | --- | --- | --- | --- |
| 1 | 开始一段 | `Control::Start` | `Capturing` | `Capturing` | 适配器进度句 | `capturing` |
| 2 | 稳定句 | `SessionUpdate::Stable` | `Capturing` | `Stable` | 适配器进度句（不变） | `capturing` |
| 3 | 等待网络 | 适配器 status = `WaitingForNetwork` | `Capturing` | `WaitingForNetwork` | 适配器进度句 | `waiting_for_network` |
| 4 | 停止一段 | `Control::Stop` | `Processing` | `AwaitingFinal` | 适配器进度句 | `processing` |
| 5 | 完整结果 + 目标的确认追加 | `Finished` → delivery `Appended` | `Result` | `Complete` | 结果句 + `Playback:` | 记录删除 |
| 6 | 完整结果但没有目标 | `Finished` → delivery `Fallback` | `Result` | `Complete` | 「没有可用目标」句 | `result`（待取结果保留） |
| 7 | 识别失败 | `SessionUpdate::Failed` | `Failure` | `Failed` | 原因 + 保留音频路径 | `failed` |
| 8 | 无语音 | `Finished` + 空白文字 | `NoSpeech` | `Complete` | 「没有有效语音」句 | 记录删除 |
| 9 | 存储上限停止采集 | 待处理音频越过预算 | `Processing` | `AwaitingFinal` | 存储停止原因 | `processing` |
| 10 | 存储上限拒绝开始 | 越预算时 `Control::Start` | `Failure` | `Failed` | 「Storage is full…」 | 仍是上一条的 `failed`，未新建 session |
| 11 | 热键注册失败 | 构造期 `Err` + `Control::Start` | `Failure` | `Failed` | 「Global hotkey unavailable」 | 无记录 |
| 12 | 没有可重试音频时重试 | `Control::Retry` | `Failure`（不变） | `Failed`（不变） | 「No retained audio…」 | `failed`（不变） |

另加 `a_storage_stop_reason_survives_later_status_refreshes`：存储停止后连续 5 次 poll，原因句不被适配器进度覆盖
（「stopped 时保留」规则；`Stopped` 类转移把原因同时写进 `storage_note`，后续 `Progress` 把它拼回段自己的句子）。

## 红 / 绿证据

全部命令都是 `.\scripts\build-listen.ps1 -Task Test`（迭代时用同一 toolchain 直接跑单个 test target，参数与脚本逐字相同）；
日志在 `E:\luyin\.worktrees\w4\.artifacts\`（未提交）。

1. **characterization（先绿，重构前）** — 基线上 `segment_transition_table` **2 passed / 0 failed**。
   转移表故意先对着旧代码写：它只通过 `Application` 的公开 seam 观察 `Snapshot` 与 `segments.json`，
   不碰任何私有方法，所以重构后仍能原样跑。**这次没有出现红**——表征测试本来就不该有红；
   红出现在窄 seam（下一项）。
2. **红（窄 seam，真红）** — `transition::tests` 在 `decide` 还是 `todo!()` 时：
   `test result: FAILED. 4 passed; 4 failed`，失败原因全是
   `not yet implemented: implemented after the red run: the frozen rules`
   （4 条句子组合测试先绿：`Cause::lines` 与 `decide` 是分开写的）。
3. **绿（窄 seam）** — 实现 `decide` 后 `transition::tests` **8 passed / 0 failed**。
4. **重构后转移表复跑** — `segment_transition_table` **2 passed**；全仓 `-Task Test` **332 passed / 0 failed / 16 ignored**。

### 过程中出现过的唯一一次红（不是语义变更）

首次全量跑挂在 `backfill_timing::an_older_segment_never_delays_a_newer_segments_save_or_delivery`：
`Finished` 事件后 `recognition` 停在 `AwaitingFinal`。原因是我把 `Stable` 与 `Completed` 合并进同一个
`Standing::Recognition` 分支并保留了 `Stable` 的 `!stopped` 守卫；但 `Completed` 恰恰是从「已停止的段」到达的。
修正：`Standing::Recognition` 无条件设置 `recognition`（`Stable` 事件本身的到达守卫仍在 `apply_session_event` 里）。
这是实现错误，不是语义改动；转移表与既有测试均未改。

另：`glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 性能阈值）在一次全量跑中
以 `117.4417ms` 失败（机器负载），单独复跑 3 次 40–50 ms 全绿，随后全量跑 `75.6ms` 绿。W3 记录过同一 flake
（基线同负载同样失败），不是本波回归。

## 转移值与应用点的形状（一句话）

一个 `Transition` 说「哪个段（`sequence`）、变成什么事实（`state`/`session`/`terminal`/`stopped`/`finalizing`）、为什么（`Cause`）」，
`Application::apply` 取一次 `is_newest`，从 `Cause::lines` 拿到「段自己的句子 / 用户读到的句子」，
交给纯函数 `decide(transition, lines, context) -> Decision { phase, recognition, detail, record, state_changed }`，
然后只做四件事：写段事实、若 `primary` 写快照、必要时置 `segments_dirty`、按 `Record` 写/删记录。

`Cause` 的 9 个变体：`Progress` / `Stable` / `Completed` / `Silent` / `Stopped` / `Delivered` / `NoSpeech` / `Failed` / `Restated`。`CleanupFailed` 是第 10 个——它只产段自己的句子，快照句归操作级失败。

## `detail` 收敛（实测，可复核）

命令（在 `app\listen` 下）：

```powershell
(Select-String -Path src\lib.rs -Pattern 'snapshot\.detail\s*=' | Measure-Object).Count
(Select-String -Path src\lib.rs -Pattern 'snapshot\.detail\.push' | Measure-Object).Count
(Select-String -Path src\lib.rs -Pattern 'index \+ 1 == self\.segments\.len\(\)' | Measure-Object).Count
(Select-String -Path src\lib.rs -Pattern 'self\.persist_segment\(' | Measure-Object).Count
```

| 计数 | 改前 `0665d96` | 改后 | 改后位置 |
| --- | ---: | ---: | --- |
| `snapshot.detail =` | **47** | **1** | `Application::say` 的 `Sentence::Replace` 分支 |
| `snapshot.detail.push*` | **10** | **4** | `say` 的 `Push` / `Line` / `Once` 分支 |
| `facts.detail =` | **12** | **3** | `apply` 写转移产出的段句；`delete_segment` 的删除失败句；`apply_no_speech` 的非 primary 清理失败句 |
| `facts.detail.push*` | **10** | **1** | `recovered_segment` 构造恢复段时补一句「术语表快照没记下」——它不是状态转移，是重启扫描的产物 |
| `index + 1 == self.segments.len()` | **12** | **1** | `Application::is_newest` |
| `self.persist_segment(` | **14** | **3** | `apply` 的 `Record::Status`；`retry` 里「保留音频已不在磁盘」的本地清理；`delete_segment` 的删除失败 |

口径说明：程序文档记录的「46 处赋值 + 17 处追加」是评审时的计数（含 `detail.push_str` 与 `panel`/`capsule` 之外的写法）；
本表用上面四条命令在同一基线上复测，得 **47 + 10**。两个数字都记在这里，避免下一波拿不同口径对账。

`note_segment_status` 里原来「适配器句子 + `storage_note` + `recovery_note`」的三段拼接、`apply_delivery` 里的
「结果句 + recovery 句」、`fail_segment` 里的「失败句 + 未完整转写 + recovery 句」现在都在 `Cause::lines`；
「stopped 保留 / waiting 刷新 / Stable 不被降级 / recovery 事实不被抹掉」四条规则在 `decide` 与 `Notes` 里。

## 被动过的既有测试（逐条）

**无。** `git status` 只有 `app/listen/src/lib.rs`（修改）与 `src/transition.rs`、`tests/segment_transition_table.rs`（新增）。
W1 的 `testing::ScriptedSessions`、W3 的 `segments::Segment`/`Change`、所有 `tests/*.rs` 均未改。

## 偏差（逐条）

1. **`fail_segment` 多了两个句子附件参数**（`advice`、`summary`）并返回 `Applied`。原因是同一类失败的句子在三条路径上
   有细微差别：重试建议（`\n` 开头）、恢复失败后「录音已保留、不会自动重试」（`recovery::KEPT_AFTER_FAILED_RESUME`，以空格开头、无换行）、
   以及恢复摘要只上快照。为保持句子逐字不变，把「附加说明」作为值传入，由 `Cause::Failed` 按原顺序拼接。
2. **`fail` 拆成 `fail_state` + `fail`**：`fail_state` 只做全局后果（phase/recognition/refine/delivery memory/retained audio），
   句子由产出它的转移负责；`fail` 保留给「没有段转移的操作级失败」（准备失败、无语音清理失败、交付清理失败、启动失败）。
   行为不变（既有断言这些句子的测试全绿）。
3. **`Cause::CleanupFailed` 没有快照句**（`Lines::snapshot = None`）。旧代码里交付成功但目录删不掉时，段自己的句子和
   快照的句子本来就是两句不同的话（后者来自 `fail("Could not save the current result or clean temporary audio…")`）。
   把两者硬合成一句会改语义，所以段句归转移、快照句归操作级失败。
4. **`Record::Delivered/Remove` 仍由 `apply_delivery` 选**：`keep_record = 交付是 Fallback/Uncertain || 清理失败` 是
   交付层的策略输入，不是快照派生。`decide` 决定「写/删/不写」，`apply` 执行；选择哪一个由调用点按这条既有规则传入。
5. **`delete_segment` 里仍有 3 处直接写**：删掉句柄时 `terminal = true`、删除失败时把 in-flight 状态改成 `Failed`
   （`facts.state` + `session_state`）。删除不是本票枚举的五类转移之一，本波没有把它塞进 `Transition`（见「留给后续票」）。
6. **`Cause::Failed` 的 advice/summary 用动词原样追加（不补换行）**。旧代码里 `KEPT_AFTER_FAILED_RESUME` 是以空格开头、
   接在同一行上的；统一补 `\n` 会改用户可见排版。调用者自己带分隔符，`Cause::Failed` 不发明格式。

## 本波没做完的边界（留给后续票）

1. **窄 seam 只做到「转移」这一层**（第三优先，部分完成）。`transition::decide` / `Cause::lines` 是纯函数：
   8 个单测不需要 `Application`、runtime 目录、设备或 poll 循环（判据「至少 3 个测试不再需要完整 Application 组装」已满足）。
   但**段存储（`segments.rs`）与重启扫描（`recovery.rs`）仍没有自己的注入 seam**：
   `recovery::plan(runtime, records, deliveries)` 已经是纯的，只差 3 条「一个临时目录 + 手写 `Segment`」的单测；
   `SegmentStore` 的 writable/degraded 语义仍只能靠 `segments.rs` 现有 4 个单测。这是下一张窄 seam 票的第一件事。
2. **`SegmentSession` 的四个布尔/状态字段仍未在类型上绑定**：`session_state`/`stopped`/`finalizing`/`terminal` 现在与
   `facts.state` 在同一个 `Transition` 里移动（不再各写各的），但「`terminal` ⇒ 没有句柄」「`finalizing` ⇒ 有交付在跑」
   这些不变量还没有类型表达；W3 说的「不同步还需显式化」只完成了「同一次写入」这一半。
3. **`start_capture` 仍手抄 12 个 `snapshot.*` 字段**（`endpoint`/`recognition`/`progress`/`detail`/`phase`/`delivery`/
   `retained_audio`/`retry_available`/`delivery_memory`/`refined_text`/`refine`/`timing`）。开始一段不是「已有段的转移」，
   本波没有为它造第二个值；它的 `detail` 已经走 `say`，但其余字段仍是手写。
4. **`delete_segment` 的直接写**（见偏差 5）。
5. **交付的 keep/remove 记录策略仍在调用点**（见偏差 4）；若要收进 `decide`，需要先把「清理结果」变成转移的一部分
   （即先删目录、再产出转移），那会改变「先写记录还是先删目录」的顺序，属于独立评审。
6. **`note_segment_status` 的进展句仍然每 tick 重写 `facts.detail`**（与旧代码一致）。这正是 `Record::StatusIfChanged`
   只在状态变化时落盘的原因；要减少段内存在内存里的字符串改动，需要另一条「进展句不落段」的产品决定。
