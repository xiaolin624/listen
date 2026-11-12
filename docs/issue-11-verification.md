# Issue #11：Windows 可运行交付包与整体验收

2026-11-12。工作树 `E:\luyin\.worktrees\issue-11`，分支 `implement/issue-11-package`，基线 `b5730ff`（集成分支，含 #2–#10）。交付包由提交 `32cf8d12aad45f20b538636740050babc1e1617d` 构建，`manifest.json` 记录当时工作树干净。未 push、未关闭 GitHub issue；主工作树未改动。

本文记录**实际执行过**的包内验收。每个数字都来自对应的 `.artifacts\issue-11\` JSON/日志；未运行的场景明确标为未覆盖，不把已有分支记录冒充为本轮新测。

## 结论摘要

- 交付目录 `dist\Listen-0.1.0\` 可直接运行：`启动 Listen.cmd` 双击启动，使用包内 `engine\`、`models\` 和本地 `runtime\`，不需要先跑开发脚本或独立推理命令。包含模型时 manifest 记录 17 个交付文件、493,270,493 字节；不包含模型时 16 个交付文件、约 4.6 MiB，并按包内 README 放入模型。清单之外另有 `manifest.json` 本身和用于安全重建的 `.listen-package` 标记。
- 同一个包内 `listen.exe` 完成：真实耳机 WASAPI 采集、真实 small+Silero 本地识别、原生 EDIT 与 Chrome textarea/contenteditable 追加、备用文本框、只读/容量截断/目标失效/写入未确认回退、识别失败重试与清理、设备切换中断。**600 秒长会话与资源受压不是包内 `listen.exe` 的界面流程**：它们由同一源码的测试 harness `app\listen\target\release\examples\long_session.exe`（用 `Tape` 替换播放设备、不打开音频设备、不操作桌面）+ 包内 engine/model 测量，见下文“600 秒长会话与资源”。
- 状态序列观测到 `Preparing → Ready → Capturing → Processing → Result`；首次启动到 Ready 的模型准备为 618 ms（进程启动/轮询后约 1.1 秒）。同包首次/后续停止→结果 6674/6652 ms，没有额外的“准备后首次识别”冷启动。
- 600 秒会话：22 个识别窗口、停止→结果 **6921 ms**、峰值积压 **1**、结束 spool **0 字节**；资源峰值/平均见下表。不是 60 分钟，也不是产品时长上限。
- 真实识别质量仍保留已知错误（繁体/标点、`WASAPI loopback`→`Waze up high loop back`、`本季`→`本机`、英文句被译写等）；不宣称任意会议逐字零错误。
- 包内三个二进制没有网络导入，真实采集期间 `listen.exe` 未加载网络模块。**提权防火墙临时封禁复验已于 2026-11-12 11:02 在用户批准的一次 UAC 下完成**：三条出站封禁规则生效期间，从交付包本身跑通完整流程（`Ready` 1.347 s、停止→结果 7.138 s、目标追加正确），规则随后删除并读回 `cleanup_complete: true`、残留 0 条。

## 环境与基线

- Windows 11 25H2（26200.9711），i5-11400H，16 GiB；Windows PowerShell 5.1。
- 验收期间一个系统更新提示（`PickerHost` / `Shell_SystemDialog`）遮挡了全部桌面窗口；只关闭该提示，未关闭任何用户应用窗口，之后继续验收。
- 真实默认多媒体端点（验收时为耳机 `耳机 (WH-1000XM4)`，设备 ID `{0.0.0.00000000}.{352149e2-8d17-4bb5-92c3-968a7b35b7ed}`）；扬声器 `扬声器 (Realtek(R) Audio)` 用于设备切换场景。
- 真实已知语音 `E:\luyin\.artifacts\issue-2-integration\source-a.wav`（8.08 秒）及 #9 固定语料（8–19 秒）。
- 引擎 whisper.cpp v1.8.2 CPU；模型 `ggml-small.bin`（SHA-256 `1BE3...987B`）与 Silero `ggml-silero-v5.1.2.bin`（`2994...2CF`）；中文、6 线程、beam 5/best 5。包内 manifest 记录完整哈希。
- 浏览器 Google Chrome 157.0.8103.39；使用独立配置目录 `accept-web\chrome-profile`，只关闭本任务创建的窗口/进程。

## RED → GREEN

先写检查并实际观察失败，再实现打包：

1. **RED：包缺失**。在任何 `dist\Listen-\*` 存在之前运行新增的 `scripts\verify-listen-package.ps1`，实际输出：
   ```text
   Package directory is missing: E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0.
   Run scripts\package-listen.ps1 first.
   ```
   退出码 1。这不是“测试跳过”或“手工判定”，而是检查脚本对缺失产物的真实失败。
2. **GREEN：无模型包**。`package-listen.ps1`（不带 `-IncludeModels`）生成 16 个文件；`verify-listen-package.ps1` 校验 16 个文件哈希后启动 `listen.exe`，实际得到 `phase=Failure`：
   ```text
   Local model is unavailable (...\models\ggml-small.bin): 系统找不到指定的文件。 (os error 2)
   ```
   说明缺失模型的反馈明确，且 `--control quit` 正常退出（`control_quit_ok=true`，启动到状态 360 ms）。
3. **GREEN：包含模型包**。同一脚本加 `-IncludeModels`，先清空目标目录再生成 17 个文件；校验 17 个哈希后实际观察 `Preparing`、`Ready`，`backend` 为进程内常驻模型，`start_to_state_ms=1096`，`--control quit` 成功。
4. **可重复执行**：脚本先删除并重建目标目录；无模型/含模型两次构建使用同一脚本，未残留上一次文件。`-IncludeModels` 会核对两个模型的已记录 SHA-256，不匹配即失败，避免把错误模型打进包。
5. **保留独立性**：`verify-listen-package.ps1` 是独立检查文件、必需文件、哈希、启动器/README 内容和启动/退出，不是打包脚本内部的自证。

## 交付包内容与依赖核实

`dist\Listen-0.1.0\`（含模型）：

| 路径 | 字节数 | 作用 |
| --- | ---: | --- |
| `listen.exe` | 1,209,344 | 正式应用 |
| `libunwind.dll` | 90,624 | gnullvm 运行库，必需 |
| `engine\whisper-cli.exe` | 481,280 | CLI 识别路径 |
| `engine\whisper.dll` | 483,840 | 进程内常驻模型 |
| `engine\ggml.dll` | 67,584 | ggml 基础 |
| `engine\ggml-cpu.dll` | 603,648 | CPU 后端 |
| `engine\ggml-base.dll` | 528,384 | ggml 依赖 |
| `engine\vad-speech-segments.exe` | 371,200 | 语音存在检测 |
| `engine\MSVCP140.dll` 等 4 个 | 937,352 | VC++ 运行库，实际导入依赖 |
| `models\ggml-small.bin` | 487,601,967 | 默认识别模型 |
| `models\ggml-silero-v5.1.2.bin` | 885,098 | 默认 VAD 模型 |
| `启动 Listen.cmd`、`README.md`、`VERSION.txt`、`manifest.json` | — | 启动器、使用说明、版本、哈希清单 |

依赖不是整目录照搬。用 PE 导入表逐个核实（`.artifacts\issue-11\offline\pe-imports.txt`）：

- `whisper-cli.exe` → `whisper.dll`、`ggml.dll`、VC++ 运行库；
- `whisper.dll` → `ggml.dll`、`ggml-base.dll`、VC++ 运行库；
- `ggml.dll` → `ggml-cpu.dll`、`ggml-base.dll`；
- `vad-speech-segments.exe` → `whisper.dll`、`ggml.dll`；
- `ggml-cpu.dll` → `ggml-base.dll`、`VCOMP140.DLL`。

因此只打包上述 10 个引擎文件（含 4 个 VC++ 运行库 DLL），**没有**打包 `SDL2.dll`、`whisper-server.exe`、`whisper-stream.exe`、`whisper-talk-llama.exe` 等无关示例/服务程序。`listen.exe` 只导入系统 DLL、`libunwind.dll` 和 UCRT。

模型准备：包内 `README.md` 给出 `ggml-small.bin` 与 `ggml-silero-v5.1.2.bin` 的来源链接、字节数和 SHA-256；不包含模型时 `models\README.txt` 指向同一说明。`启动 Listen.cmd` 在缺少 `listen.exe`、`libunwind.dll`、引擎或模型时显示具体缺失项并暂停，不静默退出。

## 包内启动和缺失反馈

| 场景 | 实际结果 | 证据 |
| --- | --- | --- |
| 双击启动器 | `启动 Listen.cmd` 启动包内 `listen.exe`，`phase=Ready`，常驻模型 backend，`preparation_ms=579`，进程 ID 44468；验收后正常 `--control quit` | `accept-launcher\launcher-evidence.json` |
| 包内直接启动（`--engine/--model/--vad-model` 指向包内） | 观察 `Preparing`→`Ready`；`start_to_state_ms=1096`，backend 写明 CPU/常驻模型，`--control quit` 成功 | `package\verify-models-final.json` |
| 无模型包 | `phase=Failure`，detail 写出缺失的 `models\ggml-small.bin` 路径；不进入 Ready，不启动采集 | `package\verify-nomodels-final.json` |
| 缺少 `libunwind.dll` | 启动器在启动应用前明确提示缺失并暂停；不出现“看起来已启动”的假象 | `feedback\feedback-evidence.json`；启动器输出 `The required runtime library libunwind.dll is missing next to listen.exe.`，退出码 1 |
| 缺少引擎相邻 DLL | `listen.exe` 启动为 `Failure`，写明 backend 不可用/检查 adjacent DLLs，不打开麦克风、不进入 Capturing | `feedback\feedback-evidence.json`；`Local whisper.cpp backend is unavailable (exit code: 0xc0000135); check its adjacent DLLs: ...\whisper-cli.exe` |

## 构建与自动测试

在本工作树实际执行：

```powershell
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Build
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task BuildHarness
```

结果：`capture_to_copy` 27 passed / 1 ignored（需操作者焦点编辑区）、`device_routing` 4 passed、`latency_and_preparation` 4 passed、`long_sessions` 9 passed、`local_quality` 2 ignored（需显式模型/语料），合计 **44 passed / 3 ignored**；`CheckFormat` 通过；release 构建和 `long_session.exe` 构建通过。显式本地模型的 `-Task Quality` 2 项沿用 #10 的通过记录（本任务用包内固定语料真实重跑代替）。

## 8 条验收条件对应

| # | 验收条件 | 本包证据 |
| --- | --- | --- |
| 1 | 可运行包、模型准备、缺失反馈；不需要开发脚本或独立推理命令 | 交付目录、`启动 Listen.cmd`、包内 README、manifest；launcher smoke；无模型/缺 libunwind/缺引擎 DLL 反馈；`listen.exe` 内部启动引擎 |
| 2 | 模型就绪后（断网）从启动到目标交付真实流程，识别本地 | 包内真实采集/识别/交付；PE 导入无网络；运行期模块无网络；**提权防火墙封禁三个包内二进制后重跑完整流程通过**（文末）；此前 #9 防火墙证据引用 |
| 3 | 真实耳机和桌面/网页编辑区集成；记录通过和平台限制；不要求应用/来源选择 | 真实 WH-1000XM4 端点原生 EDIT 流程；Chrome textarea/contenteditable 流程；状态写明端点、no microphone、不要求选择应用；限制章节 |
| 4 | 原文/采集中新增编辑保留、完整追加、无自动发送；目标失效/只读/超长/写入未确认备用输出 | 首次流程保留两份原文 + 采集中输入 + 完整追加一次；邻居/其他窗口不变；发送 0/Enter 0；只读、容量 40（39/54 截断）、目标销毁、silent 写入未确认回退 |
| 5 | 失败重试、成功清理、无自动历史、设备中断、长时间和资源受压 | 假引擎失败/同 WAV 重试/修复后成功/清理；设备真实切换中断并还原；600 秒长会话；受限 spool/延迟压力 |
| 6 | 汇总首次/后续延迟、长会话收尾、资源、真实识别质量；样例时长不是上限 | 延迟表、600 秒表、资源表、#9 固定语料本包重跑表；明确不是 SLA/上限 |
| 7 | 沿已确认使用入口，可复核；不用技术验证程序代替交付应用整体验收 | 全部流程使用包内 `listen.exe`；真实全局快捷键、`--control start/stop/retry/cleanup`、`启动 Listen.cmd`；真实 WASAPI/焦点/模型；驱动和 JSON 原始证据 |
| 8 | 发布说明保持父规格范围，明确限制，不宣称任意控件/任意会议零错误 | `docs\issue-11-release-notes.md`；限制章节与“不宣称”章节 |

## 原生桌面完整流程

驱动 `.artifacts\issue-11\accept\accept-core.ps1`（包内 `listen.exe`，runtime 在包外 `.artifacts`，避免污染包）：真实焦点选定任务自有原生多行 EDIT；开始/停止用真实 `SendInput` **Ctrl+Alt+R**（每次 6 个事件）；`System.Media.SoundPlayer` 在默认多媒体端点播放已知语音；采集中用真实 `SendInput` Unicode 输入新增编辑。

| 场景 | 开始时草稿/采集中新增 | 结果 | 目标最终内容 | 停止→结果（内部/外部） | 发送/Enter |
| --- | --- | --- | --- | --- | --- |
| 首次原生采集 | `original draft: Rust 123.\r\nsecond line draft.` + `采集中新增：English 456。` | `Result`；转写 28 字：`今天測試耳機音頻。請保留這段發言,由用戶檢查並手動發送。` | 两份原文 + 采集中新增 + `\r\n` + 完整转写，只追加一次 | 6674 ms / 6.804 s | 0 / 0 |
| 后续原生采集 | 保留上一次目标全文 | `Result`；同一段转写 | 旧全文 + `\r\n` + 完整转写，只追加一次 | 6652 ms / 6.758 s | 0 / 0 |
| 无有效目标开始 | 焦点在“普通按钮” | `Result`；`No supported editable target...` | 目标、邻居、另一窗口均逐字不变 | —（本次未记录） | 0 / 0 |
| 原目标采集中销毁 | 焦点在目标 | `Result`；`The original target was destroyed... No text was written...` | 邻居不变；完整结果在备用文本框 | —（本次未记录） | 0 / 0 |

- 观察到的完整状态序列：`Preparing, Ready, Capturing, Processing, Result` 重复 4 次；`Capturing` 状态写明 `default multimedia render endpoint '耳机 (WH-1000XM4)' (ID)`、`no microphone is used`、通信端点/其他设备在范围外。
- 成功路径剩余临时 WAV 为 0；备用文本框与 `current-result.txt` 逐字一致；重复 `--control stop` 不产生第二份追加。
- `automatic_send_count=0`、`enter_key_count=0`；完成追加后没有切换前台或抢焦点。
- 证据：`accept-core\core-evidence.json`、`first-result.png`、`subsequent-result.png`、`no-target-fallback.png`、`destroyed-target-fallback.png`。

## 网页编辑区完整流程

驱动 `.artifacts\issue-11\accept\accept-web.ps1`：包内 `listen.exe` + 真实 WASAPI + 真实 small/Silero；Chrome 独立配置打开 `app\listen\tests\fixtures\generic-editors.html`；真实 UIA 焦点选定目标；采集中修改草稿，再把焦点切到非目标；`--control start/stop`。

| 目标 | 结果 | 目标最终内容 | 非目标 | Enter/Submit | `input` 事件 | 观察 |
| --- | --- | --- | --- | --- | --- | --- |
| Chrome `<textarea id=plain>` | `Result`；完整追加 | 77 字，含原草稿、采集中编辑、完整转写 | 逐字不变；焦点留在非目标 | 0 / 0 | 2（操作者编辑 + 工具追加，平台正常） | 可写 Value 模式；按普通编辑区处理 |
| Chrome `contenteditable` | `Result`；完整追加 | 78 字，含原草稿、采集中编辑、完整转写 | 逐字不变；焦点留在非目标 | 0 / 0 | **0** | `SetValue` 不派发 `input`/`change`，富文本结构被拍平 |

网页流程的转写均为 28 字 `今天測試耳機音頻。請保留這段發言,由用戶檢查並手動發送。`；目标只追加一次，成功路径无临时 WAV。证据：`accept-web\web-evidence.json`、`web-plain.png`、`web-rich.png`。

限制：浏览器 `contenteditable` 写入不产生页面事件、结构被拍平；受控 React/Vue 编辑器可能重绘覆盖。工具只能确认写入当时可访问值，不宣称任意网页控件无条件可写。

## 备用输出与写入异常

驱动 `.artifacts\issue-11\accept\accept-fallbacks.ps1`，同一包内应用、真实音频/模型、真实焦点：

| 场景 | 目标实际内容 | 用户可见 detail（节选） | 停止→结果 | 发送/Enter |
| --- | --- | --- | --- | --- |
| 采集中改为只读 | 仍为 `readonly draft: Rust 123.`，未写入 | `The original target still holds its previous content, so nothing was written to it (The original target is read-only).` | 6681 ms | 0 / 0 |
| 宿主容量 40 字符，意图 54 字符 | 只保存 39/54 字符前缀 | `...holds only 39 of 54 characters... truncated; this is not a complete delivery.` | 6727 ms | 0 / 0 |
| 提供程序静默丢弃 WM_SETTEXT，停止时目标已不在前台 | 仍为原草稿，未写入；未发送任何粘贴按键 | `...no longer the foreground window, so the fallback writer was not used` + `nothing was written` | 8676 ms（含 2 秒 UIA 确认） | 0 / 0 |

- 三种情况完整结果都在备用文本框，且与 `current-result.txt` 一致；目标、邻居/非目标逐字不变；无临时 WAV；不自动重试、不回滚。
- **粘贴降级的正向路径**（受控编辑器、目标仍在前台、`Ctrl+A/V/C` 追加并读回确认）沿用 [Issue #4](issue-4-verification.md) 的 DeepSeek Harness 实测与 [Issue #5](issue-5-verification.md) 合并后重跑；本任务包内验证了“目标不在前台时不发按键、完整回退”的负向门禁。没有把原生 EDIT 夹具上的一次粘贴尝试当作正向验收。
- 容量场景由任务自有原生宿主在 `WM_SETTEXT` 上实施上限（真实的控件/UIA 往返，宿主策略），不是某个第三方应用字段的逐个验收。

## 识别失败、重试与清理

驱动 `.artifacts\issue-11\accept\accept-retry.ps1`，包内 `listen.exe` 指定 `--backend cli`（强制走 CLI，避免常驻模型掩盖假引擎故障），真实 WASAPI、真实 VAD、真实焦点原生 EDIT：

| 步骤 | 实际观察 |
| --- | --- |
| 假 `whisper-cli.exe`（`--help` 成功、识别退出码 2） | `Failure`；`Local whisper.cpp recognition failed (exit code: 2)`；保留 `...\segment-00000000\playback.wav`；`retry_available=true`；目标/当前结果不变 |
| 再次 `--control retry`（仍坏） | 仍是 `Failure`；detail `Local retry failed: ... exit code: 2`；同一 WAV 路径仍保留；目标不变 |
| 只把真实 `whisper-cli.exe` 覆盖回同一路径，再 retry | `Result`，不需要重放；完整转写 `今天測試耳機音頻。請保留這段發言,由用戶檢查並手動發送。`；目标恰好追加一次；备用框/结果文件一致；临时 WAV 0；`retry_available=false` |
| 再制造一次失败，`--control cleanup` | detail 含 `deleted at your request`；`retry_available=false`；WAV 0；当前结果和目标不变 |
| 清理后再次 retry | 仍 `Failure`，`No retained audio`；不新增 WAV、不改变当前结果 |

运行时最终只有 `current-result.txt`、`latency.jsonl`、`status.txt`；没有历史/归档文件。证据：`accept-retry\retry-evidence.json`。

## 设备中断与恢复

驱动 `.artifacts\issue-11\accept\accept-device.ps1`：真实默认多媒体端点 `耳机 (WH-1000XM4)` 上开始采集并播放；约 3 秒后通过 `IPolicyConfig` 把默认多媒体切换到 `扬声器 (Realtek(R) Audio)`。

- 观察到 `Capturing` 写明原端点 ID/名称；切换后进入 `Failure`，detail 同时包含原端点/新端点名称与 ID、`changed during capture`、`incomplete`、`This is not a completed transcription`。
- 已识别的 `partial-result.txt` 保留并进入备用文本框；目标不变；预置的“上一次完整结果”未被覆盖；`retry_available=false`（设备中断不是识别失败，不提供错误的重试入口）。
- `finally` 中把多媒体/通信端点还原并读回：`restore_verified=true`，多媒体/通信均回到 `耳机 (WH-1000XM4)`。证据：`accept-device\device-evidence.json`、`restore.json`、`original-endpoints.json`。

未实机拔插/禁用设备；`AUDCLNT_E_DEVICE_INVALIDATED` 的移除分支由 #8 受控测试覆盖。本场景是真实默认端点切换。

## 离线与网络证据

- 包内 `listen.exe`、`engine\whisper-cli.exe`、`engine\vad-speech-segments.exe` 的 PE 导入表没有 `ws2_32`、`winhttp`、`wininet`、`urlmon`、`dnsapi`、`mswsock`，也没有 socket/connect/http 导入（`offline\pe-imports.txt`）。
- 真实采集期间轮询包内 `listen.exe` 的已加载模块，记录在 `offline\module-evidence.json`：共 36 个模块（含 `whisper.dll`、`ggml*.dll`、`libunwind.dll` 和 VC++ 运行库），模块中没有上述网络 DLL。
- **提权防火墙临时封禁 `listen.exe`、`whisper-cli.exe`、`vad-speech-segments.exe` 后重跑包内流程已完成**，见文末“包内二进制的系统级强制禁网复验（2026-11-12）”。PE 导入/运行期模块与本条是独立证据，不能互相替代。

## 600 秒长会话与资源

使用现有 `scripts\measure-long-session.ps1`，它固定运行 `app\listen\target\release\examples\long_session.exe`（同一源码的 harness，用 `Tape` 替换播放设备，**不打开音频设备、不操作桌面**），不是包内 `listen.exe`；输入为 #6 的 96.04 秒固定样本循环（SHA-256 `9F710689...1E3F`），静默墙钟回放 600 秒；引擎/模型/VAD 全部指向**包内**文件；生产分段、真实 VAD/Whisper、同一 `Application::dispatch`。输出 `.artifacts\issue-11\long\600\`。因此这里的 600 秒/资源数字是“harness + 包内引擎/模型”的测量，**不能当作包内交付应用在真实耳机上的 600 秒应用入口验收**，也不满足 T10 AC7 的严格字面要求。

| 观测 | 结果 |
| --- | --- |
| 最终阶段 | `Result`（`terminal: true`） |
| 采集时长 | 600.0 秒（请求 600 秒，无截断、无自动停止） |
| 识别窗口 | 22 |
| 当前结果 | 1433 字（本 harness 没有选目标，走备用文本框） |
| 准备 | 693 ms |
| 停止→结果 | **6921 ms**（外部 6.937 s） |
| 收尾 finalize | 5 ms |
| 识别 | 6914 ms |
| 最后窗口排队/推理 | 0 ms / 6911 ms |
| 保存 / 交付 | 0 ms / 0 ms（无目标） |
| 峰值积压窗口 | **1** |
| 峰值 spool | 1,286,488 字节（预算 9,600,440 字节） |
| 结束 spool | 0 字节 |
| 失败/保留音频 | 无；`retained_audio` 为空 |

资源（`resources.ndjson`，每 3 秒采样本进程及直接子进程，共 186 个样本）：

| 指标 | 峰值 | 平均 |
| --- | ---: | ---: |
| 进程组合工作集 | **795.1 MB** | 547.5 MB |
| 进程组合私有字节 | **1276.2 MB** | 666.6 MB |
| 直接子进程 | `conhost`（host 路径没有 `whisper-cli` 子进程） | — |
| GPU（全设备，非本进程归因） | 13% / 719 MiB / 4096 MiB | — |

- 输入循环为固定样本，不是真实会议；600 秒不是 60 分钟，也不构成产品时长上限。60 分钟结论引用 #6 的静默回放（停止→结果 9.55 秒、129 窗口、峰值工作集 789 MB/私有 1268 MB），本任务没有重复跑 60 分钟。
- #10 的同一 30 秒窗口对照为 host 9050 ms、cli 9975 ms；本轮包内 600 秒收尾与之一致量级。

## 资源受压

使用同一长会话测量脚本主动制造受压：限制 spool 预算并把每窗识别延迟到超过窗口产生速度，使待处理窗口/spool 持续增长。实际命令和结果：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File E:\luyin\.worktrees\issue-11\scripts\measure-long-session.ps1 `
  -InputWav E:\luyin\.artifacts\issue-6-smoke\loop-96s.wav `
  -OutputDirectory E:\luyin\.artifacts\issue-11\pressure\run `
  -ToolsRoot E:\luyin\.tools -Seconds 90 -Backend host -DelayMs 25000 -SpoolBytes 1600000 `
  -Engine E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\engine\whisper-cli.exe `
  -Model E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\models\ggml-small.bin `
  -VadModel E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\models\ggml-silero-v5.1.2.bin
```

