# 重构评审修复（第二轮）：记录、原子写、状态生成与转移记录

2026-11-25。分支 `fix/refactor-review`，工作树 `E:\luyin\.worktrees\fix2`，基线 `5d1014b`
（= `integration/refactor-20261124` 尖端）。主工作树 `E:\luyin` 的 7 份用户 dirty 文档全程未被
add、commit、stash 或 `reset --hard`（只读过一次 `git status`）。

基线：**353 passed / 0 failed / 16 ignored**（29 个 target）。修复后：**359 passed / 0 failed / 16 ignored**
（+6：F1 三条、F2 一条、F6 两条）。`-Task CheckFormat` 无输出（日志长度 0）。`scripts\` **0 diff**；
`status.txt` 68 键 / 29 required 的键集合、顺序与四段格式串逐字未变（`status::tests` 原样通过）。

## 结论一览

| # | 条目 | 结果 |
| ---: | --- | --- |
| F1 | 未知 state/delivery 拼写被猜测并写回 | **已修复**：未知拼写使整份索引不可读、报告原始拼写、拒绝一切写盘（ADR 0005）；修复文件后重启即恢复正常写入 |
| F2 | 精修改良路径的 `current-result.txt` 非原子写 | **已修复**：两处 `fs::write` 收进一个 `save_current_result` → `durable::atomic_write`；拒绝替换时旧文件完整、无临时文件 |
| F3 | W1（#43）缺验证文档 | **已补齐**：`docs\issue-43-verification.md` |
| F4 | 两张结论表的「通过」应为「部分」 | **已修复**：#46 增加「段存储/恢复的窄 seam = 部分」一行；#49 的 poll 无 read_dir 一行改为 **部分** 并注明保留的写时检查 |
| F5 | 程序文档缺三件事 | **已修复**：决策 3 的已知例外 2 → 3（补 W3 孤儿文字读 `delivery.json`）；记录 W7 的 `FactsKey` 范围扩张（理由/风险）；记录 `refine=unconfirmed` 值域扩展（并同步 #24 口径）；`StartMarker::IfPossible` 记入「待决定」 |
| F6 | `gui.rs` 手列 19 个字段决定重写 | **已修复**：`Snapshot` 派生 `PartialEq` + `status::report_inputs_differ` 比较 `(Snapshot, FactsKey)` 值；手列字段删除；两条测试钉住三元组任一变化 |
| F7 | `Record::Status` 与 `Record::StatusIfChanged` 在应用点等价 | **已修复**：拆成输入 `RecordRequest`（含 `StatusIfChanged`）与输出 `Record`（4 个真实写）；`decide` 解析条件；`apply` 每个写一个分支 |

## F1 未知 state/delivery 拼写不再被猜测

**改前**（`app\listen\src\segments.rs`）：

- `state_name::deserialize`：`Ok(SegmentState::from_name(&name).unwrap_or(SegmentState::Failed))`
- `delivery_name::deserialize`：`DeliveryState::from_name(&name).map_or(Delivery::Uncertain, Delivery::from)`
- 加载成功、`store.problem()` 为空；之后任意 `apply` / `begin` / `persist` 把 `failed` / `uncertain`
  写回 `segments.json`——正是 ADR 0005 禁止的「静默重写/猜测状态」。

**改后**：

- 两个 serde 适配器对未知拼写返回**错误**，错误文本点名原始拼写（`unknown segment state "archived"` /
  `unknown delivery "maybe"`），不产生任何替代值。
- `durable::open_versioned` 把它归入既有的「记录不可读」降级：`store.problem()` 得到带原始拼写的说明，
  `persist()` 在 `problem` 存在时拒绝一切写盘，并只报告一次（既有 `reported` 机制）。
- `segments.json` **逐字节不变**；恢复扫描仍能从运行时目录把 `segment-*.txt` / `session-*` 当孤儿重建
  （与损坏索引同一条路径）。
- `SegmentState` 的文档删掉「未知名字回退到 failed」；`segments.rs` 注释明确与
  `delivery.rs::DeliveryState::from_name` 的「never guessed into a safer-looking state」一致。

**为什么拒绝整份索引，而不是把原始字符串留在内存继续用**：

- 保留原始值需要给 `Segment` 加旁路字段或给 `SegmentState`/`Delivery` 加「未知」变体；随后**恢复扫描、
  面板、status.txt 都必须对「未知」做一次产品决定**。在这个决定存在之前，任何继续使用该段的行为
  （恢复、重试、显示为 failed）都是猜测，ADR 0005 明确禁止。
- 整份拒绝与「未知版本 / JSON 损坏」是同一条 fail-closed 路径：文件不动、报告一次、不写盘。
  代价是**本次启动不使用该索引**（逐段术语快照与费用在修复前不显示）；文本与音频仍由孤儿扫描恢复。
  用户把那一处拼写改对并重启后，写入立即恢复。

**红 / 绿**：

- **红（真红，回退/变异实验）**：把两个适配器临时改回基线写法后，三条新测试全部失败（日志 `red-f1.log`）：
  - `an_unknown_state_spelling_is_reported_and_never_written_back` →
    `an unknown state spelling must be reported`
  - `an_unknown_delivery_spelling_is_reported_and_never_written_back` →
    `an unknown delivery spelling must be reported`
  - `a_hand_repaired_index_with_a_known_spelling_loads_and_writes_again` →
    `the hand edit starts from the degraded record`（基线根本不认为该记录降级）
  同轮其余 90 passed。实验后源码恢复到修复版并核对哈希（`12E521C4…`），随后 `.green-1.log` 全绿。
- **绿**：`.green-1.log`，3 条新测试 ok，全量 359 / 0 / 16。

**新增测试**（`segments::tests`，3 条）：未知 state 不写盘、未知 delivery 不写盘、人手修好后正常继续。
**被改的既有测试**：无。

## F2 `current-result.txt` 的两处精修写改为原子写

**改前**：`lib.rs:1122` 与 `lib.rs:1177` 各有一处 `fs::write(&current_result, &refined)`。恢复扫描
（`lib.rs:1387` 一带）直接读这个文件；直接写会在打开时截断，崩溃或中断可能留下半截文件。

**改后**：

- 新增一个私有函数 `save_current_result(path, text)`，函数体只有
  `durable::atomic_write(path, text.as_bytes())`。
- 两条精修路径（无目标的 fallback 精修、已确认目标的精修改良）都调用它——**两条写路径调用同一个函数**。
- 与 `lib.rs` 原有另外两处 `current-result.txt` 写（清空与保存完整结果，已走 `durable::atomic_write`）
  现在同一语义。`Select-String -Path app\listen\src\lib.rs -Pattern 'fs::write\(&current_result'` 结果 **0**。

**新增测试**：`atomic_current_result_tests::a_refused_current_result_replace_keeps_the_previous_file_and_leaves_no_temporary`。
把目标文件设为只读（Windows `MoveFileExW(REPLACE_EXISTING)` 对只读目标返回 `ERROR_ACCESS_DENIED`，
已在本机用 Win32 探针确认 `ok=False err=5`），调用 `save_current_result`：

1. 返回错误，但旧的完整文件逐字不变（不是被截断的新文件）；
2. 目录里没有 `current-result.<id>.tmp` 残留（失败的替换会删除临时文件）；
3. 去掉只读后再次调用成功，文件被完整替换。

**红证据的边界（如实记录）**：这条测试无法做成稳定红。用旧的 `fs::write` 在同样的「目标只读」场景下
也会在打开时直接失败、旧文件同样不动，因此这个失败模式区分不出两种实现；真要制造「写到一半失败」需要
给 `durable::atomic_write` 加故障注入 seam（为一条测试增加生产面），本轮不做。代替证据是：

- 代码级：`fs::write(&current_result` 为 0；两条生产精修路径都是 `save_current_result(`；该函数唯一
  函数体是 `durable::atomic_write`（`Select-String` 输出见上，日志在 `f2-code-evidence.txt`）。
- 行为级：上面的「拒绝替换」测试钉住「旧文件完整 + 无半截临时文件 + 修复后可写」。

**被改的既有测试**：无（`refine_backfill.rs` 的既有精修测试全部原样通过）。

## F3 W1（#43）验证文档

新建 `docs\issue-43-verification.md`，按其余七波的格式补齐：

- 结论表：默认 `preparation=Ready` 的 fail-closed 解释、默认 `retry` 返回 `RETRY_UNSUPPORTED` 且不启动 session、
  两个生产 adapter（`LocalSessions` / `CloudSessions`）都显式实现这两个方法所以路径不变、剧本 adapter 的形状、
  6 份 fake 复用、`status.txt` 29 键不变、测试 306 → **309**、`-Task CheckFormat` 无输出。
- 红/绿：接口红（`MinimalSessions` 只实现 `start`，旧 trait 还要求 `preparation`/`retry`，E0046）与绿
  （3 条 `recognition_testing` 测试）；并明确标注「W1 当时没有日志，本文件是修复分支按合并提交与基线源码补记，
  不冒充实测日志」。
- `RETRY_UNSUPPORTED` 是文案单一来源（`recognition.rs:151`；trait 默认实现与 `testing.rs` 都用它；
  集成测试断言可见句包含 `does not support retrying`；生产 adapter 有自己的 retry 文案）。
- `restart_recovery` 为何保留自己的 adapter（记录重试音频字节、术语快照、PCM spool 写入）。

## F4 两张结论表的「部分」

- `docs\issue-46-verification.md`：结论表新增一行「段存储 / 恢复的窄 seam」= **部分**，说明转移层
  （`transition::decide` / `Cause::lines` 纯函数 + 8 个单测）已完成，`segments.rs` / `recovery.rs` 仍无注入
  seam，与文末「本波没做完的边界」第 1 条一致。
- `docs\issue-49-verification.md`：「`poll()` 路径不再做测量 I/O」一行从「通过」改为 **部分**，注明仍保留
  `save_complete_text → check_text_budget → text_bytes`（`lib.rs:3277` → `storage.rs:355`）这一处写时
  `read_dir`：每次保存完整结果一次，不在 10 ms tick 上（原有「偏差 1」）。

## F5 程序文档补齐

`docs\refactor-program-2026-11-24.md`：

- **决策 3** 的已知例外从两处扩到三处：补「W3 起孤儿文字改读 `delivery.json`，恢复可得到
  `Appended`/`Fallback`」，依据 #45 票面「顺带修掉 W2 留下的那条」（另见 `issue-45-verification.md`
  偏差 8 与提交 `703e0e6`）。
- 新增小节记录 **W7 超出票面的范围扩张**：`status.txt` 生成改为变化驱动（`FactsKey`），写出理由（`poll`
  里唯一的非状态工作就是 68 键格式化）与风险（`Snapshot` 内字段仍靠手列，W6/W7 已漏补三次
  `endpoint`/`mode`/`current_sequence`），并注明本修复 F6 已把该比较改成值比较、风险已消除。
- 新增小节记录 **`refine` 键值域扩展**：W2/#44 增加 `unconfirmed`；键名/键数/顺序未变，只扩值域，
  原因仍只进 `refine_reason`。同步在 `docs\issue-24-verification.md` 的「口径」加一句：按取值集合断言的
  验收必须接受 `unconfirmed`。
- 新增「待决定（用户）」一节，记 `StartMarker::IfPossible`。

另外把程序文档里与新事实矛盾的旧文更正为「这些负载敏感测试」（原文写「这两条」，但清单是三条）。

## F6 变更检测由值本身比较

**改前**：`gui.rs:384-406` 手列 19 个 `Snapshot` 字段与 `state` 逐项比较，决定是否重新生成/写
`status.txt`。W6/W7 已因为漏列补过三次（`endpoint` / `mode` / `current_sequence`）。

**改后**：

- `Snapshot` 派生 `PartialEq`（它所有字段类型本来都有 `PartialEq`）。
- `status::report_inputs_differ(previous: Option<(&Snapshot, &FactsKey)>, current: (&Snapshot, &FactsKey))`
  是唯一的重写判定：`previous != Some(current)`。`FactsKey` 是 `HostFacts` + `WindowFacts` 的逐字段副本
  （`status::tests::the_cheap_report_key_changes_with_every_fact_outside_the_snapshot` 已钉住 8 个事实），
  所以这个比较就是 `(Snapshot, HostFacts, WindowFacts)` 三元组的值比较。
- `gui.rs` 删除整段手列字段，调用该函数；静止 tick（三个输入都不变）不生成、不写盘；任一输入变化才重新生成，
  且仍只在 68 键文本真的变化时写文件。
- `status.txt` 的键名/键数/顺序没有动（`status::report` 未改）。

**边界（写清楚，避免误解）**：`Snapshot` 里有 `report` 不读取的字段（例如 `progress`）。它的变化会触发重新生成
（因为三元组值变了），但生成的文本相同，所以**不会写文件**。这是有意的「只在文本变化时写盘」，不是漏输入；
反过来，任何 `report` 读取的值变化都会产生不同文本并写盘。

**新增测试**（`status::tests`，2 条，bin target）：

- `a_change_in_the_snapshot_host_facts_or_window_facts_requires_a_new_report`：三组输入各改一个值，
  `report_inputs_differ` 都为真且生成的文本真的变化；完全相同则为假。
- `the_fields_the_hand_written_comparison_missed_now_change_the_report`：`endpoint` / `mode` /
  `current_sequence` 三个历史漏列字段现在都参与判定并改变 `status.txt` 文本（`pending_results` 按身份排除
  当前结果：3 个待取结果在有 `current_sequence` 时显示 2）。

**红证据**：F6 是结构收敛，没有可稳定构造的红——基线的手列字段在 W6/W7 后已经补齐，漏字段只会在**下一次**
给 `Snapshot` 加字段时出现。两条测试是这次的防线：若判定退回「只比某几个字段」，它们会失败；若手列字段再次
漏项，新增的测试会通过但新字段仍可能漏，所以真正的保证来自**删掉了手列、改为 `PartialEq`**这一结构。

## F7 `Record::Status` 与 `StatusIfChanged` 的等价

**改前**：`transition::Record` 同时承担两个阶段：`Transition.record` 的输入请求与 `Decision.record` 的输出。
`decide` 已把 `StatusIfChanged` 归一成 `Status` / `Keep`，所以 `apply` 里
`Record::Status | Record::StatusIfChanged => persist_segment` 的第二个模式**永远不会到达**，是死分支。

**改后**（选择「拆类型」，而不是改名）：

- `RecordRequest`（输入，5 个变体，含 `StatusIfChanged`）：`Transition.record` 的类型。
- `Record`（输出，4 个变体：`Keep`/`Status`/`Delivered`/`Remove`）：`Decision.record` 的类型，也是
  `Application::apply` 唯一执行的类型。`apply` 每个真实写一个分支，没有两个变体做同一件事。
- `decide` 的条件规则保留并只在这里出现：`StatusIfChanged + !state_changed → Keep`；否则 → `Status`。
- 把请求传给 `apply` 现在是编译错误（类型不同），差异由类型强制，而不是靠注释。

**为什么不是改名**：只改名会留下同一枚举里两个阶段、`apply` 里仍有一个死分支，等于把问题藏进名字；
而删掉 `StatusIfChanged` 会丢掉「进展 tick 只在状态变化时落盘」这条规则（`note_segment_status` 依赖它）。
拆成两个类型既保住规则，又让输出类型里不再有重复语义。

**被改的既有测试**（`app\listen\src\transition.rs` 的单元测试，**只改构造类型，断言一行未改**）：

| 测试/位置 | 改动 |
| --- | --- |
| `a_transition_decision_is_independent_of_whether_the_segment_is_newest` | `record: Record::Remove` → `RecordRequest::Remove` |
| `the_record_follows_the_transition_and_only_a_state_change_forces_a_tick_write` | `record: Record::StatusIfChanged` → `RecordRequest::StatusIfChanged`；`unchanged.record == Record::Keep`、`changed.record == Record::Status` 断言不变 |
| 测试辅助 `transition_with(cause)` | `Record::Keep` → `RecordRequest::Keep`（所有使用它的测试行为不变） |

## 被动过的既有测试（逐条，全仓）

| 文件 | 测试 | 改动 | 原因 |
| --- | --- | --- | --- |
| `src\transition.rs` | `a_transition_decision_is_independent_of_whether_the_segment_is_newest` | 构造值类型 `Record` → `RecordRequest` | F7 两阶段拆类型 |
| `src\transition.rs` | `the_record_follows_the_transition_and_only_a_state_change_forces_a_tick_write` | 同上（断言不变） | 同上 |
| `src\transition.rs` | 辅助函数 `transition_with` | `Record::Keep` → `RecordRequest::Keep` | 同上 |

其余 `tests\*.rs` **0 diff**；`segments.rs` / `status.rs` / `lib.rs` 的既有测试只新增、未修改；`panel.rs` /
`capsule.rs` / `main.rs` / `scripts\` 0 diff。`src\status.rs` 的测试 import 增加 `CaptureEndpoint`（新测试用），
不涉及既有断言。

## 没有修的东西 + 理由

1. **W4 剩下的三块**（`start_capture` 手抄 12 个 `snapshot.*`、`delete_segment` 的 3 处直写、段存储/恢复的
   窄 seam）没有动：任务明确说明已另开票跟踪；把它们塞进本修复会扩大状态转移单点的范围，并再次触碰
   `Application` 的大面积状态赋值。
2. **`StartMarker::IfPossible`**（正常交付在 `started` 无法落盘时仍写目标）没有改代码：这是待用户决定的
   安全取舍，只在程序文档「待决定」记了一条。**原表述**：「正常交付在 `started` 标记无法落盘时仍会写目标。
   这是待用户决定的安全取舍——要么保持『尽量写』的现有行为，要么改成『标记落不了盘就拒绝写目标』。
   本修复分支只记录，不改代码。」
3. **F1 选择「整份索引拒绝」而不是把原始拼写留在内存继续用**：理由见 F1（在没有「未知状态」的产品语义前，
   继续使用该段就是猜测；整份拒绝与未知版本/损坏记录同一条 fail-closed 路径）。这是有意的降级代价，
   不是遗漏。
4. **F2 的「写到一半失败」没有做故障注入 seam**：理由见 F2 边界；代之以「两条写路径同一函数」的代码级证据
   与「拒绝替换不留半截文件」的行为测试。
5. **`status.txt` 的键名/键数/顺序没有动**：任务硬约束；`status::report` 一行未改，68 键/29 required 测试原样通过。
6. **`scripts\`、`gui.rs` 的 `pre_existing` 未用 import `UsageSummary` 警告未顺手清**：该警告在基线就存在
   （`.baseline-test.log` 同样 3 条 warning），与本轮修复无关，避免扩大 diff。

## 提交与合并

- 代码 + 测试提交：`dea3ebc`（F1/F2/F6/F7）。
- 文档提交：本文件与 F3/F4/F5 的文档在同一提交（见 `git log -- docs/issue-review-fixes-2.md`）。
- `git merge integration/refactor-20261124`：`Already up to date.`（integration 仍是 `5d1014b`，无冲突）。
- 合并后复跑 `-Task Test`：**359 / 0 / 16**；`-Task CheckFormat`：无输出。工作树干净，无残留 `listen` 进程。

## 证据文件

| 文件 | 内容 |
| --- | --- |
| `E:\luyin\.artifacts\fix2\baseline.log` | 基线 353 / 0 / 16 |
| `E:\luyin\.artifacts\fix2\red-f1.log` | F1 回退实验：3 条新测试失败 |
| `E:\luyin\.artifacts\fix2\green-1.log` | 修复后 359 / 0 / 16，6 条新测试 ok |
| `E:\luyin\.artifacts\fix2\checkformat.log` | `-Task CheckFormat` 0 输出 |
| `E:\luyin\.artifacts\fix2\f2-code-evidence.txt` | F2 的两条调用点与唯一 `atomic_write` 证据 |
| `E:\luyin\.artifacts\fix2\green-post-merge.log` | 合并后复跑 359 / 0 / 16 |
