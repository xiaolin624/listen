# 重构程序：把浅 module 加深（2026-11-24）

来源：`/improve-codebase-architecture` 的只读评审（11 个候选，全部过了删除测试）。评审报告是一次性的 HTML，位于 `%TEMP%\architecture-review-20261124-1920.html`；本文件是它的**可执行版本**，也是后续波次的权威计划。

## 决策（用户 2026-11-24 确认）

1. **程序形状**：全部排成分波计划，每波独立可交付；每波结束停下来汇报，由用户决定是否继续。
2. **顺序**：先测试基建，再安全缺口，再机械记录层，再核心状态转移，最后契约与性能。
3. **行为口径：冻结。** 重构只许让矛盾**显式化**，不许顺手改语义。已知两处矛盾（`Delivery::None` 的三个互斥解释、`status.txt` 与面板的「等待段数」两口径）各自需要一次独立的产品决定，不在重构波里做。
4. **验收 oracle 是 interface 的一部分**：`status.txt` 现有 29 个键的名字与含义不得静默改变；要改必须在该波里同时改 `scripts\*.ps1` 断言、`docs\issue-24-verification.md` 口径，并在提交信息里点明。
5. **跟踪**：每波一张票，不开父规格。

## 词汇约定

用 `codebase-design` 的词汇（module / interface / implementation / depth / deep / shallow / seam / adapter / leverage / locality），领域概念用 `GLOSSARY.md`（段、待处理录音、待取结果、转写文本、目标输入框、状态胶囊、精修）。

`GLOSSARY.md` 是用户自有文件（只读）：若某波需要给一个新概念命名（例如「目标写入会话」），在本文件与票里先用描述性说法，**由用户决定是否写进术语表**。

## 波次

| 波 | 票 | 内容 | 关键约束 |
| ---: | --- | --- | --- |
| 1 | #43 | 识别 seam 的可选能力默认化 + `testing::ScriptedSessions` 剧本 adapter，收掉重复 fake | 纯测试基建；产品代码只加默认实现 |
| 2 | #44 | 目标写入会话：锁 / 草稿 / 拼接 / 读回 / 失败分类收进一个 module，三条路径共用；ADR 0005 的 `started` 落盘统一 | 安全语义只强化不改向 |
| 3 | #45 | 记录层：`record_mut` 退出 interface，一次已命名的变更；原子替换回到持久化层；一份权威段表示 + 四个方向转换 | 磁盘格式与版本不变 |
| 4 | #46 | 段状态转移单点（phase / recognition / detail / dirty / 记录由唯一应用点派生）+ 比 `Application` 更窄的段 seam | 最大波次；转移表先行 |
| 5 | #47 | `Delivery` 的用户后果收成一处分类（`None` 矛盾显式化，不改语义） | 三处措辞保留 |
| 6 | #48 | Snapshot 契约：识别模式成为类型、当前结果有身份、`status.txt` 收成纯函数 + 键集合测试 | 29 键冻结 |
| 7 | #49 | `poll()` 状态推进与磁盘测量拆开 | 10 ms 循环不再做 I/O |
| 8 | #50 | 胶囊视觉规格单一表示（可选，Speculative） | 视觉不变 |

## 每波的完成判据（通用）

- `.\scripts\build-listen.ps1 -Task Test`：测试数量不得减少；若新增测试，说明覆盖了哪条新不变量。
- `-Task CheckFormat` 无输出。
- 现有 29 个 `status.txt` 键逐字兼容（除非该波明确要改，并同步 `.ps1` 与文档）。
- 桌面验收脚本（`scripts\verify-*.ps1`）不因重构而改松：断言只能加强或显式更新。
- 每个"加深"必须能被一次删除测试解释：删掉新 module，复杂度应当在调用者处重新出现。

## 已知的两处矛盾（等产品决定，不在本程序内改）

1. **`Delivery::None`**：`refine_note` 把它当"已替换目标中的转写"，`summary` 当"本次没有向任何目标写入文字"，`headline` 当"已完成"；`pending_results` / `deletable` / `auto_open` 又把它排除。三处断言互斥。
2. **等待段数**：`panel::segments_note` 排除最新段，`status.txt` 的 `waiting_segments` 统计全部；单段采集中两者显示不同的数字。

## 已知负载敏感测试（不要当回归追）

本机在**并发跑另一个 worktree 的构建/测试**时，下列测试会假红；单独跑或机器空闲时全绿。判定方法：单跑 3 次 + 机器空闲时全量复跑。

1. `glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 预算）：负载下 108–138 ms 失败，空闲时 40–50 ms。
2. `storage_bounds::deleting_a_selected_pending_result_removes_only_it_and_a_late_event_cannot_resurrect_it`（10 s 等待）：W4 期间并发构建下 `timed out waiting for segment 2's stop-time draft`，单跑 0.27–0.31 s 通过。
3. `backfill_timing::an_older_segment_never_delays_a_newer_segments_save_or_delivery`（10 s 截止）：88% 负载下 `both deliveries were not confirmed in time`，单跑 5/5 通过（0.32–0.34 s）。

规矩：**不要为这两条把断言改松**；若真的改了阈值，必须同时记录改前/改后的实测分布。

## 评审证据摘要（供后续波次复核，不必重跑）

- `lib.rs` 3752 行；`impl Application` 2407 行 / 66 方法 / 29 字段；`Snapshot` 18 字段。
- 近 120 个提交：`lib.rs` 改 32 次，其中 17 次同改 `gui.rs`、12 次同改 `main.rs`、11 次同改 `panel.rs`。
- `Delivery` 语义分支 11 处（面板 7、核心 2、持久化两方向 2），其中 4 处集合判断是复制粘贴。
- 「只有最新段驱动快照」被重算 12 次；`snapshot.detail` 46 处赋值 + 17 处追加。
- 目标写入协议三份实现（`deliver` / `deliver_late` / `refine_delivery`），`DeliveryLedger` 只在其中两份出现。
- 测试：205 个（15 `#[ignore]`）、16,401 行；`lib.rs` / `recovery.rs` / `recognition.rs` / `streaming.rs` / `cloud\*` 均 0 单测。
- 脚手架税：7 份 `RecognitionSessions` fake、19 个测试文件各自定义 `struct Runtime`；`restart_recovery.rs` 第一个测试在第 501 行。
