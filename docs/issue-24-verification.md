# #24 全量桌面验收回归

状态：**完成**——工单 #24 的 6 条验收点里 4 条 **PASS**、1 条 **部分**（精修的真实 API 路径未覆盖）、
1 条（文档）**PASS**。实施日期 2026-11-24，分两段完成，合并在同一分支
`implement/issue-24-desktop-acceptance`（工作树 `E:\luyin\.worktrees\issue-24`）。主工作树 `E:\luyin`
的 7 份用户改动全程未被触碰、未 add、未 commit、未 stash。

## 口径（全篇适用）

> **验收 oracle 是控制通道 `--control` + `status.txt` / `panel.txt` / `current-result.txt`
> （以及 `delivery.json`）；窗口类名、窗口样式、控件 ID 只作自检，不作验收依据。**

- 所有桌面验收**串行**执行，同一时刻只跑一个，项与项之间确认 `Get-Process listen` 为空。
- 识别一律走本地引擎（包内 `engine\sherpa` + `models\sense-voice` 的 SenseVoiceSmall q8）；
  `LOCALAPPDATA` 指向隔离目录，`LISTEN_ALI_API_KEY` / `LISTEN_ALI_WORKSPACE_ID` 未设置，
  全程不走云端识别路径、不产生付费请求、不消耗百炼额度。
- 交付件直接用 `E:\luyin\.worktrees\issue-23\dist\Listen-0.1.0`（manifest `git_commit=9a53e9aa…`、
  `git_dirty_at_package_time=false`、21 文件 / 264,881,704 B），**没有重新打包**。该目录下另有 #23
  流程遗留的 `runtime\`（7 文件 / 5,707 B，git 忽略、不在 manifest 内）；本段所有运行都用 `--runtime`
  指到 `.artifacts` 下，未向该遗留目录写入任何内容。另有一份逐字节相同的副本放在
  `%TEMP%\listen-24-user-machine\Listen-0.1.0`，用来证明包不依赖仓库里的 engine/model 目录。
- 浏览器通道用 **Edge 158 + `--force-renderer-accessibility`**（§2.2）；Chrome 157 在本机不向 UIA
  暴露渲染树（§10 偏差 2）。
- 证据目录 `.artifacts\issue-24\` 被 Git 忽略；仓库内的交付物是本文件与 `scripts\` 下的脚本。

## 结论摘要

| # | 工单 #24 验收点 | 结果 | 证据位置 |
| --- | --- | --- | --- |
| 1 | 原有目标追加、重复停止、邻居不变等场景全部通过 | **PASS** | 原生 EDIT 目标 4 例（§2.1）+ 浏览器夹具 `plain\|rich`（§2.2） |
| 2 | 只读、容量截断、目标失效仍进备用面板且不误写 | **PASS** | 三例全过（§3）；只读的**浏览器**子路径今日未复现，见 §3.1 与偏差 1 |
| 3 | 识别失败重试、成功清理、设备中断、长会话回归通过 | **PASS** | 失败重试 5 例（§4）+ 设备中断（§5）+ 600 s 长会话（§6） |
| 4 | 胶囊置顶、不抢焦点、共享隐身有截图或可复核证据 | **PASS** | 胶囊 43/43 + 面板 46/46 验收（§8），5 张 PNG |
| 5 | 精修自动回填与手改不覆盖两条路径都有证据 | **部分** | 应用层 15 项测试 + 包上「无 key 不可用」桌面证据（§7）；**真实 DeepSeek API 往返未覆盖**，两条路径缺桌面级证据 |
| 6 | 验收记录更新到仓库文档 | **PASS** | 本文件 |

## 1. 两个验收脚本的 SenseVoice / 包内 exe 支持（红 → 绿）

### 1.1 改了什么

`scripts\verify-capsule.ps1`、`scripts\verify-panel.ps1` 增加同一套引擎选择参数，两种用法都支持：

| 参数 | 语义 |
| --- | --- |
| `-Engine sensevoice\|sense-voice\|sense_voice` | 本地 SenseVoice；四个路径缺省时由应用按 `root()` 推导（仓库内构建） |
| `-Engine whisper` / `-Engine <whisper-cli.exe 路径>` | 原 whisper 用法（关键字不再是路径，见下） |
| `-PackageRoot <解包目录>` | 包布局：自动推导 `listen.exe` + `engine\sherpa` + `models\sense-voice\*`，隐含 SenseVoice |
| `-SherpaLib` / `-SenseVoiceModel` / `-SenseVoiceTokens` / `-SenseVoiceVad` / `-SenseVoiceLanguage` | 显式 SenseVoice 路径，用于任意位置的包或仓库构建 |
| `-Model` / `-VadModel` | whisper 模型覆盖（不变） |

两个脚本都新增引擎来源断言（属验收组）：`recognition_mode=local`、`backend` 含 `SenseVoice q8`，
并在给了 `-SherpaLib` 时断言运行中进程**实际加载的** `sherpa-onnx-c-api.dll` 就是该目录下的那份
（`Process.Modules`）。失败态改用参数覆盖而不是追加重复的 `--sensevoice-model`。

顺带修掉一个真实缺陷：旧 `verify-capsule.ps1` 的 `-Engine` 会经过 `ConvertTo-QuotedWindowsPath`
变成绝对路径，所以 `-Engine sensevoice` 会被应用当成“whisper-cli 路径”。现在引擎关键字先分类
（`sensevoice` / `whisper` / 其它=路径），只有路径形态才转绝对路径。

另外 `scripts\verify-generic-editor.ps1` 原本是**无 BOM 的 UTF-8 且含中文字面量**，Windows
PowerShell 5.1 按 ANSI 读取会直接解析失败（`The '<' operator is reserved`）。加了 UTF-8 BOM，
使 5.1 能解析。**维护提示：任何工具改写该文件后必须确认首字节仍是 `EF BB BF`**，否则 5.1 会重新
解析失败（本段踩到过一次，已恢复）。

### 1.2 红（未改之前，对**新包**跑旧脚本）

命令（工作目录 `%TEMP%\listen-24-user-machine\Listen-0.1.0`，隔离 `LOCALAPPDATA`）：

```powershell
# 旧 verify-panel.ps1 完全没有引擎参数
powershell -File scripts\verify-panel.ps1 -Exe <包>\listen.exe -Runtime <red>\panel-runtime `
  -Evidence <red>\panel-evidence -TimeoutSeconds 25
# 旧 verify-capsule.ps1 只有 -Engine/-Model/-VadModel
powershell -File scripts\verify-capsule.ps1 -Exe <包>\listen.exe -Engine sensevoice `
  -Runtime <red>\capsule-runtime -Evidence <red>\capsule-evidence -TimeoutSeconds 25
