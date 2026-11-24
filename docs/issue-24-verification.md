# #24 全量桌面验收回归（第 1/2 段）

状态：**本段范围已完成**（4 项中 3 项 PASS、1 项部分 BLOCKED）。实施日期 2026-11-24。
工作树 `E:\luyin\.worktrees\issue-24`，分支 `implement/issue-24-desktop-acceptance`，基线
`a10fca1`（`integration/sensevoice-v0.2` 尖端）。主工作树 `E:\luyin` 的 7 份用户改动未被触碰、
未提交、未 stash。

本段只做 4 块：两个桌面验收脚本的 SenseVoice / 包内 exe 支持；目标追加回归；只读 / 容量截断 /
目标失效；识别失败重试 + 成功清理。设备中断、长会话、精修实测由另一段续写，本文件在 §7 留出续写
位置。

## 口径（全篇适用）

> **验收 oracle 是控制通道 `--control` + `status.txt` / `panel.txt` / `current-result.txt`
> （以及 `delivery.json`）；窗口类名、窗口样式、控件 ID 只作自检，不作验收依据。**

所有桌面验收都**串行**执行，每项之间确认 `Get-Process listen` 为空。全部使用
`--engine sensevoice`（包内 sherpa-onnx + q8 模型），不走云端路径、不产生任何付费请求：
`LOCALAPPDATA` 被指向隔离目录，`LISTEN_ALI_API_KEY`/`LISTEN_ALI_WORKSPACE_ID` 被清除。

交付件（`E:\luyin\.worktrees\issue-23\dist\Listen-0.1.0`，manifest `git_commit=9a53e9aa…`、
`git_dirty_at_package_time=false`、21 文件 / 264,881,704 B）**直接使用**，没有重新打包。另外
在 `%TEMP%\listen-24-user-machine\Listen-0.1.0` 放了一份逐字节相同的副本（同样 21 文件 /
264,881,704 B），该目录没有任何 `.tools` 祖先，用来证明包不依赖仓库里的 engine/model 目录。

## 结论摘要

| # | 本段验收项 | 结果 | 证据 |
| --- | --- | --- | --- |
| 1 | `verify-capsule.ps1`、`verify-panel.ps1` 支持 SenseVoice / 包内 exe | **PASS** | 红→绿见 §1；胶囊 **43/43** 检查通过，面板 **46/46 验收 + 42/42 自检**通过；两者都断言进程实际加载的是**包内** `engine\sherpa\sherpa-onnx-c-api.dll` |
| 2 | 目标追加回归（含重复停止、邻居不变、0 Enter / 0 Submit） | **部分 BLOCKED / 部分 PASS** | Chrome 夹具路径 **BLOCKED**（Chrome 157 不向 UIA 暴露渲染树，§4.1）；同样的语义在**新包**的原生 EDIT 目标上 **PASS**：追加恰好一次、重复停止为 no-op、邻居与其他窗口不变、`enter=0`/`send=0`（§4.2） |
| 3 | 只读 / 容量截断 / 目标失效 | **PASS** | 新包上三例全过：只读不写、容量 40 字符截断（39/54，`uncertain`，完整结果在面板）、目标销毁不写；邻居与其他窗口不变、0 Enter/0 Send、无临时音频（§5）。只读的浏览器 `-Scenario readonly` 路径与 §4.1 同一个 BLOCKED |
| 4 | 识别失败重试 + 成功清理 | **PASS** | 新包上：真识别失败（`preparation=Ready`、音频保留、retry 可用）→ 故障未修复时重试仍失败且**同一份音频**仍可重试 → 修复后 `--control retry` → `Result` 且**不重放语音**、只追加一次 → 后一次失败 `--control cleanup` 删除保留音频、禁用重试、结果与目标不变（§6） |
| — | 附带覆盖：胶囊可见性 / 共享隐身、无目标面板 | **PASS** | 第 1 项跑胶囊与面板脚本时顺带取证：胶囊可见、行为级置顶、`WDA_EXCLUDEFROMCAPTURE`、`capsule_capture_excluded=1`、拖拽持久化、托盘；无目标时结果进面板且不追加（§7） |