命令按预期以非零码退出（脚本拒绝把 `Failure` 当成完整结果）；`exit.json` 和 `flow.ndjson` 仍保留完整的失败记录。

| 观测 | 结果 |
| --- | --- |
| 终止阶段 | `Failure` |
| detail | `Playback storage budget exhausted; capture interrupted, received audio and checkpoint retained.`（完整 detail 见 `pressure\run\flow.ndjson`） |
| 已接收/识别窗口 | 音频 48.0 秒；识别 1 个窗口；峰值积压 1；spool 达到 1,600,000 B 预算 |
| 保留音频/检查点 | `segment-00000001\playback.wav`（639,956 B）+ `partial-result.txt`（249 B） |
| 是否静默丢数据 | **否**：明确 `Failure` 并标注不完整，保留音频/检查点，未覆盖上一次完整结果 |
| 资源峰值 | 峰值工作集 780.8 MB / 私有 1263.3 MB；20 个资源样本；停止→结果 16,057 ms |

这验证了边界条件下显式中断并保留可恢复数据，而不是把超预算/积压伪装成完整结果。

## 首次/后续与端到端延迟汇总

| 指标 | 本包首次 | 本包后续 | #10 同分支记录（对照） |
| --- | ---: | ---: | ---: |
| 启动到 Ready（模型准备） | 618 ms（`preparation_ms`；进程启动/轮询后 1.15 s） | 不重复准备 | 596 ms |
| 停止→结果（应用内部） | **6674 ms** | **6652 ms** | host 7246 / 7368 ms；cli 7714 / 8287 ms |
| 外部 stopwatch 停止→结果 | 6.804 s | 6.758 s | — |
| finalize | 9 ms | 8 ms | 10–29 ms |
| recognition | 6657 ms | 6638 ms | — |
| 最后窗口推理 | 6650 ms | 6631 ms | 7228 / 7342 ms |
| 保存 / 交付 | 0 / 6 ms | 0 / 5 ms | 保存 0；交付 7–14 ms |
| 识别窗口 | 1 | 1 | 1 |

