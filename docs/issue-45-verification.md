# #45 重构验证：记录层读写一对与单一权威段表示（W3）

分支 `implement/w3-record-layer`，基线 `6e0fa81`（= `integration/refactor-20261124` 尖端，W1/W2 已合入）。
票：GitHub issue #45；计划：`docs/refactor-program-2026-11-24.md` 第 3 波；ADR：`docs/adr/0005-durable-segment-and-delivery-records.md`。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| `record_mut` 不存在 | 通过 | 全仓 0 处（`segments.rs` 模块文档里只有一句"没有 `record_mut`"的说明） |
| 三步舞在调用者处消失 | 通过 | `lib.rs` 里 7 处 `record_mut` + 5 处「改字段 / `persist` / 查 `problem`」全部变成 7 种命名变更 `Change`；`persist_segment` 收成一条 `Change::Status`；`Application::record` 是唯一应用点（同时写运行期副本，二者不会漂移） |
| 写失败只报一次 / 降级不写盘 | 通过 | `SegmentStore::apply` 在 `problem` 存在时不写、只报一次（`reported` 标记）；单测 `a_degraded_index_is_never_rewritten_and_reports_the_degradation_once`、`a_failed_index_write_is_reported_once_and_the_change_is_kept` |
| 原子替换回到持久化层 | 通过 | `durable::atomic_write` + `durable::replace_file`（含共享冲突重试）；9 个调用点（`delivery.rs` 2、`lib.rs` 2、`segments.rs` 2、`cloud/pcm.rs` 2、`cloud/budget.rs` 1）；`cloud::budget` 里不再有这两个函数，也没有 `pub(crate) use ... atomic_write` |
| 一份权威表示 + 四方向 | 通过 | `segments::Segment`；位置见下表；`SegmentSession` 只留运行期字段（handle/target/`Instant`/验证过的目录/计时） |
| 加一个字段的改动面 | 通过（实测） | 改前 **15 处 / 3 个生产文件 / 28 行**；改后 **4 处 / 2 个文件 / 17 行**，其中入盘、出盘、恢复扫描 **0 处**（详见「加一个字段」） |
| W2 遗留：恢复读到已确认的精修 `appended` | 通过 | 新测试 `a_confirmed_refine_on_a_drained_segment_is_read_after_a_restart_not_reported_as_never_written`；`Plan::adopt` 对孤儿文字读 `delivery.json` |
| 磁盘格式 / `schema_version` | 通过 | 冻结形状单测 `the_stored_index_keeps_its_frozen_version_1_shape`；旧（无 `glossary_terms`）索引单测；`restart_recovery.rs` 手写旧 `segments.json` / `delivery.json` 全部原样通过；`STORE_VERSION` 仍为 1，无字段改名/删除 |
| `status.txt` 29 键 | 通过 | `gui.rs`、`panel.rs`、`capsule.rs` **0 diff** |
| `-Task Test` | 通过 | **322 passed / 0 failed / 16 ignored**（基线 316/0/16，+6）；最终合入后在 `final-20.log` 复跑同数 |
| `-Task CheckFormat` | 通过 | 无输出（exit 0）；`gui.rs` 0 diff，`status.txt` 键集合与格式串逐字未变 |

## 红 / 绿证据

