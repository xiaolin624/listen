# #48 重构验证：Snapshot 契约（W6）

分支 `implement/w6-snapshot-contract`，基线 `5720039`（= `integration/refactor-20261124` 尖端，W1–W5 已合入）。
票：GitHub issue #48；计划：`docs/refactor-program-2026-11-24.md` 第 6 波；证据：`%TEMP%\listen-arch-20261124\surface.md`
的三个候选（「把本地/云端识别模式变成 seam 的一等事实」「给当前结果一个序号身份」「把 status.txt 的派生字段收进面板的同一套快照 model」）。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 识别模式是一等事实 | 通过 | `RecognitionMode { Local, CloudTrial }`（`recognition.rs:62`）+ `RecognitionSessions::mode()`（`recognition.rs:163`，默认 `Local`）+ `CloudSessions` 覆写 `CloudTrial` + `Snapshot.mode`（`lib.rs:580`）；`Application` 构造时读一次（`lib.rs:1345`） |
| 嗅探函数不存在 | 通过 | `Snapshot::cloud_trial()` 已删除；全仓 `starts_with("Cloud trial:")` 仅剩测试里的展示断言（`tests/cloud_default.rs:315,358`），**0 处行为分支**；`grep 'cloud_trial('` = 0 |
| 改一句 `backend` 文案不改行为分支 | 通过 | 穷举测试 7 phase × 3 preparation × 2 mode：两份只有 `backend` 文案不同的 Snapshot，去掉展示值后面板文本逐字相同（`panel.rs:1201`）；反向用例「本地模式 + 以 `Cloud trial:` 开头的文案」仍按本机渲染（`panel.rs:1238`）；`status.txt` 同断言（`status.rs:584`） |
| 当前结果有身份 | 通过 | `Snapshot.current_sequence: Option<u64>`（`lib.rs:607`）；面板待取过滤 `panel.rs:260`、`auto_open_key` `panel.rs:590`、`gui.rs` 的 `last_auto_open`（key 变成 `fallback:<序号>`）与 poll 变化比较全部改读它 |
| 两段文本完全相同：旧段仍在待取列表 | 通过 | `panel.rs:1269`（断言 `pending_results` = `[1]`，且第 1 段仍以「待取结果」出现在面板正文） |
| 两段文本完全相同：新 fallback 仍自动开面板 | 通过 | 同一测试：`auto_open_key` 两次事件不同（`fallback:1` ≠ `fallback:2`），key 不含正文 |
| `status.txt` 收成纯函数 | 通过 | `status::report(snapshot, host, window)`（`status.rs:46`）返回整份文本；`gui.rs` 只剩「取窗口事实 + 写盘」（221 行 diff 中删掉 193 行拼接） |
| 纯函数有键集合/顺序/条件块测试 | 通过 | `status.rs` 7 个测试：无 usage/storage、有 usage、有 storage、全有（68 键 + 29 required）、取值与单行规则、两种模式拼写、backend 文案无关 |
| 现有 29 个 `status.txt` 键逐字不变 | 通过 | 四段 `format!` 字面量与基线**逐字节相同**（679/313/236/362 字符）；68 键顺序相同；`verify-panel.ps1` 的 29 个 `$requiredKeys` 顺序相同；`scripts/` **0 diff**（证据见下） |
| `scripts\*.ps1` 无 diff | 通过 | `git status --porcelain scripts/` 空；`git diff --stat` 无 `scripts/`；未新增键，故无需同步脚本与 `docs/issue-24-verification.md` |
| `-Task Test` | 通过 | **344 passed / 0 failed / 16 ignored**（基线 334/0/16，+10；bin target 44 → 54） |
| `-Task CheckFormat` | 通过 | 无输出，exit 0 |

## 形状（各一句）

- **`RecognitionMode`**：`enum { Local（默认，音频不出本机）, CloudTrial }`，带 `is_cloud_trial()` 与
  `name() -> "local" | "cloud-trial"`（ADR 0006 的唯一拼写处）；`RecognitionSessions::mode()` 由适配器回答
  （默认 `Local`，`CloudSessions` 覆写 `CloudTrial`），`Application` 构造时读一次存进 `Snapshot.mode`，之后
  面板 / `status.txt` / 托盘只读这个字段。
