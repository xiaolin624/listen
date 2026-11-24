# #43 重构验证：识别 seam 的可选能力默认化与共享剧本 adapter（W1）

分支 `implement/w1-recognition-testing-seam`（已合并进 `integration/refactor-20261124`，提交 `22d621f`），
基线 `32153dc`（程序文档提交）。票：GitHub issue #43；计划：`docs/refactor-program-2026-11-24.md` 第 1 波。

**补记说明**：W1 当时没有写验证文档；本文件于修复分支 `fix/refactor-review` 按合并提交、票面与基线源码补记，
红/绿中凡不是当时日志的部分都明确标注。W1 的产品代码只加了默认实现，行为冻结。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 次要能力有默认实现 | 通过 | `RecognitionSessions::preparation` 默认 `Ready`（`recognition.rs:182`）；`retry` 默认 `Err(RETRY_UNSUPPORTED)` 且不启动 session（`recognition.rs:194`） |
| 默认值不隐藏 fail-closed | 通过 | `start` 仍是唯一的启动门（`recognition.rs:190`）：本地 adapter 在 `start` 里 `check_ready()`，云端 adapter 在 `start` 里 `available()` 拒绝；默认 `Ready` 只回答「这个引擎没有要准备的东西」，不把未就绪引擎放进来 |
| 默认值对生产 adapter 无影响 | 通过 | `LocalSessions` 显式实现 `preparation`（`recognition.rs:323`）与 `retry`（`recognition.rs:391`）；`CloudSessions` 显式实现 `preparation`（`cloud\mod.rs:402`）与 `retry`（`cloud\mod.rs:527`）。两者都没有落到默认实现 |
| 剧本 adapter | 通过 | `#[doc(hidden)] pub mod testing`（`lib.rs:29-34`，323 行）；`ScriptedSessions` / `ScriptedControl` / `ScriptedSessionControl`（`testing.rs:36` / `:155` / `:210`）：有序 `Finished`/`Failed`/`Stable` 队列、按 start 顺序取 session、stop 计数、retry 计数、可设 state 与逐段 usage；不引入 dev-dependency 自引用、不加 cargo feature |
| fake 去重 | 通过 | 6 份手写 fake 改为复用剧本 adapter（清单见下）；各文件保留自己的断言薄包装。`restart_recovery` 保留自己的 adapter（见「偏差与边界」1） |
| 新增「不实现 retry 不被要求重试」测试 | 通过 | `tests\recognition_testing.rs::an_engine_that_does_not_implement_retry_is_answered_without_a_retry_session`：`Control::Retry` 后没有新 session，失败句含 `does not support retrying`，目标草稿未变 |
| `status.txt` 29 键不变 | 通过 | `gui.rs` 0 diff；产品代码只加默认实现与 `testing` module |
| 测试数量 | 通过 | **309 passed / 0 failed / 16 ignored**（W1 起点 306，+3 个 `recognition_testing` 测试）；W2 的验证文档也以 309 为基线（`docs\issue-44-verification.md`） |
| `-Task CheckFormat` | 通过 | 无输出（exit 0） |

## 红 / 绿证据

命令统一为 `.\scripts\build-listen.ps1 -Task Test` / `-Task CheckFormat`。W1 未写验证文档，日志没有入库；
下面第 1 条按基线源码复核并如实标注，第 2 条有可交叉核对的合并记录。

1. **红（接口红，编译期）** —— 新测试 `tests\recognition_testing.rs` 里的 `MinimalSessions` 只实现 `start`。
   W1 之前的 trait（`git show 22d621f^:app/listen/src/recognition.rs`）里 `preparation` 与 `retry` 都是
   **必需方法**（`fn preparation(&mut self) -> RecognitionReadiness;`、`fn retry(...) -> AppResult<...>;`），
   所以这个测试在旧 interface 上无法编译（`E0046: not all trait items implemented`）。这正是本波要收掉的
   "最小 adapter 也得手写一整套 fake" 税。
   **本文件补记时没有当时的编译日志**；本条是按基线 trait 源码复核的接口红，不冒充运行日志。
