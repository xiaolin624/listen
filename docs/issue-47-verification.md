# #47 重构验证：Delivery 的用户后果一处分类（W5）

分支 `implement/w5-delivery-classification`，基线 `55f08ef`（= `integration/refactor-20261124` 尖端，W1–W4 已合入）。
票：GitHub issue #47；计划：`docs/refactor-program-2026-11-24.md` 第 5 波；证据：`%TEMP%\listen-arch-20261124\surface.md`
的「把 `Delivery` 的用户后果收成一处分类」候选。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 集合判断只在一处定义 | 通过 | `DeliveryEffect::needs_user()`（唯一 `NothingWritten \| UncertainWrite`）；原 4 处 `matches!(Fallback\|Uncertain)` 全部改读它；全仓该集合的 `matches!` **0 处** |
| 新增一个变体时编译器指出位置 | 通过（实测，见下） | 给 `Delivery` 加变体 → **3 处** E0004；给 `DeliveryEffect` 加变体 → **7 处**（lib 2、panel 5） |
| 持久化与删除保护读同一个方法 | 通过 | `lib.rs` 2689（删除保护）与 3430（持久化 keep_record）都只读 `DeliveryEffect::needs_user()` |
| `None` 的三处互斥解释显式化 | 通过（行为不变） | 三处都从 `None` 改为显式 `DeliveryEffect::Unrecorded` 分支，并各自取一个具名读法：`NoRecord::Complete` / `NoRecord::NothingWritten` / `NoRecord::ReplacedInPlace` |
| `Phase::Result` + `delivery == None` 有测试 | 通过 | 新增 `panel::tests::a_result_without_a_delivery_record_is_complete_and_leaves_nothing_pending`（此前无覆盖） |
| 「需要用户」与「不需要」路径后果一致 | 通过 | 新增 `panel::tests::one_delivery_classification_decides_pending_cleanup_and_auto_open`：四种取值 × 待取列表 / 清理菜单 / 自动开面板逐一一致 |
| 三处措辞保留 | 通过 | headline 短句、summary 长句、segment_label 多段列表的字符串逐字未改（diff 里只有取值来源变化） |
| `panel.rs` / `capsule.rs` 既有断言 | 通过 | **0 处**断言修改；`capsule.rs` **0 diff** |
| `status.txt` 29 键 | 通过 | 只改 `lib.rs` + `panel.rs`；`gui.rs` **0 diff**（`format!` 与键集合逐字未变） |
| `-Task Test` | 通过 | **334 passed / 0 failed / 16 ignored**（基线 332/0/16，+2） |
| `-Task CheckFormat` | 通过 | 无输出，exit 0 |

## 分类的形状（一句话）

`Delivery` 只有一个穷尽 match（`Delivery::effect()`）映射到
`DeliveryEffect { WroteTarget, NothingWritten, UncertainWrite, Unrecorded }`；唯一集合判断是
`DeliveryEffect::needs_user()`（`NothingWritten | UncertainWrite`），另有 `wrote_target()` 只认 `WroteTarget`；
面板待取列表、清理菜单、自动开面板与 lib 的删除/持久化保护全部读这一处。

| `Delivery` | `DeliveryEffect` | `needs_user()` | `wrote_target()` | 用户后果 |
| --- | --- | ---: | ---: | --- |
| `Appended` | `WroteTarget` | false | true | 目标已确认持有全文，没有待取 |
| `Fallback` | `NothingWritten` | true | false | 什么都没写，文字留作待取结果 |
| `Uncertain` | `UncertainWrite` | true | false | 可能已写入但无法证明，待取且不再自动写 |
| `None` | `Unrecorded` | false | false | 结果完成，但目标侧无任何证据；不待取 |

`DeliveryEffect::of(Option<Delivery>)` 是 `Option` 侧唯一入口（`None` 不再落进任何「已证明」分支）；
`Snapshot::delivery_effect()` 是面板读它的唯一入口。

## `None` 三处现在各自怎么表达

三处句子本身一字未改，改的是「取哪种 none」从隐式 `None` 臂变成显式取值：

