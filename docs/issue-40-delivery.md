# #40 云端默认交付：已完成与未验证的边界

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)。分支
`implement/issue-40-delivery`，工作树
`E:\listen-worktrees\ticket-40-delivery-20261121`，基于
`integration/cloud-recognition` 的 `3e3eb61`（合并 #35 后的 tip）。未 push、未合并回
集成分支、未关闭 issue。

**结论先行：日常默认没有切到云端。** #26 要求的正式验收（20 段真实音频的内容错误率对比、
数字/技术词/否定与不确定表达逐项核对、正常网络 20 次至少 19 次在停止后 1 秒内完成真实目标
完整读回、账单核对）**尚未运行**；没有已通过门槛的候选。#40 的验收条件允许在没有合格候选时
维持现有默认，因此本票只交付**可离线验证的 Windows 包、显式云端试用入口、本机凭据配置路径、
实际厂商/型号/地域显示和明确的本地备用**，不声称任何候选已通过，不提供虚构的准确率、延迟或
费用数字。

## 交付了什么

### 日常默认与两种显式入口

- 源码默认仍是本地识别：`Config.cloud_trial` 默认 `false`，只有显式 `--cloud-trial ali`
  才选择云端；`--cloud-trial` 仍不能与 `--engine` 组合，不存在任务内的自动云端/本地切换。
- 交付包保留原有的本地启动器 `启动 Listen.cmd`：使用包内 `engine\`（whisper.cpp v1.8.2
  CPU）和 `models\`，运行目录 `runtime\`，仍是双击即用的日常路径。
- 新增显式云端启动器 `启动 Listen 云端试用.cmd`：`--cloud-trial ali`，固定
  **阿里云百炼 Model Studio / `qwen-audio-3.1-asr-flash-message` / `cn-beijing`**，运行
  目录 `runtime-cloud\`。它与本地运行目录分开，避免把云端 PCM 交给本地识别，也避免本地
  WAV 被云端当作连续 PCM 恢复。
- 包内 `README.md` 与 `CLOUD-TRIAL.md` 说明两种入口、首次配置、失败提示、费用与数据边界。
- **没有新增**自动摘要、自动发送、自动入会或自动文字精修行为；面板/复制/目标回填仍沿用既有
  能力，精修仍需用户单独触发。

### 本机凭据配置与运行时来源显示

- 包内新增 `scripts\configure-cloud-trial.ps1`：隐藏输入 API Key，写入当前 Windows 用户的
  Credential Manager 通用凭据 `Listen/CloudTrial/Alibaba/cn-beijing`；非秘密配置只在
  `%LOCALAPPDATA%\Listen\cloud-trial\ali.json` 保存 `workspace_id`、`model`、`region`。
  支持 `-ShowStatus`、`-RemoveKey`、`-SelfTest`，不发起网络或付费请求。
- 运行时 `status.txt` 的 `backend=` 行与结果面板显示实际来源。实测两条路径：
  - 配置非法时：`Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message;
    region=cn-beijing; shared single-vendor trial cap=CNY 50`，并拒绝开始；
  - 本机配置有效（仅合成假 Key，未开始采集）时：同上，包内构建进入 `Ready`。
- 原来残留的 “shared two-provider” 措辞已改为 “shared single-vendor”，反映豆包已退出、
  当前只有阿里云一家；`--help` 也改为说明每次开始都要通过实时默认工作空间、精确型号免费保护
  和共享 CNY 50 账本核验。
- 共享账本语义未改：CNY 50 是首轮硬上限；CNY 100/月只是提醒线，不会中止采集或自动购买/
  提高额度；未知费用不显示成 0。**实际现金账单仍未对本工具做过对账。**

### 包内没有携带的内容

`scripts\verify-listen-package.ps1` 现在对交付包做独立检查：

- 必需文件、启动器内容、README/CLOUD-TRIAL 关键说明、manifest 固定厂商/型号/地域、
  `Daily default: local`、哈希与大小；
- **禁带项**：`.wav`/`.pcm` 等测试录音、`runtime`/`session-*`/`tests`/`.tools`/`.artifacts`
  等本机测试环境目录、`status.txt`/`panel.txt`/`current-result.txt`/`segments.json`/
  `delivery.json`/`ledger.json` 等运行数据、`.env`/`.pem`/`.key`；
- **凭据扫描**：文本文件中的 `LISTEN_ALI_API_KEY=<值>`、`API key=...`、
  `Authorization:...`，以及所有小文件中的 `Bearer <长串>`、`sk-<长串>`、`LTAI<长串>`；
  manifest/VERSION 不允许出现本机绝对路径；
- 扫描器自身有合成假 Key 的正向自检，扫描前先证明模式不是空转；
- 不读取真实 Key、不发起付费调用；云端启动检查只验证启动/配置表面，账本必须保持 0 次预留。

## 实际执行的验证（2026-11-21）

### 构建与自动测试

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\build-listen.ps1 -Task Build
.\scripts\package-listen.ps1 -IncludeModels
.\scripts\verify-listen-package.ps1 -PackageRoot .\dist\Listen-0.1.0 `
  -Evidence .\.artifacts\issue-40\package-verify.json