- **`CurrentResult` 的等价物**：`Snapshot.current_sequence: Option<u64>` —— 当前结果的身份是**采集序号**，不是文本；
  `text` / `delivery` 保持为它的展示字段与后果字段（票面允许「或等价的 `current_sequence`」），身份在
  `Application::snapshot()` 一处从唯一的内部序号投影（`lib.rs:3827`），所以两者不可能漂移。

## `status.txt` 纯函数的位置与入参

`app\listen\src\status.rs:46`：

```text
report(snapshot: &Snapshot, host: &HostFacts<'_>, window: &WindowFacts<'_>) -> String
```

3 个入参：

1. `snapshot`：应用快照（库 crate 的公开值）。
2. `HostFacts`：`current_result_path`（`runtime\current-result.txt` 的路径）、`glossary`（`status.txt` 拼写的术语表路径或
   `none`）、`process_id`。
3. `WindowFacts`：5 个窗口事实 —— `panel_visible`、`panel_error`、`capsule`（胶囊实际显示的视觉状态名）、
   `startup_capsule_ms`、`capture_excluded`。

函数内完成全部派生：`refine_reason`、`notice`、`panel_error`、`storage_fault` 的「压成一行」，9 个耗时键的
空值规则，以及三段条件块（usage 12 键 / storage 9 键 / #38 延迟 13 键）。`gui.rs` 的对应代码只剩：取
`panel_visible` 与两个窗口事实、取路径/术语表/进程号、调用 `report`、比较并写盘。

放在 bin crate（`main.rs` 的 `mod status`）的原因：它必须调用面板的**同一个** `panel::pending_results`
（计划明确要求「等待/待取计数改成调用 panel 的同一函数」），而 `panel.rs` 在 bin crate。测试因此跑在 bin target 的
单测里（`cargo test` 已覆盖，见下）。

## 29 键不变的证据

口径：`scripts\verify-panel.ps1:620` 的 `$requiredKeys` 共 **29** 个键；`status.txt` 在「usage + storage 都存在」时
实际写出 **68** 个键。本波两者都逐字不变（68 比 29 更强）。

1. **四段 `format!` 字面量逐字节相同**。脚本把基线 `HEAD:app/listen/src/gui.rs` 与新 `status.rs` 的四个字面量
   抽出来逐字比较（`.artifacts\compare_status_keys.py`，日志 `.artifacts\status-keys-compare.log`）：

   ```text
   base format literal identical: True (679 chars)
   usage format literal identical: True (313 chars)
   storage format literal identical: True (236 chars)
   timing format literal identical: True (362 chars)
   base keys identical: True (34 keys)
   usage keys identical: True (12 keys)
   storage keys identical: True (9 keys)
   timing keys identical: True (13 keys)
   conditional fallback arms identical: True
   all-present key count: 68 (old 68)
   all-present order identical: True
   required 29 keys present in order: True
   RESULT: IDENTICAL
   ```

   顺序（前 12 / 后 13）：`phase, detail, endpoint_role, endpoint_id, endpoint_name, current_result,
   retained_audio, retry_available, backend, recognition_mode, glossary, preparation, …` /
   `timing_stop_to_confirmed_ms, timing_cloud_connect_ms, timing_first_task_started_ms, timing_cloud_drain_ms,
   timing_cloud_final_ms, timing_event_wait_ms, timing_delivery_queue_ms, timing_apply_wait_ms, timing_schedule_ms,
   timing_delivery_lock_ms, timing_delivery_read_ms, timing_delivery_write_ms, timing_delivery_save_ms`。

2. **Rust 侧钉住**：`status.rs:396`（无 usage/storage = 34+1+1+13）、`:407`（usage = 34+12+1+13）、
   `:419`（storage = 34+1+9+13）、`:431`（全有 = 68 键、无重复键、29 个 `$requiredKeys` 顺序一致）。
   期望列表是独立写死的字面量，不是用代码重算。

3. **`scripts\*.ps1` 0 diff**：`git status --porcelain scripts/` 为空；本波未新增/改名/改序任何键，因此
   `verify-panel.ps1` 与 `docs/issue-24-verification.md` 都无需同步（完成判据允许的情况没有发生）。

