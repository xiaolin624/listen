# #23 交付包换引擎（SenseVoice）

状态：**已完成**。实施日期 2026-11-24。工作树 `E:\luyin\.worktrees\issue-23`，分支
`implement/issue-23-sensevoice-package`，基线 `880ddef`（`integration/sensevoice-v0.2` 尖端）。
主工作树 `E:\luyin` 的用户改动未被触碰、未提交、未 stash。

本票只换交付包的引擎以及由此必需的打包/验证/文档：识别、交付、绑定、追加、截断、设备中断
等业务语义没有改动，`Cargo.toml` 没有改动，也没有新增第三方依赖。

## 结论摘要

| # | 验收条目（issue #23） | 结果 | 证据 |
| --- | --- | --- | --- |
| 1 | 包内不再有 whisper 模型或运行库，包含 SenseVoice 运行库与 q8 模型 | 通过 | `manifest.json` / 包内清单：`engine\sherpa\{sherpa-onnx-c-api.dll, onnxruntime.dll, onnxruntime_providers_shared.dll}` + `models\sense-voice\{model.int8.onnx, tokens.txt, silero_vad.onnx}`；包内 `whisper|ggml` 文件名 **0** 个（见 §1） |
| 2 | 双击启动 → 胶囊出现 → 采集 → 结果追加或进入面板，完整流程可用 | 通过 | 用包内 `启动 Listen 本地备用.cmd` 真实启动：**36 项检查全过**；`status.txt` `recognition_mode=local`、`backend=SenseVoice q8…`、`endpoint_name=耳机 (WH-1000XM4)`；面板可见且含完整结果（§2） |
| 3 | 禁网条件下完整流程通过 | **部分：替代证据**（未做真防火墙禁网，原因 UAC） | 全程 66 次 `Get-NetTCPConnection` 采样：远程连接 **0**、监听端口 **0**；本地运行目录无云端凭据/待处理云端音频；同一完整流程 37 项检查全过（§4） |
| 4 | 包体约 260 MB，文件哈希校验通过 | 通过 | 实测 **264,881,295 B**（264.88 MB 十进制 / 252.61 MiB）；`manifest.json` 19 项逐文件哈希全过，5 个 pinned 引擎/模型哈希独立复核全过（§1） |
| 5 | README 或「关于」里有 FunASR/SenseVoice 署名与许可证说明 | 通过 | `packaging\README.md`「署名与许可」节 + 包内 `THIRD-PARTY-NOTICES.md` + `licenses\` 三份上游原文 + `VERSION.txt` 署名行；验证脚本断言字样与原文标记（§6） |
| 6 | 没有 key 时精修不可用，本地功能不受影响 | 通过 | 同一流程：`refine=unavailable`、原因含 `DEEPSEEK_API_KEY`、按钮 `精修（不可用）` 置灰、`--control refine` 被拒且 `current-result.txt` 逐字符不变（§5） |

- 交付包（工作树 `dist\Listen-0.1.0`）由干净工作树构建，`manifest.json` 的
  `git_dirty_at_package_time=false`、`git_branch=implement/issue-23-sensevoice-package`。
- 旧 whisper 包 `E:\luyin\dist\Listen-0.1.0` 保持原样未改，只作为红灯对照。

## 1. 包内清单、体积与哈希

### 1.1 文件清单（21 个文件，含 `manifest.json` 与 `.listen-package`）

| 相对路径 | 字节 | 归属 |
| --- | ---: | --- |
| `listen.exe` | 2,323,968 | 应用 |
| `libunwind.dll` | 90,624 | gnullvm 运行库 |
| `engine\sherpa\sherpa-onnx-c-api.dll` | 4,294,656 | SenseVoice 运行库（pinned） |
| `engine\sherpa\onnxruntime.dll` | 17,799,168 | SenseVoice 运行库（pinned，MT/静态 CRT） |
| `engine\sherpa\onnxruntime_providers_shared.dll` | 104,960 | ONNX Runtime provider 桥（防御性随包） |
| `models\sense-voice\model.int8.onnx` | 239,233,841 | SenseVoiceSmall q8（pinned） |
| `models\sense-voice\tokens.txt` | 315,894 | SenseVoice tokens（pinned） |
| `models\sense-voice\silero_vad.onnx` | 643,854 | Silero VAD（pinned） |
| `README.md` | 21,851 | 用户说明（含署名与许可） |
| `CLOUD-TRIAL.md` | 8,346 | 云端指南 |
| `THIRD-PARTY-NOTICES.md` | 4,459 | 第三方署名与许可证索引 |
| `licenses\FunASR-MODEL_LICENSE.txt` | 5,306 | FunASR MODEL_LICENSE 原文 |
| `licenses\sherpa-onnx-LICENSE.txt` | 11,358 | Apache License 2.0 原文 |
| `licenses\onnxruntime-LICENSE.txt` | 1,073 | MIT 原文 |
| `glossary.txt` | 2,357 | 术语表 |
| `scripts\configure-cloud-trial.ps1` | 9,515 | 云端配置助手 |
| `启动 Listen.cmd` | 1,168 | 云端默认启动器 |
| `启动 Listen 本地备用.cmd` | 1,765 | 显式本地备用启动器（SenseVoice） |
| `VERSION.txt` | 720 | 版本与署名 |
| `manifest.json` | 6,390 | 逐文件大小与 SHA-256 |
| `.listen-package` | 22 | 重打包授权标记（不属于交付内容） |

| 项 | 实测 |
| --- | --- |
| 文件总数 | 21（清单 TSV：`.artifacts\issue-23\package-inventory.tsv`） |
| 包内总字节 | **264,881,295 B**（264.88 MB 十进制 / 252.61 MiB） |
| 其中 SenseVoice 引擎+模型 | 262,392,373 B |
| 其中应用与运行库 | 2,414,592 B |
| 其中文档/启动器/脚本/许可证 | 67,918 B |
| 旧 whisper 包对照 | 494,424,525 B → 减少 229,543,230 B（−46.4%） |
| 工单目标「约 260 MB」 | 差 +4.88 MB（+1.9%），在验证脚本 245–285 MB 带内 |

### 1.2 whisper 负断言

包内递归枚举后，文件名匹配 `^(whisper|ggml)` 的文件 **0** 个，目录段匹配 `whisper|ggml` 的
**0** 个；`engine\` 下只有 `sherpa\` 一个子目录，不再有 `whisper-cli.exe`、`whisper.dll`、
`ggml*.dll`、`vad-speech-segments.exe`，也不再复制 `MSVCP140.dll`/`VCRUNTIME140*.dll`/
`VCOMP140.DLL`（ONNX Runtime 为 MT/静态 CRT，导入表不需要这些）。`manifest.json` 的
`engine_source` 与 `engine_files` 同样不含 whisper 项。

### 1.3 5 个 pinned SHA-256（独立于 manifest 复核）

验证脚本把期望值写死在脚本内（来源 `app\listen\src\sensevoice.rs:50-77`，不是从被检查的
`manifest.json` 读取），因此重打包时同步重算 manifest 也无法蒙混：

| 文件 | 字节 | SHA-256 | 结果 |
| --- | ---: | --- | --- |
| `engine\sherpa\sherpa-onnx-c-api.dll` | 4,294,656 | `0965A6E4C0254DA2BEFF489CF7A8AAFFAF7DC94F90B43B3E66448B8CAB4177EA` | 一致 |
| `engine\sherpa\onnxruntime.dll` | 17,799,168 | `7F66F939A881BAF4F46A2216496798EDF4A1429878B646D12674AA62F27D8A25` | 一致 |
| `models\sense-voice\model.int8.onnx` | 239,233,841 | `C71F0CE00BEC95B07744E116345E33D8CBBE08CEF896382CF907BF4B51A2CD51` | 一致 |
| `models\sense-voice\tokens.txt` | 315,894 | `F449EB28DC567533D7FA59BE34E2ABCA8784F771850C78A47FB731A31429A1DC` | 一致 |
| `models\sense-voice\silero_vad.onnx` | 643,854 | `9E2449E1087496D8D4CABA907F23E0BD3F78D91FA552479BB9C23AC09CBB1FD6` | 一致 |

打包脚本另外还核对 `onnxruntime_providers_shared.dll`（`551D0E1F…`，104,960 B）：应用代码
不校验它，但打包时不接受与本次记录不一致的副本。

### 1.4 验证脚本结果（绿）

```powershell
.\scripts\verify-listen-package.ps1 -PackageRoot .\dist\Listen-0.1.0 `
  -Evidence E:\luyin\.artifacts\issue-23\package-verify.json
```

