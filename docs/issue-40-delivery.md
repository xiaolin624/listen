# #40 云端默认交付：已通过验收的云端路径成为日常默认

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)。分支
`implement/issue-40-cloud-default`，工作树 `E:\listen-worktrees\ticket-40-default-20261121`，基于
`integration/cloud-recognition` 的 `01fb54d`（合并 #35/#36/#37/#38 后的 tip）。未 push、未合并回
集成分支、未关闭 issue。

**结论：日常默认已经切到通过验收的云端路径。** 依据
[`docs/issue-40-acceptance-20261121.md`](issue-40-acceptance-20261121.md) 的 2026-11-21 单厂商
实测：同一批 20 段真实音频、真实应用工作流，交付 **20/20**，停止到结果可见中位 656 ms、均值
668 ms、**最慢 844 ms**（20/20 ≤ 1 秒），聚合准确率 **95.86%**（CER 4.14%），相对同一批音频
的本地基线 75.39%（CER 24.61%）错误下降 83.2%。需求门槛是“错误不高于基线一半、20 次至少
19 次 1 秒内完整回填”，两项均以余量通过。

本地识别保留为**显式备用**，不是云端失败后的自动降级，也不是“缺凭据时先本地跑一遍”的兜底。

## 默认现在如何选择

- `Config.cloud_trial` 默认 `true`，`Config::parse` 默认返回云端；默认启动使用固定单厂商
  阿里云百炼 Model Studio / `qwen-audio-3.1-asr-flash-message` / `cn-beijing`。
- `--cloud-trial ali` 仍可显式写出云端选择；`--engine sensevoice|whisper|<whisper-cli.exe>`
  显式选择本地，并取消云端默认。
- **两个方向都拒绝组合**：`--cloud-trial ali --engine ...` 与 `--engine ... --cloud-trial ali`
  都返回 `--cloud-trial cannot be combined with --engine; cloud trial has no local fallback`，
  不会按参数顺序静默选一个。
- `Config::local_application` 在云端配置下直接拒绝构建本地应用，测试与调用方无法把默认配置
  悄悄变成本地运行。

## 默认无法运行时（无凭据/工作空间/型号/地域）

- 应用启动即进入失败状态，`detail` 沿用既有可操作文案：
  - 缺 Key：`No Alibaba cloud API key is configured. Use scripts/configure-cloud-trial.ps1 ... no cloud request was sent`；
  - 凭据格式非法：`The local cloud credential could not be read or has an invalid format; configure it again locally`；
  - 工作空间/型号/地域非法：`Cloud trial requires a valid Alibaba workspace, cn-beijing region and qwen-audio-3.1-asr-flash-message model; check the local configuration`。
- 开始采集被拒绝：不打开播放设备、不创建 `session-*` 采集段、不发起连接、不预留付费请求、
  不写任何目标、不保存本地识别文本；`current-result.txt` 和当前结果保持不变。
- 状态仍报告云端来源（`recognition_mode=cloud-trial`，`backend=Cloud trial: ...`），不会出现
  本地 backend，也不存在任务内的自动重试或本地替代。
- 应用级测试覆盖三种拒绝原因（缺 Key、非法 Key、空工作空间），并显式断言播放边界未打开、
  目标未改、结果文件未变、没有采集目录、没有本地 backend。

## 本地显式备用

- 命令行：`listen.exe --engine sensevoice`（默认本地引擎）或 `--engine whisper` /
  `--engine <whisper-cli.exe>`（whisper.cpp）。