```

| 脚本 | 结果 | 关键输出 |
| --- | --- | --- |
| 旧 `verify-panel.ps1` | 退出码 **1**，`acceptance 0/0` | `Timed out waiting for phase Ready. Last status: phase=Failure`；`detail=Cloud trial requires a valid Alibaba workspace…`；`recognition_mode=cloud-trial`。即：旧脚本只能启动**云端默认**，与包内本地引擎无关。 |
| 旧 `verify-capsule.ps1` | 退出码 **1**，`0 checks, 1 failures` | `Timed out waiting for phase Ready. Last status: phase=Failure`；`detail=Local whisper.cpp executable is missing: …\Listen-0.1.0\sensevoice`。即：`-Engine sensevoice` 被转成了绝对路径，应用去把它当 whisper CLI。 |

证据：`.artifacts\issue-24\red\red-panel-old-script.log`、`red\red-capsule-old-script.log` 及各自的
`*-verification.json`。

### 1.3 绿（改之后，同一批包）

```powershell
powershell -File scripts\verify-capsule.ps1 -PackageRoot <包> -Runtime <..> -Evidence <..>
powershell -File scripts\verify-panel.ps1   -PackageRoot <包> -Runtime <..> -Evidence <..>
```

| 脚本 | 结果 | 耗时 | 引擎来源证据 |
| --- | --- | --- | --- |
| `verify-capsule.ps1` | **PASS**，43 检查 / 0 失败 | 15 s | `status.txt` `recognition_mode=local`、`backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s)…`；进程模块 `…\Listen-0.1.0\engine\sherpa\sherpa-onnx-c-api.dll` 与 `onnxruntime.dll` 均来自包内 |
| `verify-panel.ps1` | **PASS**，46/46 验收 + 42/42 自检 / 0 失败 | 15 s | 同上（两条新验收断言 + 模块路径断言） |

包级其它实测（`status.txt`）：`startup_capsule_ms=35`、`capsule=Ready`、`preparation=Ready`、
`capsule_capture_excluded=1`；停止到结果、拖拽位置持久化、`--control quit` 干净退出、无残留进程。

证据：`.artifacts\issue-24\green-capsule-package.log`、`capsule\capsule-verification.json`、
`capsule\capsule-{ready,capturing,processing,failure}.png`、`capsule\desktop-capsule-excluded.png`；
`green-panel-package.log`、`panel\panel-verification.json`、`panel\panel-{ready,capturing,fallback,failure}.png`。

## 2. 目标追加、重复停止、邻居不变（验收点 1）

### 2.1 原生 EDIT 目标（新包原地）— **PASS**

`scripts\verify-native-target.ps1` 第一例：

| 断言 | 实测 |
| --- | --- |
| 追加语义 | `delivery.json` 最新记录 `state=appended`；目标文本 = 原草稿 + `\n` + 完整结果，且转写串只出现 **1** 次 |
| 重复停止 | 第一次 `--control stop` 后立刻再发一次 `--control stop`，结果仍只有一次追加；结果出来后再发一次 stop，目标文本与 `current-result.txt` 逐字符不变 |
| 邻居不变 | 相邻 EDIT 与其他窗口 EDIT 逐字符不变 |
| 0 Enter / 0 Submit | `PackageFixture` 计数 `enter_key_count=0`、`send_count=0`（真实 SendInput/回车钩子计数） |
| 面板 | `panel_visible=0`（追加成功不打开面板） |
| 临时音频 | 成功后 runtime 下无 `*.wav` |

证据：`.artifacts\issue-24\native-target-delivered\target-evidence.json`、`target-append.png`、
`native-target-delivered.log`；隔离副本同结果 `.artifacts\issue-24\native-target\target-evidence.json`。

### 2.2 浏览器夹具（`verify-generic-editor.ps1`）— **PASS**

第 1 段此路径记为 BLOCKED：Chrome 157.0.8103.79 在本机**不向 UIA 暴露渲染树**（夹具窗口
`docs=0 edits=1`，唯一 Edit 是地址栏；同机其它 Chrome 窗口 `docs=1 edits=2`，说明客户端与跨进程访问
正常）。本段换用 **Edge 158.0.4370.33**，并定位到真正的原因：

- `-Chrome` 本来就接受任意 Chromium 可执行文件路径。Edge 默认同样不建渲染树，
  但**加 `--force-renderer-accessibility` 后**，夹具窗口对 UIA 暴露 5 个 Edit / 4 个页面 Edit，
  其中 `Listen plain target`、`网页富文本编辑区`、`Listen non-target` 三个具名 Editor 全部可见，
  满足驱动需要。
- 因此给 `verify-generic-editor.ps1` 的浏览器命令行加了该开关（对 Chrome 无害，对 Edge 必需），
  并把清理用的进程过滤从写死的 `chrome.exe` 改成**所传入浏览器 exe 的文件名**，`-Chrome` 才真正
  能用任意 Chromium 构建（含 `msedge.exe`）。

两例实测（隔离 `LOCALAPPDATA`，`-Profile` 每次用新目录，`-Handshake` 各自独立）：

| 组合 | 结果 | 关键输出 |
| --- | --- | --- |
| `plain` × `normal` | **PASS**，退出码 0 | `RESULT target=plain delivery=Some(Appended) enter=0 submit=0 input=2 active=Listen non-target` |
| `rich` × `normal` | **PASS**，退出码 0 | `RESULT target=rich delivery=Some(Appended) enter=0 submit=0 input=0 active=Listen non-target` |

`FINAL_TARGET` 两例都等于「原有草稿 + 换行 + 采集中新增行 + 换行 + 完整结果」，`DETAIL` 为
`The complete result was appended to the original target…`，非目标编辑区未变，输入事件计数表明
**没有自动 Enter / Submit**。

证据：`.artifacts\issue-24\edge-plain-normal.log`、`browser-edge\plain-normal\driver-evidence.json`
+ `page.png`；`edge-rich-normal.log`、`browser-edge\rich-normal\driver-evidence.json` + `page.png`。
第 1 段的 Chrome 探针记录保留在 `.artifacts\issue-24\chrome-fixture-probe.md` 与
`editor-{plain,rich}-{normal,readonly}.log`。

> 稳定性注记（实测现象，不隐藏）：同一 `-Profile` 目录在上一实例未退出时，新启动会交给已在运行的
> 浏览器实例，而那个实例没有带 `--force-renderer-accessibility`，于是又退化成「找不到夹具窗口」。
> 改为**每次新 `-Profile`** 后 plain/rich 各一次通过；两次 `readonly` 重试仍卡在同一处（见 §3.1）。

## 3. 只读 / 容量截断 / 目标失效（验收点 2）

### 3.1 只读

- **原生 EDIT 目标（新包原地）— PASS**：采集期间把目标设为只读，`detail` 明确 “The original target
  still holds its previous content, so nothing was written to it (The original target is read-only).”，
  `delivery.json = fallback`，目标逐字符不变，完整结果进面板（`panel_visible=1`，`panel.txt` 含完整
  结果），邻居与其他窗口不变，0 Enter / 0 Send，无临时音频。
- **浏览器子路径（`-Scenario readonly`）— 今日未复现**：第 1 段因 Chrome 渲染树缺失未跑；本段用 Edge
  重试 3 次，其中一次已成功找到夹具窗口并启动子流程，但子流程被**与场景无关**的全套测试前置检查
  挡住（`glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range` 在机器有负载时
  用了 124 ms，超过 100 ms 预算；重跑通过），另两次与 §2.2 注记同一个「夹具窗口 30 s 内不可见」现象。
  **没有任何一次场景断言失败**，但也没有拿到通过记录——因此不把浏览器只读记为 PASS。只读行为本身
  已由原生目标覆盖。

### 3.2 容量截断（40 字符夹具）

复用 `.artifacts\issue-11\accept\PackageFixture.cs` 的 `SetCapacity(40)`（宿主 `WM_SETTEXT` 只接受
前 40 字符）与新包 exe：

| 项 | 实测 |
| --- | --- |
| 目标容量 | 40 |
| 目标最终长度 | **40**（草稿 24 + `\r\n` 2 + 转写前 14 个字符） |
| 应有长度 | **54** |
| `detail` | `The original target holds only 39 of 54 characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. … The complete result is in the expanded result panel.` |
| `delivery.json` | `uncertain`（**不是** `appended`；与 Rust 契约 `Delivery::Uncertain` 一致） |
| 前缀性质 | 目标内容确实是“应有文本”的前缀，且未超过 40 |
| 面板 | `panel_visible=1`，`panel.txt` 含完整结果 |
| 其它 | 邻居与其他窗口不变、0 Enter / 0 Send、无临时音频 |

### 3.3 目标失效（销毁）

采集期间 `DestroyWindow` 掉目标：

| 项 | 实测 |
| --- | --- |
| `detail` | `The original target was destroyed; replacement controls are not used. No text was written to any control. The complete result is in the expanded result panel. …` |
| `delivery.json` | `fallback` |
| 是否误写 | 未向任何控件写入（目标句柄已销毁，邻居与其他窗口逐字符不变） |
| 面板 | `panel_visible=1`，`panel.txt` 含完整结果 |

证据（三例）：`.artifacts\issue-24\native-target-delivered\target-evidence.json`、
`target-readonly.png`、`target-capacity.png`、`target-destroyed-target.png`、`native-target-delivered.log`；
隔离副本同结果 `.artifacts\issue-24\native-target\`。

## 4. 识别失败重试 + 成功清理（验收点 3）

脚本：`scripts\verify-package-retry.ps1`。**为什么不是“坏模型”起手**：SenseVoice 的坏模型是 *准备*
失败（`preparation=Failed`，见第 5 例），此时**没有任何采集**，应用因此没有可重试的音频——
“坏模型 → retry → Result”在新包上不可达（这是设计，不是缺陷）。工单要验的“识别失败重试”走的是
**识别期失败**这条真实路径：采集完成后引擎读不到刚录的 WAV。故障注入只动包外的运行目录里的
`playback.wav`（给它加当前用户的 deny-read ACE，`icacls /deny <user>:(RD)`），产品二进制、模型、
引擎文件一律不动；ACE 在成功重试前撤销，并在 `finally` 兜底撤销，全部记录在证据里。

| # | 步骤 | 实测 |
| --- | --- | --- |
| 1 | 采集中制造识别失败 | `phase=Failure`、**`preparation=Ready`**、`retry_available=true`、`retained_audio` 非空、WAV 仍在；`detail=The local SenseVoice engine cannot read the captured WAV: … (os error 5). Audio retained for local recovery: …`；目标未写、`current-result.txt` 为空、0 Enter/0 Send |
| 2 | 故障未修复就 `--control retry` | 仍 `Failure`、**同一份** `retained_audio`、retry 仍可用、WAV 未删、目标不变 |
| 3 | 撤销 ACE 后 `--control retry` | `Processing → Result`；`current-result.txt` 为完整转写；`delivery.json=appended`；目标 = 原草稿 + 转写**恰好一次**；`retry_available=false`；无 `*.wav` 残留；语音**未重放** |
| 4 | 再次制造失败后 `--control cleanup` | `detail=The temporarily retained audio from the failed capture was deleted at your request. It will not be retried, and the current result is unchanged.`；`retry_available=false`；无 `*.wav`；`target` 与 `current-result.txt` 逐字符不变；随后再 `--control retry` 不会复活 |
| 5 | 坏模型（对照，记录差异） | `phase=Failure`、`preparation=Failed`、`retry_available=false`、未采集任何音频；`--control retry` → `No retained audio is available for a retry…`；`--control cleanup` → `There is no failed capture audio to clean…`；目标不变 |

观测到的 phase 序列：`Preparing → Ready → Capturing → Failure → Processing → Result → Capturing → Failure`。

证据：`.artifacts\issue-24\retry-delivered\retry-evidence.json`、`retry-delivered.log`（交付包原地，
退出码 0）；隔离副本同结果 `.artifacts\issue-24\retry\retry-evidence.json`；ACE 步骤逐条记录在
`acl_log` 里。

## 5. 设备中断（验收点 3）

脚本：`scripts\verify-package-device.ps1`（新增，由第 1 段的思路与
`.artifacts\issue-11\accept\accept-device.ps1` 适配到新包 + SenseVoice，驱动方式改为控制通道）。
它用 `AudioRoute.cs`（MMDeviceEnumerator + Windows 策略 COM 服务器）读/写真实默认端点，并用
`StartPlayback` 把真实 PCM 播到原端点，让 loopback 采集有样本；产品二进制、模型、引擎不动。

| 断言 | 实测（2026-11-24 重跑一次结果一致；下列证据为最后一次） |
| --- | --- |
| 采集中确实用原默认端点 | `phase=Capturing` 的 `status.txt` 含 `endpoint_role=default multimedia render endpoint` 与 `endpoint_id=<原端点>` |
| 采集中把默认多媒体端点切到另一个可用渲染设备 | 原 `耳机 (WH-1000XM4)`（`{…352149e2…}`）→ `扬声器 (Realtek(R) Audio)`（`{…f611c5b7…}`），回读确认切换成功 |
| 采集中断 | `phase=Failure`；`detail` 同时含 `incomplete`、原端点名与 ID、新端点名与 ID，并命中 `changed during capture|AUDCLNT_E_DEVICE_INVALIDATED` |
| 不静默继续 / 不跟随新设备 | 切后连续 3 s 每 200 ms 复读：始终 `phase=Failure`，`endpoint_id` 仍是**原**端点；没有自动开始替代采集 |
| 已收音频保留 | `runtime\partial-result.txt` 仍在（`retained_audio` 为空、无 `*.wav`——设备失效路径保留的是检查点而不是可重试音频） |
| 结果标记不完整、不误写 | `current-result.txt` 仍是运行前写入的哨兵值；目标、相邻 EDIT、其他窗口逐字符不变 |
| 不冒充识别失败 | `retry_available=false`（设备中断不提供识别重试） |
| 0 Enter / 0 Send | `automatic_send_count=0`、`enter_key_count=0` |
| 面板 | `--control show` 后 `panel_visible=1`，`panel.txt` 同样含 `incomplete` 与中断原因 |
| **恢复原默认端点** | `finally` 里恢复多媒体与通信两个角色并回读；`restore.json` `restore_verified=true`；脚本结束后独立复查 `Multimedia/Communications/Console` 三个角色都回到 `耳机 (WH-1000XM4)`，与 `original-endpoints.json` 一致 |

证据：`.artifacts\issue-24\device\device-evidence.json`、`original-endpoints.json`、`restore.json`、
`status-failure.txt`、`device-failure.png`、`device-panel.png`、`runtime\{status.txt,current-result.txt,
partial-result.txt,panel.txt,delivery.json,…}`、`device.log`。

## 6. 长会话（验收点 3）

### 6.1 代码改动

`app\listen\examples\long_session.rs` 原先写死 whisper（`Whisper::with_backend`）。现在：

- `--engine sensevoice` 选择常住 SenseVoice，参数形状与应用一致：
  `--engine sensevoice --sherpa-lib --sensevoice-model --sensevoice-tokens --sensevoice-vad
  --sensevoice-language --threads`（与 `scripts\prepare-meeting-baseline.ps1:71-74` 一致）；
- 其它 `--engine <path>` 仍是原 whisper-cli 路径，`--model` / `--vad-model` / `--backend` 语义不变；
- 延迟包装器改为 `Arc<dyn LocalTranscriber>`，**产品语义未改**（同一 Start/Stop/结果接缝、同一分段
  写入器、同一输出生命周期；仍然完全不打开音频设备）。

`scripts\measure-long-session.ps1` 增加 SenseVoice 透传：`-Engine sensevoice`（或 `-PackageRoot` /
`-SherpaLib`）切换到 SenseVoice 并自动推导包内四个文件，`-Backend` 对 SenseVoice 无意义时**忽略并
在 metadata 里写明**，metadata 记录包路径与 sherpa dll / model / tokens / vad 的 SHA-256。

### 6.2 600 s 实测（SenseVoice，包内引擎）

命令（输出目录必须是新的）：

```powershell
.\scripts\build-listen.ps1 -Task BuildHarness
.\scripts\measure-long-session.ps1 -InputWav .artifacts\issue-6-smoke\loop-96s.wav `
  -OutputDirectory .artifacts\issue-24\long-session -Seconds 600 -Threads 6 `
  -PackageRoot .worktrees\issue-23\dist\Listen-0.1.0