- 首次与后续差异在测量噪声内；首次没有额外 per-window 冷启动，主要一次性代价是 0.62 秒准备（准备期间 Start 被拒绝）。
- 这些是 8.3 秒固定语音、原生 EDIT、本机负载下的观测；不是延迟 SLA。
- #10 的 CLI 对照和 30 秒/96 秒窗口对照说明每窗模型加载被消除，但 30 秒以上窗口仍有约 0.76–0.93 秒节省；本包的数字与该方向一致。

## 真实识别质量（本包重跑）

同一包内 `listen.exe`、真实 WASAPI、small + Silero、CPU 6 线程、固定语料（`quality-corpus\manifest.json`）。没有播放/识别替身。

| 样本 | 阶段 | 字数 | 输出/主要错误 |
| --- | --- | ---: | --- |
| mandarin | Result | 41 | `這段語音只在本機處理,不會上傳到雲端。請保留完整的發言內容,由我檢查以後手動發送。`；繁体/标点变化 |
| mixed-technical | Result | 72 | `我们用"Rust"实现音频采集 通过"Waze up high loop back"获取声音模型在本纪运行 使用"CPU"和"Cuda"进行对照`；WASAPI/loopback、本机→本纪、CUDA 大小写 |
| english-numbers | Result | 109 | `The API uses Rust, Windows, Whisper, CPU, and GPU.Version 1.8.2.Port 8080.Retry 3 times, then wait 2 seconds.`；数字保留，句间空格/标点合并 |
| homophones | Result | 31 | `请确认本机的音频设置。本机的项目已经完成。不要把本机写成本机。`；“本季”仍误写“本机” |
| multiline | Result | 56 | `第一段,保留原來的草稿,不要自動發送。第二段,覆蓋 API 回應。第三段,端口是 8080,重視三次,等待兩秒。`；英文句被译写，重试→重視 |
| silence | NoSpeech | — | 无追加，旧结果/目标不变，临时音频清理 |
| instrumental | NoSpeech | — | 同上 |
| notification | NoSpeech | — | 同上 |