| 位置 | 现在的写法 | 具名读法 | 句子（未改） |
| --- | --- | --- | --- |
| `panel::headline` | `DeliveryEffect::Unrecorded => NoRecord::Complete.sentence()` | `Complete`（短句） | 「已完成」 |
| `panel::segment_label` | 同上 | `Complete`（多段列表里同一短句） | 「已完成」 |
| `panel::summary` | `DeliveryEffect::Unrecorded => NoRecord::NothingWritten.sentence()` | `NothingWritten`（长句） | 「本次没有向任何目标写入文字；当前完整结果在下方。」 |
| `panel::refine_note`（`Refine::Applied`） | `DeliveryEffect::Unrecorded => NoRecord::ReplacedInPlace.sentence()` | `ReplacedInPlace`（精修行） | 「精修已应用：目标输入框中的本次转写已替换为精修结果，请检查后手动发送。」 |
| `pending_results` / `deletable` / `auto_open_key` / lib 两处守卫 | `needs_user()` / 显式 `WroteTarget \| Unrecorded => None` | 「不是待取结果」 | — |

`NoRecord` 是 `panel.rs` 内的私有枚举，只承载这三句历史措辞；它不参与任何决定。
`NothingWritten` 与 `ReplacedInPlace` 对同一 `None` 仍然互斥——本波按计划**只命名、不调和**。

## 红 / 绿证据