```

| 断言 | 实测 |
| --- | --- |
| 末条 NDJSON `phase=Result terminal=true` | 是：`flow.ndjson` 122 行，phase 计数 `Ready=1 / Capturing=120 / Result=1`，无 `Failure` / `NoSpeech` |
| 跑满 600 s | `audio_seconds=600.0`、`received_samples=9,600,000`（脚本实际耗时 605.5 s，含准备与收尾） |
| 识别持续工作 | `recognized_segments=22`、`result_characters=1,325` |
| 待处理音频有界 | `peak_pending_segments=1`、`buffered_samples` 最大 32,000（= 2 s 窗口，未增长）、`spool_bytes` 峰值 **1,052,888 B ≈ 1.05 MB**（约为 9,600,440 B 上限的 11%，不随时间累积）；末条 `spool_bytes=0`（收尾已清理） |
| 内存有界 | `resources.ndjson` 189 个采样（3 s 一次）：工作集 245.4 → 峰值 410.3 → 末次 407.4 MB；私有内存 231.5 → 峰值 408.4 MB。曲线在模型常驻后进入平台，未见随时间线性增长 |
| 无异常中断 | `stderr.txt` 0 字节；120 条采集期报告之间的**最大间隔 5.04 s**（报告本身就按 5 s 节奏），无 > 15 s 的停滞 |
| 收尾 | `stop_to_result=460 ms`（`finalize=4`、`recognition=399`、`last_window_inference=397`），`preparation=1851 ms`，`backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s)…` |

> 资源采样里的 `gpu` 是**整机**显卡用量（不是本进程），只作环境记录，不能当作 Listen 的显存占用。

证据：`.artifacts\issue-24\long-session\metadata.json`、`flow.ndjson`、`resources.ndjson`、
`stderr.txt`、`exit.json`、`process-id.txt`；构建日志 `build-harness.log`、运行日志
`long-session-measure.log`。`metadata.json` 里四个 SenseVoice 文件的 SHA-256 与包内文件一致
（sherpa dll `0965A6E4…`、model `C71F0CE0…`、tokens `F449EB28…`、vad `9E2449E1…`）。

## 7. 精修自动回填 / 手改不覆盖（验收点 5）— **部分**

### 7.1 应用层行为证据（`app\listen\tests\refine_backfill.rs`，15 项，全部通过）

| 测试 | 覆盖 |
| --- | --- |
| `a_successful_refine_replaces_the_appended_transcript_and_reads_it_back` | 成功精修替换本次追加的转写并**读回确认** |
| `the_refine_port_receives_exactly_the_transcript_text_and_nothing_else` | 只发文本（不含音频、目标里的其它内容） |
| `a_target_edited_by_the_user_is_never_overwritten_and_keeps_the_refined_text_in_the_panel` | 手改不覆盖，精修文本留在面板 |
| `a_target_that_cannot_be_read_after_delivery_is_not_written_and_keeps_the_refined_text` | 目标不可读时不写 |
| `a_failed_refine_keeps_the_local_result_and_never_appends_it_again` | 精修失败保留本地结果、不重复追加 |
| `a_fallback_result_with_no_target_is_refined_into_the_panel_and_the_current_result` | 无目标时精修进面板 |
| `no_refine_port_refuses_explicitly_and_leaves_the_result_and_target_untouched` | 无精修端口时明确拒绝 |
| `a_port_whose_availability_fails_refuses_with_its_own_reason` / `refine_is_unavailable_while_capturing_or_processing_and_never_calls_the_port` / `starting_a_capture_is_refused_while_a_refine_is_in_flight` / `a_second_refine_request_while_running_is_refused_without_calling_the_port_again` / `no_speech_keeps_the_previous_result_but_does_not_leave_refine_available` / `a_new_capture_clears_the_refine_state_and_a_new_result_offers_refine_again` / `an_empty_original_draft_is_replaced_with_only_the_refined_text` / `line_endings_reported_by_the_target_do_not_prevent_a_safe_backfill` | 可用性/并发/空草稿/换行等边界 |

这些是**应用层**（`Application` 边界 + 假精修端口）的测试，不是桌面验收。

### 7.2 桌面「无 key 时精修不可用」证据

- 第 1 段包上实测（复用）：`.artifacts\issue-23\flow-evidence.json` —
  `refine=unavailable`、`refine_reason` 指名 `DEEPSEEK_API_KEY`、按钮标签 `精修（不可用）` 且禁用、
  `--control refine` 后 `status_after_refine.detail=Refine is unavailable: … The current result is
  unchanged…`、`current-result.txt` 逐字符未变。
- 本段**复跑一次确认**（新证据）：`.artifacts\issue-24\refine\refine-unavailable.json` —— 包内
  SenseVoice 真实采集一次到 `Result`（28 字），随后 `refine=unavailable`、原因指名
  `DEEPSEEK_API_KEY`、`--control refine` 被产品拒绝并在 `detail` 给出理由、`current-result.txt`
  逐字符不变、面板同时给出英文说明与「精修不可用」；`deepseek_api_key_present=false`、
  `network_call_made=false`。注意：`--control refine` 本身退出码为 0（命令被接受），拒绝是**产品层
  的语义拒绝**，不是控制通道错误。

### 7.3 明确未覆盖

**真实 DeepSeek API 往返未覆盖**（用户 2026-11-24 决定不测）：没有调用 DeepSeek API、没有读任何
密钥文件、没有向用户索取 key。因此：

- 「精修自动回填」与「精修后手改不覆盖」这两条路径**没有桌面级证据**，只有应用层测试（§7.1）+ 包上
  「不可用」路径的桌面证据（§7.2）。
- `app\listen\tests\refine_deepseek_api.rs` 里真实 API 的用例是 `#[ignore]`，本段未运行。
- 不得把 §7.1 的应用层测试当成桌面验收。