- 与 #9 基线一致；small 保留了更多英文数字/术语，但没有修复同音和语言切换。#10 的 host/CLI 对照在同一模型/音频上逐字一致，说明本包常驻模型路径没有引入新的识别差异。
- 这些是合成固定语料，不是真实会议；不构成准确率承诺。真实会议的噪声、口音、多人重叠和分段边界未评估。

## 无自动历史

成功路径的 runtime 只有：

```text
current-result.txt
latency.jsonl
status.txt
```

没有 `history`/`archive`/`meeting`/`transcript-<date>` 文件；成功/无语音后临时 WAV 为 0；失败时只为本次保留音频和检查点。`status.txt` 不包含转写正文。

## 已知限制与未覆盖

- **断网防火墙复验**：已完成（文末）；三个包内二进制在系统级出站封禁下跑通完整流程。限制是短样本、单场景，不是长时间浸泡或物理断网。
- **60 分钟真实耳机**：没有重复跑 60 分钟；600 秒是静默回放+包内引擎，60 分钟结论引用 #6 的历史记录。真实耳机连续 60 分钟未测。
- **真实会议软件并发**：没有加入真实会议软件；600 秒在后台桌面、无其他会议负载下测得。真实视频/网络/共享屏幕负载未覆盖。
- **GPU**：本机无 CUDA toolkit，`use_gpu=false`；没有 GPU 加速交付。
- **粘贴降级正向路径**：包内只验证负向门禁；正向 Harness 流程引用 #4/#5，未在本包上重跑（避免占用用户正在使用的 Harness 输入框）。
- **所有编辑器**：只验证了原生多行 EDIT 和 Chrome 普通 textarea/contenteditable；没有逐个验证第三方 UIA 提供程序、浏览器扩展、Codex 桌面端、单行 EDIT/RichEdit。
- **设备**：真实默认端点切换已测；物理拔插/禁用、蓝牙免提/立体声切换、会议独占模式未测。
- **容量截断**：由任务自有原生宿主在 `WM_SETTEXT` 实施上限；不是第三方应用 `maxlength` 的逐个验收。
- **UIA 非事务**：读取/检查/写入之间的并发编辑仍可能发生；失败后只诊断读回，不重试/回滚。
- **识别质量**：同音、繁体/标点、中英切换、专有名词、数字仍需人工检查；不宣称任意会议逐字零错误。
- **延迟**：所有数字为单机观测，不构成 SLA；600 秒不是产品上限。
- **未覆盖**：其他 Windows 版本、USB/HDMI/虚拟音频设备、所有第三方输入法/编辑器、浏览器扩展输入区、真实会议录音分段边界。