| 字段 | 实测 |
| --- | --- |
| 必需文件 / manifest 哈希 / pinned 哈希 / 启动器 / 扫描文件 | 17 / 19 / 5 / 2 / 19 |
| 包内总字节 | 264,881,295 |
| 本地备用 | `Ready`，`recognition_mode=local`，phase 序列 `Preparing → Ready`，启动到状态 1–3 s（每次运行不同，见 `package-verify*.json`） |
| 本地 backend | `SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s); Silero VAD gates every window and each detected utterance is decoded separately.` |
| 云端默认（无凭据） | `Failure` / `cloud-trial`，backend 显示 `Alibaba Model Studio / qwen-audio-3.1-asr-flash-message / cn-beijing`，detail 含 `valid Alibaba workspace` |
| 云端默认（合成凭据） | `Ready` / `cloud-trial`，`cloud_paid_requests=0`（账本 0 次预留） |

退出码 0，输出见 `.artifacts\issue-23\green-verify.log`。第 1.2/1.3 节与体积带都是本次
**新增**的断言；第 1.4 节其余检查沿用并保留原有云端语义。

## 2. 完整流程（包内本地备用启动器）

驱动脚本（git 忽略的 artifacts，非仓库内容）：
`E:\luyin\.artifacts\issue-23-offline\run-packaged-sensevoice.ps1`。它用
`cmd.exe /c "启动 Listen 本地备用.cmd"` **双击等价**启动打包件，之后只用
`--control` + `status.txt` / `panel.txt` / `current-result.txt` 取证（窗口类名与控件 ID 只作
自检，不作验收 oracle）。