## 8. 胶囊置顶 / 不抢焦点 / 共享隐身（验收点 4）— **PASS**

第 1 段跑通两个脚本时在新包上顺带取证（属新包实测）：

| 场景 | 结果 | 证据 |
| --- | --- | --- |
| 胶囊可见性 + 共享隐身 | 可见、空标题、`LAYERED\|TOOLWINDOW\|NOACTIVATE\|TOPMOST`、行为级置顶（探针窗覆盖后 `WindowFromPoint` 仍是胶囊）、`WDA_EXCLUDEFROMCAPTURE`、`capsule_capture_excluded=1`、`capsule-failure.png` | `capsule\capsule-verification.json` + 5 张 PNG |
| 胶囊不抢焦点 | `WM_MOUSEACTIVATE → MA_NOACTIVATE`；真实点击前后前台窗口不变、胶囊不移动 | 同上 |
| 拖拽位置持久化 + 重启恢复 | 拖动 140×60 px，`capsule-position.txt` 记录 `(962,108)`，重启后恢复同一位置 | 同上 |
| 托盘菜单 | 宿主隐藏窗存在、右键菜单可打开/取消 | `capsule\capsule-verification.json`、`panel\tray-menu.png` |
| 无目标面板 | 焦点在不可编辑探针窗：结果**不追加**、`panel_visible=1`、`current-result.txt` 完整（49 字符）、`panel.txt` 含完整结果与解释、`--control copy` 与复制按钮一致 | `panel\panel-verification.json` |
| `NoSpeech` | 静音 → `NoSpeech`、不覆盖旧结果、不自动开面板、`panel.txt` 说明 | 同上 |
| 坏模型失败态 | 不自动开面板、`--control show` 后 `panel.txt` 给出完整原因、重试/清理按钮置灰 | 同上 |