## 1. 两个脚本的 SenseVoice / 包内 exe 支持（红 → 绿）

### 1.1 改了什么

`scripts\verify-capsule.ps1`、`scripts\verify-panel.ps1` 增加同一套引擎选择参数，两种用法都支持：

| 参数 | 语义 |
| --- | --- |
| `-Engine sensevoice\|sense-voice\|sense_voice` | 本地 SenseVoice；四个路径缺省时由应用按 `root()` 推导（仓库内构建） |
| `-Engine whisper` / `-Engine <whisper-cli.exe 路径>` | 原 whisper 用法（关键字不再是路径，见下） |
| `-PackageRoot <解包目录>` | 包布局：自动推导 `listen.exe` + `engine\sherpa` + `models\sense-voice\*`，隐含 SenseVoice |
| `-SherpaLib` / `-SenseVoiceModel` / `-SenseVoiceTokens` / `-SenseVoiceVad` / `-SenseVoiceLanguage` | 显式 SenseVoice 路径，用于任意位置的包或仓库构建 |
| `-Model` / `-VadModel` | whisper 模型覆盖（不变） |

两个脚本都新增引擎来源断言（属验收组）：`recognition_mode=local`、`backend` 含
`SenseVoice q8`，并在给了 `-SherpaLib` 时断言运行中进程**实际加载的**
`sherpa-onnx-c-api.dll` 就是该目录下的那份（`Process.Modules`）。失败态改用参数覆盖而不是追加
重复的 `--sensevoice-model`。

顺带修掉一个真实缺陷：旧 `verify-capsule.ps1` 的 `-Engine` 会经过
`ConvertTo-QuotedWindowsPath` 变成绝对路径，所以 `-Engine sensevoice` 会被应用当成
“whisper-cli 路径”。现在引擎关键字先分类（`sensevoice`/`whisper`/其它=路径），只有路径形态才转
绝对路径。

另外 `scripts\verify-generic-editor.ps1` 原本是**无 BOM 的 UTF-8 且含中文字面量**，Windows
PowerShell 5.1 按 ANSI 读取会直接解析失败（`The '<' operator is reserved`）。加了 UTF-8 BOM，
文件内容未改（`git diff` 只有首行）。这是第 2/3 项能在 5.1 上运行的前提。

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

证据：`.artifacts\issue-24\red\red-panel-old-script.log`、
`.artifacts\issue-24\red\red-capsule-old-script.log` 及各自的 `*-verification.json`。

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
`.artifacts\issue-24\green-panel-package.log`、`panel\panel-verification.json`、
`panel\panel-{ready,capturing,fallback,failure}.png`、`panel\tray-menu.png`。

## 2. 目标追加回归

### 2.1 规定路径：`verify-generic-editor.ps1` — **BLOCKED**

四个组合（`plain|rich` × `normal|readonly`）全部在 Chrome 夹具阶段失败，全部退出码 1：

```text
The owned fixture window did not appear
At E:\luyin\.worktrees\issue-24\scripts\verify-generic-editor.ps1:76 char:5
```

**试了什么、缺什么**（完整记录 `.artifacts\issue-24\chrome-fixture-probe.md`）：

- Chrome 能用与脚本完全相同的参数启动，窗口标题 `Listen owned editor verification - Google
  Chrome`、地址栏就是夹具 URL，说明页面已导航；
- 但同一个 UIA 客户端在这台机器上读到：夹具窗口 `docs=0 edits=1`（唯一 Edit 是地址栏），
  而桌面上其它两个 Chrome 窗口 `docs=1 edits=2` —— 客户端、UIA 程序集、跨进程访问都正常，
  是**这个 Chrome 实例不向 UIA 暴露渲染树**；
