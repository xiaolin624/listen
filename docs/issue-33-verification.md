# #33 多段可连续采集，新段优先且旧结果不覆盖：实现与验证

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)，基于 `integration/cloud-recognition`
（已含 #32 断网不停采与 #37 累计用量面板）。**本票交付可离线验证的应用级多段语义；真实付费网络、
真实编辑器与一秒回填验收属于 #38、#40，未在本票运行。**

## 已实现行为

- **每段独立任务。** 每次开始/停止生成一个独立分段（`sequence` 从 1 递增）。开始只被三件事拒绝：
  已有采集在运行（播放设备独占）、有失败录音等待重试/清理、精修仍在进行。旧段等待联网或仍在处理
  都不再拒绝新段（规格要求的行为变化），也不清除旧段的目标、文字或状态。
- **事件按身份归属。** 每段有自己的 `SessionId`、开始时的目标、采集目录、术语快照、计时和费用。
  `poll` 按采集顺序刷新所有仍在会话中的分段，但每轮最多应用一个会话事件和一次交付结果；旧段的
  完成、失败、重复与乱序事件只影响自己，既不会结束新采集，也不会串入新段的文字或目标。
- **新段优先、旧段有序。** 新段一启动就成为阶段/胶囊描述的“当前”分段；旧段在后台完成。积压的多段
  结果按采集顺序逐段落盘（测试用一次 poll 只写一段来验证），不会同时压满工作线程或目标交付。
  目标写入串行化（同一时刻只有一个交付写编辑器）；恢复出的旧结果按 #32 语义统一保守留在面板，
  绝不自动回填（#34 才做安全回填）。
- **面板区分多段。** `Snapshot` 新增 `segments: Vec<SegmentSummary>`（采集顺序、状态、
  逐段状态说明、完整文字、交付方式、逐段费用）。`panel::status_text` 增加“多段记录”块：
  等待段数、待取结果段数和逐段状态；`panel::result_text` 在当前结果之后完整列出每个待取结果并
  标注段号，后一段不会覆盖前一段，长文本可整体选择/复制。单段正常流程的面板文字保持原样。
- **胶囊仍然只表达状态。** 没有数字、计数或实时字幕；多段状态只在面板。`capsule.rs` 增加测试固定
  这一点。
- **独立持久化。** 运行时目录新增：
  - `segments.json`：带版本的多段索引（采集序号、会话目录名、状态、文字文件名、交付方式、
    每段费用、更新时间、下一个可用序号）；
  - `segment-NNNNNNNN.txt`：每段自己的完整文字，用与 `current-result.txt` 相同的 atomic_write。

  顺序是“先写完整文字 → 再更新索引 → 才删除该段生产音频”。追加交付成功的结果在文字可靠保存后
  清掉自己的索引与文字副本（已进入目标与 `current-result.txt`）；待取（Fallback）结果、失败保留
  录音和未完成段各自留在索引里，互不覆盖。下一个进程从 `next_sequence` 继续，绝不覆盖已存在的
  `segment-*.txt`；无法解析的索引只报告降级，不会静默重写。
- **逐段费用。** 每段费用（厂商/型号/地域、请求次数、已知估算、未知标记）随段保存；读取账本
  只发生在该段会话结束之后、且**没有任何识别会话在飞**时（`recognition_in_flight` +
  `flush_deferred_usage`）。账本独占锁同时是付费请求准入锁（#37），因此应用侧绝不在采集中读账本；
  锁忙或账本不可读时该段费用保持未知，绝不显示为 0。
- **兼容既有契约。** `current-result.txt` 仍是最新完整结果；`status.txt` 原键不变，新增
  `waiting_segments`、`pending_results` 两个计数键；#37 的 `Snapshot::usage`、面板用量块、
  月度提醒与 `usage()` seam 原样保留。

## 应用入口测试

新增 `app/listen/tests/multi_segment.rs`（6 个，全部通过 `Application::dispatch` / `poll` /
`snapshot` 观察，只替换识别会话与编辑器，不发起付费请求）：