## 9. 脚本清单（本轮新增/修改）

| 文件 | 变更 |
| --- | --- |
| `scripts\verify-capsule.ps1` | +`-PackageRoot`/`-SherpaLib`/`-SenseVoiceModel`/`-SenseVoiceTokens`/`-SenseVoiceVad`/`-SenseVoiceLanguage`；引擎关键字不再被当路径；+引擎来源验收断言；失败态改为参数覆盖 |
| `scripts\verify-panel.ps1` | 同上（此前完全没有引擎参数，只能跑云端默认） |
| `scripts\verify-generic-editor.ps1` | 加 UTF-8 BOM 使 5.1 可解析；浏览器命令行加 `--force-renderer-accessibility`；清理按传入浏览器 exe 名过滤（`-Chrome` 支持任意 Chromium） |
| `scripts\verify-native-target.ps1` | **新增**（第 1 段）：新包上的追加 + 重复停止、只读、40 字符容量截断、目标销毁 4 例 |
| `scripts\verify-package-retry.ps1` | **新增**（第 1 段）：新包上的识别失败 → 重试 → 结果 → 清理 5 例（含坏模型对照） |
| `scripts\verify-package-device.ps1` | **新增**（本段）：新包上的真实默认端点切换 → 中断 → 证据 → 恢复 3 个角色 |
| `scripts\measure-long-session.ps1` | +`-PackageRoot`/`-SherpaLib`/`-SenseVoice*`，`-Engine sensevoice` 透传；metadata 记录 SenseVoice 文件哈希；`-Backend` 对 SenseVoice 忽略并注明 |
| `app\listen\examples\long_session.rs` | +SenseVoice 引擎选择与参数（形状照抄应用），whisper 路径不变 |

