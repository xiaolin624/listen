# #35 退出和重启后恢复录音及待取结果：实现与验证

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)，基于 `integration/cloud-recognition`
（已含 #32 断网不停采、#33 多段、#34 迟到安全回填、#36 有界存储、#37 用量与费用，以及 #38 一秒回填的
计时与唤醒改动）。**本票交付可离线验证的退出/重启恢复语义；真实付费网络、真实编辑器与一秒回填验收
属于 #38、#40，未在本票运行。**

## 已实现行为

- **退出不再等待云端。** `--control quit`、托盘退出与窗口关闭都走 `Application::prepare_exit`：
  先 `Stop` 正在采集的段，然后只在有限时间（默认 1.5 秒；本机引擎 300 毫秒宽限）内等待播放设备把
  已收到的声音落盘（云端连续 PCM 以 `playback.state` 结束标记为准），**从不等待云端确认**。退出时
  状态文件仍写明“下次启动会恢复这段未完成录音”。此前退出会一直等到云端返回或超时，属于本票修复的
  行为。
- **重启自动发现并继续。** `Application` 构造时扫描运行时目录：读取 `segments.json`、`delivery.json`、
  每段的音频与已保存文字，立即在面板中重建各段（身份、采集顺序、状态、逐段费用、术语快照、交付状态），
  并在第一次 `poll`（GUI 的第一个 tick）起按采集顺序**逐段**自动补处理。新采集始终优先：有采集在飞时
  不启动旧段；旧段之间严格按序号一次一段；旧段失败不会挡住新采集。
- **重启后绝不用旧目标。** 目标句柄无法跨进程存活，所以恢复出的每一段都以“无可复用目标”标记：迟到的
  恢复结果只能进入面板，`started` / `uncertain` 的交付永不重放，`appended` 的记录保持原样，
  没有任何恢复路径会写窗口或输入框。
- **交付状态如实呈现。** 新增 `Delivery::Uncertain`：上一进程已经开始的写入（或交付记录不可读、被裁剪）
  既不能说“已写入”，也不能像 `Fallback` 那样声称“还没写入任何输入框”。面板与 `status.txt` 用
  `uncertain` 拼写，结果保留给用户核对与复制。`pending`/`fallback` 记录缺失时仍按面板结果处理
  （fail-closed）；`delivery.json` 被裁剪或不可读时一律 `uncertain`，绝不自动追加。
- **结果已保存但音频未清理。** 若某段的完整文字已可靠保存（`segment-NNNNNNNN.txt` 可读），恢复时
  直接重建为待取结果、**不重复识别**，并在确认文字可读之后释放该段生产音频；保存失败或文字不可读时
  保留全部音频。
- **音频已清理但仍有待取文字。** 文字是唯一恢复依据，恢复只读不删；待取结果跨重启一直保留，不按日期
  过期，也不形成永久音频库。
- **异常退出的部分音频。** 新增 `playback.state`（`committed_bytes=` + `clean_stop=` 两个键），由
  `cloud/pcm.rs` 在录制结束时原子写入；被强制结束的进程没有该标记。恢复时校验标记、字节数、16 kHz
  单声道 16 位格式描述与整样本边界：
  - 可读部分完整且已正常结束 → 直接补处理；
  - 尾部多出半个样本、或标记的已提交字节少于文件可读字节 → 在同一段目录内生成 `recovered.pcm`
    修复副本，只处理完整样本；
  - 本机引擎的识别窗口若有一窗损坏，则用可读窗口重新拼成 `recovered.wav`（按
    `capture-manifest.txt` 去掉重叠）。
  以上情况都会在段状态中明确写出“恢复出的录音不完整或无法证明完整；只处理可读部分，这不是完整录音，
  也不是合格转写”，且该说明不会被适配器后续的状态刷新擦掉。
- **索引损坏。** `segments.json` 无法解析时仍从磁盘恢复：索引未描述的完整文字按文件名序号成为待取结果
  （交付未知 → `uncertain`），索引未描述的采集目录被收养为新的采集序号并自动补处理；原始文件不删除，
  新采集绝不重用这些序号。索引被裁剪/丢失的术语快照会在真正补处理时改用当前术语表，并在段说明中写明。
- **自动补处理失败不无限重试。** 失败段保留音频、明确写出“录音保留在本机、不会自动重试”，不再阻塞
  新采集；同一段在下次启动不会再次自动产生付费请求，用户可用显式的 `delete-segment:<n>` 清理。

## 持久化契约（全部为追加，旧版本可读）

- `segments.json`（版本仍为 1）新增带 `#[serde(default)]` 的：
  - `current_sequence`：当前结果所属采集序号，随完整文字一起写入；重启据此知道哪一段拥有
    `current-result.txt`，较旧段的结果不会抢占当前结果。
  - 每段 `glossary_terms`：采集时固定的术语快照，重启补处理沿用；旧索引读为空并回退当前术语表。
- `playback.state`（会话目录内）：`committed_bytes=<n>`、`clean_stop=<0|1>`；`PcmSpoolWriter::finish`
  写 `clean_stop=1`，非正常结束的 `Drop` 写 `clean_stop=0`，被强杀则没有该文件。
- `recovered.pcm` / `recovered.wav`：仅在需要修复时生成于该段目录内，计入 #36 的待处理音频预算，
  随该段文字保存后的清理一起释放。