1. `a_new_capture_starts_while_the_previous_segment_waits_and_each_segment_keeps_its_own_target_and_text`：
   旧段等待联网时停止后仍可开始新段；旧段迟到结果不写目标、不改变新采集阶段；新段结果追加到
   自己开始时绑定的目标，旧段结果仍完整保留在分段记录中。
2. `three_interleaved_captures_keep_their_own_state_and_the_newest_result_is_current`：三段交错采集，
   状态各自独立；积压按乱序返回结果时全部保留，最新采集顺序是当前结果，所有恢复结果都不写目标。
3. `a_failed_backlog_segment_does_not_end_the_new_capture_or_clear_other_results`：旧段失败不结束
   新采集、不清除另一段的待取结果，失败段录音仍可重试。
4. `a_finished_backlog_is_recovered_one_segment_at_a_time_in_capture_order`：三段同时完成时，
   一次 poll 只按采集顺序落盘一段，旧段不会越过更早的段，最新采集顺序仍是当前结果。
5. `every_segment_saves_its_own_audio_text_state_and_cost_without_overwriting_an_earlier_one`：
   每段有独立声音目录、独立文字文件、独立状态、独立费用；后一段不覆盖前一段；`segments.json`
   持久化序号和费用，重启外的下一段继续递增。
6. `duplicate_and_foreign_events_cannot_change_another_segments_result`：重复确认、完成后的迟到
   失败、携带别的段身份的事件都不能改另一段的结果或目标。

另有：

- `src/segments.rs` 单元测试 3 个：索引/文字/费用往返、已有序号绝不重用、未来版本索引不被重写；
- `src/panel.rs` 多段渲染测试 3 个：等待/待取计数与逐段顺序、待取结果完整且不覆盖当前结果、
  单段面板文字保持原样；
- `src/capsule.rs` 1 个：多段快照不改变胶囊词汇，状态名不含数字。

## 既有测试调整

- `capture_to_copy.rs` 的 `busy_operations_explain_the_current_work_and_do_not_replace_or_duplicate_the_result`
  与 `unconfirmed_output_keeps_the_full_fallback_and_repeated_controls_never_retry_the_append`：处理中
  的拒绝改用 `Control::Retry`；`Control::Toggle`/`Start` 在处理中现在按规格开始下一段，不再拒绝。
- `repeated_stop_held_hotkey_and_processing_controls_deliver_each_result_once` 改名为
  `repeated_hotkey_events_form_independent_segments_and_deliver_each_result_once`：按住/自动重复的
  热键现在形成多个独立分段，每段只向自己开始时绑定的目标追加一次；结果不再互相覆盖。
- 其余既有测试无需改动；`recognition_sessions.rs` 的“旧 SessionId 迟到事件不能改新采集”继续通过。

## 验证命令与结果

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
```

离线、锁定依赖构建与测试通过：**236 passed / 0 failed / 16 ignored**（忽略项都是需要真实模型、
真实密钥或操作员编辑器的既有测试，不计为通过）。`cargo fmt --check` 通过。

## 合并说明（#37 / dc7c1e1）

- `Snapshot` 同时保留 `usage`（#37，紧跟 `notice`）与本票的 `segments`（在其后），字段顺序未打乱。
- `panel::status_text` 先渲染 #37 的累计用量块，再在 detail 之后插入本票的“多段记录”块；两个块
  相互独立，单段正常流程不变。
- `status.txt` 在 #37 追加的 `usage_*` 键之后继续追加 `waiting_segments`、`pending_results`。
- 账本读取：构造时与“无任何识别会话在飞”的完成点才读取（`refresh_usage` /
  `flush_deferred_usage`），不会在采集中抢账本准入锁；#37 的提醒与试用上限行为不变。

## 本票未覆盖（后续任务）

- 退出/重启后从 `segments.json` 与保留音频恢复待处理录音和待取结果（#35）；本票只保证状态、
  文字、费用和音频都已独立落盘且序号不重用。
- 迟到结果在原目标安全时自动回填（#34）；当前恢复结果统一保守留在面板。
- 1 GiB 待处理音频总预算、磁盘余量与显式清理（#36）；本票不做过期删除。
- 真实付费网络、真实弱网积压与真实编辑器一秒回填验收（#38、#40）。