- 交付包：`启动 Listen 本地备用.cmd`，明确标注“显式本地备用”，使用 `runtime\`；默认启动器
  `启动 Listen.cmd` 走云端 `runtime-cloud\`。
- 状态与面板写明实际来源：`status.txt` 的 `recognition_mode=local|cloud-trial` 与 `backend=`；
  本地面板显示“识别方式：本机识别（...）”。
- 开发启动器 `scripts\run-listen.ps1` 不传 `-Engine` 时与产品默认一致走云端；本地用
  `-Engine sensevoice|whisper`；云端与本地开发运行目录分开
  （`.artifacts\listen` / `.artifacts\listen-local`）。

## 验收证据、费用与到期后果

| 指标 | 实测 | 门槛 |
| --- | --- | --- |
| 交付成功 | 20 / 20 | — |
| 停止到结果可见 | 最慢 844 ms（中位 656 ms） | 19/20 ≤ 1000 ms（实际 20/20） |
| 聚合准确率 | 95.86%（37/894 字符错误） | ≥ 87.70%（错误 ≤ 基线一半） |
| 相对基线错误下降 | 83.2%（220 → 37 字符） | ≥ 50% |

证据、逐段结果、未覆盖项与限制见
[`docs/issue-40-acceptance-20261121.md`](issue-40-acceptance-20261121.md)。

- **免费额度到 2026-12-21 为止；之后按厂商实际计费转为付费。** 交付文档不提供固定金额承诺。
- 每次请求前仍做**实时免费保护核验**（唯一默认业务空间、精确型号、额度 > 0、`autoStop=true`、
  到期日）；核验不通过就不发请求。
- 共享账本语义不变：CNY 50 是首轮硬上限，所有已结算估计与未决预留之和受限；CNY 100/月只是
  提醒线，达到 90 元提前提醒，不中止采集、不取消录音、不自动购买套餐或提高额度。未知用量
  不记为 0，自动重试同样计入。
- 只使用一个厂商、一个已验收型号和地域；不常规双发、不在失败时跨厂商切换、不用本地结果冒充
  云端结果。
- 现金账单**仍未对账**；验收只证明免费保护使额外现金扣费路径不成立，不声称“账单为 0 元”。

## 交付包

包目录 `dist\Listen-0.1.0\`（含模型），由 `scripts\package-listen.ps1` 生成：

- 默认启动器 `启动 Listen.cmd`：不传 `--engine`、不传 `--cloud-trial`，以二进制默认启动云端，
  使用 `runtime-cloud\` 和 `glossary.txt`；缺少配置助手或指南时明确提示。
- 显式本地备用启动器 `启动 Listen 本地备用.cmd`：`--engine` + 包内 whisper.cpp/模型 +
  `runtime\`；不传 `--cloud-trial`。
- 原来的 `启动 Listen 云端试用.cmd` 已删除（默认即云端，保留一个“试用”入口会误导）。
- `manifest.json`：`default_mode=cloud`、`cloud_trial.selected_by_default=true`、固定
  provider/model/region、`local_fallback={explicit_only=true, launcher=<本地启动器>, runtime=runtime, engine=whisper.cpp v1.8.2 CPU}`。
- `VERSION.txt`：`Daily default: cloud (...; free-only protection verified before every request)`、
  `Local fallback (explicit only): whisper.cpp v1.8.2 CPU...`。
- `scripts\verify-listen-package.ps1` 现在断言：云端默认（manifest 与启动器内容）、默认启动
  不带 `--engine`/`--cloud-trial`、无配置默认启动被拒且显示云端来源、合成配置默认启动 Ready、
  显式本地备用启动器存在且实际以 `recognition_mode=local` 运行、账本 0 次预留；原有密钥/测试
  环境/哈希扫描保留。脚本没有新增“大文件”，模型仍走现有 `contentScanAllowlist` 的
  `models\ggml-small.bin` 允许项。

## 本轮验证

`-Task Test`：**306 passed / 0 failed / 16 ignored**。新增
`app/listen/tests/cloud_default.rs` **4 项**（默认选择并报告厂商/型号/地域；不可用凭据/工作空间
拒绝且不采集、不交付、不本地替代；显式本地端到端；云端+不同 engine 两个方向拒绝）。未忽略
任何测试，ignored 数量与原基线一致（16）。

`-Task CheckFormat`：通过。`-Task Build`：离线锁定 release 构建通过。

交付包验证（`scripts\package-listen.ps1 -IncludeModels` + `verify-listen-package.ps1`）结果：

| 检查 | 实际结果 |
| --- | --- |
| 文件/哈希/启动器/文档/固定选择 | 15 项必需文件、21 个哈希、2 个启动器、22 个可扫描文件全部通过 |
| 默认启动，无本机配置 | `Failure`：`Cloud trial requires a valid Alibaba workspace, cn-beijing region and qwen-audio-3.1-asr-flash-message model; check the local configuration`；`recognition_mode=cloud-trial`；`backend` 显示固定厂商/型号/地域；未开始采集 |
| 默认启动，合成本地配置 | `Ready`；`recognition_mode=cloud-trial`；`backend=Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message; region=cn-beijing; ...` |
| 显式本地备用启动 | `Preparing → Ready`；`recognition_mode=local`；`backend` 为进程内常驻本地模型（whisper.cpp，非云端） |
| 账本与密钥扫描 | 账本 0 次预留；扫描器自检通过；合成 Key 未进入任何运行文件；`--control quit` 退出码 0 |

启动到状态约 1.5 秒是单机冒烟观测，不是延迟承诺。证据见
`.artifacts\issue-40\package-verify.json`（本机，Git 忽略）。

## 变更文件

- `app/listen/src/launch.rs`（新增；`Config`/`parse`/`Recognition`/application 构造）
- `app/listen/src/main.rs`（薄入口；帮助文本改为云端默认 + 显式本地备用）
- `app/listen/src/lib.rs`（注册 `launch` 模块）
- `app/listen/src/gui.rs`（`status.txt` 追加 `recognition_mode`）
- `app/listen/src/panel.rs`（本地来源行）
- `app/listen/tests/cloud_default.rs`（新增 4 项应用级测试）
- `scripts/package-listen.ps1`、`scripts/verify-listen-package.ps1`
- `scripts/run-listen.ps1`（默认云端；`-Engine` 为显式本地；模式分离运行目录）
- `packaging/启动 Listen.cmd`（改为云端默认）、`packaging/启动 Listen 本地备用.cmd`（新增）、
  删除 `packaging/启动 Listen 云端试用.cmd`
- `packaging/README.md`、`packaging/CLOUD-TRIAL.md`
- `README.md`
- `docs/issue-29-cloud-trial.md`（仅加历史状态说明）
- `docs/adr/0006-cloud-default-after-acceptance.md`（新增）
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
- `dist\Listen-0.1.0\manifest.json` 记录构建源码提交与 `git_dirty_at_package_time`；交付包应在
  本分支干净工作树上重建，`git_dirty_at_package_time` 必须为 `false`。

## 仍未验证 / 已知限制

1. **现金账单对账**：没有核对阿里云实际现金扣款。免费保护使额外现金路径不成立，但这不等于
   “账单已核对为 0”。
2. **数据留存/训练条款**：只有官方文档引用，未按实际账号、地域、型号独立核实；不宣称零留存。
3. **真实弱网/断网演练**：验收批次未执行真实网络演练；恢复由离线可控测试覆盖。
4. **未知用量**：验收 1 段未返回最终用量，如实记为未知，未记为 0。
5. **测量口径**：延迟含 ≤ 50 ms 测试轮询滞后；交付目标为受控原生编辑框夹具，不是所有第三方
   编辑器。
6. **交付包本地引擎仍是 whisper.cpp**；SenseVoice 本地换包属于 #23，本票没有改变，也不声称
   包内是 SenseVoice。
7. 包内两个启动器使用不同运行目录是刻意选择：云端连续 PCM 与本地识别窗口格式不同，混用会让
   恢复路径把不属于当前引擎的音频交给错误适配器。切换来源应显式关闭并重新启动。