| 项 | 实测 |
| --- | --- |
| 启动方式 | 包内 `启动 Listen 本地备用.cmd` → 恰好 1 个 `listen.exe`，启动器退出码 0 |
| 胶囊 | `status.txt` `capsule=Ready`、`startup_capsule_ms=30`、`capsule_capture_excluded=1`；窗口自检 1 个 `ListenStatusCapsuleV1`，275×33，无标题、`LAYERED|TOOLWINDOW|NOACTIVATE` |
| 识别来源 | `recognition_mode=local`，`backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s)…`，`preparation=Ready` |
| 采集端点 | `endpoint_name=耳机 (WH-1000XM4)`，`endpoint_role=default multimedia render endpoint` |
| 阶段 | `Ready → Capturing → Result`（`--control start`/`stop`） |
| 结果 | `current-result.txt` 26 字符完整结果；停止到结果 **0.389 s** |
| 面板 | `--control show` 后 `panel_visible=1`，`panel.txt` 包含完整结果（830 字符），面板窗口可见并截图 |
| 运行目录 | 仅 `status.txt`、`panel.txt`、`current-result.txt`、`segments.json`、`segment-00000001.txt`、`delivery.json`、`latency.jsonl`（无云端凭据、无云端连续 PCM） |
| 退出 | `--control quit` 退出码 0，无残留 `listen` 进程 |
| 检查数 | **36 项全过** |

说明：本次没有把结果追加到编辑器目标（脚本未绑定编辑区目标），因此走的是工单允许的另一条
路径——**完整结果进入面板**。目标追加语义没有改动，其证据属于 #24 的全量回归。

证据：`.artifacts\issue-23\flow-evidence.json`、`panel-ready.png`、`panel-result.png`、
`flow.log`。

## 3. 默认启动器仍走云端

驱动脚本：`.artifacts\issue-23\verify-default-launcher.ps1`，用包内 `启动 Listen.cmd` 启动，
并把 `LOCALAPPDATA` 指向隔离目录、清空 `LISTEN_ALI_API_KEY` / `LISTEN_ALI_WORKSPACE_ID`，
因此**不可能**读到真实凭据，也不会有任何付费请求。