## 复现

```powershell
cd E:\luyin\.worktrees\issue-11
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task BuildHarness
.\scripts\package-listen.ps1 -ToolsRoot E:\luyin\.tools -IncludeModels
.\scripts\verify-listen-package.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0

# 包内真实验收（会播放声音；只操作本任务窗口/进程）
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-launcher.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-launcher
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-core.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-core
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-fallbacks.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-fallbacks
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-retry.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-retry
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-web.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-web
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-quality.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-quality
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-device.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-device
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-11\accept\accept-offline.ps1 -PackageRoot E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0 -Output E:\luyin\.artifacts\issue-11\accept-offline

# 600 秒；输出目录必须全新
powershell.exe -NoProfile -ExecutionPolicy Bypass -File E:\luyin\.worktrees\issue-11\scripts\measure-long-session.ps1 `
  -InputWav E:\luyin\.artifacts\issue-6-smoke\loop-96s.wav `
  -OutputDirectory E:\luyin\.artifacts\issue-11\long\600 `
  -ToolsRoot E:\luyin\.tools -Seconds 600 -Backend host `
  -Engine E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\engine\whisper-cli.exe `
  -Model E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\models\ggml-small.bin `
  -VadModel E:\luyin\.worktrees\issue-11\dist\Listen-0.1.0\models\ggml-silero-v5.1.2.bin

# 压力场景（输出目录必须全新；具体参数见下方证据）
```