- 已排除：无 BOM 导致的解析失败（已修，脚本现在能跑）、缺少 `--force-renderer-accessibility`
  （已加，无效）、新配置目录首启（用已初始化的配置目录重启，无效）、窗口不在前台（用
  `AllowSetForegroundWindow(-1)`+ALT+`SetForegroundWindow` 确认 `GetForegroundWindow()` 就是夹具
  窗口，仍无效）、等待不足（每次 12–20 s）。
- Chrome 版本 **157.0.8103.79**；`docs\issue-4-verification.md` 当年记录的是 **157.0.8103.39**。

结论：驱动需要 3 个 `ControlType.Edit`，而渲染树不存在，`Verify-FixtureWindow` 无法满足。
需要在“能暴露渲染树的 Chrome 构建/策略”“改用 CDP”“换一个可访问的夹具浏览器”之间选择其一。

### 2.2 替代证据：新包上的原生 EDIT 目标 — **PASS**

`scripts\verify-native-target.ps1`（新增，§5 共用）在**交付包原地**跑第一例：

| 断言 | 实测 |
| --- | --- |
| 追加语义 | `delivery.json` 最新记录 `state=appended`；目标文本 = 原草稿 + `\n` + 完整结果，且转写串只出现 **1** 次 |
| 重复停止 | 第一次 `--control stop` 后立刻再发一次 `--control stop`，结果仍只有一次追加；结果出来后再发一次 stop，目标文本与 `current-result.txt` 逐字符不变 |
| 邻居不变 | 相邻 EDIT 与其他窗口 EDIT 逐字符不变 |
| 0 Enter / 0 Submit | `PackageFixture` 计数 `enter_key_count=0`、`send_count=0`（真实 SendInput/回车钩子计数） |
| 面板 | `panel_visible=0`（追加成功不打开面板） |
| 临时音频 | 成功后 runtime 下无 `*.wav` |

证据：`.artifacts\issue-24\native-target-delivered\target-evidence.json`、`target-append.png`、
`native-target-delivered.log`。同一脚本在隔离副本上同样 4/4 通过
（`.artifacts\issue-24\native-target\target-evidence.json`）。

## 3. 只读 / 容量截断 / 目标失效

### 3.1 只读

- 浏览器路径 `verify-generic-editor.ps1 -Scenario readonly`：与 §2.1 同一个 BLOCKED（夹具起不来），
  不是断言失败。
- 新包原生目标（`verify-native-target.ps1` 第 2 例）：采集期间把目标设为只读，`detail` 明确
  “The original target still holds its previous content, so nothing was written to it (The original
  target is read-only).”，`delivery.json` = `fallback`，目标逐字符不变，完整结果进面板
  （`panel_visible=1`，`panel.txt` 含完整结果），邻居与其他窗口不变，0 Enter/0 Send，无临时音频。

### 3.2 容量截断（40 字符夹具）

复用 `.artifacts\issue-11\accept\PackageFixture.cs` 的 `SetCapacity(40)`（宿主 `WM_SETTEXT` 只接受
前 40 字符）与新包 exe：

| 项 | 实测 |
| --- | --- |
| 目标容量 | 40 |
| 目标最终长度 | **40**（草稿 24 + `\r\n` 2 + 转写前 14 个字符） |
| 应有长度 | **54** |
| `detail` | `The original target holds only 39 of 54 characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. … output is not retried automatically and the original text is not reverted. The complete result is in the expanded result panel.` |
| `delivery.json` | `uncertain`（**不是** `appended`；与 Rust 契约 `Delivery::Uncertain` 一致） |
| 前缀性质 | 目标内容确实是“应有文本”的前缀，且未超过 40 |
| `detail` 自述计数 | `only 39 of 54`；与应用把 `\r\n` 记作 1 个字符、目标实际 40 个 UTF-16 单元的差异一致（脚本不把 39 当阈值，另断言“目标 ≤ 40”“目标是应有文本前缀”） |
| 面板 | `panel_visible=1`，`panel.txt` 含完整结果 |
| 其它 | 邻居与其他窗口不变、0 Enter/0 Send、无临时音频 |

