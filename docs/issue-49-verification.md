# #49 重构验证：poll() 状态推进与磁盘测量拆开（W7）

分支 `implement/w7-poll-measurement`，基线 `0081e46`（= `integration/refactor-20261124` 尖端，W1–W6 已合入）。
票：GitHub issue #49；计划：`docs/refactor-program-2026-11-24.md` 第 7 波；证据：`%TEMP%\listen-arch-20261124\core.md`
的「`poll()` 里把状态推进和磁盘测量拆开」候选（行号已漂，按符号名重找）。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 磁盘测量离开 `poll()` | 通过 | `poll()`（`lib.rs:2915-3027`）体内 **0 处** `measure_storage` / `read_dir` / `tree_bytes` / `pending_audio_bytes` / `text_bytes` / `free_bytes`；唯一字符串命中在注释里（见下「证据 1」） |
| 测量是可缓存的值 + 纯判定 | 通过 | `MeasuredBytes`（`storage.rs:255`）是值；`Storage::measure`（`storage.rs:325`）是唯一 I/O；`Storage::status(bytes, free_disk)`（`storage.rs:334`）不接收 runtime 路径、不碰磁盘 |
| 测量低频或由调用者显式要求 | 通过 | 显式：构造 + 三个 builder + 每个 `dispatch` + `start_capture`；低频：`Application::measure_storage_if_due`（`lib.rs:1492`，1 s，`STORAGE_MEASURE_INTERVAL` `lib.rs:3907`），桌面 tick 在 `gui.rs:336` |
| 面板 `storage_note` / `deletable_segments` / 接近上限 `Notice` 行为不变 | 通过 | 数据来源仍是 `Snapshot::storage`（`panel.rs:423`）；`deletable_segments`（`panel.rs:411`）只读 `Snapshot::segments`，本波未动。新测试 `a_measurement_refresh_keeps_the_approaching_limit_and_the_deletable_segment`（`tests/storage_measurement.rs:254`）钉住「刷新后两者仍在」 |
| `poll()` 路径不再做测量 I/O | 通过（一处写时检查保留，见偏差 1） | `tree_bytes` 只被 `pending_audio_bytes` 调用（`storage.rs:464`），后者只被 `DiskBytes::measure` 调用（`storage.rs:272`）；`free_bytes` 只被 `Storage::measure` 调用（`storage.rs:327`）；`Storage::measure` 只有两个调用点：`measure_storage`（`lib.rs:1483`）与 `start_capture`（`lib.rs:2397`） |
| 新增测试覆盖「poll 不改变 StorageStatus / 显式测量才改变」 | 通过 | `tests/storage_measurement.rs` 3 个测试（见下「红/绿」）；变异实验能把它们打红 |
| 29 个 `status.txt` required 键逐字不变 | 通过 | `status::report` 的 4 段 `format!` 与键顺序未改（本波只改「何时调用它」）；`storage_*` 9 键的取值规则未改，仅可能来自上一次测量 |
| `scripts/` 0 diff | 通过 | `git status --porcelain scripts/` 空；没有新增/改名/改序 `status.txt` 键 |
| `-Task Test` | 通过 | **348 passed / 0 failed / 16 ignored**（基线 344/0/16；+3 集成 +1 bin；29 个 target） |
| `-Task CheckFormat` | 通过 | 无输出，exit 0 |

## 形状（各一句）

- **测量**：`MeasureBytes::measure(&Path) -> MeasuredBytes` 是唯一读运行目录的入口（`storage.rs:263`）；生产实现
  `DiskBytes`（`storage.rs:269-278`，每个 `session-*` 递归 + 根级音频 + 保存文字），加既有的 `FreeSpace` 探针；
  `Storage::measure` 把两者合成 `StorageStatus`。
- **判定**：`StorageStatus` 本身就是可缓存的测量值；`level()` / `near_limit()` / `blocking()` / `start_refusal` /
  `capture_stopped_detail` 全是纯函数；`Storage::status(bytes, free_disk)` 只把测量 + 策略拼成 `StorageStatus`。
- **动作**：`Application::measure_storage` 测量一次并应用纯判定（阻塞时停正在采集的段，与旧
  `check_storage_pressure` 同一句话、同一顺序）；`poll()` 只推进状态。