| 项 | 实测 |
| --- | --- |
| 启动方式 | 包内 `启动 Listen.cmd`，启动器退出码 0 |
| `recognition_mode` | `cloud-trial` |
| phase / detail | `Failure`；`Cloud trial requires a valid Alibaba workspace, cn-beijing region and qwen-audio-3.1-asr-flash-message model; check the local configuration` |
| backend | `Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message; region=cn-beijing; shared single-vendor trial cap=CNY 50` |
| 是否开始采集 | 否（`recognition` 不含 `Capturing`） |
| 付费预留 | 账本 `attempts` = **0** |
| 检查数 | **11 项全过** |

证据：`.artifacts\issue-23\default-launcher-evidence.json`、`default-launcher.log`。`runtime-cloud\`
在脚本的 `finally` 中删除，包内不残留运行目录。

## 4. 禁网：替代证据（未做真防火墙禁网）

### 4.1 为什么没有真禁网

沿用 `.artifacts\offline-firewall-helper-final-package.ps1` 的做法需要
`New-NetFirewallRule`，即**管理员令牌**。本会话未提权，且无人值守会话无法批准 UAC：

```json
{ "administrator": false, "firewall_rule_created": false,
  "reason": "New-NetFirewallRule requires an elevated token; this unattended session cannot approve a UAC prompt, so no firewall rule was created and none had to be removed." }