### 3.3 目标失效（销毁）

采集期间 `DestroyWindow` 掉目标：

| 项 | 实测 |
| --- | --- |
| `detail` | `The original target was destroyed; replacement controls are not used. No text was written to any control. The complete result is in the expanded result panel. …` |
| `delivery.json` | `fallback` |
| 是否误写 | 未向任何控件写入（目标句柄已销毁，邻居与其他窗口逐字符不变） |
| 面板 | `panel_visible=1`，`panel.txt` 含完整结果 |

证据（三例）：`.artifacts\issue-24\native-target-delivered\target-evidence.json`、
`target-readonly.png`、`target-capacity.png`、`target-destroyed-target.png`、
`native-target-delivered.log`；隔离副本同结果 `.artifacts\issue-24\native-target\`。

## 4. 识别失败重试 + 成功清理

脚本：`scripts\verify-package-retry.ps1`（新增）。**为什么不是“坏模型”起手**：SenseVoice 的坏
模型是 *准备* 失败（`preparation=Failed`，见第 5 例），此时**没有任何采集**，应用因此没有可重试的
音频——“坏模型 → retry → Result”在新包上不可达（这是设计，不是缺陷）。工单要验的“识别失败重试”
走的是**识别期失败**这条真实路径：采集完成后引擎读不到刚录的 WAV。故障注入只动包外的运行目录里的
`playback.wav`（给它加当前用户的 deny-read ACE，`icacls /deny <user>:(RD)`），产品二进制、模型、
引擎文件一律不动；ACE 在成功重试前撤销，并在 `finally` 兜底撤销，全部记录在证据里。

| # | 步骤 | 实测 |
| --- | --- | --- |
| 1 | 采集中制造识别失败 | `phase=Failure`、**`preparation=Ready`**、`retry_available=true`、`retained_audio` 非空、WAV 仍在；`detail=The local SenseVoice engine cannot read the captured WAV: … (os error 5). Audio retained for local recovery: …`；目标未写、`current-result.txt` 为空、0 Enter/0 Send |
| 2 | 故障未修复就 `--control retry` | 仍 `Failure`、**同一份** `retained_audio`、retry 仍可用、WAV 未删、目标不变 |
| 3 | 撤销 ACE 后 `--control retry` | `Processing → Result`；`current-result.txt` 为完整转写；`delivery.json=appended`；目标 = 原草稿 + 转写**恰好一次**；`retry_available=false`；无 `*.wav` 残留；语音**未重放**（脚本全程只播放一次） |
| 4 | 再次制造失败后 `--control cleanup` | `detail=The temporarily retained audio from the failed capture was deleted at your request. It will not be retried, and the current result is unchanged.`；`retry_available=false`；无 `*.wav`；`target` 与 `current-result.txt` 逐字符不变；随后再 `--control retry` 不会复活（仍 `Failure`、无新音频、结果不变） |
| 5 | 坏模型（对照，记录差异） | `phase=Failure`、`preparation=Failed`、`retry_available=false`、未采集任何音频；`--control retry` → `No retained audio is available for a retry…`；`--control cleanup` → `There is no failed capture audio to clean…`；目标不变 |

观测到的 phase 序列：`Preparing → Ready → Capturing → Failure → Processing → Result → Capturing →
Failure`。

证据：`.artifacts\issue-24\retry-delivered\retry-evidence.json`、`retry-delivered.log`（交付包原地，
退出码 0）；隔离副本同结果 `.artifacts\issue-24\retry\retry-evidence.json`、`retry.log`。ACE 步骤逐条
记录在证据的 `acl_log` 里（`deny-read` / `allow-read`，`exit_code=0`）。

## 5. 脚本清单（本段新增/修改）

| 文件 | 变更 |
| --- | --- |
| `scripts\verify-capsule.ps1` | +`-PackageRoot`/`-SherpaLib`/`-SenseVoiceModel`/`-SenseVoiceTokens`/`-SenseVoiceVad`/`-SenseVoiceLanguage`；引擎关键字不再被当路径；+引擎来源验收断言；失败态改为参数覆盖 |
| `scripts\verify-panel.ps1` | 同上（此前完全没有引擎参数，只能跑云端默认） |
| `scripts\verify-generic-editor.ps1` | 仅加 UTF-8 BOM（内容未改），使 5.1 能解析 |
| `scripts\verify-native-target.ps1` | **新增**：新包上的追加 + 重复停止、只读、40 字符容量截断、目标销毁 4 例 |
| `scripts\verify-package-retry.ps1` | **新增**：新包上的识别失败 → 重试 → 结果 → 清理 5 例（含坏模型对照） |

两个新脚本只依赖 git 忽略的既有夹具
`E:\luyin\.artifacts\issue-11\accept\{PackageFixture.cs,ListenNativeQa.cs}`（`-FixtureRoot` 可改），
不复制它们入库。

## 6. 偏差与限制

1. **目标追加的浏览器路径 BLOCKED**（§2.1）。浏览器 `<textarea>` / `contenteditable` 适配器的最后
   一次通过记录仍是 `docs\issue-4-verification.md`（Chrome 157.0.8103.39）；本机当前 Chrome
   **157.0.8103.79** 不向 UIA 暴露渲染树。这一条需要决定：降级/换 Chrome 构建、改用 CDP 驱动、还是
   接受原生 EDIT 证据。
2. **追加语义的替代证据用的是原生 EDIT 目标**，不是浏览器 DOM；它覆盖“追加一次 / 重复停止 /
   邻居不变 / 0 Enter / 0 Submit / 不抢焦点”的**行为**，但不覆盖浏览器适配器的代码路径。
3. **容量截断的 `detail` 计数与目标长度差 1**：`detail` 报 `only 39 of 54 characters`，目标实际
   40 字符，差的是末尾换行符。脚本同时断言“目标不超过 40”“目标是应有文本前缀”“`detail` 计数
   与实际长度的差异只来自换行”，没有把 39 当成功阈值。
4. **失败重试用 deny-read ACE 注入**，不修改产品、不换引擎。它验证的是“识别期失败可重试”；不
   覆盖“磁盘满 / 设备中断”等其它失败源（设备中断属另一段）。
5. **“坏模型 → retry → Result”不可达**：坏模型是准备期失败，没有采集就没有保留音频。第 5 例把
   这一差异写成显式断言，避免用“坏模型”冒充“识别失败”。
6. **`-Engine whisper` 老用法没有实跑**：交付包已不含 whisper（#23），且本次硬约束要求桌面验收
   一律走 SenseVoice。老用法的参数构造保持原样（关键字 `whisper` 与“路径即 whisper-cli”两个分支
   各一行），仅做了代码级保留。
7. **只读的浏览器 `-Scenario readonly` 未跑**，原因同偏差 1；只读行为在原生 EDIT 上已验（§3.1）。
8. **重试脚本依赖 `playback.wav` 在采集期间出现**：该文件在第一批采样到达时才创建，所以脚本先
   播放语音再注入故障。若某次采集完全没有音频输入，脚本会在 15 s 后明确失败，而不是继续跑空。
9. **`-PackageRoot` 只覆盖 #23 包布局**（`engine\sherpa` + `models\sense-voice`）；别的布局用
   四个显式路径参数。
10. **未重打包**：本段只用 #23 的交付件（`git_dirty_at_package_time=false`）。本段改的都是
    `scripts\` 与文档，不影响 `listen.exe` 字节，因此 `docs\issue-23-verification.md` 的包哈希仍然
    成立。

## 7. 附带覆盖（本段顺带取证，供另一段引用）

第 1 项需要跑通两个脚本，因此顺带产生了下列证据；它们不属于本段要交付的 4 块，但都是新包上的实测：

| 场景 | 结果 | 证据 |
| --- | --- | --- |
| 胶囊可见性 + 共享隐身 | 可见、空标题、`LAYERED\|TOOLWINDOW\|NOACTIVATE\|TOPMOST`、行为级置顶（探针窗覆盖后 `WindowFromPoint` 仍是胶囊）、`WDA_EXCLUDEFROMCAPTURE`、`capsule_capture_excluded=1`、`capsule-failure.png` | `capsule\capsule-verification.json` + 5 张 PNG |
| 胶囊不抢焦点 | `WM_MOUSEACTIVATE → MA_NOACTIVATE`；真实点击前后前台窗口不变、胶囊不移动 | 同上 |
| 拖拽位置持久化 + 重启恢复 | 拖动 140×60 px，`capsule-position.txt` 记录 `(962,108)`，重启后恢复同一位置 | 同上 |
| 托盘菜单 | 宿主隐藏窗存在、右键菜单可打开/取消 | 同上 |
| 无目标面板 | 焦点在不可编辑探针窗：结果**不追加**、`panel_visible=1`、`current-result.txt` 完整（49 字符）、`panel.txt` 含完整结果与解释、`--control copy` 与复制按钮一致 | `panel\panel-verification.json` |
| `NoSpeech` | 静音 → `NoSpeech`、不覆盖旧结果、不自动开面板、`panel.txt` 说明 | 同上 |
| 坏模型失败态 | 不自动开面板、`--control show` 后 `panel.txt` 给出完整原因、重试/清理按钮置灰 | 同上 |

<!-- 第 2/2 段续写：设备中断、长会话、精修实测、其余截图。 -->

## 8. 测试与格式

- `.\scripts\build-listen.ps1 -Task Test`：**306 passed / 0 failed / 16 ignored**（与 #40、#23
  记录的基线完全一致）。日志 `.artifacts\issue-24\baseline-test.log`，耗时 108 s。
- 本段没有改 `app\listen\src` 与 `app\listen\tests`（`git diff --stat a10fca1..HEAD` 只有
  `scripts\` 与 `docs\`），所以测试套件用于确认没有回归，不需要新增 Rust 测试。
- `.\scripts\build-listen.ps1 -Task CheckFormat`：无输出，退出码 0。

## 9. 证据索引

| 证据 | 路径 |
| --- | --- |
| 红：旧脚本对包（capsule） | `.artifacts\issue-24\red\red-capsule-old-script.log`、`red\capsule-evidence\capsule-verification.json` |
| 红：旧脚本对包（panel） | `.artifacts\issue-24\red\red-panel-old-script.log`、`red\panel-evidence\panel-verification.json` |
| 绿：capsule（新包） | `.artifacts\issue-24\green-capsule-package.log`、`capsule\capsule-verification.json`、`capsule\*.png` |
| 绿：panel（新包） | `.artifacts\issue-24\green-panel-package.log`、`panel\panel-verification.json`、`panel\*.png` |
| 追加 / 只读 / 容量 / 销毁（交付包原地） | `.artifacts\issue-24\native-target-delivered\target-evidence.json`、`native-target-delivered.log`、`target-*.png` |
| 追加 / 只读 / 容量 / 销毁（隔离副本） | `.artifacts\issue-24\native-target\target-evidence.json`、`native-target.log`、`target-*.png` |
| 失败重试 + 清理（交付包原地） | `.artifacts\issue-24\retry-delivered\retry-evidence.json`、`retry-delivered.log` |
| 失败重试 + 清理（隔离副本） | `.artifacts\issue-24\retry\retry-evidence.json`、`retry.log` |
| Chrome 夹具 BLOCKED 记录 | `.artifacts\issue-24\chrome-fixture-probe.md`、`editor-{plain,rich}-{normal,readonly}.log` |
| 测试 / 格式 | `.artifacts\issue-24\baseline-test.log`、`checkformat.log` |

以上 `.artifacts` 目录被 Git 忽略；仓库内的交付物是本文件与 `scripts\` 下的脚本。