## 测量触发策略（一句话）

**桌面 tick 每 1 s 最多测量一次（`measure_storage_if_due`，`gui.rs:336`），另外在构造、每个控制命令
（`dispatch` 尾部）和每次开始采集前各测量一次；`poll()` 永不测量。**

理由：

1. 一次测量 = runtime 目录一次 `read_dir` + 每个 `session-*` 递归 `tree_bytes` + 保存文字再一次 `read_dir` +
   一次 `GetDiskFreeSpaceExW`。`Phase::Processing` 时 tick 是 10 ms，即旧路径 **100 次/秒**；现在 **1 次/秒**，
   空闲时 10 次/秒 → 1 次/秒。
2. 1 s 远低于用户可见量的分辨率：默认预算是 1 GiB，警告线是 4/5（差 200 MiB）；播放音频是 16 kHz mono
   16-bit = 32 KB/s，所以 1 s 的陈旧度把「越过预算之后还能录多久」限制在 ≤32 KB（占 1 GiB 的 0.003%）。
3. 面板 `storage_note`、`status.txt` 的 `storage_*` 仍读同一份 `Snapshot::storage`，1 s 的陈旧不会让某一行
   出现/消失的顺序变化。
4. 不能只靠「快照变化驱动」：采集中音频持续增长，越界本身不改变任何状态字段，所以必须有时间驱动的测量；
   也不能只靠阶段变化：跨过预算不改变阶段。
5. 不用 100 ms：比 1 s 贵 10 倍，而最近的一档用户可见阈值（警告线）在 200 MiB 之外。

## 「poll 不做 I/O」的证据

### 证据 1：`poll()` 体内（代码行，忽略注释）

用脚本把 `poll()` 函数体（`lib.rs:2915-3027`）切出来，去掉整行 `//` 注释后计数：

```text
measure_storage = 0   read_dir = 0   tree_bytes = 0
pending_audio_bytes = 0   text_bytes = 0   free_bytes = 0
```

（不去注释时 `measure_storage = 2`，两处都在同一段说明「这里故意不测量」的注释里，`lib.rs:3008-3013`。）

### 证据 2：调用图（测量侧）

```text
DiskBytes::measure            (storage.rs:272)
  ├─ pending_audio_bytes      (storage.rs:454) → tree_bytes (storage.rs:499)   ← 唯一调用者
  └─ text_bytes               (storage.rs:483)
Storage::measure              (storage.rs:325)  ← 唯一调用 free_bytes 的地方 (storage.rs:327)
  ├─ Application::measure_storage        (lib.rs:1483)
  └─ Application::start_capture          (lib.rs:2397, 开始采集前的显式检查)
Application::measure_storage 的调用者
  ├─ 构造 (lib.rs:1440) / with_storage_policy (1452) / with_free_space (1460) / with_storage_measure (1469)
  ├─ dispatch 尾部（每个控制命令一次，lib.rs:2381）
  ├─ start_capture 成功分支 (lib.rs:2508)
  └─ measure_storage_if_due (lib.rs:1497) ← gui tick (gui.rs:336)
```

`poll()` 不在上面任何一条调用链上。

### 证据 3：频率

`Phase::Processing` + 非等待网络时，GUI 定时器是 10 ms（`gui.rs:57`）；旧路径每个 tick 一次全量测量，
即 **100 次/秒**。现在同一秒内只有 1 次（`STORAGE_MEASURE_INTERVAL = 1 s`），且这次发生在 `poll()` 之后，
不在 `poll()` 里。

### 一处保留（偏差 1）

`poll()` 仍可经 `apply_session_event → finish_recognition → save_complete_text → check_text_budget → text_bytes`
（`lib.rs:3277` → `storage.rs:355`）到达**一次** `read_dir`。这是保存一个完整结果时的文字上限写时检查，
每次保存结果一次、不是每 tick；详见「偏差 1」。

## 红 / 绿

