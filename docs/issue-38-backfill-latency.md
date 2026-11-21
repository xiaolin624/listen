# #38 停止到完整确认回填的延迟：实现与受控回归

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)，基于
`integration/cloud-recognition`（`875f3a0`，已含 #32/#33/#34/#36/#37）。
**本轮完成的是统一计时、可测量的阶段拆分、以及可离线受控回归的调度/等待优化；
真实付费云识别 + 真实编辑器的端到端 1 秒验收没有在本轮运行，因此本文不声称 1 秒门槛已满足。**
固定分母的 20 次正式验收属于后续任务。

## 一个从停止到确认的数字

`Snapshot::timing` 新增 `stop_to_confirmed_ms`：从用户按下停止（`Control::Stop`）到
**真实目标输入框的完整替换文字被读回确认、且确认追加已可靠写入 `delivery.json`** 为止。
它是 #38 唯一的总数；只有真正发生确认追加（`Delivery::Appended`）时才存在；回填到面板
（`Delivery::Fallback`）没有目标读回，一律为 `None`，不会用别的时刻冒充。对迟到/恢复结果，
这个数字仍然从用户的停止算起，所以包含等待联网的时间——但正常路径的 1 秒指标只按正常
会话评估。

原有的 `stop_to_result_ms` 保持原义（停止到应用线程应用该结果），用于区分"目标已经写完"
与"应用已经显示/记录完"。两者之差是 `apply_wait_ms`：确认之后等待下一次 poll 的时间，
它不可能改变目标文字。

阶段拆分（全部是墙钟毫秒，同一次停止路径的同一时钟）：

| 字段 | 阶段 |
| --- | --- |
| `finalize_ms` | 停止收尾：本地设备 join；云端为设备 join + PCM 落盘完成 |
| `cloud_drain_ms` | 云端：音频收尾完成 → 发出 `finish-task`（最后的上传排空） |
| `cloud_final_ms` | 云端最终确认：`finish-task` → 厂商 `task-finished`（只认整段确认） |
| `event_wait_ms` | 应用调度：会话最终确认 → 应用开始可靠保存（停止时的持久化 + 事件投递 + poll 节拍） |
| `save_ms` | 可靠保存：本段完整文字 + `segments.json` + `current-result.txt` |
| `delivery_queue_ms` | 应用调度：保存完成 → 交付线程真正开始 |
| `delivery_ms` | 目标交付：草稿读取 + 一次写入 + 完整读回确认 + 交付记录两次落盘 |
| `delivery_read_ms` / `delivery_write_ms` | 交付内部：写前草稿读取 / 写入+读回确认 |
| `delivery_lock_ms` | 等待"同一目标只有一个写入者"的锁（旧段交付积压在这里可见） |
| `delivery_save_ms` | `delivery.json` 写入开始与确认两次保存（随历史增长的部分） |
| `apply_wait_ms` | 确认完成 → 应用应用结果（只影响显示/记录，不影响目标） |
| `schedule_ms` | `event_wait_ms + delivery_queue_ms + apply_wait_ms` |
| `cloud_connect_ms` / `first_task_started_ms` | 首次连接：最后一次连接握手 / 会话开始到首个 `task-started` |

`latency.jsonl` 追加以上全部键；`status.txt` 追加 `timing_stop_to_confirmed_ms` 等 13 个
`timing_*` 键，原有键与语义不变。受控测试断言这些阶段与总量在同一容差内对得上
（每个阶段各自截断到毫秒，8 段误差 ≤ 8 ms）。

## 受控实测（同一 `Application::dispatch` / `poll` / `snapshot` 接口）

测试文件 `app/listen/tests/backfill_timing.rs`（5 个）和 `app/listen/tests/cloud_trial.rs`
新增的 1 个测试。识别适配器与目标都注入"已确认的目标行为"：识别适配器只发最终事件，
目标编辑器读草稿 5 ms、写+读回 25 ms；云测试把整段确认延迟注入为已知的 40 ms。
没有任何真实网络、音频设备或桌面编辑器参与。

### 一次确认回填的实际阶段

`one_confirmed_backfill_reports_stop_to_confirmed_and_every_stage` 的实测输出：

```text
measured stages: stop_to_confirmed=79 stop_to_result=84 save=22 event_wait=12
delivery_queue=0 delivery=45 delivery_read=5 delivery_write=25 apply_wait=4
```

（本地路径，`finalize_ms`/`cloud_final_ms` 为 `None`；目标读草稿注入 5 ms、写+读回注入 25 ms。
`delivery=45` 里 30 ms 是注入的目标耗时，其余是 `delivery.json` 的两次落盘。
`saved=22` 是本机 `%TEMP%` 上三次可靠保存（本段文字 + 索引 + `current-result.txt`）的实际耗时。）