全部命令都是 `.\scripts\build-listen.ps1 -Task Test`（`CheckFormat` 同脚本）；日志在
`E:\luyin\.worktrees\w5\.artifacts\`（未提交）。

1. **characterization（先绿，不是红）** — `green-3-test1.log`
   新增测试 1 后全量跑：bin target **43 passed / 0 failed**，其中
   `panel::tests::a_result_without_a_delivery_record_is_complete_and_leaves_nothing_pending ... ok`；
   lib target 89 passed。**它是绿的**：现有代码本来就把 `None` 排除在待取/清理/自动开面板之外，三处句子也各有
   显式历史分支；缺的只是覆盖，不是行为。按票面要求如实记录（这一条不是红）。
   同一轮后面的 `backfill_timing::an_older_segment_never_delays_a_newer_segments_save_or_delivery`
   在 10.26 s 超时（机器被并行任务压满），属环境性时序 flake，后续轮次通过。
2. **红 #1（接口，真红）** — `red-1-classification.log`
   加测试 2 后编译失败：`error[E0432]: unresolved import listen::DeliveryEffect`（panel.rs:12）、
   `error[E0599]: no method named effect found for enum listen::Delivery` ×3（panel.rs:770–772）。
   旧 interface 连这条测试都无法表达。
3. **绿** — `green-4-refactor.log`
   `334 passed; 0 failed; 16 ignored`（exit 0）；`-Task CheckFormat` 无输出。
   合入 `integration/refactor-20261124`（仍是 `55f08ef`，`Already up to date`）后复跑 — `final-4.log`：
   同数 `334 / 0 / 16`，exit 0。复跑前的三轮 `final-1..3.log` 只有 `glossary` 的 100 ms 性能阈值失败
   （108.6 / 136.5 / 122.5 ms；空闲后同一测试 44.2 ms 通过），与 W3/W4 记录的环境性 flake 相同，
   基线在同负载下同样失败，不是本波回归。

### 新增变体的编译强制力（实测，临时改动已全部回退）

| 探针 | 编译错误 | 位置 | 说明 |
| --- | ---: | --- | --- |
| `Delivery` 加第 4 个变体 `Considered` | 3 处 E0004 | `lib.rs` `Delivery::effect()`；`delivery.rs` `From<Delivery> for DeliveryState`；`recovery.rs` `adopted_text_detail` | 分类、持久化桥、恢复措辞各被逼一次；**panel.rs 不报错**——措辞读的是分类，不是 `Delivery` 变体 |
| `DeliveryEffect` 加第 5 个变体 `Considered` | 7 处 E0004（lib 2 + panel 5） | `lib.rs` `needs_user` / `wrote_target`；`panel.rs` `refine_note` / `headline` / `summary` / `segment_label` / `auto_open_key` | lib 先失败时 panel 不会被编译；把 `needs_user`/`wrote_target` 临时加 `_` 臂后，panel 侧 5 处各在两个 target 报错（10 个错误块） |

日志：`probe-new-delivery-variant.log`、`probe-new-effect-variant.log`、`probe-new-effect-variant-step2.log`。
回退后 `git diff --name-only` 只有 `app/listen/src/lib.rs` 与 `app/listen/src/panel.rs`，全仓无 `Considered` 残留。

结论：新增 `Delivery` 变体时，编译器先把人按在**唯一分类点**上；若新后果需要新措辞/新决定，再加
`DeliveryEffect` 变体会把 7 个消费点全部报出来。集合判断不再可能被静默漏掉。

## 被动过的测试（逐条）

| 文件 | 改动 | 类型化还是语义变了 |
| --- | --- | --- |
| `panel.rs` | **新增** `a_result_without_a_delivery_record_is_complete_and_leaves_nothing_pending` | 新增覆盖（`Phase::Result` + `None`），无既有断言改动 |
| `panel.rs` | **新增** `one_delivery_classification_decides_pending_cleanup_and_auto_open` | 新增覆盖（分类值 × 三处后果一致） |
| `panel.rs` | 模块顶部 import：`Delivery` 移入 `mod tests` 的 `use listen::{…}`，顶部改引 `DeliveryEffect` | 类型化（非测试代码不再直接名 `Delivery`） |
| `capsule.rs` | 无改动（0 diff） | — |
| 全部 `tests/*.rs` | 无改动 | — |

既有面板断言（Appended/Fallback/Uncertain 三分与 `Uncertain` 的「没有再次写入」等）逐字保留，仍全绿。

## 删除保护的既有覆盖（未改，仍绿）

lib 的删除保护由既有集成测试
`storage_bounds::a_result_being_written_to_its_target_cannot_be_deleted_until_the_write_finishes`
（含「已交付后再删 → `already delivered`、什么都不删」）继续钉住；本波把它的 delivery 条件换成
`needs_user()` 后该测试原样通过。

## 偏差（逐条）

1. **`DeliveryEffect` 放在 `lib.rs` 而不是 `delivery.rs`**：`panel.rs` 是 bin crate 的模块，只能读 `listen::`
   的公开 interface；`Delivery` 本来就在 `lib.rs`，分类与它同址，`delivery.rs` 只保留持久化记录与账本。
2. **`gui.rs:629` 仍是 `state.delivery == Some(Delivery::Appended)`**：那是托盘通知文案的单值判断（不是集合），
   新变体会落到保守的「Check the original target」分支。为了 `gui.rs` 0 diff 与 29 键不动，本波不动它。
3. **`recovery.rs` 仍用 `delivery == Delivery::Appended` 决定「已交付」与恢复措辞**：它问的是「写入是否已被
   证明」，不是「用户还要不要取」；且 `adopted_text_detail` 是穷尽 match，加变体会被编译器点名（见探针 A）。
4. **`Snapshot::delivery_effect()` 是新公开方法**：面板不再自己 `match snapshot.delivery`；`Delivery::effect()`
   的 `match` 是唯一分类点。
5. **`NoRecord::sentence()` 与 `WroteTarget` 的精修句子重复同一字面量**：前者是 `None` 的历史读法，后者是已证明
   的写回；语义不同，故意不合并（合并会把两种含义重新混在一起）。

## 是否建议把持久化侧 `DeliveryState` 纳入同一分类

**本波不做，建议也不要把 `needs_user()` 直接套上去。** 理由：

1. `DeliveryState`（`Pending` / `Started` / `Appended` / `Fallback` / `Uncertain`）描述的是**证据强度**，不是
   用户后果。`From<DeliveryState> for Delivery` 已经把 `Pending | Started | Uncertain` 保守塌成 `Uncertain`
   （W3 冻结：索引里的 `pending`/`started` 读成 `uncertain`，不算已证明）。把 `DeliveryEffect` 套上去，要么
   丢掉「started 标记丢失」这条证据，要么被诱惑把保守读法升级成断言。
2. 持久化侧真正的集合判断只有这一条桥（`delivery.rs:182-202`），而且它是穷尽 match：加一个 `DeliveryState`
   变体已经会编译报错，没有静默漏点。
3. 若将来确实想把「证据强度」也命名，应该在 `DeliveryState` 上加 `evidence()`（如 `Proven` / `Attempted` /
   `Unknown`），而不是复用 `DeliveryEffect`；那是另一张票（第 6 波 `Snapshot` 契约或之后）。

## 本波没做的边界（留给后续票）

1. `auto_open_key` 的 key 仍是「前缀 + 全文」，重复文本的两段不会重新开面板（surface.md 候选 2）——不在本票。
2. `Snapshot` 没有「当前结果序号」，待取列表仍靠全文相等排除当前结果（候选 2）——不在本票。
3. 索引 `pending`/`started` 的保守读法、`recovery.rs` 的两处 `== Appended` 保持原样。
