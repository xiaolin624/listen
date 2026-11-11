# Issue #8 实现与验证记录：耳机播放路由变化与采集中断反馈

范围：[T7：耳机播放路由变化与采集中断反馈](https://github.com/xiaolin624/listen/issues/8)。工作树 `E:\luyin\.worktrees\issue-8`，分支 `implement/issue-8-device-routing`，开始基线 `9faa9c3`（含 #3/#4/#5/#6/#7/#9）。实现提交 `c20576e`，路由检查细化提交 `fa578d7`，本文档随后单独提交。未推送、未关闭 issue，主工作树未改动。

本次实现采集的端点身份、默认多媒体端点切换、当前端点移除/失活、以及中断后保留可恢复结果。工具不跟随新端点，而是在无法确认新端点仍是用户耳机路径时立即中断并如实标注。没有打开麦克风，也没有新增按应用筛选。

## 实现行为

- **端点身份**：`audio.rs` 在开始采集时通过 `IMMDeviceEnumerator::GetDefaultAudioEndpoint(eRender, eMultimedia)` 绑定端点，读取设备 ID（`IMMDevice::GetId`）和友好名称（`IPropertyStore` + `PKEY_Device_FriendlyName`），经 `Recording::endpoint` 进入 `Snapshot.endpoint`。`status.txt` 新增 `endpoint_role`、`endpoint_id`、`endpoint_name`。角色固定写为 `default multimedia render endpoint`，不笼统称“耳机”，也不把友好名称当作设备唯一标识。
- **路由监视**：采集线程每 250 ms 重新读取默认多媒体端点并与绑定时的 ID 比较。默认端点切换时返回中断错误，消息同时给出原端点和新端点的名称与 ID，并写明新端点的声音没有被混入。`GetNextPacketSize` / `GetBuffer` / `ReleaseBuffer` / `Stop` 的 `AUDCLNT_E_DEVICE_INVALIDATED`（以及音频服务停止）也转换为带端点身份的明确中断原因。`Recording::failure` 返回该精确原因，而不是泛化的“Playback capture was interrupted”。
- **中断语义**：`Application::poll` 观察到 `Recording::failure` 后自动执行 Stop，进入 `Failure`。已接收音频由原有 `fail` 保留路径登记；流式识别已写出的 `partial-result.txt` 被读入备用文本框。detail 明确写“The capture was interrupted, so this text is incomplete … This is not a completed transcription.”。不会写入 `current-result.txt`，不会冒充完整结果。
- **状态范围说明**：采集中和结果里的 Playback 描述都写明“default multimedia render endpoint '名称' (ID)”，并写明“audio routed only to the default communication endpoint or to another device is outside this capture”“no microphone is used”。无法读取端点身份时明确写“could not be identified”，不声称覆盖某个具体端点。
- **不跟随的理由**：默认多媒体端点切换到另一个设备时，新端点可能正是用户耳机（例如插拔后自动切换），也可能是扬声器。工具无法在不理解用户意图的情况下确认“新端点就是用户正在听的耳机路径”；把两个设备的声音拼成一次采集还会把另一个设备的声音误声明为耳机内容。因此选择立即中断并保留可恢复结果，而不是静默跟随。

## RED → GREEN

按纵向切片先写测试，实际观察到的失败如下。

### 1. 编译 RED（新测试先于实现）

`app/listen/tests/device_routing.rs` 先引用尚不存在的 `CaptureEndpoint`、`Recording::endpoint`、`Snapshot.endpoint`：

```text
error[E0432]: unresolved import `listen::CaptureEndpoint`
error[E0407]: method `endpoint` is not a member of trait `Recording`
error[E0609]: no field `endpoint` on type `Snapshot`
```

### 2. 行为 RED（补齐最小 API 后，3/4 失败）

最小 API 只记录端点，不生成状态文案；`fail` 也仍使用旧措辞。实际失败：

```text
capture status must state "default multimedia render endpoint":
  Snapshot { phase: Capturing, detail: "Capturing playback audio; press the hotkey again to stop.", ..., endpoint: Some(...) }

capture_without_a_reported_endpoint_never_claims_a_specific_route ... FAILED
  detail: "Capturing playback audio; press the hotkey again to stop."

the recoverable text must be labelled incomplete:
  detail: "... Incomplete recoverable text is in the fallback box and ... This is not a completed transcription."
```

第三个中断测试在旧代码上已通过（它验证既有失败保留路径），没有虚构 RED；`removed_endpoint_is_reported_with_its_identity_and_retains_received_audio` 也在第一次运行即通过，作为回归保护保留。

### 3. GREEN

新增 `tests/device_routing.rs` 4 项全部通过：

| 测试 | 验证内容 |
| --- | --- |
| `capture_status_names_the_default_multimedia_endpoint_and_its_limited_scope` | 状态包含角色、设备 ID、友好名称，以及“默认通信端点/其他设备不在采集范围”“不使用麦克风”；`Snapshot.endpoint` 保留该身份 |
| `capture_without_a_reported_endpoint_never_claims_a_specific_route` | 无法报告端点时不出现“default multimedia render endpoint”，明确写 `could not be identified` |
| `switched_default_endpoint_interrupts_capture_clearly_and_keeps_partial_audio_and_text` | 默认端点切换原因被逐字传递；进入 `Failure`；部分文本被标为 `text is incomplete` 和 `not a completed transcription`；WAV 保留；不覆盖上一次 `current-result.txt`；`endpoint` 仍是原端点 |
| `removed_endpoint_is_reported_with_its_identity_and_retains_received_audio` | `AUDCLNT_E_DEVICE_INVALIDATED` 原因、端点 ID/名称和 `incomplete` 均可见；无检查点时文本为空，不虚构结果；WAV 保留 |

最终全量：`capture_to_copy.rs` 27 passed / 1 ignored，`device_routing.rs` 4 passed，`long_sessions.rs` 9 passed，`local_quality.rs` 2 ignored（需要显式模型与语料）。共 **40 passed / 3 ignored**。

## 验收条件对应证据

| 验收条件 | 实现与证据 |
| --- | --- |
| 在实际耳机播放路径上验证采集范围，涵盖默认通信与多媒体设备不同、应用单独路由；不得以按会议软件筛选缩小范围 | 实机在 WH-1000XM4 耳机默认多媒体端点上采集并转写成功；把默认通信端点设为 Realtek 扬声器后，将 8.08 s 已知语音**只**渲染到通信端点，耳机端采集结果为 `NoSpeech`、`Playback: 0.000 s, peak=0.00000`，目标与当前结果不变。工具没有应用白名单/会议软件筛选。应用单独路由使用可控替代（见限制），同样只渲染到非默认端点，耳机端结果 `NoSpeech`。 |
| 能可靠识别并跟随设备变化时继续采集；无法确定或保持正确路径时明确反馈中断，避免继续显示一切正常 | 采集中用 `IPolicyConfig` 真实把默认多媒体端点从耳机切到扬声器；250 ms 轮询周期内发现后自动 Stop，状态 `Failure` 写明“changed during capture from '耳机 (WH-1000XM4)' (ID) to '扬声器 (Realtek(R) Audio)' (ID)”“stopped before audio from the new endpoint was mixed in”“incomplete”。没有继续显示 `Capturing` 或产生完整 `Result`。本实现选择不跟随新端点，理由见上。 |
| 设备断开或路由中断后保留已有音频或可恢复结果，清楚标明状态，不将不完整内容冒充完整采集 | 真实切换后 `partial-result.txt` 中的 `今天測試耳機音頻` 被读入备用文本框，detail 指向该文件并写“This is not a completed transcription”；`current-result.txt` 保持上一次完整结果，目标未被写入。自动测试中受控 `Recording` 同时保留一个 WAV，证明 WAV 保留路径；真实流式采集中已识别窗口的音频按 #6 设计被消费，故留的是检查点文本。 |
| 不主动开启麦克风，不误把其他播放设备的音频声明为用户耳机正在听到的内容 | 源码仅使用 `eRender`/`eMultimedia`，无 `eCapture` 或采集端点 API（验收 JSON `capture_endpoint_api_in_source=false`）；状态写明 `no microphone is used`。结果和状态总是给出实际端点角色/ID/名称；通信端点或非默认端点的 8.08 s 语音均未进入转写。真实切换时明确写“audio from the new endpoint was mixed in”。 |
| 通过既有结果交付入口完成设备变化端到端验收；没有自动回填时可用备用文本框 | 使用真实系统快捷键、真实 WASAPI 采集、真实 whisper.cpp 和焦点选定的原生 EDIT；成功场景结果只追加一次到目标，备用文本框与 `current-result.txt` 逐字一致。切换/中断场景通过同一 `Application` 交付路径进入 `Failure`，可恢复文本进入备用文本框，目标未被改写。 |
| 对实际硬件可覆盖场景给出实机证据，其余事件处理使用可控环境复现并明确标注；未实测配置不宣称通过 | 默认端点切换、默认通信/多媒体分离、耳机实际采集与交付均为实机证据（见下节）。物理拔除/禁用端点没有实机执行；`AUDCLNT_E_DEVICE_INVALIDATED` 的移除/失活分支由受控 `Recording` 测试复现，并明确标注为可控环境。应用单独路由使用可控替代，明确标注不是 Windows 11 每应用设置。 |

## 真实 Windows 桌面设备验收

2026-11-12（Asia/Shanghai），Windows 11，真实端点：

| 角色 | 名称 | 设备 ID |
| --- | --- | --- |
| 原始默认多媒体/通信/控制台 | 耳机 (WH-1000XM4) | `{0.0.0.00000000}.{352149e2-8d17-4bb5-92c3-968a7b35b7ed}` |
| 切换目标 | 扬声器 (Realtek(R) Audio) | `{0.0.0.00000000}.{f611c5b7-1080-4bbf-97e2-5c4c4b04edd0}` |

开始前把三个角色的原始端点写入 `.artifacts/issue-8-acceptance/original-endpoints.json`，随后才调用 `IPolicyConfig`（`CPolicyConfigClient`，CLSID `870af99c-171d-4f9e-af0d-e63df40c2bc9`）。脚本全程包在 `try/finally` 中，结束时还原并用 `IMMDeviceEnumerator` 重新读取三个角色确认。证据 JSON 中 `restore_verified=true`，实测还原后多媒体/通信/控制台均为原始耳机端点。

被测程序为最终格式化后源码的 release 构建：`E:\luyin\.worktrees\issue-8\app\listen\target\release\listen.exe`；转写引擎 whisper.cpp v1.8.2 CPU、`ggml-small.bin`、Silero `ggml-silero-v5.1.2.bin`、中文模式、6 线程。验收音频为 8.08 s 的已知语音：

`今天测试耳机音频。请保留这段发言，由用户检查并手动发送。`

驱动命令：

```powershell
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File `
  E:\luyin\.artifacts\issue-8-acceptance\accept-device-routing.ps1 `
  -Binary E:\luyin\.worktrees\issue-8\app\listen\target\release\listen.exe
```

脚本只操作本次创建的 Listen 进程、本次创建的测试窗口和本次已知端点；所有开始/停止都通过系统快捷键，目标由真实焦点选定，未向 Listen 传入控件句柄。

| 实测场景 | 观察结果 |
| --- | --- |
| 耳机默认多媒体端点正常采集并交付 | 采集状态写明 `default multimedia render endpoint '耳机 (WH-1000XM4)' (ID)`、完整范围说明，`Capturing` 期间只显示状态。真实转写为 `今天測試耳機音頻。請保留這段發言,由用戶檢查並手動發送。`；目标恰为 `原始草稿：Rust 123。\r\n第二行保留。\r\n` + 该转写，只追加一次；备用文本框与 `current-result.txt` 逐字一致。截图 `routing-result.png` 可见端点与完整结果。 |
| 默认通信与多媒体不同，声音只走通信端点 | 设置多媒体=耳机、通信=扬声器；把同一 8.08 s 语音只渲染到扬声器（通信端点），同时采集耳机。停止后为 `NoSpeech`，`Playback: 0.000 s, 48 kHz/2 ch/32 bit; peak=0.00000`；目标、当前结果不变。截图 `routing-communication-not-captured.png` 可见 `NoSpeech`、多媒体端点身份和保留的旧结果。 |
| 采集中真实切换默认多媒体端点 | 在耳机上采集并播放同一语音，约 3 s 后通过 `IPolicyConfig` 把默认多媒体端点切到扬声器。状态变成 `Failure`：`The default multimedia render endpoint changed during capture from '耳机 (WH-1000XM4)' (ID) to '扬声器 (Realtek(R) Audio)' (ID). Capture stopped before audio from the new endpoint was mixed in; received audio is retained and this capture is incomplete.` 随后 `partial-result.txt` 的 `今天測試耳機音頻` 进入备用文本框，detail 写明文本不完整、不是完整转写；`current-result.txt` 和目标保持上一次完整结果；`retained_audio=` 为空（流式窗口已被识别消费），检查点文本仍在。截图 `routing-switch-failure.png` 目视可见两个端点、`Failure` 和不完整文本。 |
| 应用单独路由的非默认端点（可控替代） | 恢复默认多媒体/通信均为耳机后，把同一语音只渲染到非默认扬声器端点，采集耳机。结果为 `NoSpeech`、`0.000 s`、`peak=0.00000`；目标、当前结果不变。截图 `routing-nondefault-not-captured.png`。该场景是**替代**：由验收进程用 WASAPI 直接渲染到非默认端点，没有使用 Windows 11“每应用声音输出”设置，也没有修改用户的应用路由配置。 |

真实切换后已用 `SetDefaultEndpoint` 把多媒体、通信端点还原为原始耳机端点；脚本 finally 还再次还原并读取确认。另一次独立探测（`probe-endpoints.ps1`）在验收结束后重新读取，多媒体/通信/控制台均为 `耳机 (WH-1000XM4)`、ID 与原始值一致。设备切换次数为 4 次 `SetDefaultEndpoint`：通信→扬声器、多媒体→扬声器、多媒体→耳机、通信→耳机；均为本任务分离/切换/还原所必需。

证据：

- `.artifacts/issue-8-acceptance/device-routing-evidence.json`：原始/还原端点、4 个场景完整状态、转写、目标全文、部分文本、保留音频、输入事件计数、`restore_verified=true`、`automatic_send_count=0`、`enter_key_count=0`。
- `.artifacts/issue-8-acceptance/original-endpoints.json`：切换前的原始端点。
- `.artifacts/issue-8-acceptance/routing-result.png`、`routing-communication-not-captured.png`、`routing-switch-failure.png`、`routing-nondefault-not-captured.png`：均已目视检查，文字清楚。
- 驱动源码 `accept-device-routing.ps1`、`AudioRoute.cs`、`ListenNativeQa.cs`、`NativeTargetFixture.cs` 与音频均只放在 `.artifacts`，未入库。

## 默认通信/多媒体分离的实测结论

- 本机原始配置三个角色都指向 WH-1000XM4 耳机，因此不能靠原始配置证明通信/多媒体分离。
- 验收中实际把默认通信端点切到 Realtek 扬声器，使多媒体（耳机）与通信（扬声器）不同；把已知语音**只**渲染到通信端点后，耳机端点采集为 `NoSpeech`、`0.000 s`、`peak=0.00000`，目标与当前结果都没有变化。
- 结论：本工具只采集默认多媒体渲染端点。仅路由到默认通信端点的声音**不在本次采集范围内**，状态和本文档都明确说明，不能声称已覆盖。默认通信端点即使与多媒体端点不同，也不会成为采集源，也不会被声明为用户耳机正在听到的内容。

## 应用单独路由的结论

Windows 11 的“每应用声音输出”需要修改系统每应用策略，可能持续影响用户配置；本次没有通过该 UI 或未公开的应用路由 API 实测。按任务允许，使用可控替代：验收进程通过 WASAPI 把同一已知语音直接渲染到非默认扬声器端点，同时工具采集默认耳机端点；结果 `NoSpeech`、`0.000 s`、目标与当前结果不变。该结果证明“非默认端点的声音不会被当作默认耳机内容”，但**不能**代替真实 Windows 每应用路由 UI 的端到端验收；文档与证据均标注为替代。

## 设备移除/失活的结论

真实硬件没有拔除/禁用端点，因此没有把物理移除记为已实测。`AUDCLNT_E_DEVICE_INVALIDATED` 和音频服务停止路径由 `audio.rs` 映射为带端点身份的明确中断；自动测试 `removed_endpoint_is_reported_with_its_identity_and_retains_received_audio` 用受控 `Recording` 复现相同事件顺序，验证状态、保留 WAV 和不虚构文本。真实默认端点切换已实测，覆盖了“旧客户端仍存活但默认路由已变”的更隐蔽情况。

## 构建与静态检查

```powershell
cd E:\luyin\.worktrees\issue-8
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test        # 40 passed / 3 ignored
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat # 通过
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Build       # 离线锁定 release 构建通过
git diff --check                                                        # 通过
```

最终 `-Task Build` 为增量 release 构建（11.79 s）；实现初期干净依赖构建为 49.25 s。产物为 `app\listen\target\release\listen.exe` 和相邻 `libunwind.dll`。自动行为测试不连接桌面、不运行真实 WASAPI 或识别引擎；桌面证据来自上节独立验收。

## 已知限制与未覆盖

- **不跟随新端点**：本实现选择中断而不是跟随。若产品需要“插拔耳机后自动继续采新端点”，需要额外确认新端点是否仍是用户预期路径，不能直接复用本次策略。
- **轮询而非 `IMMNotificationClient`**：默认端点变化靠 250 ms 轮询发现（移除/失活另有 WASAPI 错误即时返回），没有注册 COM 通知回调。真实切换已验证会在轮询周期内中断；更短延迟或事件回调未验证。
- **物理移除/禁用未实测**：没有拔线、禁用设备或重启音频服务；该分支只由受控测试覆盖，未宣称实机通过。
- **每应用路由是替代**：没有使用 Windows 11 每应用声音输出设置，也没有修改用户应用路由；只验证了直接渲染到非默认端点的声音不会被采集。
- **端点/设备覆盖有限**：本机只有 WH-1000XM4 与 Realtek 扬声器两个活动渲染端点；未验证蓝牙免提/立体声切换、USB 耳机、HDMI、虚拟音频设备或其他 Windows 版本。
- **流式中断的音频保留**：真实流式采集中已识别的窗口音频按 #6 有界设计被删除，真实切换场景留下的是 `partial-result.txt` 检查点，不是 WAV；自动测试覆盖了 WAV 保留路径。状态已明确说明文本不完整，不把检查点冒充完整采集。
- **未验证“全局默认切换”之外的系统级路由**：Windows 的每应用/通信路由策略、独占模式应用和音频增强驱动不在本轮实机覆盖内。
- 本机原始默认多媒体与通信端点已在验收 finally 中还原并读取确认；原始值也保存在 `.artifacts/issue-8-acceptance/original-endpoints.json`。
