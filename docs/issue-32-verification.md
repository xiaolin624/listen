# #32 断网不停采、联网后自动补处理：实现与验证

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)，基于 `integration/cloud-recognition` 的 #29 云端试用实现。**本票交付的是可离线验证的应用级恢复能力；真实付费网络、真实目标的一秒回填与正式验收属于后续任务（#38、#40），未在本票运行。**

## 已实现行为

- **采集与识别连接分离。** 开始后立即保存连续 PCM；首次连接未就绪、连接失败或录到一半断线都不结束采集。连接未就绪时开头音频已在磁盘，不因握手晚到而丢失。
- **等待状态真实可见。** 短暂断线、超时、限流和临时服务错误进入 `SessionState::WaitingForNetwork`，面板/状态说明为“Waiting for the network…”，采集阶段仍为 `Capturing`（停止后为 `Processing`）；不把等待联网显示成识别失败，也不把设备中断显示成等待联网。
- **有界自动退避重连。** 重连延迟从 100 ms 起按次倍增，单次上限 2 s；每个片段最多 8 次尝试，且**每次临时故障开启自己的 20 s 重试窗口**（不是从采集开始计时），所以长采集进行到数分钟后才断线仍会自动恢复；窗口内再次失败仍受 8 次总上限约束，无限失败一定会在上限内停止。达到界限即停止自动重连、保留录音并说明原因；用户停止后仍可通过现有 `Retry` 重新处理同一段音频，无需重录。
- **不可自动恢复的故障不重试。** 凭据、余额、型号/额度、配置、协议、预算、账本和存储类错误给出原有可操作提示，不发起第二次连接；采集继续保存，用户停止后进入可重试的失败状态。
- **设备中断按真实采集失败处理。** 播放设备消失时立即结束采集并以设备原因失败，与等待联网区分；不会把已无法收音误报成只需等待网络。
- **从保留音频重建会话，不假定断点续传。** 已连接后的断线会重建新任务并从头重放本段连续 PCM；新任务从空文字开始，因此上一连接的未确认分句不会重复拼接。重放按磁盘文件分块读取，内存不保存整段音频。乱序/重复的 `task-finished` 不会二次交付；有未定稿分句的提前确认仍拒绝。
- **迟到结果保守交付。** 经历等待或恢复的结果标记为迟到结果，完整文字先原子写入 `current-result.txt`，再作为 `Delivery::Fallback` 放入面板，不自动写入原目标；自动回填的安全条件判断留给 #34。
- **先保存再释放音频。** 完整文字可靠保存后才删除本段生产临时音频；保存失败保留音频、不写目标，完整文字保留在面板。
- **请求与费用绑定同一段。** 每次连接/重建都是独立 task，先持久化共享账本预留再发送；账本 `segment_id` 始终是同一个片段身份，重试费用不串段，也不承诺重传免费。
- **边界沿用既有预算。** 单段 PCM 上限、上传字节上限、转写文字和网络帧/分句内存上限继续生效；本票不新增存储清理策略（#36）。

## 应用入口测试

新增 `app/listen/tests/cloud_offline.rs`，全部通过 `Application::dispatch` / `poll` / `snapshot` 观察，仅替换播放设备、凭据和远端 socket，不发起真实付费请求：

1. `capture_started_while_offline_keeps_the_beginning_and_recovers_without_re_recording`：开始即离线，开头保留，自动连上后完整上传并得到完整文字；恢复结果进入面板，音频释放，费用记录同段。
2. `a_mid_capture_drop_keeps_recording_and_rebuilds_the_task_from_retained_audio`：录到一半断线仍继续采集；重连后从头重放已保留音频，文字不重复、不缺失，结果保守进入面板。
3. `a_drop_at_stop_waits_for_the_network_and_confirms_the_saved_recording_once`：停止时断线显示等待；恢复后用完整录音重建，只确认一次，重复确认不重复交付。
4. `account_and_model_errors_keep_the_capture_alive_and_are_never_retried`：余额/型号/凭据错误在采集中如实提示且不重试；停止后保留完整可重试录音，目标未写入。
5. `a_credential_rejection_at_connect_keeps_the_capture_and_is_not_retried`：连接阶段凭据被拒同样不重试、不结束采集。
6. `a_playback_device_failure_is_a_capture_failure_not_a_network_wait`：设备中断与等待联网区分，并停止后续重连。
7. `bounded_automatic_retries_then_an_explicit_retry_replays_the_retained_recording`：用可注入时钟推进自动重试窗口，确认尝试次数/请求数有界且同段；随后 `Retry` 只重放保留音频得到完整文字。
8. `a_recovered_result_that_cannot_be_saved_keeps_the_audio_and_never_writes_the_target`：恢复结果保存失败时保留音频与完整文字，不写目标。
9. `a_connection_dropped_long_after_capture_start_gets_its_own_retry_window`：采集进行 45 秒后再断线，仍按故障时刻的新窗口自动重连并恢复完整结果，不会因采集已超过 20 秒而零重试失败。
10. `an_endless_failure_still_stops_after_the_bounded_attempts`：时钟不前进、每次连接立即失败的无限故障，仍在 8 次尝试上限后停止并保留可重试录音。

## 既有测试调整

`cloud_trial.rs` 中把“云端错误立即失败”的旧断言改为本票语义：可自动恢复错误进入等待；余额/型号/额度和时限错误在采集中提示并保留录音，停止后才失败；手动重试不再声称是本地识别。`recognition_sessions.rs` 的测试适配器补上 `SessionResult::recovered = false`。

## 验证命令与结果

```powershell
.\scripts\build-listen.ps1 -Task Format
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task Build
```

离线、锁定依赖构建与测试通过：**211 passed / 0 failed / 16 ignored**（忽略项均为需要真实模型、真实密钥或操作员编辑器的既有测试）。本票新增的 8 个应用级测试全部通过。

## 本票未覆盖（后续任务）

- 多段并行采集、新段优先、旧段队列与多结果面板（#33）。
- 迟到结果在原目标安全且未被改写时自动回填（#34）；当前统一保守进入面板。
- 退出/重启后恢复未完成录音与待取结果（#35）。
- 1 GiB 待处理音频总预算、磁盘余量底线、跨段统计与用户清理（#36）。
- 累计用量/费用面板与每月提醒（#37）。
- 停止到完整回填的真实一秒延迟优化与真实目标验收（#38、#40）。
- 本票未运行任何真实付费网络调用；原生适配器的真实弱网表现仍需在 #40 的验收中确认。