4. **取值规则未变**：`status.rs:459` 断言 `phase=/current_result=/retained_audio=（" | " 连接）/retry_available=
   true|false/recognition_mode=local|cloud-trial/refine=failed/refine_reason` 与 `notice` 压行/`panel_visible=1`/
   `capsule_capture_excluded=1`/缺失耗时为**空**而不是 0/`recognition=none`/`waiting_segments`/`pending_results`/
   文件以 `\n` 结尾。`detail=` 内嵌换行的历史行为保持原样（本波不动它，它不是键）。

## 「两段文本完全相同」的证据

测试 `panel::tests::two_captures_with_identical_text_are_still_two_pending_results`（`panel.rs:1269`）：

- 两段都产出 `同一句话。`，第 1 段与第 2 段都是 `Fallback`（都需要用户取走）；
- 第 2 段是当前结果（`current_sequence = Some(2)`）：
  - `pending_results` = `[1]`（旧段仍在待取列表），且面板正文含 `【第 1 段 · 待取结果；可手动复制】`；
  - 当前段自己不出现在待取列表；
  - `auto_open_key` 从 `fallback:1` 变为 `fallback:2`（key 不含正文），所以新的 fallback 会再次自动开面板。

**变异实验（临时改回旧逻辑，测试立刻变红，改动已全部回退）**：

| 变异 | 日志 | 失败断言 |
| --- | --- | --- |
| 待取过滤改回 `snapshot.text != segment.text` | `.artifacts\mutation-1-pending-text.log` | `left: []` / `right: [1]`（旧段被相同文本吞掉） |
| `auto_open_key` 改回 `fallback:{snapshot.text}` | `.artifacts\mutation-2-auto-open-text.log` | `left != right` 失败：两边都是 `"fallback:同一句话。"`（不会重新开面板） |
| `headline` 临时改回按 `backend` 前缀判断 | `.artifacts\mutation-3-mode-sniff.log` | `左 "正在云端识别" / 右 "正在本机识别"`（`phase=Processing preparation=Pending mode=Local`） |

生产路径等价性说明：旧过滤只在**文本不同**时与新过滤等价；文本相同时旧逻辑会隐藏旧段、并让同文本的第二次 fallback
不再开面板——这正是本票要修的两个后果，其余状态（`current_sequence` 未知的恢复态、无结果态）两者一致，理由见「偏差 4」。

## 红 / 绿

命令统一为 `.\scripts\build-listen.ps1 -Task Test`（`CheckFormat` 同脚本）；日志在
`E:\luyin\.worktrees\w6\.artifacts\`（未提交、已被 `.gitignore` 忽略）。

1. **红（编译红，票面允许）** — `red-1-contract-tests.log`：先只写三个测试。25 个 `error[E…]`：
   `E0432` 未找到 `listen::recognition::RecognitionMode`（×2）、`E0609/E0560` `Snapshot` 无 `mode` /
   `current_sequence`、`E0425/E0422` 无 `status::report` / `HostFacts` / `WindowFacts`。旧 interface 连这三条
   契约都无法表达。
2. **绿 #1（隔离失败）** — `green-1.log`：lib target 89/0 全绿，bin target 53 passed / 1 failed。失败的是**新测试自身的
   期望值写错**：`refine=Failed`，实际拼写是 `refine=failed`（历史拼写，本波未改）。修正测试期望后进入下一轮。
3. **绿 #2** — `green-2.log`：exit 0，**344 / 0 / 16**。
4. **变异实验后复跑（绿 #3）** — `green-3-final.log`：exit 0，**344 / 0 / 16**（28 个 target）。
   过程注记：第一次 `green-3` 曾因回退变异用 `Copy-Item` 保留了旧 mtime，cargo 复用了变异过的二进制而假红；
   把 `panel.rs` 的 mtime 更新为新时间后复跑得到真绿。与产品代码无关，记录以免误读。
5. **`-Task CheckFormat`** — `checkformat-2.log` 长度 **0**、exit 0（`-Task Format` 之后）。
6. **本机负载敏感测试（本机当时 ~79% CPU，其它 agent 的构建/浏览器在跑）**：两次记录在案的环境性假红，**均未改阈值**：
   - `glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 预算）：
     会话开始时的基线跑 `baseline-test.log` 就以 **101.8 ms** 失败过；机器空闲后基线复跑
     `baseline-test-2.log` 为 **334 / 0 / 16**（该测试 94.7 ms 通过）。合并后的三次全量跑
     `final-post-merge-2/3/4.log` 分别在 **124.25 / 111.03 / 130.70 ms** 失败；随后单跑 3 次
     `singlerun-glossary-1/2/3.log` 为 **79.5 / 63.9 / 72.6 ms** 全绿。CPU 降到 ~45% 后
     `final-post-merge-5.log` 全量跑（该测试 80.3 ms 通过）拿到 **344 / 0 / 16**。
   - `storage_bounds::deleting_a_selected_pending_result_removes_only_it_and_a_late_event_cannot_resurrect_it`
     （10 s 等待）：`final-post-merge.log` 以 `timed out waiting for segment 2's stop-time draft`（10.27 s）失败；
     单跑 3 次 `singlerun-storage-bounds-1/2/3.log` 为 **0.36 / 0.34 / 0.30 s** 全绿（与程序文档记录的
     0.27–0.31 s 分布一致）。
   两次假红都发生在**同一台机器的负载尖峰**里，且出现在与它们无关的文件上（本波没有改 `glossary.rs` / `storage.rs` /
   任何等待阈值）；代码本身在 `green-2.log` / `green-3-final.log` 两次拿到 **344 / 0 / 16**。