命令统一为 `.\scripts\build-listen.ps1 -Task Test` / `-Task CheckFormat`；日志在
`E:\luyin\.worktrees\w7\.artifacts\`（未提交、已被 `.gitignore` 忽略）。

1. **红（编译红，票面允许）** — `red-1.log`：先只写 `tests/storage_measurement.rs` 三个测试。1 个
   `error[E0432]`：`unresolved imports listen::storage::MeasuredBytes, MeasureBytes`（`tests\storage_measurement.rs:14`）；
   `Listen Test failed (101)`。旧 interface 连「注入一个假测量」都无法表达。
2. **绿 #1（只有主改动）** — `green-1.log`：exit 0，**347 / 0 / 16**（344 + 3 个新集成测试）。
   新测试逐个 ok：`consecutive_polls_never_measure_the_runtime_directory`、
   `only_an_explicit_measurement_changes_the_published_storage_status`、
   `a_measurement_refresh_keeps_the_approaching_limit_and_the_deletable_segment`。
3. **变异实验（证明新测试不是空转）** — `mutation-poll-measures.log` + `mutation-storage-measurement.log`：
   临时把 `self.measure_storage();` 放回 `poll()` 尾部（旧行为），全量套件 exit 101；单跑变异后的
   `storage_measurement` 测试二进制：
   - `consecutive_polls_never_measure_the_runtime_directory`：`left: 51, right: 1`（50 次 poll 测了 51 次）；
   - `only_an_explicit_measurement_changes_the_published_storage_status`：`left: 900, right: 100`
     （poll 把新值发布了）。
   变异已全部回退。
4. **绿 #2（含可选第 2 项）** — `green-2.log`：exit 0，**348 / 0 / 16**（+1 bin 测试
   `status::tests::the_cheap_report_key_changes_with_every_fact_outside_the_snapshot`）。
5. **绿 #3（回退变异后、提交前）** — `green-3.log`：exit 0，**348 / 0 / 16**，29 个 target。
6. **`-Task CheckFormat`** — `checkformat-1.log` 有 2 处 `cargo fmt` diff（都在新测试文件里）；
   `-Task Format` 后 `checkformat-2.log` **长度 0**、exit 0。
7. **合并后复跑** — `git merge integration/refactor-20261124` 输出 `Already up to date.`（integration 仍是
   `0081e46`，无冲突）；随后 `green-4-post-merge.log`：exit 0，**348 / 0 / 16**；`-Task CheckFormat` 仍
   **0 输出**、exit 0。
8. **提交前的最后一次全量（含一次负载假红）** — 之后只清理了新测试文件里一条自证式断言（`runtime().is_dir()`）
   和随之无用的 helper，产品代码未变。第一次全量 `green-5-final.log` 在机器 ~72–93% CPU 时以 1 failed 结束，
   唯一失败是计划文档记录的已知负载敏感假红
   `glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 预算，实测
   114.9 ms）；本波没改 `glossary.rs`，**未改阈值**。单跑 3 次 `singlerun-glossary-1/2/3.log` 全绿
   （各 0.06–0.07 s）；CPU 降到 ~35% 后 `green-6-final.log`：exit 0，**348 / 0 / 16**。

## 新增测试（4 个）

| 文件 | 测试 | 钉住什么 |
| --- | --- | --- |
| `tests/storage_measurement.rs:179` | `consecutive_polls_never_measure_the_runtime_directory` | 构造后 50 次空闲 poll、采集中 50 次 poll，假测量的调用计数不变；一次显式测量恰好 +1 |
| `tests/storage_measurement.rs:216` | `only_an_explicit_measurement_changes_the_published_storage_status` | 改假测量后 poll 25 次仍是旧值、`Notice` 不变；显式测量后 `pending_audio_bytes` / `text_bytes` / `Notice::StorageWarning` 一起更新 |
| `tests/storage_measurement.rs:254` | `a_measurement_refresh_keeps_the_approaching_limit_and_the_deletable_segment` | 刷新后仍是警告级、`Notice::StorageWarning`，且停下的段仍在（面板可删），并跨后续 poll 保持 |
| `src/status.rs:404`（bin target） | `the_cheap_report_key_changes_with_every_fact_outside_the_snapshot` | 快照外的 8 个事实每一个都进入 `FactsKey`；两个输入相同则 key 相同 |

## 主动改动（非测试）