2. **绿** —— 默认实现落地后，`tests\recognition_testing.rs` 的 3 个测试通过：
   `an_adapter_with_only_start_is_ready_and_completes_a_capture`、
   `an_engine_that_does_not_implement_retry_is_answered_without_a_retry_session`、
   `scripted_sessions_hands_back_queued_outcomes_in_order_and_records_stops`。
   全量 **309 passed / 0 failed / 16 ignored**（起点 306/0/16；+3）。
3. **`-Task CheckFormat`** —— 无输出、exit 0（本波完成判据；补记分支复跑同样 0 输出）。
4. **`status.txt`** —— `gui.rs` 0 diff，29 键的名字/顺序/条件块未动。

## 6 份 fake 的复用方式

| 文件 | 改动 | 保留的独有断言 |
| --- | --- | --- |
| `tests\backfill_timing.rs` | 删掉手写 adapter/session，改用 `ScriptedSessions` + `ScriptedControl` | 停止→回填的计时预算、older/newer 段的交付顺序 |
| `tests\recognition_sessions.rs` | 同上 | session 事件/状态/停止计数与 delivery 的对应关系 |
| `tests\late_backfill.rs` | 同上 | 迟到结果的目标判定（append / keep）与草稿不变性 |
| `tests\multi_segment.rs` | 同上 | 多段各自的文字/状态/费用、旧段不被覆盖 |
| `tests\storage_bounds.rs` | 同上 | 预算停止、保留音频、显式删除与迟到事件 |
| `tests\usage_ledger.rs` | 同上 | 台账累计、未知用量、逐段费用归属 |

每个文件只做等价替换：`ScriptedSessions` 提供 adapter 与按顺序的 outcome，文件自己的断言行继续钉住各自语义。

## `RETRY_UNSUPPORTED` 是文案单一来源

- 唯一常量：`pub const RETRY_UNSUPPORTED`（`recognition.rs:151-152`）——
  `"This recognition engine does not support retrying a retained recording"`。
- trait 的默认 `retry` 返回它（`recognition.rs:194`）；剧本 adapter 的 `retry` 也返回它（`testing.rs:135`）。
- 用户可见句由 `Application` 的失败路径原样展示；`tests\recognition_testing.rs:276` 断言该句包含
  `does not support retrying`，没有第二处定义这句话。
- 生产 adapter 不走这个默认值：`LocalSessions::retry` 会真的启动本地 worker 并给出
  `Local retry failed: …`；`CloudSessions::retry` 有自己的云端重试路径。因此这句话只描述
  "这个引擎不支持重试" 这一种情况，不会被生产路径误用。

## 偏差与边界

1. **`restart_recovery` 没有改用剧本 adapter**：它要记录重试音频字节、glossary 快照与 PCM spool 写入，
   剧本 adapter 不建模这些，硬塞会丢掉该文件独有的断言。票面要求「至少 5 份复用」，6 份已达到；
   第 7 份保留自己的 adapter，理由与合并提交一致。
2. **默认 `preparation = Ready` 的解释**：`Preparation` 没有 unknown 变体；一个没有准备概念的引擎报告
   Ready 是事实，不是猜测。真正未就绪的引擎必须在 `start` 上拒绝，这是既有的 fail-closed 门，本波没有放宽。
3. **`testing` module 进入库的公开面**：`#[doc(hidden)]`，没有产品路径构造它；集成测试是独立 crate，
   共享 adapter 只能放在库里（或引入 dev-dependency 自引用，票面明确不要）。
4. **验证记录是补记**：W1 没有留下运行日志；红/绿、测试数与格式结论按合并提交、基线 trait 源码与
   W2 的基线记录重建。所有可复核的锚点（提交、文件、测试名、行号）都在上面，未冒充实测日志。