```

- `-Task Test`：**293 passed / 0 failed / 16 ignored**（忽略项均需要真实模型、操作者编辑器、
  真实付费 Key 或语料；不计为通过）。本票新增
  `app/listen/tests/delivery_config.rs` **4 项**，覆盖：
  1. 运行时描述只列一个厂商，并显示精确型号/地域，不再出现 two-provider；
  2. 空/非法/超长工作空间、非法型号、非法地域在**打开播放设备、建立连接、预留账本之前**
     被拒绝，且运行目录不出现 Key；
  3. 缺失或格式非法的 Key 被拒绝，不泄露 Key；`Secret` 接受 4096 个可打印 ASCII，拒绝
     4097、空格、制表/换行；
  4. `ali.json` 的未知字段、缺字段、非 JSON 被拒绝（`deny_unknown_fields`）。
- `-Task CheckFormat`：通过（退出码 0）。
- `-Task Build`：离线锁定 release 构建通过。
- 既有已知 flake `multi_segment::a_finished_backlog_is_recovered_one_segment_at_a_time_in_capture_order`
  本轮通过；未修改、未降低该断言。

### 交付包与冒烟结果

包目录 `dist\Listen-0.1.0\`（含模型）：

- 23 个文件、约 494.4 MB（模型约 488 MB）；21 个 manifest 条目全部通过大小与 SHA-256
  校验；
- 两个启动器；`scripts\configure-cloud-trial.ps1` 与 `CLOUD-TRIAL.md` 存在；
- `VERSION.txt` 记录 `Daily default: local` 与固定云端厂商/型号/地域；
- `manifest.json` 记录 `default_mode=local`、`cloud_trial.selected_by_default=false`、
  provider/model/region、配置助手和指南路径；不含本机绝对路径。

包内真实启动冒烟（`verify-listen-package.ps1`，证据
`.artifacts\issue-40\package-verify.json`）：

| 检查 | 实际结果 |
| --- | --- |
| 文件/哈希/启动器/文档/固定选择 | 15 项必需文件、21 个哈希、2 个启动器全部通过 |
| 本地启动（包内 engine/model） | `Preparing → Ready`，backend 为进程内常驻本地模型 |
| 云端启动，无本机配置 | `Failure`：`Cloud trial requires a valid Alibaba workspace, cn-beijing region and qwen-audio-3.1-asr-flash-message model; check the local configuration` |
| 云端启动，合成本地配置 | `Ready`，backend 显示 `Alibaba Model Studio / qwen-audio-3.1-asr-flash-message / cn-beijing` |
| 云端启动是否付费 | 账本 0 次预留；未开始采集，无模型任务、无音频外发 |
| 控制退出 | `--control quit` 退出码 0 |
| 密钥/运行数据/测试环境扫描 | 22 个可扫描文件全部通过；扫描器自检通过 |
| 包内凭据助手可用性 | `scripts\configure-cloud-trial.ps1 -SelfTest` 输出 `Credential Manager self-test passed; temporary entry removed. No network or paid call was made.` |

最终干净构建（本文件所在提交，工作树干净）的 `manifest.json` 记录 `git_commit` 为该提交、
`git_dirty_at_package_time=false`；同一 `verify-listen-package.ps1` 结果保持为 15 项必需文件、
21 个哈希、2 个启动器、22 个扫描文件、本地 `Ready`、无配置云端 `Failure`、合成本地配置
云端 `Ready`、账本 0 次预留。启动到状态约 1.2–1.5 秒是单机冒烟观测，不是延迟承诺；最终
JSON 在 `.artifacts\issue-40\package-verify.json`。`--control quit` 退出码 0。

负向探针（证明检查不是空转；均在同一临时包副本内注入后立即删除）：

| 注入内容 | 实际失败信息 |
| --- | --- |
| `probe-key.txt` 写入 `LISTEN_ALI_API_KEY=sk-aaaa...` | `The package contains key-like material in: probe-key.txt` |
| `probe.wav` | `The package contains a forbidden file type: probe.wav` |
| `runtime\status.txt` | `The package contains a forbidden directory: runtime\status.txt` |

### 交付包无关的本地回归

既有 289 项测试与 16 个 ignored 的基线保持；#35 的重启恢复、#36 的存储上限与显式清理、
#37 的累计用量/月提醒、#38 的阶段计时与调度测试全部通过。本票未修改这些语义。

## 明确未验证的内容

以下项目**没有**运行，不能写成已通过：

1. **真实付费云端识别**：没有使用真实 API Key 发起 Message 任务。包内配置检查用进程环境
   注入的合成假 Key 验证设置读取、有效性检查和来源显示；包内凭据助手只运行了
   `-SelfTest`（临时假凭据写入/读回/删除），没有写入真实 Key。两者都不能证明账号/型号权限、
   免费保护实时状态、网络协议或最终文字。
2. **准确率正式验收**：#26 的 20 段真实耳机音频、同音频本地基线、内容 CER、数字/技术词/
   否定与不确定表达逐项核对均未运行；没有“错误至少减半”的结论。
3. **真实编辑器一秒验收**：没有在真实编辑器上做 20 次正常网络停止→完整读回计时；没有
   19/20 在 1 秒内的证据。离线阶段拆分与受控调度测试不能替代这一项。
4. **账单对账**：没有核对阿里云实际现金扣款；共享账本只是本工具自己的估算，未知仍为未知。
5. **数据留存/训练条款**：没有按实际账号、地域、型号核实条款；本包不宣称零留存。
6. **真实弱网/积压/重启的云端运行**：离线适配器已覆盖断线、补传、多段、重启等语义，但
   没有真实付费网络下的运行记录。
7. **交付包的完整桌面验收**：本票只执行了包内启动/配置/来源冒烟；没有在包内二进制上重跑
   真实耳机采集、真实编辑器回填、设备切换等 #11 级别的桌面全流程。

## 为什么不切换日常默认，以及解除阻塞需要什么

`#26` 的启用门槛是：候选在固定 20 段真实音频上错误不高于基线的一半，且正常网络 20 次中
至少 19 次在停止后 1 秒内完成真实目标完整读回；失败、超时和未完成回填都计入分母，不能重试
替换。该验收**没有运行，也没有合格候选**。在这种状态下把云端设为默认，等于用标签、文档或
离线测试替代验收，正是 #40 验收条件禁止的放行方式。因此本票选择：