| 文件 | 改动 |
| --- | --- |
| `storage.rs` | 新增 `MeasuredBytes` / `MeasureBytes` / `DiskBytes`；`Storage` 增加 `bytes` 注入与 `set_measure`；`status(runtime)` 拆成 `measure(runtime)`（I/O）+ `status(bytes, free_disk)`（纯）。既有 `pending_audio_bytes` / `text_bytes` / `tree_bytes` / 4 个纯判定函数 / 6 个单测一行未改 |
| `lib.rs` | `refresh_storage` → `pub measure_storage`；新增 `measure_storage_if_due`、`publish_storage`、`note_storage_fault`、`clear_storage_fault`、`last_measure` 字段、`STORAGE_MEASURE_INTERVAL`；删除 `check_storage_pressure` 与 `poll()` 尾部的调用（改成说明注释）；`start_capture` 的预检查改用 `Storage::measure`；故障改由 `note_storage_fault`/`clear_storage_fault` 同时改记录与已发布值 |
| `gui.rs` | `poll()` 之后加 `measure_storage_if_due()`（`gui.rs:336`）；`status::report` 改为变化驱动（`FactsKey` 比较，`gui.rs:435-446`），`changed` 比较补上 `endpoint` 与 `mode`（report 读它们） |
| `status.rs` | 新增 `FactsKey`（`status.rs:46`）与它的测试；`report` 与 29 键逐字未改 |

## 被动过的既有测试（逐条）

全部只加了一句显式测量（`dispatch` 之外唯一会做测量的公开入口），**没有改任何断言、没有删除测试**。

| 文件 | 测试 | 改动 | 类型 |
| --- | --- | --- | --- |
| `tests/storage_bounds.rs` | `pending_audio_is_counted_across_segments_and_a_full_budget_stops_the_capture_and_the_next_start` | 写入 500 B 后加 `h.app.measure_storage()`，再 `poll_until` | 调用点更新（旧靠 poll 每 tick 测量） |
| `tests/storage_bounds.rs` | `audio_over_the_budget_never_blocks_saving_the_complete_text_to_its_own_bound` | 写入 2 000 B 后加 `h.app.measure_storage()` | 同上 |
| `tests/storage_bounds.rs` | `the_free_disk_floor_stops_a_running_capture_and_keeps_the_received_audio` | `set_free(999_999)` 后加 `h.app.measure_storage()` | 同上 |
| `tests/segment_transition_table.rs` | `StorageStop` 场景（`fn scenario` 内） | 写入 2 000 B 后加 `h.app.measure_storage()` | 同上 |
| `tests/segment_transition_table.rs` | `a_storage_stop_reason_survives_later_status_refreshes` | 写入 2 000 B 后加 `h.app.measure_storage()` | 同上 |

其余 23 个测试文件（26 个文件里除上表 2 个与新文件）、`panel.rs` / `capsule.rs` / `storage.rs` / `status.rs` 的既有测试：**0 处改动**。

## 偏差（逐条）

1. **一处 `read_dir` 仍可从 `poll()` 到达**：`save_complete_text` 里的文字上限检查
   （`check_text_budget` → `text_bytes`，`lib.rs:3277`）。没有改成读缓存值，因为它是**写时安全判定**
   （决定「这一次完整结果能不能写」），读到上一次测量会让上限被多写一个结果，属于用户可见语义变化（票面
   禁止）。它每次保存完整结果才跑一次，不在 10 ms tick 上；`tree_bytes`（递归遍历每个 `session-*`，本波
   真正的开销）在 `poll()` 路径上已完全不存在。
2. **预算/磁盘底线触发的采集停止改在「下一次测量」发生**：最多晚 1 s，越界最多 ~32 KB（16 kHz mono
   16-bit）。这是「测量低频」的必然代价，票面明确允许数据来自上一次测量；停止的判定、文案、保留音频的行为
   都没变（`capture_stopped_detail` / `stop_capture_for_storage` 未改）。
3. **存储故障的发布时机**：原来要等 `poll()` 尾部那次测量才会进 `Snapshot::storage.fault`；现在
   `note_storage_fault` / `clear_storage_fault` 在记录/清除的同时改已发布值（同一 tick 内可见，且不再依赖测量
   频率）。失败/清除的触发条件与文案未变。
4. **新增公开面**：`Application::measure_storage`、`Application::measure_storage_if_due`、
   `Application::with_storage_measure`、`storage::MeasuredBytes`、`storage::MeasureBytes`。
   前两个是「由调用者显式要求测量」的入口，后两个是测试 seam（假的测量要能注入，否则「poll 不测量」只能用
   计时推断）。
