# #44 重构验证：目标写入收敛为一个写入会话（W2）

分支 `implement/w2-target-write-session`，基线 `22d621f`（`integration/refactor-20261124` 尖端，第 1 波已合入）。
票：GitHub issue #44；计划：`docs/refactor-program-2026-11-24.md` 第 2 波；ADR：`docs/adr/0005-durable-segment-and-delivery-records.md`。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 同一协议只有一份实现 | 通过 | `TARGET_WRITER.lock()` 全仓 1 处（`src/target_write.rs:203`）；草稿读取 1 个函数（`read`，:320）；append 值构造/拆解 1 处（`append_value` :56 / `appended_part` :67 / `separator` :50）；写后读回 1 处（`write` 内唯一 `readback`，:267）；失败分类 1 份（`failed_write`，:342，原 `lib.rs` 60 行整体搬入） |
| 三条路径共用 | 通过 | `deliver`（`lib.rs:788`）、`deliver_late`（:867）、`refine_delivery`（:982）全部走 `TargetWriteSession::open` + `write`；差异见下表 |
| `refine_delivery` 拿到 `DeliveryLedger` | 通过 | `fn refine_delivery(refine, text, memory, sequence: u64, ledger: Arc<DeliveryLedger>, current_result)`（`lib.rs:982-989`），由 `dispatch(Control::Refine)` 传入同一个 `self.delivery` |
| ADR 0005 在第三条路径落地 | 通过（边界见下） | 写前 `begin_append`（`started`）、读回确认后 `confirm_append`（`appended`）；`started` 无法落盘时拒绝写目标。新增测试覆盖「写后确认前」窗口 |
| 精修写入后重启不重复写 | 通过 | `refine_backfill.rs` 新增 4 个测试；重启后目标写次数为 0 |
| 测试数量 | 通过 | 316 passed / 0 failed / 16 ignored（基线 309/0/16，+7：4 个 refine 集成测试、1 个 late 集成测试、2 个单元测试） |
| `-Task CheckFormat` | 通过 | 无输出（exit 0） |
| `status.txt` 29 键 | 通过 | `gui.rs` 仅 1 行（`refine_reason` 的 match 增加新终态）；`status.txt` 格式串逐字未改，键名/键数/顺序不变 |
| 交付/迟到/精修既有测试 | 通过 | 既有测试 **0 处断言修改** |

## 红 / 绿证据