## 被动过的既有测试（逐条）

| 文件 | 测试 | 改动 | 类型 |
| --- | --- | --- | --- |
| `panel.rs` | `a_recovered_result_whose_write_may_have_happened_is_not_shown_as_a_plain_panel_fallback` | 设 `current_sequence = Some(2)`；auto_open 期望 `uncertain:当前结果` → `uncertain:2` | 契约变化（key 由正文改为身份），断言句本身未变 |
| `panel.rs` | `one_delivery_classification_decides_pending_cleanup_and_auto_open` | 设 `current_sequence = Some(2)`（原来靠文本相等排除当前段） | 契约变化 |
| `panel.rs` | `every_new_fallback_result_produces_a_new_auto_open_key` | 改用新 helper `result_at(1/2, …)`，让两个事件有两个采集序号 | 契约变化（两个 key 现在由序号区分） |
| `panel.rs` | `pending_results_stay_in_capture_order_and_never_replace_the_current_result` | 设 `current_sequence = Some(3)` | 契约变化 |
| `panel.rs` | `cloud_cost_is_shown_as_confirmed_conservative_and_unknown_never_as_zero` | 增 `state.mode = CloudTrial`（原来由 backend 前缀选云端布局） | 类型化；所有文案断言逐字未改 |
| `panel.rs` | `approaching_the_monthly_reminder_line_is_rendered_as_a_reminder` | 同上 | 类型化 |
| `panel.rs` | `reaching_the_monthly_reminder_line_is_a_reminder_not_a_stop` | 同上 | 类型化 |
| `panel.rs` | 测试 helper `snapshot()` / `result()` | 补 `mode` / `current_sequence` 字段；`result()` 声明自己来自第 1 段（新增 `result_at`） | 编译必需 |
| `capsule.rs` | 测试 helper `snapshot()` | 补两个字段 | 编译必需（0 断言改动） |
| `tests/cloud_default.rs` | `the_default_configuration_selects_the_accepted_cloud_trial_and_reports_its_source` | `assert!(state.cloud_trial())` → `assert_eq!(state.mode, CloudTrial)` | 接口替换（断言更强：现在也钉住字段） |
| `tests/cloud_default.rs` | `explicit_local_selection_runs_the_local_path_end_to_end_and_reports_it` | 两处 `!cloud_trial()` → `mode == Local` | 接口替换 |
| `tests/usage_ledger.rs` | helper `application()` | `ScriptedSessions` 增 `.with_mode(CloudTrial)`（它的 backend 文案一直声称云端） | 剧本适配器如实声明路径 |

没有删除任何既有测试；`panel.rs` / `capsule.rs` 的其它断言 0 处改动。

## 删除测试（每个加深都挣工资）

1. 删掉 `Snapshot::mode` / `RecognitionSessions::mode()`：8 个消费点必须各自回到「解析 backend 文本」或硬编码
   （`panel.rs` 6、`gui.rs` 2），「云端还是本地」这个事实就会重新散落在文案前缀里。