结果：`stop_to_confirmed_ms` 与 `stop_to_result_ms` 都来自实际墙钟；`delivery_read_ms` ≥ 5、
`delivery_write_ms` ≥ 25 是注入的真实耗时；`save_ms` 是真实落盘；`schedule_ms` 等于三个
调度间隙之和；阶段和与总量的差在截断误差内。`stop_to_confirmed` 正好比 `stop_to_result`
少一次 `apply_wait`（应用应用结果的那一拍）。

### 云端最终确认与收尾

`the_cloud_final_confirmation_and_drain_are_measured_as_their_own_stages` 用真实
`CloudSessions` + 受控 socket，把厂商 `task-finished` 延迟 40 ms：

- `cloud_final_ms` 实测 ≥ 40 ms（不是常量，是实际等待）；
- `finalize_ms`、`cloud_drain_ms`、`cloud_connect_ms`、`first_task_started_ms` 均被记录；
- `stop_to_confirmed_ms ≥ cloud_final_ms`，且 `stop_to_result_ms ≥ stop_to_confirmed_ms`。

### 积压时新段的路径

`an_older_segment_never_delays_a_newer_segments_save_or_delivery`：两段各自停止后同时确认，
一次 `poll` 内新段的事件就被应用并开始可靠保存；旧段仍按采集顺序、每 poll 最多处理一段。
第二次 `poll` 先应用新段已确认的交付，旧段的交付在后续 poll 应用；最终两段文字各追加一次。

### 交付记录增长

`the_delivery_history_is_bounded_while_confirmed_appends_keep_working`：连续 60 次确认追加后
`delivery.json` 只剩 5 条记录、约 57 KiB；第一次与最后一次的 `delivery_save_ms` 见输出：

```text
有界   （本轮）：records=5  bytes=56957  first_save_ms=14  last_save_ms=19
未修剪（同一测试、临时关闭修剪）：records=60 bytes=361628 first_save_ms=13 last_save_ms=34
```

未加界限时每条已确认记录都保留 `baseline`/`before`/`after`（各自是整份草稿），文件与两次
保存都随会话线性增长（60 段时文件已 353 KiB，最后一次交付的记录保存是最初的 2.6 倍）；
本轮按"只保留最近已解决记录 + 字节上限"收敛，未解决的
`pending`/`started`/`uncertain` 记录永不删除。修剪只可能让迟到结果更保守（缺少链节 →
保留为待取结果），不可能让一次写入被误判为安全。

## 这次改了哪些真实的等待与调度

1. **应用调度节拍（`gui.rs`）**：空闲仍是 100 ms；停止后 `Phase::Processing` 期间改为
   10 ms。停止路径上有两次"等下一拍"（会话最终事件、交付完成的应用），旧节拍下每次最多
   100 ms，新节拍下每次最多 10 ms。等待联网（`WaitingForNetwork`）不加快，避免把长时间
   断网变成忙轮询。真实 GUI 的节拍差没有在本环境实测（没有真实端到端运行），这里报告的是
   机制与上界，不当作实测延迟。
2. **积压队头阻塞（`poll`）**：最新一段的会话事件先处理，随后每 poll 至多处理一段更旧的
   积压；结果也先应用最新段。旧段之间仍按采集顺序逐段处理（#33 的"后台一次一段"保留），
   但新段不再为旧段的每个事件/交付各等一整拍。代价：交付结果进入应用状态的先后仍按"新段优先"，
   但**目标输入框的追加已由单条有序交付通道按采集顺序先旧后新执行**；本票最初的"目标里可能新段在先"
   取舍已被后续审查修正，因为规格要求语音顺序不能倒置，而"新段优先"只适用于识别/保存/面板调度。
3. **云端排空（`cloud/mod.rs`）**：原来每个循环只发一个 100 ms PCM 分片，然后固定
   `sleep(2 ms)`；恢复重放或停止排空要发 N 个分片就白等 2N ms（30 s 录音 = 600 分片 ≈
   1.2 s 纯睡眠）。现在一次循环最多连续发 64 片，且只有"这一轮既没收到事件也没发出音频"
   时才睡 2 ms；节奏由网络写决定，不由固定睡眠决定。