全部命令都是 `.\scripts\build-listen.ps1 -Task Test`（日志在 `E:\luyin\.worktrees\w2\.artifacts\`，未提交）。

1. **红 #1（行为，先写测试后重构）** — `red-1.log`、`red-2.log`
   新增的 4 个 ADR 边界测试全部失败，既有测试与 characterization 测试全部通过：

   | 测试 | 红时的失败 |
   | --- | --- |
   | `a_refine_write_persists_its_start_before_the_target_write_and_a_crash_then_stays_uncertain` | 写被阻塞（写入已落地、确认未到）时 `delivery.json` 仍是 `appended`，没有 `started` |
   | `a_refine_write_with_a_lost_response_is_reported_and_recorded_as_uncertain` | 报告 `NotApplied("writing the refined text ... failed ..., so the target was left as it was")`：目标其实已被改写；记录仍是 `appended` |
   | `a_refined_result_is_recorded_and_never_written_again_after_a_restart` | 记录 `after` 仍是原转写（精修结果没有落盘） |
   | `a_refine_whose_started_marker_cannot_be_saved_does_not_write_the_target` | 记录不可用时仍写目标（替换调用 +1） |

   这一批里已有语义被复用/新增钉住（见「characterization」），红时它们是绿的——它们断言的是**现有**行为，不是本波要改的部分。

2. **红 #2（编译，单值来源）** — `red-3.log`
   先加 `target.rs` 的 `the_direct_write_and_the_paste_fallback_use_the_one_append_value`，模块尚不存在：
   `error[E0432]: unresolved import crate::target_write`。

3. **绿** — `green-2.log`（重构完成）、`green-3.log`、`green-4.log`（清理读回后复跑）
   `316 passed; 0 failed; 16 ignored`，`-Task CheckFormat` 无输出，`-Task Test` exit 0。

### characterization（先钉住、重构后必须仍绿）

| 语义 | 测试 | 状态 |
| --- | --- | --- |
| 写后确认前崩溃 → 重启按 `uncertain` 处理、不重放 | `restart_recovery.rs::a_started_or_uncertain_delivery_is_never_replayed_after_a_restart`（既有，未改） | 复用 |
| 精修失败保留本地结果且目标不变 | `refine_backfill.rs::a_failed_refine_keeps_the_local_result_and_never_appends_it_again`（既有，未改） | 复用 |
| 迟到路径 `started` 落盘失败必须放弃 | `late_backfill.rs::an_unreadable_delivery_record_keeps_the_complete_late_result_pending`（既有，未改） | 复用 |
| 正常路径 `started` 落盘失败仍可继续（原无测试） | `late_backfill.rs::a_normal_delivery_still_appends_when_the_started_marker_cannot_be_saved`（新增） | 新增，红时即绿 |

## 三条路径现在各自声明什么

协议（拿锁 → 读草稿 → 落盘标记 → `replace` → 读回 → 记录/分类）在 `target_write.rs` 只有一份。调用点只声明 `WriteRule` 与前置证明：

| 路径 | 前置证明（调用点） | `WriteRule`（start / confirm / unproven） | 不完整写的措辞 |
| --- | --- | --- | --- |
| `deliver` | 无：取最新草稿 | `IfPossible` / `Adapter` / `KeepPending` | 分类 detail 原文 |
| `deliver_late` | `wait_baseline`（Stop 时草稿）+ `plan_late_append`（链式证明） | `Required` / `Adapter` / `KeepPending` | `late_failed(detail)` |
| `refine_delivery` | refine port 成功、`memory.appended == text`、目标仍等于 `memory.after` | `Required` / `ReadBack` / `RestoreAppend` | `NotApplied` / `Unconfirmed` |

三条规则的含义就是票里说的「这次要不要先落盘哪个标记」：

- `start`：`started` 是可选（正常交付，行为冻结）还是必须（迟到/精修；写不出就不写）。
- `confirm`：交付以 adapter 的 `replace` 契约为确认（`Target::replace` 文档即「读回完整替换才成功」）；精修自己再读回一次，因为它替换的是用户能看到的文本。
- `unproven`：目标证明「什么都没写」时，正常/迟到交付记 `fallback`；精修恢复上一条已确认的 `appended` 记录（`before = memory.before`、`after = 观察到的目标内容`），否则会把「已追加」误报成「什么都没写」。

其余差异只剩前置安全证明与用户可见措辞——它们是两条**规则**本身，不是写入协议。

## ADR 0005 在精修路径上的边界

落地部分：

1. 写目标前 `begin_append(sequence, 当前完整草稿)` 落盘 `started`；`started` 无法落盘时**拒绝写目标**（`Required`），精修文本留在面板。这是 ADR 的 fail-closed 规则第一次作用在精修路径上（原来完全不看 `delivery.json`）。
2. `replace` 之后读回，只有「adapter 成功 + 读回等于完整替换」才 `confirm_append` 记 `appended`（`before`/`after` 是这次写入前后的完整目标内容）。
3. 写返回错误、或读回与替换不完全一致时，走同一份 `failed_write` 分类：
   - 读回证明目标仍是写入前内容 → 恢复上一条已确认的 `appended` 记录（不降级成 `fallback`）；
   - 其他（完整但未获确认、截断、目标又变了、读不回来）→ `uncertain`。
4. 进程在「写后确认前」死掉：磁盘上留 `started`（终态）。恢复把它映射为 `uncertain`（见 `recovery::recovered_delivery` 与既有 `restart_recovery` 测试），从不降级、从不重放。

边界（本波明确不修，留给第 3/4 波记录层）：

- **一次已确认的追加会删除该段的 `segments.json` 记录与文字文件**（`apply_delivery` → `remove_segment_record` → `SegmentStore::remove`）。因此在「精修写后确认前崩溃」这个窗口里，重启后没有段条目可以把 `delivery.json` 的 `started` 挂上去：转写文本仍从 `current-result.txt` 回到面板（`snapshot.text`）、`Refine` 仍可用，但面板没有该段的 delivery 徽标，也不会读出 `uncertain`。重启后再次精修只写面板（`current-result.txt`），目标写次数为 0；`delivery.json` 的 `started` 原样保留。该行为在精修接入 ledger 之前就已存在（那次删除与精修无关），本波只保证「记录不降级、目标不重复写、文本留面板」。
- 由此也只剩一条「重启后能看到 `started → uncertain`」的路径：段索引里仍有该段（例如清理失败保留的段）。既有 `restart_recovery` 测试覆盖的正是这条路径。
- 精修改短文本时，`plan_late_append` 的链式证明会在长度守卫处停下（保守 `Keep`）。精修改长文本时，链会经过精修写下的 `appended` 记录（比精修前更准确）。两种都不放松安全性。

## 偏差（逐条）

1. **新增 `Refine::Unconfirmed(String)` 终态**（`Refine::name()` = `unconfirmed`；`panel::refine_note` 与 `gui.rs` 的 `refine_reason` 各加一行）。原因：精修写目标可能「已落地但无法确认」；原来的 `NotApplied` 会说「target was left as it was」，是过度声明。这是**安全语义只强化不改向**：`status.txt` 的键集合不变（`refine` 的值新增一个终态），没有新增键。
2. **精修写失败改用共享失败分类的措辞**（原来是一条自造句子）。覆盖「截断 / 读回失败 / 目标又变了」这些原措辞会误报的情况；既有测试没有断言这些句子，所以没有被迫改测试。
3. **精修在 `started` 无法落盘时拒绝写目标**（`Required`）。可观察后果：`delivery.json` 不可用（如未来版本的记录）时，正常交付仍追加（冻结行为），而精修只更新面板并在 detail 里说明记录写不出来。
4. **交付路径的确认强度不变**：不为交付增加第二次读回（`Confirmation::Adapter`），避免把真实编辑器正常的空白/换行差异误报成 `uncertain`。精修保留自己的读回确认（`Confirmation::ReadBack`）。
5. **`target.rs` 只改 `plan_paste` 的分隔符来源**（`appended_part` / `separator`），`Target` 两方法 seam 与 `replace_content` 内部未动；新增一个测试证明直接写与粘贴回退用的是同一个值。

被迫修改的既有测试：**无**。新增 7 个测试（见上），既有测试文件只增加了测试夹具字段（`refine_backfill.rs` 的 `Gate`/`ReleaseOnDrop`，用于注入「写已落地、响应未到」），没有改任何既有断言。

## 删除测试（本波加深的理由）

删掉 `src/target_write.rs`，三个调用点各自要重建：全局写者锁、草稿读取、`草稿 + 分隔符 + 新文本`、写后读回、60 行失败分类，以及 ADR 0005 的 `started`/`appended` 顺序。`target.rs` 的粘贴回退还要再算一次分隔符——直接写与粘贴路径就会再次有产生不同结果的可能。复杂度确实会在调用者处重现。

## 后续波次需要注意的接口问题

1. **记录层（#45/#46）**：`delivery.json` 的 `before/after` 由 delivery 模块写，`segments.json` 的 `delivery` 字段由 `apply_delivery` 写一半；精修写下的 `appended` 记录落在一条已被删除的 index 记录上（见 ADR 边界）。统一记录层后应让恢复能读到它，届时「精修崩溃窗口」才能在上层报 `uncertain`。
2. **`Written::Refused` 在正常交付里不可达**：规则声明 `IfPossible`，代码只能防御性处理。第 5 波把 `Delivery::None` 矛盾显式化时，可以把「标记必需性」做成类型上不可误用的规则。
3. **`refine_delivery` 现在还需要 `sequence`**：调用点用 `current_sequence`（精修只作用于当前结果）。若将来精修可以作用于面板里的待取结果，入口必须带目标段自己的 sequence，而不是当前结果序号。
4. **精修改短文本会保守截断迟到证明的链**：第 4 波把交付历史换成「权威段表示 + 四方向转换」时，需要显式表达「Listen 自己的一次替换」，而不是靠 `before/after` 链的长度单调性。
5. **模块外唯一剩下的目标读取**是 `capture_stop_draft`（`lib.rs:2373`）：它是 Stop 时的草稿基线读取，用于迟到证明，不是写入协议，所以本波没有并入；若第 3 波统一记录层，可以考虑把它也收进同一个 module。