2. 删掉 `Snapshot::current_sequence`：面板只能回到比较正文——重复文本时旧段被吞掉、第二次 fallback 不再开面板
   （上面两个变异实验就是这次删除的后果）。
3. 删掉 `status::report`：约 170 行拼接回到 `gui.rs` 的 100 ms `refresh()`，且 Rust 侧没有任何测试能钉住键集合
   （契约只剩 `scripts\*.ps1`）。

## 偏差（逐条）

1. **用 `current_sequence: Option<u64>` 而不是 `current: Option<CurrentResult { sequence, text, delivery }>`**：票面
   明确允许「或等价的 `current_sequence`」。理由是 `text` / `delivery` 已在 `Snapshot` 上且是它的展示/后果事实，再包一层
   会在同一个值里出现两份正文，反而制造新的漂移点。身份在 `Application::snapshot()` 一处投影，正好对应「文本只做展示」。
2. **`status.txt` 的纯函数在 bin crate**：因为它必须调用面板唯一的 `panel::pending_results`（计划要求同一函数，不要第二份
   口径）。代价是新测试跑在 bin target；`cargo test` 覆盖它（bin 44 → 54）。
3. **`RecognitionMode::name()` 现在拥有 `local` / `cloud-trial` 拼写**：原来这两个字面量在 `gui.rs` 的 `format!` 参数里。
   移到 enum 后，ADR 0006 的拼写只有一处。
4. **`auto_open_key` 在身份未知时返回 `None`**：生产路径到不了「有 pending 后果但没有身份」——`Snapshot::delivery` 只在
   `apply_delivery`（此时 `current_sequence == delivered.sequence != 0`）和启动恢复的
   `recovered_current_delivery()`（序号为 0 时返回 `None`）里被写；启动恢复出的文本因此是 `delivery=None` →
   `Unrecorded`，本来就不开面板。测试里合成的快照必须显式给序号。
5. **`waiting_segments` 仍是「全部 in-flight」**，面板 `segments_note` 仍排除最新段：这是程序文档记录的已知矛盾（#33 的
   产品决定），本波按行为冻结不动。
6. **`gui.rs` 仍保留**：poll 间隔、胶囊视觉推送、`panel.txt`、auto-open、`changed` 比较、托盘菜单/通知。本波只把
   `status.txt` 的生成搬走；`changed` 里新增了 `current_sequence` 比较，因为两段文本/交付完全相同时它是唯一的变化事实。
7. **没有新增/改名任何 `status.txt` 键**，所以 `scripts\*.ps1` 与 `docs\issue-24-verification.md` 都 0 diff。

## 后续波次注意事项

1. **#49（poll 与磁盘测量拆开）**：`status::report` 现在是纯函数，但仍在每次 100 ms `refresh()` 里被调用并整串比较；
   #49 可以把「生成 → 比较 → 写盘」移到变化驱动或测量点，纯函数本身不用改。测试的入参形状（snapshot + 两个 facts）就是
   后续复用的边界。
2. **新增 `RecognitionSessions` 实现时**：`mode()` 的默认值是 `Local`，编译器不能强制云端适配器覆写（今天只有
   `CloudSessions`，已覆写）。若出现第二个云端适配器，应把 `mode()` 从默认方法改成必填方法。
3. **`Snapshot` 字面量变多了**：任何新测试构造 `Snapshot` 都要多写 `mode` / `current_sequence`。若后续波次再加字段，
   建议先给 `Snapshot` 一个只用于测试的构造入口，再扩契约。
4. **`status.txt` 契约只有两个守卫**：`status.rs` 的 4 个键集合测试 + `verify-panel.ps1` 的 29 键。任何新增键都必须同时改
   这两处与 `docs/issue-24-verification.md`；程序文档的「29 键」指脚本的 `$requiredKeys`，文件实际 68 键，写后续票时请写清口径。
5. **重复文本的语义已修**：面板待取过滤与 auto_open 都不再看正文。任何新的「这是不是同一个结果」判断都应读
   `current_sequence` / `SegmentSummary::sequence`，不要再比较 `text`。
6. **`mode` 只在构造时读一次**：适配器在本进程内不切换识别路径；若将来要热切换，需要在 `poll()` 里重读并重新投影。