两个新桌面脚本只依赖 git 忽略的既有夹具
`E:\luyin\.artifacts\issue-11\accept\{AudioRoute.cs,PackageFixture.cs,ListenNativeQa.cs}`
（`-FixtureRoot` 可改），不复制它们入库。

## 10. 偏差、限制与未覆盖

1. **只读的浏览器子路径未复现**（§3.1）：Edge 下 3 次尝试，1 次走到子流程但被无关的全套测试计时
   用例挡住，2 次卡在夹具窗口不可见；**没有任何场景断言失败**，但也没有通过记录。只读行为已由原生
   EDIT 目标覆盖。
2. **浏览器通道依赖 Edge + `--force-renderer-accessibility`**：本机 Chrome 157.0.8103.79 即使加该
   开关也不向 UIA 暴露渲染树；Edge 158.0.4370.33 加开关后正常。驱动的 `-Chrome` 默认值仍是 Chrome
   路径，跑浏览器夹具时需显式传 Edge 路径。这是特定版本的 Chromium 可访问性行为，未做跨版本矩阵。
3. **浏览器夹具的稳定性**：同一 `-Profile` 目录在旧实例未退出时会被复用且丢失开关；必须每次用新
   `-Profile`（§2.2 注记）。
4. **真实 DeepSeek API 往返未覆盖**（用户 2026-11-24 决定）：精修的两条桌面路径因此缺桌面级证据，
   只有应用层测试 + 不可用路径桌面证据（§7.3）。