- `current-result.txt` 仍是当前结果镜像；恢复**不会**为待取结果再复制一份文字（避免文字预算翻倍），
  当前序号由索引持久化。

## 应用入口测试

新增 `app/listen/tests/restart_recovery.rs`（8 个，全部通过 `Application::dispatch` / `poll` /
`snapshot` 观察；第二个 `Application` 打开同一运行时目录，只继承磁盘状态；只替换识别会话与编辑器，
不发起付费请求、不使用真实播放设备）：

1. `a_stopped_capture_is_resumed_after_a_restart_and_its_result_waits_in_the_panel`：正常停止后立即退出；
   重启自动补处理同一段、沿用采集时术语快照（术语文件在两次启动之间删除）、结果只进面板、旧目标未被
   写入、完整文字落盘后音频释放；第三个进程仍显示待取结果且**从不重复识别**。
2. `several_recovered_segments_keep_their_identity_and_order_and_a_new_capture_wins`：两段同时等待；
   恢复按序号 [1, 2] 逐段补处理；旧段在飞时新采集照常开始并正常追加到它自己绑定的目标；旧段恢复结果
   仍在面板、不抢当前结果、不写目标。
3. `a_result_saved_before_the_audio_was_cleaned_is_kept_pending_and_never_re_recognized`：直接构造
   “文字已保存、音频未清理、交付未落下”的崩溃点；恢复不重建识别会话、释放音频、把决定写回索引；
   再次重启仍保留待取结果与文字。
4. `a_started_or_uncertain_delivery_is_never_replayed_after_a_restart`：`started` 与 `uncertain`
   交付的两段恢复为 `Uncertain`，目标未被写入，`delivery.json` 原样保留（不倒退回 `fallback`）。
5. `corrupt_state_does_not_hide_saved_results_or_lose_an_unindexed_recording`：索引损坏时仍恢复
   未描述的完整文字（序号保持 3）与未描述的录音目录（收养为序号 4、自动补处理）；不删原始文字；
   新采集使用序号 5，不覆盖任何恢复内容。
6. `partial_audio_from_an_abnormal_exit_is_repaired_and_clearly_marked_incomplete`：无结束标记且尾部
   半个样本、以及标记只提交一半字节的两种异常退出；只处理完整样本、生成修复副本、保留原文件，段说明
   明确“不是完整录音/合格转写”，且该说明在补处理过程中一直可见。
7. `a_failed_automatic_resume_keeps_the_audio_and_never_blocks_a_new_capture`：自动补处理被适配器拒绝
   时保留录音并如实报告；新采集照常工作；下一次启动不再自动重试；显式删除仍可用。
8. `quitting_does_not_wait_for_the_cloud_and_the_next_launch_recovers_the_capture`：云端一直不返回时
   `prepare_exit` 在限定时间内返回、录音已带结束标记落盘；下次启动恢复同一段并补出结果，旧目标不变。

另有 `src/segments.rs` 1 个单元测试：没有术语快照字段的旧版索引仍可读取（空快照）。

## 验证命令与结果

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
```

离线、锁定依赖构建与测试通过：**288 passed / 0 failed / 17 ignored**（忽略项都是需要真实模型、真实
密钥或操作员编辑器的既有测试，不计为通过）。`cargo fmt --check` 通过。

## 与 #38（200e9ef）合并后的核对

- **不改 `Timing` / `latency.jsonl` 既有键**：本票只给恢复段使用 `Timing::default()`，`record_latency`
  未改，`timing_*` 与新增云端阶段键保持追加顺序。
- **恢复路径没有固定 sleep**：启动恢复只在 `poll` 中推进；`prepare_exit` 仅在退出时以 5 毫秒间隔等待
  设备落盘（有界 1.5 秒），不属于恢复路径。GUI 的 100 ms / 10 ms 轮询没有被本票改动扩大。
- **交付记录被裁剪 → 一律面板**：`delivery.json` 裁剪只影响已解决的 `appended`/`fallback`；
  缺失记录或缺链一律按面板结果处理，`started`/`uncertain` 永不裁剪、永不重放。
- **不把 `segment-NNNNNNNN.txt` 当作“每个完成结果都存在”的前提**：正常追加成功后该文件与索引一起
  删除；只有待取/清理失败/崩溃点才保留。因此恢复以索引 + 交付记录为主，文字文件只在索引指向它且可读时
  使用；索引损坏时才把未描述的文字文件作为 `uncertain` 面板结果保留。
- **当前结果顺序显式持久化**：`current_sequence` 随文字写入索引，避免“较旧段后完成抢占当前结果”；
  恢复与测试都显式断言采集顺序 [1, 2]。
- **交付顺序变化**：本票不改变 #38 的“最新段优先、积压一次一段”应用顺序；恢复补处理的顺序由本票显式
  保证并按序号逐段进行。

## 本票未覆盖（后续任务）

- 真实付费网络、真实弱网/重启组合、真实编辑器与一秒回填验收（#38、#40）。
- 恢复出的旧结果自动回填：规格明确重启后一律面板，本票不实现跨进程回填。
- 退出时把“已收到的声音”补齐到用户松开停止键为止：异常退出仍可能丢失最后一次未落盘的 100 ms 音频，
  该段会被明确标记为不完整。