```

证据：`.artifacts\issue-23-offline\firewall-attempt.json`。因为**一条规则都没建**，不存在需要
撤销的规则；也没有把半成品规则留在系统里（脚本没有执行 helper）。

### 4.2 采用的替代证据

同一个包内本地备用启动器流程（`-PollNetwork` 打开）重跑一次，并额外断言：

| 项 | 实测 |
| --- | --- |
| `Get-NetTCPConnection -OwningProcess <listen pid>` 采样 | **66 次**（等待 Ready、Capturing 全程、结果后各阶段） |
| 非环回远程连接 | **0**（`remote_connections=[]`） |
| 监听端口 | **0**（`listening_sockets=[]`） |
| 本地运行目录里的云端凭据/待处理云端音频 | **0**（无 `ali.json`、`ledger.json`、`ledger.lock`、`playback.state`、`recovered.pcm/wav`） |
| 运行目录内容 | `status.txt`、`panel.txt`、`current-result.txt`、`segments.json`、`segment-00000001.txt`、`delivery.json`、`latency.jsonl` |
| 完整流程 | **37 项全过**；`recognition_mode=local`；端点 `耳机 (WH-1000XM4)`；82 字符结果；停止到结果 1.167 s |

证据：`.artifacts\issue-23-offline\offline-flow-evidence.json`、`offline-flow.log`、
`panel-ready.png`、`panel-result.png`。

### 4.3 边界（明确标注）

- 这是**可复核的替代证据，不是真防火墙禁网**：它证明本地识别流程运行期间没有打开远程连接、
  没有落云端凭据或待处理云端音频，但它不能证明“若代码尝试联网会被系统阻断”。
- 本地备用路径没有子进程引擎：SenseVoice 通过 `sherpa-onnx-c-api.dll` 在应用进程内常驻，
  因此只需观察 `listen.exe` 一个进程（旧 whisper 包的 `whisper-cli.exe` /
  `vad-speech-segments.exe` 规则不再适用）。
- 有管理员令牌时可在 5 分钟内补做：只封 `dist\Listen-0.1.0\listen.exe`（`-Direction Outbound
  -Program <exe> -Action Block -Profile Any`），跑同一驱动脚本，`finally` 删除规则并回读
  `still_present=[]`；证据目录按 `.artifacts\issue-23-offline\` 复用。

## 5. 没有 key 时精修不可用

本机 `DEEPSEEK_API_KEY` 未设置（`[bool]$env:DEEPSEEK_API_KEY` 为 `False`），包内也不带任何 key。
在同一份完整流程里断言：

| 检查 | 实测 |
| --- | --- |
| `status.txt` 的 `refine` | `unavailable`（Ready 时与结果后都是） |
| `refine_reason` | `No DeepSeek API key is configured. Set DEEPSEEK_API_KEY in the local environment to enable refine; local recognition, delivery and the current result are unaffected.` |
| 面板按钮（控件 ID 204，仅自检） | `IsWindowEnabled=false`，文案 `精修（不可用）` |
| `--control refine` | 命令送达（退出码 0，与其他控制命令一致），`status.txt` 的 `detail` 明确写 `Refine is unavailable…The current result is unchanged…` |
| 拒绝后的本地结果 | `current-result.txt` 逐字符不变 |
| `panel.txt` | 含 `精修不可用` 与 `DEEPSEEK_API_KEY` |

因此“没有 key 时精修不可用、本地识别与交付不受影响”在**包内二进制**上成立，而不是只在开发
构建上成立。证据同 §2：`.artifacts\issue-23\flow-evidence.json`。

## 6. 署名与许可证

### 6.1 落点

- 包根 `THIRD-PARTY-NOTICES.md`：三类组件的署名、许可证名称、上游 URL、许可证文本的
  SHA-256、以及 whisper 已移除的说明。
- 包根 `licenses\`：三份**上游原文**（FunASR MODEL_LICENSE、sherpa-onnx Apache 2.0、
  ONNX Runtime MIT）。
- `packaging\README.md` 新增「署名与许可」节；`VERSION.txt` 增加一行署名；
  包内 `manifest.json` 增加 `third_party_notices` 与 `licenses[]` 字段。
- 应用内没有「关于」对话框（#23 之前也没有），所以按工单的“README 或关于”落在 README 与
  包内 notices 上。

### 6.2 许可证原文的来源与哈希

| 文件 | 上游地址 | 字节 | 文本 SHA-256 |
| --- | --- | ---: | --- |
| `licenses\FunASR-MODEL_LICENSE.txt` | <https://raw.githubusercontent.com/modelscope/FunASR/main/MODEL_LICENSE> | 5,306 | `7DBA975A2069691DB4992B0592D70828B330D2F8A30A71450F4E152A554E84F8` |
| `licenses\sherpa-onnx-LICENSE.txt` | <https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/master/LICENSE> | 11,358 | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |
| `licenses\onnxruntime-LICENSE.txt` | <https://github.com/microsoft/onnxruntime/blob/main/LICENSE>（经同仓库镜像取得，见下） | 1,073 | `2F07C72751AED99790B8A4869CF2311DF85A860B22DED05FA22803587A48922C` |

- FunASR MODEL_LICENSE v1.1 的 2.2 条要求“注明出处以及作者信息，并保留相关模型名称”，
  因此 README、`VERSION.txt`、`THIRD-PARTY-NOTICES.md` 都保留 `SenseVoiceSmall` / `FunASR` 字样。
- 上游 `FunAudioLLM/SenseVoice` 的 README「License」节也确认：仓库源码是 MIT，**模型权重**
  按各自模型卡条款分发，官方 SenseVoiceSmall 模型卡指向 FunASR MODEL_LICENSE。
- 取证偏差：本机无法直接读取 `microsoft/onnxruntime` 仓库（`github.com` 与
  `raw.githubusercontent.com` 对该仓库路径均返回 HTTP 404；同组织其他仓库正常），因此 MIT
  原文取自该仓库的官方镜像 `https://raw.gitcode.com/microsoft/onnxruntime/raw/main/LICENSE`，
  并与另一份逐字副本（Hugging Face 上的 ONNX Runtime 许可证副本）比对一致。这两份副本的
  版权行均为 `Copyright (c) Microsoft Corporation`；上游某些历史版本写 `2018 Microsoft
  Corporation`。差异只在版权行年份，MIT 正文一致。这一点写进了
  `THIRD-PARTY-NOTICES.md`，不冒充“逐字节等于上游 main”。

### 6.3 验证脚本断言