5. **可选第 2 项已做**：`status::report` 的生成从「每 100 ms 无条件整串生成再比较」改为「快照变化（`changed`）
   或快照外事实变化（`FactsKey`）才生成」，静止 tick 不再格式化 68 个键。`changed` 因此补了 `endpoint` 与
   `mode` 两个比较——它们是 `report` 的输入（`endpoint_*` / `recognition_mode`）而之前不在比较里。
   活动采集时 `detail`/`timing` 仍会每 tick 变化，所以那里照旧生成；收益在静止/空闲 tick。
6. **`storage.rs` 的 `text_bytes` 仍有两类调用者**（`DiskBytes::measure` 与 `check_text_budget`）；它是同一个
   「保存文字之和」口径，没有第二份实现。
7. `snapshot.storage` 仍是 8 字段的 `StorageStatus`，`status.txt` 的 9 个 storage 键语义未变；本波没有新增/改名
   任何键，因此 `scripts\*.ps1` 与 `docs/issue-24-verification.md` 无需同步。

## 「数据可能变陈旧」对用户可见行为的实际影响

| 用户可见面 | 陈旧上限 | 实际影响 |
| --- | --- | --- |
| 面板 `storage_note` 的数字 | ≤1 s | 采集时增长 32 KB/s，显示的是 0.0 MiB 级别；只有跨过警告线/预算/底线的那 1 s 内可能晚显示一行 |
| 面板「可逐段清理」提示 | 0 | 只读 `Snapshot::segments`，与测量无关 |
| 接近上限的 `Notice::StorageWarning` | ≤1 s 出现；用户动作（清理/删除）立即清（`dispatch` 会测量） | 提示只是提醒，不停止任何东西，晚 1 s 无行为后果 |
| 采集因预算/磁盘底线被停 | ≤1 s | 最多多录 ~32 KB 再停；停止后的处理（保留音频、文案、继续完成）不变 |
| `status.txt` 的 `storage_*` | ≤1 s | 任何 `--control` 动作后立即刷新（`dispatch` 尾部测量）；只有「外部脚本往 runtime 目录写文件、又不发任何控制命令」时最多晚 1 s。仓库内 `scripts\*.ps1` 不读这些键（0 处） |
| 存储写失败 `storage_fault` | 0 | 记录时立即发布（偏差 3），比旧路径更早，不依赖测量频率 |
| 真正的磁盘满/写失败 | 0 | 仍由写错误本身 fail closed（`storage_fault_message`），测量只是提前提醒 |

结论：没有任何用户可见行为改变语义；变化是「某些数字最多晚 1 s」，且最坏情况（采集中越界）的越界量被
1 s×32 KB/s 限制。

## 后续波次注意事项

1. **`poll()` 的测量边界是结构性的**：以后若要在 poll 里加磁盘扫描，`tests/storage_measurement.rs` 的
   `consecutive_polls_never_measure_the_runtime_directory` 会直接变红（变异实验里 `left: 51, right: 1`）。
2. **`deletable_segments` 的断言在 bin target**：`panel.rs` 属于 bin crate，集成测试拿不到它；本波用「同一
   snapshot 里有警告 + 有可删段」在 lib 测试里钉住输入，`panel.rs` 既有的
   `only_segments_that_still_have_pending_work_are_offered_for_explicit_deletion` 继续钉住输出。
3. **`status::report` 的输入集合现在被两处分开维护**：Snapshot 内的字段靠 `changed`，快照外的 8 个事实靠
   `FactsKey`。以后给 `Snapshot` 加字段并让 `report` 读它时，必须同时把它加进 `changed`，否则 `status.txt`
   会停在旧值（`FactsKey` 的测试只覆盖快照外事实）。
4. **`check_text_budget` 的写时测量是有意保留的**（偏差 1）；若将来要把它也变成缓存值，需要一次产品决定，
   因为它改变的是「文字上限在哪一刻生效」。
5. **测量间隔是常量不是配置**：`STORAGE_MEASURE_INTERVAL`（`lib.rs:3907`）。如果以后预算改小到 MiB 级或
   音频采样率提高，需要重新核算「1 s 的越界量」是否仍可接受。