- 维持本地为日常默认；
- 保留显式云端试用入口与清晰的本机配置；
- 在任务和文档中如实记录未验证项与阻塞条件。

解除阻塞至少需要（不是本票可独立完成的）：

1. 对本轮真实付费调用（共享 CNY 50 上限内）的明确预算授权；
2. 操作者本机配置真实凭据，并完成 `bl` CLI 登录、默认工作空间与精确型号免费保护的现场
   核验；账号/型号权限和实际额度可用；
3. 确定的 20 段真实耳机音频、固定参考原文与术语，以及启用有效术语表的同音频本地基线；
4. 操作者运行的真实编辑器夹具（真实焦点、真实读回确认），用于逐次记录停止到完整读回的
   墙钟时间；失败样本保留在分母；
5. 每次调用的 task ID、账本预留、最终用量与实际账单的逐笔核对；
6. 按实际账号、地域、型号核实留存/训练条款。

满足上述条件并由后续验收给出通过证据后，才可以把默认从本地切换为云端；届时应重新打包、
重新验证交付包配置与验收时一致，而不是用本票的离线证据替代。

## 变更文件

- `app/listen/tests/delivery_config.rs`（新增 4 个测试）
- `app/listen/src/cloud/mod.rs`、`app/listen/src/cloud/budget.rs`、`app/listen/src/panel.rs`、
  `app/listen/src/main.rs`（单厂商/免费保护/月提醒的运行时措辞；未改状态键）
- `scripts/package-listen.ps1`（两个启动器、配置助手、指南、manifest/VERSION 元数据、
  去除本机模型路径）
- `scripts/verify-listen-package.ps1`（凭据/测试环境/本机路径检查、扫描器自检、本地与显式
  云端启动冒烟、账本 0 预留）
- `scripts/configure-cloud-trial.ps1`（只改 “two-provider” 为 “single-vendor”，配置/校验
  逻辑未放宽）
- `packaging/README.md`、`packaging/CLOUD-TRIAL.md`（新增）、
  `packaging/启动 Listen 云端试用.cmd`（新增）
- 本文件

未修改七个用户自有脏文档：`GLOSSARY.md`、`docs/adr/0001-*`、`docs/adr/0003-*`、
`docs/adr/0004-*`、`docs/cloud-recognition-design.md`、
`docs/performance-analysis-2026-11-14.md`、`docs/recognition-quality-analysis-2026-11-14.md`。

## 复现与证据位置

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\build-listen.ps1 -Task Build
.\scripts\package-listen.ps1 -IncludeModels
.\scripts\verify-listen-package.ps1 -PackageRoot .\dist\Listen-0.1.0 `
  -Evidence .\.artifacts\issue-40\package-verify.json
```

- 包内冒烟 JSON：`.artifacts\issue-40\package-verify.json`（本机，Git 忽略）
- 打包脚本自动证据：`.artifacts\issue-11\package\verify-<时间>.json`（本机，Git 忽略）
- `dist\Listen-0.1.0\manifest.json` 记录构建源码提交与 `git_dirty_at_package_time`；交付包
  应在本分支干净工作树上重建，`git_dirty_at_package_time` 必须为 `false`。

## 已知限制

- 交付包的本地路径仍使用 whisper.cpp；源码中的 SenseVoice 本地换包属 #23，本票没有改变它，
  也不声称本包包含 SenseVoice。
- 包内两个启动器使用不同运行目录是刻意选择：云端连续 PCM 与本地识别窗口格式不同，混用会
  让恢复路径把不属于当前引擎的音频交给错误的适配器。切换引擎应显式关闭并重新启动。
- 本票的云端冒烟没有开始采集，因此不验证免费保护、连接、上传、整段确认或费用路径；这些
  只能在真实账号和付费预算下验证。