`verify-listen-package.ps1` 现在要求包内存在 `THIRD-PARTY-NOTICES.md` 与三份许可证文本，
并断言：notices 含 `FunASR`、`SenseVoice`、`MODEL_LICENSE`、`sherpa-onnx`、`onnxruntime`、
`Apache License`、`MIT License` 以及三个 `licenses\…` 路径；每份许可证文本含各自的标志性条款
（`FunASR Model Open Source License Agreement`/`Alibaba Group`/`2.2 Restrictions`、
`Apache License`/`Version 2.0`、`MIT License`/`Microsoft Corporation`）；README 含
`SenseVoice`、`FunASR`、`sherpa-onnx`、`MODEL_LICENSE`、`THIRD-PARTY-NOTICES.md`。旧包在
第一组断言就失败（见 §7）。

## 7. 红 → 绿

本票没有改 Rust 行为，所以测试接缝是**打包验证脚本本身**：`scripts\verify-listen-package.ps1`
是对交付件的可执行规格。按 TDD 顺序：

### 7.1 红（先写断言，对现有旧 whisper 包跑）

```powershell
powershell -File E:\luyin\.worktrees\issue-23\scripts\verify-listen-package.ps1 `
  -PackageRoot E:\luyin\dist\Listen-0.1.0 `
  -Evidence E:\luyin\.artifacts\issue-23\red-package-verify.json
```

退出码 **1**，第一组新断言（SenseVoice 运行库/模型 + notices + licenses）直接失败：

```text
Package files are missing: THIRD-PARTY-NOTICES.md, licenses\FunASR-MODEL_LICENSE.txt,
licenses\sherpa-onnx-LICENSE.txt, licenses\onnxruntime-LICENSE.txt,
engine\sherpa\sherpa-onnx-c-api.dll, engine\sherpa\onnxruntime.dll,
engine\sherpa\onnxruntime_providers_shared.dll, models\sense-voice\model.int8.onnx,
models\sense-voice\tokens.txt, models\sense-voice\silero_vad.onnx
```

证据：`.artifacts\issue-23\red-verify.log`（完整输出）。

### 7.2 变异测试（证明新断言不是空断言）

把新包的 `manifest.json` 复制到一个只放占位文件的合成目录，逐项触发：

| 变异 | 期望 | 实测 |
| --- | --- | --- |
| 合成目录 + `engine\whisper-cli.exe` | 负断言命中 | 退出码 1：`The package still contains whisper.cpp engine or model files: engine\whisper-cli.exe` |
| 去掉 whisper 文件、保留占位文件 | 体积带命中 | 退出码 1：`The package size is outside the accepted local-model band (6478 bytes; expected 245000000-285000000).` |

证据：`.artifacts\issue-23\mutation-checks.log`。

### 7.3 绿（重新打包后同一脚本）

干净工作树 `git status --porcelain` 为空 → `package-listen.ps1 -IncludeModels` → 退出码 0，
`Files: 19; bytes (without manifest): 264874883`；随后验证脚本退出码 **0**（§1.4）。
证据：`.artifacts\issue-23\package-build.log`、`green-verify.log`、`package-verify.json`。

### 7.4 最终重打包复核

本文件提交后，在同一干净工作树上再打包一次（`manifest.json` 的 `git_commit` 记录该提交、
`git_dirty_at_package_time=false`），并用同一验证脚本复核，证据
`.artifacts\issue-23\package-verify-final.json`。两次打包之间只有 `manifest.json` 里的提交/
时间字段变化，引擎、模型、文档、许可证、启动器的字节与哈希完全相同（§1.3 的 5 个 pinned
哈希不变），因此 §2/§4 的桌面流程证据仍然对应当前交付件；为了避免重复消耗采集与时间，没有
仅因 manifest 元数据变化而重跑桌面流程。

## 8. 测试与格式

- `.\scripts\build-listen.ps1 -Task Test`：见 §8.1（日志 `.artifacts\issue-23\test.log`）。
- `.\scripts\build-listen.ps1 -Task CheckFormat`：无输出、退出码 0。
- 本票没有改 `app\listen\src`（`git diff --stat 880ddef..HEAD` 只有 `README.md`、`docs`、
  `packaging\*`、`scripts\package-listen.ps1`、`scripts\verify-listen-package.ps1`），所以不需要
  新增/修改 Rust 测试；测试套件用于确认没有回归。