全部命令都是 `.\scripts\build-listen.ps1 -Task Test`；日志在 `E:\luyin\.worktrees\w3\.artifacts\`（未提交）。

1. **characterization（先绿）** — `baseline.log`
   基线 316 passed / 0 failed / 16 ignored；其中已有三处最关键的 `record_mut` 调用点行为：
   `multi_segment.rs::every_segment_saves_its_own_audio_text_state_and_cost_without_overwriting_an_earlier_one`
   （交付后索引条目被删、`fallback` 记录留下）、
   `storage_bounds.rs::deleting_a_selected_pending_result_removes_only_it_and_a_late_event_cannot_resurrect_it`
   （删除后索引只剩另一条）、
   `restart_recovery.rs::corrupt_state_does_not_hide_saved_results_or_lose_an_unindexed_recording`
   （恢复 adopt 孤儿文字/孤儿目录）。
   另补一条冻结形状 characterization（`the_stored_index_keeps_its_frozen_version_1_shape`），此时即绿。

2. **红 #1（行为，真红）** — `red-1.log`
   `restart_recovery.rs::a_confirmed_refine_on_a_drained_segment_is_read_after_a_restart_not_reported_as_never_written`
   在未改代码上失败：
   `left: Some(Uncertain)` / `right: Some(Appended)`——一次已确认的精修 append 因为段索引条目被删，被恢复读成"什么都没写"。
   同一轮其余 317 个测试通过（含 1 个新 characterization）。

3. **红 #2（接口，编译红，在一次性基线工作树上取证）**
   把 W3 的新接口测试（`store.begin(|sequence| Segment::capturing(..))`、`store.apply(Change::..)`、`SegmentSummary::of`）
   放到基线树上编译，得到 15 个错误：`E0433 cannot find type Segment / Change`、
   `E0599 no method named apply found for SegmentStore`、`E0599 no associated function named of for SegmentSummary`、
   `E0308 mismatched types`（`begin` 仍是二元组）。即旧 interface 连测试都无法表达。
   （该基线工作树为一次性取证，已删除；错误清单见上。）

4. **绿** — `green-5.log`（首次全绿）、`green-10.log`（收尾后复跑）
   `322 passed; 0 failed; 16 ignored`，`-Task CheckFormat` 无输出，`-Task Test` exit 0。

### characterization（先钉住、重构后必须仍绿）

| 语义 | 测试 | 状态 |
| --- | --- | --- |
| 已确认 append 的段在交付后从索引消失、文字文件被删 | `multi_segment.rs::every_segment_saves_its_own_audio_text_state_and_cost_without_overwriting_an_earlier_one`（既有，未改） | 复用 |
| 删除一段只删它自己的记录与文字 | `storage_bounds.rs::deleting_a_selected_pending_result_removes_only_it_and_a_late_event_cannot_resurrect_it`（既有，未改） | 复用 |
| 恢复 adopt 孤儿文字/孤儿目录并各自落一条记录 | `restart_recovery.rs::corrupt_state_does_not_hide_saved_results_or_lose_an_unindexed_recording`（既有，未改） | 复用 |
| 旧 `segments.json` / `delivery.json` 兼容 | `restart_recovery.rs` 的 `index_record` / `write_delivery` 手写 JSON（既有，未改） | 复用 |
| 版本 1 索引的键名与拼写 | `segments::tests::the_stored_index_keeps_its_frozen_version_1_shape`（新增，重构前即绿） | 新增 |

## 四个方向转换的位置

一份权威表示：**`segments::Segment`**（`app/listen/src/segments.rs`，`pub(crate)`）。

| 方向 | 位置 | 说明 |
| --- | --- | --- |
| 入盘 | `SegmentStore::persist` → `serde_json::to_vec(&Index { segments: Vec<Segment> })` | 零手写映射；`state` / `delivery` 的拼写由 `state_name` / `delivery_name` 两个 serde 适配器固定成冻结的字符串（`capturing`… / `appended`·`fallback`·`uncertain`），`text` 内容 `#[serde(skip)]`，`text_file` / `text_chars` 是记录自己的事实 |
| 出盘 | `durable::open_versioned::<Index>` + `Segment` 的 `Deserialize` | 同上，零手写映射；旧索引缺 `glossary_terms` 仍读成空快照 |
| 投影到面板 | `impl SegmentSummary { fn of(&Segment) }`（`lib.rs`，本轮唯一一处手写的段字段投影） | `panel.rs` / `capsule.rs` 只读 `SegmentSummary`，从不接触记录层 |
| 从扫描重建 | `recovery::plan`（`record.clone()` + 只覆盖扫描决定的字段）+ `recovery::Plan::adopt`（孤儿文字/孤儿目录构造 `Segment`，经 `SegmentStore::begin` / `adopt` 落盘） | 新增字段自动随 `clone` 进入扫描结果，不必在这里重列；扫描结果只读，运行期字段（`handle`/`target`/`Instant`）不会进来 |

运行期段：`SegmentSession { facts: Segment, id, directory(已验证路径), handle, target, … }`（`lib.rs`）。

## 写入口（`record_mut` 的替代形状）

`SegmentStore` 只暴露四个已命名入口：

- `begin(|sequence| Segment) -> (sequence, facts, Option<String>)`：开一段（分配序号）。
- `adopt(Segment) -> Option<String>`：采纳扫描重建的一段（沿用它的序号）。
- `apply(Change) -> Option<String>`：对一条记录做**一次已命名的变更**；`Change` 有 7 种：
  `Status`、`Text`、`Usage`、`Recovered`、`GlossaryTerms`、`Delivered`、`CurrentResult`。
  内部决定要不要落盘（记录没变就不写）、降级时不写、写失败只报一次（返回 `Option<String>`）。