## 证据索引

全部位于 `E:\luyin\.artifacts\issue-11\`（Git 忽略）：

| 路径 | 内容 |
| --- | --- |
| `package\verify-*.json` | 含模型/无模型包的文件/哈希/启动状态验证 |
| `accept-launcher\launcher-evidence.json` | 双击启动器到 Ready |
| `accept-core\core-evidence.json` + PNG | 原生首次/后续/无目标/目标销毁、状态序列、延迟 |
| `accept-fallbacks\fallback-evidence.json` | 只读、容量截断、写入未确认回退 |
| `accept-retry\retry-evidence.json` | 失败/重试/修复成功/清理 |
| `accept-web\web-evidence.json` + PNG | Chrome textarea/contenteditable |
| `accept-device\device-evidence.json`、`restore.json` | 默认端点切换与还原 |
| `accept-quality\quality-evidence.json` | 固定语料本包重跑 |
| `accept-offline\module-evidence.json`、`offline\pe-imports.txt` | 运行期模块/PE 导入 |
| `long\600\` | 600 秒 flow/resources/metadata/exit |
| `pressure\` | 受压场景 flow/resources/exit |
| `feedback\` | 缺 libunwind / 缺引擎 DLL 的反馈 |

本机截图只用于验收，不作为产品代码或会议历史提交。

## 打包产物的本地运行与网络能力复核（2026-11-12，主会话补做）

在没有启用防火墙的条件下，把**打包目录本身**（`dist\Listen-0.1.0`）跑了一遍完整真实流程：

- 从包内启动：`Ready` 用时 0.838–0.886 秒，`backend` 为常驻模型，端点 `耳机 (WH-1000XM4)`；
- 真实焦点选定的原生多行 EDIT 目标，真实 WASAPI 采集 8.08 秒已知语音，停止→结果 **7.16 / 7.30 秒**；
- 目标最终内容恰为"原草稿 + 换行 + 转写"一份，发送计数 0、Enter 计数 0；
- 证据：`.artifacts\issue-11-offline\offline-package-evidence.json`。

网络能力复核（打包产物）：

- PE 导入：`listen.exe`、`engine\whisper-cli.exe`、`engine\vad-speech-segments.exe`、`engine\whisper.dll` 都**没有** `ws2_32`/`winhttp`/`wininet`/`urlmon`/`dnsapi`/`mswsock` 名称；
- 运行期：同一次打包运行中，三个进程共加载 **57 个模块，没有网络模块**。

**当时未完成（如实记录）**：该轮尝试用提权防火墙临时封禁包内三个二进制，但 UAC 提权被取消（`The operation was canceled by the user`），所以当时**打包产物没有在系统级强制禁网下运行过**。**随后已补做（2026-11-12 11:02）**：用户批准一次 UAC 后，同一步骤重跑通过，规则已删除、残留 0；详见文末“包内二进制的系统级强制禁网复验（2026-11-12）”。
## 最终整体审查后的修复与回归（2026-11-12）

本节的数字来自本工作树 `implement/final-review-fixes`（基线 `041019d`，修复提交 `87a75cf`）的离线锁定 release 构建和提交后重新打包的 `dist\Listen-0.1.0\`（含模型；`manifest.json` 记录 `git_commit=87a75cf…`、`git_dirty_at_package_time=false`）。证据在 `.artifacts/final-review-fixes\`。

### 自动测试

- `capture_to_copy` **27 passed / 1 ignored**；`long_sessions` **11 passed**（新增 speech→silence→speech 尾部保留、已识别窗口后失败的范围说明）；`device_routing` 4、`latency_and_preparation` 4、`model_readiness` 2、lib unit tests 16；合计 **64 passed / 3 ignored**。`-Task CheckFormat` 通过；`-Task Build` 离线锁定 release 构建通过；提交后 `package-listen.ps1 -IncludeModels` + `verify-listen-package.ps1` 通过（`Preparing`→`Ready`，`start_to_state_ms=1839`，`control_quit_ok=true`）。
- RED/GREEN：SP-01 的旧 `paste_output` 逻辑输出 `Append { text: "第一行\n第二行\n今天轉寫" }`（整段原草稿重复），修复后输出 `Append { text: "今天轉寫" }`；SP-03 旧代码输出 `开始第三段`，修复后 `开始尾部第三段`；SP-05 旧 `prepare()` 对损坏 host 返回 `Ok`、CLI backend 返回 `None`，修复后都返回明确 `Err`/Failure。原始日志见 `.artifacts/final-review-fixes/red-sp01.log`、`red-sp03.log`、`red-sp05.log` 与对应 GREEN 运行。

### 真实桌面回归（包内 `listen.exe`，真实 WASAPI/模型/焦点）

| 场景 | 结果 | 证据 |
| --- | --- | --- |
| 原生 EDIT 首次/后续追加 | PASS；停止→结果 6.746 / 6.784 s；原文+采集中编辑+转写只追加一次；无目标/目标销毁均完整回退、不误写 | `accept-core\core-evidence.json`、`accept-core.log` |
| 只读、容量 40 截断、静默写入且目标失焦 | PASS；只读不写；容量只保存 39/51 字符并明确“不是完整交付”；失焦时不发粘贴按键，完整回退 | `accept-fallbacks\fallback-evidence.json`、`accept-fallbacks.log` |
| 识别失败→重试仍失败→换回真实 CLI→重试成功→清理→无效重试 | PASS；失败的同一 WAV 保留并可重试；修复后不重放成功；目标只追加一次；清理后不调用识别 | `accept-retry\retry-evidence.json`、`accept-retry.log`；驱动为适应新的真实就绪检查，fake engine 增加了“零帧 WAV 加载验证”分支，并用 `--backend cli` 强制走被测 CLI |
| DeepSeek Harness 粘贴降级（SP-01/SP-02 端到端） | PASS；已知多行草稿 `SP01-FINAL-REVIEW-DRAFT-7F3A` 在最终输入框中出现 **1** 次，转写出现 **1** 次，最终内容逐字等于草稿+换行+转写；`phase=Result`、`detail` 为完整追加 | `accept-harness\harness-draft-evidence.json`、`accept-harness.log`。Harness 的 UIA 可访问值为 LF，不触发 CRLF 分支；CRLF 分支由 `target::tests` 的真实 RED/GREEN 覆盖 |
| 粘贴序列中途焦点离开（SP-06） | PASS（安全中止）；受控原生 EDIT 在 `Ctrl+A/C` 后把焦点移到同进程窗体，工具检测到后 **未发送 `Ctrl+V`**（`v_keys=0`、`paste_calls=0`），目标不变、完整结果进入备用文本框 | `accept-paste-crlf\paste-crlf-evidence.json`、`accept-paste-crlf.log` |
| `--control` 失败不弹模态 | 未运行实例时 `listen.exe --control stop` 在 **66 ms** 内以退出码 1 结束，stderr 为 `Listen control failed: Listen is not running`，无对话框、无残留进程 | `.artifacts/final-review-fixes/control-failure.txt` |

### 就绪与模型加载（SP-05/S-01）

- 损坏模型（保留 `ggml` magic，内容随机）：真实 `listen.exe` 进入 `Failure`，detail 为 `The configured local model could not be loaded by the whisper.cpp CLI: ... exit code: 0xc0000409 ... this is not a claim of readiness`；进程未崩溃，无虚假 `Ready`。
- host-only 失败（另一构建的 `whisper.dll`，SHA-256 不符）+ CLI 可加载：`Ready`，backend 明确写 `resident local model is unavailable ... CLI was verified ...`，`timing_preparation_ms=1117`。
- 默认 host 正常路径：`timing_preparation_ms=1453`（外部观察 1711 ms）。这比本记录早先的 570–712 ms 增加了一次性 CLI 模型加载验证；识别路径未变，但**旧 0.62 s 准备时间不再适用于当前构建**。
- `whisper.dll` 在 `LoadLibrary`/任何导出调用前按 SHA-256 固定校验；默认参数改为 `whisper_full_default_params_by_ref` 并校验 40 余个字段。单测 `an_unknown_whisper_dll_is_rejected_before_any_call`、`sha256_of_a_known_payload_is_the_standard_digest`、`the_layout_check_is_not_vacuous`。

### 打包清理（S-07）与控制脚本（SP-12）

- `package-listen.ps1` 删除目标前要求目录内有 `Listen` 的 `manifest.json` 或 `.listen-package` 标记；对含用户文件的 `Listen-notes` 目录拒绝删除并保留文件，对上一次生成的 Listen 包可重复重建。证据：`s07-hostile.log`、`s07-owned1.log`、`s07-owned2.log`。
- `verify-listen-package.ps1` 的 control 进程在超时后强制结束；`run-listen.ps1 -Control` 增加了 15 秒上限、stderr 输出和 `retry|cleanup` 参数；`--control` 失败不再弹出 `MessageBoxW`。

### 仍未满足/未重跑

- **SP-08 / T10 AC2**：已于 2026-11-12 补做通过——三个包内二进制在系统级出站封禁下跑通完整流程（文末）；本条不再是缺口。
- **SP-09 / T10 AC7**：600 秒/资源数字来自 `examples\long_session.exe` harness + 包内引擎/模型，不是包内 `listen.exe` 的界面流程；本记录和发布说明已按此改写，但 T10 AC7 的严格字面要求仍未被 harness 测量满足。
- **SP-11 / T9 AC5**：真实会议软件并发流畅度仍未验证；本轮未新增会议软件负载。
- **准备时间**：新增的真实模型加载验证使默认 host 准备从约 0.57–0.71 s 增至约 1.45 s；8 样本/600 秒的停止→结果数字是准备改动前构建的记录，识别路径未变，但若要引用当前构建的完整延迟，应重新跑一次延迟验收。

## 最终交付包（审查修复后重建）

审查修复合并后（`cab4ef5`），用 `scripts\package-listen.ps1 -IncludeModels -SkipBuild` 从干净工作树重建了交付包，并用 `scripts\verify-listen-package.ps1` 重新校验：17 个文件、493,311,730 字节（不含 manifest），`Preparing → Ready`，`start_to_state_ms = 2027`，`control_quit_ok = true`，哈希校验 17/17。证据：`.artifacts\issue-11\package\verify-20261112-033626.json`。

## 包内二进制的系统级强制禁网复验（2026-11-12 11:02）

用 `.artifacts\offline-firewall-helper-final-package.ps1` 提权创建三条 `Outbound / Block / Program / 所有配置` 的临时规则，封禁 `listen.exe`、`engine\whisper-cli.exe`、`engine\vad-speech-segments.exe`；规则状态写入 `rules-ready.json` 后，用 `.artifacts\issue-11-offline-driver\run-packaged-offline.ps1` 从 `dist\Listen-0.1.0` 启动并跑完整流程，随后写 `run-finished.signal`，辅助进程在 `finally` 中删除规则并写 `rules-removed.json`。

| 观测 | 结果 |
| --- | --- |
| 临时规则 | 3 条，`Outbound` / `Block` / `Program` / `Profile=Any` / `Enabled=True`（`rules-ready.json`，03:02:24Z） |
| 启动 | 直接运行包内 `listen.exe`（包内 engine/models；runtime 在包外），`Ready` **1.347 s** |
| 后端 | 进程内常驻模型（加载 `whisper.dll`）；端点 `耳机 (WH-1000XM4)` |
| 采集 | 默认多媒体端点播放 11.320 s 已知语音；`peak=0.92730`、`discontinuities=3` |
| 停止→结果 | **7.138 s**（内部 `stop_to_result_ms=6996`，识别 6957 ms、1 个窗口、`delivery_ms=16`） |
| 交付 | 原生多行 EDIT：原草稿保留，末尾追加转写恰一份；`target_ok=true`、`send=0`、`enter=0` |
| 清理 | `cleanup_complete=true`、`still_present=[]`；事后查询残留 **0** 条（03:04:54Z） |

证据：`.artifacts\offline-firewall-final\rules-ready.json`、`.artifacts\issue-11-offline-final\offline-package-evidence.json`、`.artifacts\offline-firewall-final\rules-removed.json`。

可复现性说明：这次为最终包重跑驱动时，只改了 QA 脚本的两处健壮性问题——焦点改为先激活测试窗体再真实点击进入 EDIT（子控件不能直接 `SetForegroundWindow`），以及 `status.txt` / `current-result.txt` 被写入方短暂占用时重试读取。产品代码与包内二进制未改。

替代证据（仍成立，但不替代上述实测）：

- 包内 `listen.exe`、`engine\whisper-cli.exe`、`engine\vad-speech-segments.exe`、`engine\whisper.dll` 都没有任何 `ws2_32`/`mswsock`/`winhttp`/`wininet`/`urlmon`/`dnsapi` 导入；**没有 `ws2_32`/`mswsock` 就无法建立任何出站连接**。
- 打包产物的真实运行中，相关进程共加载 57 个模块，没有网络模块。
- 同一套采集与识别源码在 `9faa9c3` 上做过一次**系统级强制禁网**的完整 8 样本验收并全部通过。

为把本节写进包内 README 与仓库记录，之后用 `-SkipBuild` 重建了一次交付目录；重建前后的四个二进制 SHA-256 相同（`listen.exe` 96FF88F2…D8B7、`whisper-cli.exe` DC164875…14E8、`vad-speech-segments.exe` 92DDB93A…56CF、`whisper.dll` 4155EDB6…61FF），因此上面的实测适用于重建后的包。