§8.1 结果：**306 passed / 0 failed / 16 ignored**（与 #40 记录的基线一致；`ignored` 不计通过），
`CheckFormat` 无输出，退出码 0。

## 9. 偏差与限制

1. **未做真防火墙禁网**（原因：需要 UAC；本会话未提权、无人值守无法批准）。替代证据见 §4，
   不声称“系统层面已断网”。
2. **onnxruntime 许可证不是从 GitHub 上游仓库逐字节取得**（该仓库路径在本机返回 404），
   取自同仓库官方镜像并交叉比对；差异只在版权行年份，已写入 notices。
3. **`onnxruntime_providers_shared.dll` 不在应用代码的 pinned 常量里**，只由打包脚本按记录
   哈希核对。它只有 104,960 B；带上它是为了避免 ONNX Runtime 在缺 provider 桥时的加载差异，
   应用实际只 `LoadLibraryExW` 两个 DLL（`sherpa-onnx-c-api.dll` + `onnxruntime.dll`）。
4. **体积带取 245–285 MB（约 ±8%）**：工单只说“约 260 MB”。该带宽能同时拒绝旧 whisper 包
   （494 MB）和缺模型包（约 25 MB），但没有更窄的上游依据。
5. **只验证了 `-IncludeModels` 的包**：无模型分支的存在性/失败文案断言已改成 SenseVoice
   （`The local SenseVoice … is missing` 一类），但本次没有实际构建无模型包跑一遍。
6. **默认启动器的“云端 Ready”用合成凭据验证**（验证脚本内），不是真凭据；真实付费请求仍然
   为 0（账本 `attempts=0`）。真云端采集属于 #40 的验收范围，本票不重复消耗额度。
7. **结果路径走面板而不是编辑器追加**：驱动脚本没有绑定编辑区目标，符合工单“结果追加**或**
   进入面板”。目标追加/截断/重试/设备中断的完整回归属于 #24。
8. **包内没有「关于」对话框**：按工单的“README 或关于”落在 README + `THIRD-PARTY-NOTICES.md`
   + `VERSION.txt`。
9. **#40 的历史记录只改了一句现状描述**：`docs\issue-40-delivery.md` 第 6 条从“交付包本地
   引擎仍是 whisper.cpp”改成“#40 交付包（2026-11-21）的本地引擎是 whisper.cpp，#23 已完成
   换包”，其余 #40 验收数字未改动；#40 那次交付的事实保持不变。
10. **README 里没有删除开发侧的 whisper 回退说明**：`--engine whisper` 仍是可用的显式回退，
   只是交付包不再携带 whisper 运行库与模型。这是代码现状，不是包的承诺。

## 10. 证据索引

| 证据 | 路径 |
| --- | --- |
| 红灯（旧包） | `.artifacts\issue-23\red-verify.log` |
| 绿灯（新包） | `.artifacts\issue-23\green-verify.log`、`package-verify.json` |
| 最终重打包复核 | `.artifacts\issue-23\package-verify-final.json` |
| 变异测试 | `.artifacts\issue-23\mutation-checks.log` |
| 打包日志 | `.artifacts\issue-23\package-build.log` |
| 包内清单 TSV | `.artifacts\issue-23\package-inventory.tsv` |
| 完整流程（本地备用启动器） | `.artifacts\issue-23\flow-evidence.json`、`flow.log`、`panel-ready.png`、`panel-result.png` |
| 默认启动器（云端） | `.artifacts\issue-23\default-launcher-evidence.json`、`default-launcher.log` |
| 禁网替代证据 | `.artifacts\issue-23-offline\offline-flow-evidence.json`、`offline-flow.log`、`firewall-attempt.json`、`panel-ready.png`、`panel-result.png` |
| 全套测试 / 格式 | `.artifacts\issue-23\test.log`、`.artifacts\issue-23\checkformat.log` |
| 驱动脚本（artifacts，非仓库内容） | `.artifacts\issue-23-offline\run-packaged-sensevoice.ps1`、`.artifacts\issue-23\verify-default-launcher.ps1` |

以上 `.artifacts` 目录被 Git 忽略；仓库内的交付物是本文件、打包/验证脚本、`packaging\`
文档与许可证原文。