- `remove(sequence) -> Option<String>`：删一段。
- 文字文件仍由 `write_text` 单独写（记录只存指针；先文字后索引的顺序不变）。

`Application::record(change)` 把同一个变更同时写进该段的运行期 `facts` 与持久层，所以调用者不再有
"改字段 → `persist` → 查 `problem`" 的三步舞，也没有第二个作者（`recovery::Plan::adopt` 现在也走 `begin`/`adopt`）。

## 磁盘兼容性证据

1. **冻结形状单测**：`segments::tests::the_stored_index_keeps_its_frozen_version_1_shape` 用一份独立手写的
   JSON 字面量逐键比对 `segments.json`（`version`、`next_sequence`、`current_sequence`、每个记录的
   `sequence`/`directory`/`state`/`detail`/`text_file`/`text_chars`/`delivery`/`usage`/`glossary_terms`，
   以及 `updated_utc` 必须是数字）。这是格式的独立陈述，不是写入者的回声。
2. **旧索引**：`an_index_from_before_the_term_snapshot_still_loads_with_an_empty_snapshot`（缺少
   `glossary_terms` 的 v1 索引）原样通过；新增同类断言（`state`/`delivery` 反序列化结果）。
3. **旧 `delivery.json`**：`restart_recovery.rs` 的 `write_delivery`/`delivery_record` 手写记录（`started`、
   `uncertain`、`pending`、`appended`）原样通过，文件字节不被改写。
4. **版本语义**：`STORE_VERSION` / `delivery::STORE_VERSION` 均未改；未知版本仍被拒绝且不重写
   （`an_unsupported_index_is_reported_and_never_rewritten`）。
5. **没有新增/删除/改名任何键**：字段仍是那 10 个（`Segment` 内部多一个内存字段 `text`，`#[serde(skip)]`，
   不落盘）。

## 偏差（逐条）

1. **`state` / `delivery` 由字符串变成类型化**（`SegmentState` / `Delivery`），磁盘拼写由 serde 适配器
   保持逐字一致。唯一语义差异在**损坏输入**上：索引里的未知 `state` 现在读成最保守的 `failed`
   （旧行为：原样把该字符串拼进用户可见的说明），未知 `delivery` 读成 `uncertain`（旧：同样落到
   `uncertain`）。产品路径写不出未知值，版本门禁也拒绝未来格式；新读法仍 fail-closed。
   **2026-11-25 更正**：这条读法在本修复分支被替换——未知拼写不再读成 `failed`/`uncertain`，而是让整份
   `segments.json` 不可用、报告原始拼写并拒绝一切写盘（ADR 0005）；见 `docs\issue-review-fixes-2.md` F1。
   本段保留 W3 当时的完成记录，不追溯改写。
2. **`Segment` 的完整文字内容不落盘**：`text` 是 `#[serde(skip)]`；`text_file`/`text_chars` 仍是记录里的
   事实。这与 ADR 0005 的布局一致（文字在 `segment-NNNNNNNN.txt`），只是把"文字在哪"和"读完是什么"
   放在同一份权威表示里。
3. **`Change::Text` 只记录指针与字数，不把文本提前放进面板**：完整文本仍和以前一样在交付/失败/精修时
   进入 `SegmentSummary::text`。这一条是被既有测试逼出来的（先出现文本会让"三个完整段"的条件提前成立）。
4. **恢复期的记录写入由一次批量 `persist` 改成逐段 `Change::Recovered`**：掉电窗口更小（每段独立），
   代价是启动时多 N−1 次小索引写入。
5. **`remove` 对 `text_file` 增加单段文件名校验**（`Path::components().count() == 1`）：损坏索引指向
   runtime 目录外时不再删除外部文件；新增单测
   `removing_one_segment_never_reads_or_deletes_outside_the_runtime_directory`。
6. **`recovered_segment()` 的运行期目录**由 `release` 或 `recovery::safe_directory` 解析，不再
   `unwrap_or_default()` 得到一个裸路径；空/非法名字解析成空路径（不会退化成 runtime 根目录）。
7. **`Plan::adopt` 签名去掉 `runtime`**（不再需要拼接路径），`Plan` 仍只读扫描。
8. **孤儿文字的 delivery 现在读 `delivery.json`**（本票要求的修复）：`appended` → `Appended` +
   "previous session had already appended"；`fallback` → `Fallback`；缺失/`pending`/`started` → `Uncertain`
   （与索引记录同一套 `recovered_delivery` 规则）。