5. **“坏模型 → retry → Result”不可达**：坏模型是准备期失败，没有采集就没有保留音频；第 5 例把这一
   差异写成显式断言，避免用“坏模型”冒充“识别失败”（§4）。
6. **故障注入方式**：识别期失败用包外运行目录里 `playback.wav` 的 deny-read ACE（`icacls /deny`），
   设备中断用真实默认端点切换（`AudioRoute`）。未覆盖：磁盘写满、驱动崩溃、USB 拔出等其它失败源。
7. **容量截断的 `detail` 计数与目标长度差 1**：`detail` 报 `only 39 of 54 characters`，目标实际
   40 字符，差的是末尾换行符。脚本另断言“目标不超过 40”“目标是应有文本前缀”，没有把 39 当阈值。
8. **`-Engine whisper` 老用法没有实跑**：交付包已不含 whisper（#23），硬约束也要求桌面验收一律走
   SenseVoice；老用法的参数构造保持原样，仅做代码级保留。
9. **长会话不覆盖 WASAPI 真采集与界面**：harness 用替代播放设备 + 真实分段/识别链路，且资源采样
   3 s 一次，短峰值可能漏采；`resources.ndjson` 的 `gpu` 是**整机**显卡用量（本机采样值约 `20, 831, 4096`＝利用率 20% / 已用 831 MB / 总 4096 MB），不归属 Listen 进程。