4. **粘贴回退的固定等待（`paste.rs` / `target.rs`）**：删除"写完剪贴板先睡 140 ms"和
   "Ctrl+V 后固定睡 220 ms"两段固定等待，把可访问值确认窗口从 300 ms 扩到 520 ms
   （= 220 + 300），因此目标发布粘贴结果的**总窗口不变**（Ctrl+V 后 140 + 520 ms，与旧
   140 + 220 + 300 ms 相同），响应快的编辑器第一轮确认就返回。上一次真实目标实测的
   "第二次交付 830 ms"中约 360 ms 属于这两段固定等待；本轮**没有真实编辑器可用，
   未重新实测**，只报告删除的是可证明被确认窗口覆盖的固定等待，不声称新的真实数字。
5. **交付记录有界（`delivery.rs`）**：已解决记录（`appended`/`fallback`）按最新 64 条与
   64 KiB 双上限修剪，每次修剪成批进行而不是逐条重新序列化；`pending`/`started`/
   `uncertain` 永不删除，最近 4 条已解决记录始终保留。安全性见上：缺链只导致保守保留。
6. **原子替换的短重试（`cloud/budget.rs`）**：Windows `MoveFileExW(REPLACE_EXISTING)` 在
   目标文件被另一个句柄短暂打开时会报共享冲突。`atomic_write` 现在只对
   `ERROR_ACCESS_DENIED` / `ERROR_SHARING_VIOLATION` 做最多 3 次、每次 2 ms 的替换重试；
   写入与 `sync_all` 成功后只重做原子改名，失败时目标文件不会被部分替换。这直接覆盖了
   已知的 `late_backfill::a_target_that_became_read_only_keeps_the_complete_late_result_pending`
   偶发失败成因（读句柄让原子替换失败→应用按设计 fail-closed）。语义未放宽：其它错误立即
   返回，`delivery.json`/账本/`current-result.txt` 的替换仍是原子的。

## 交付安全保证未变

- 写后仍读回确认；`Delivery::Appended` 只在目标读回完整替换后报告。
- 完整文字仍先可靠保存（本段文字 + 索引 + `current-result.txt`），保存失败不写目标、
  不释放音频。
- 一次交付：`begin_append` 仍在写之前落盘；`started`/`uncertain` 仍是终态，重复事件、
  重复停止、重复 poll 都不会再写一次。
- 截断仍被识别并保留完整结果；剪贴板仍先备份后恢复（非文本/不可读剪贴板不借用）；
  永不发送 Enter；不自动发送。
- 未知目标的确认窗口没有缩短：直写 `CONFIRM_TIMEOUT` 仍是 2 s；粘贴路径的总发布窗口
  通过"确认窗口 520 ms"保持与旧固定等待之和一致。
- 未定稿文字不会交付：只有会话报告整段 `Finished` 后才保存与交付。
- 旧段仍不能在未证明原草稿未变时自动回填（#34 的 fail-closed 决策不变）。

## 尚未达到 / 未测

- **真实端到端（真实云 + 真实编辑器）没有运行**：本环境没有 `LISTEN_ALI_API_KEY`，也没有
  获得本轮付费计时调用（50 元预算内）的显式授权；按 #26 的测试决策，"跳过/没有 key"
  不算成功，因此这里不报告真实端到端秒数，也不声称达到 1 秒门槛。
- 预计真实路径的主要成本在 `cloud_final_ms`（厂商整段确认往返）、首次交付到未见过的
  目标时的 2 s 确认窗口（安全设计，故意不缩短），以及丢写编辑器的粘贴回退（上次真实
  目标实测约 830 ms，本轮删除固定等待后预计下降，但未实测）。后续固定分母 20 次验收
  必须用这些阶段数字逐次报告，不能用均值、重跑最好值或离线吞吐替代。
- 首次连接、首次编辑器交付、后续运行：本轮把对应阶段变成可观测的
  `cloud_connect_ms`/`first_task_started_ms`/`delivery_read_ms`/`delivery_write_ms`，
  真实数值待真实运行产生。

## 测试与命令

- 新增 `app/listen/tests/backfill_timing.rs`（5 个）：一次确认回填的全阶段与总量对账；
  云阶段保留；fallback 不声称读回；积压不堵新段；60 次追加后交付记录有界。
- `app/listen/tests/cloud_trial.rs` 新增 1 个：真实云适配器的整段确认与排空阶段。
- `.\scripts\build-listen.ps1 -Task Test`：**279 passed / 0 failed / 16 ignored**（基线
  `875f3a0` 为 273 passed / 0 failed / 16 ignored，本轮新增 6 个测试）。
- `.\scripts\build-listen.ps1 -Task CheckFormat`：退出码 0。

## 分支与提交

- 分支 `implement/issue-38-latency`，工作树
  `E:\listen-worktrees\ticket-38-latency-20261121`，基于 `875f3a0`。
- 提交：见 `git log`（本文件所在提交，conventional message 引用 #38）。