## 被动过的既有测试（逐条）

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `src/segments.rs`（单测，既有 4 个） | 改写调用方式：`record_mut` + `persist` → `store.apply(Change::…)` / `store.begin(闭包)` / `store.adopt`；断言语义不变 | 接口按票据移除；`records()[0].state` 由 `"result"` 断言改为 `SegmentState::Result`（同一值，类型化） |
| `src/segments.rs`（单测） | `an_unsupported_index_is_reported_and_never_rewritten`：`begin` 返回值从二元组改为三元组 | `begin` 现在同时返回权威事实 |
| `tests/restart_recovery.rs` | **只新增** 1 个测试 | W2 遗留 |
| 其余测试文件 | 0 处修改 | — |

另外：`src/glossary.rs::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 性能阈值）在本机
高负载下会偶发失败；**基线 `6e0fa81` 在同一负载下同样失败**（在一次性基线工作树上复跑得到
`correcting 300 terms took 138.811ms`，基线 75 passed / 1 failed），不是本波回归，未修改该测试。

## 加一个字段的改动面（实测）

方法：在基线与本分支各加同一个事实 `capture_endpoint: Option<String>`（采集起点产生、要落盘、要在面板显示），
一直改到编译通过，取 `git diff --numstat`（只算 `app/listen/src` 的生产代码），随后全部回滚。

| | 文件 | 处 | 行 | 分布 |
| --- | ---: | ---: | ---: | --- |
| 改前 `6e0fa81` | 3 | 15 | 28 新增 / 1 改写 | `SegmentRecord`、`SegmentSession`、`RecoveredSegment`、`SegmentSummary` 四个结构体 + 7 处字面量/映射 + `begin` 签名 + start 调用 |
| 改后 `703e0e6` | 2 | 4 | 17 新增 / 2 改写 | 权威表示 1 处（`Segment` 字段 + serde 默认值）、面板投影 2 处（`SegmentSummary` 字段 + `of` 一行）、采集时赋值 1 处；**入盘 / 出盘 / 恢复扫描 0 处** |

两边都还各要改 3 处面板测试字面量（`panel.rs` 1、`capsule.rs` 2），与重构无关。结论：新事实现在只改权威
表示就自动进索引、自动过重启扫描；要显示在面板上再加"一处转换"（`SegmentSummary::of` 一行）。

## 删除测试（本波加深的理由）

删掉 `segments::Segment` + `Change`：`record_mut` 的 7 个调用点各自恢复"整条记录替换"、5 处三步舞重现、
`segments.json` 重新出现两个作者（`Application` 与 `recovery::Plan::adopt`），四个结构体与 6 处手写映射也回来。
删掉 `durable::atomic_write`：9 个调用点各自重建 temp + `sync_all` + `rename` + Windows 共享冲突重试。
删掉 `Plan::adopt` 里的"读 delivery 记录"：W2 的精修崩溃窗口再次被误报成"什么都没写"。

## 后续波次注意事项

1. **第 4 波（段状态转移单点）**：`Change` 已经是"一次已命名的变更"，但 `Status`/`Delivered`/`Recovered`
   三种都各自带 `state + detail`。转移表落地时应让它们由同一个转移函数产出一条事实，`Application::record`
   仍是唯一应用点；`SegmentSession.session_state/stopped/finalizing/terminal` 与 `facts.state` 的不同步
   还需显式化。
2. **磁盘契约**：`Segment` 的 serde 就是 `segments.json`。以后加字段必须 `#[serde(default)]`（旧记录要能读），
   不要改名、不要改拼写；`state_name`/`delivery_name` 是唯一的拼写边界。
3. **路径安全**：`SegmentSession.directory` 是唯一经过验证的运行期路径；`facts.directory` 只是名字。
   清理/删除必须用前者（或 `recovery::safe_directory`），绝不要 `runtime.join(&facts.directory)` 处理空名。
4. **`pending`/`started` 在索引里读成 `uncertain`**：第 5 波把 `Delivery::None` 矛盾显式化时，不要把这个
   保守读法当"已证明"。
5. **孤儿文字依赖 `delivery.json`**：若将来修剪把某段的 `appended` 记录删掉，孤儿文字会退回 `uncertain`
   （与索引记录同规则）。第 5/6 波若要保留"历史被修剪"的说明，应把修剪事实也带进 `Segment`。
6. **`atomic_write` 现在服务所有记录写**（`segments.json`、`delivery.json`、`current-result.txt`、
   `segment-NNNNNNNN.txt`、PCM 结束标记、试算台账）；新增持久化文件也走它，不要各自发明替换。