10. **一次与场景无关的计时用例抖动**：`glossary::tests::correcting_hundreds_of_terms_stays_in_the
    _millisecond_range`（100 ms 预算）在有负载时用了 124 ms；重跑通过。它不是本次改动引入的，但确实
    挡住过一次浏览器只读尝试，故记录。
11. **未重打包**：只用 #23 的交付件（`git_dirty_at_package_time=false`）。本段改的是 `scripts\`、
    `app\listen\examples\long_session.rs` 与本文件；`long_session` 是 example，不进入交付包字节，
    因此 `docs\issue-23-verification.md` 的包哈希仍然成立。
12. **文档现状核对**：`docs\issue-40-delivery.md` 与 `README.md` 没有与新包/新验收矛盾的说法
    （#40 已注明「交付包的本地引擎是 whisper.cpp；SenseVoice 换包由 #23 完成」，README 已写
    SenseVoice 包与 `--control` + `status.txt`/`panel.txt`/`current-result.txt` 的验收口径），
    因此本次未改动这两份文件。

## 11. 测试与格式

- `.\scripts\build-listen.ps1 -Task Test`：**306 passed / 0 failed / 16 ignored**（与 #40、#23 记录的
  基线一致；本段只改了 example、脚本与文档，没有新增/删除测试）。日志
  `.artifacts\issue-24\final-test.log`。
- `.\scripts\build-listen.ps1 -Task CheckFormat`：无输出，退出码 0。
- 构建：`.\scripts\build-listen.ps1 -Task BuildHarness` 成功，产物
  `app\listen\target\release\examples\long_session.exe`；日志 `.artifacts\issue-24\build-harness.log`。

## 12. 证据索引

| 证据 | 路径 |
| --- | --- |
| 红：旧脚本对包（capsule / panel） | `.artifacts\issue-24\red\red-{capsule,panel}-old-script.log` + 各自 `*-verification.json` |
| 绿：capsule / panel（新包） | `green-{capsule,panel}-package.log`、`capsule\capsule-verification.json`、`panel\panel-verification.json`、`*.png` |
| 追加 / 只读 / 容量 / 销毁（交付包原地） | `native-target-delivered\target-evidence.json`、`native-target-delivered.log`、`target-*.png` |
| 同三例（隔离副本） | `native-target\target-evidence.json`、`native-target.log` |
| 浏览器夹具 plain/rich normal（Edge） | `edge-{plain,rich}-normal.log`、`browser-edge\{plain,rich}-normal\driver-evidence.json` + `page.png` |
| 浏览器夹具只读尝试 | `edge-plain-readonly.log`、`browser-edge\plain-readonly\`（未产出 driver-evidence） |
| Chrome 探针（第 1 段） | `chrome-fixture-probe.md`、`editor-{plain,rich}-{normal,readonly}.log` |
| Edge 探针脚本（本段诊断用） | `probes\probe-edge-uia.ps1`、`probes\probe-edge-windows.ps1`、`probes\endpoints.ps1`、`probes\refine-unavailable.ps1`、`probes\parse-check.ps1` |
| 识别失败重试 + 清理 | `retry-delivered\retry-evidence.json`、`retry\retry-evidence.json`、`*.log` |
| 设备中断 | `device\device-evidence.json`、`original-endpoints.json`、`restore.json`、`status-failure.txt`、`device-{failure,panel}.png`、`device.log` |
| 长会话 600 s | `long-session\{metadata.json,flow.ndjson,resources.ndjson,stderr.txt,exit.json}`、`build-harness.log`、`long-session-measure.log` |
| 精修不可用（复跑） | `refine\refine-unavailable.json` |
| 精修不可用（#23 桌面流程） | `.artifacts\issue-23\flow-evidence.json` |
| 测试 / 格式 | `final-test.log`、`checkformat-final.log`（旧 `checkformat.log` 是更早分支的存档，勿引用） |
| 胶囊 / 面板 / 托盘截图 | `capsule\*.png`、`panel\*.png`（含 `panel\tray-menu.png`） |

以上 `.artifacts` 目录被 Git 忽略；仓库内的交付物是本文件与 `scripts\` 下的脚本。
