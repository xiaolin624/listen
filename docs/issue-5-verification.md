# Issue #5 实现与验证记录

范围：[T4：写入异常、超长结果和重复操作下保全结果](https://github.com/xiaolin624/listen/issues/5)。实现工作树 `E:\luyin\.worktrees\issue-5`，分支 `implement/issue-5-write-failures`，基线为集成分支 `integration/issue-3-native-target` 的 `44f7272`（已包含 #3、#4、#6 的完整流程与原生目标适配器）。本任务没有改动采集、识别、托盘、备用文本框或复制路径。

## 测试边界

沿用已确认的单一业务边界：通过 `Application::dispatch(Control)` 驱动一次开始 → 停止 → 本地转写 → 交付/回退，保留真实临时文件、真实异步完成线程和真实备用结果。只替换播放设备、本地识别引擎和目标编辑区。新增测试继续使用既有的 `TargetSelection` / `Target` 替身扩展其状态（只读、禁用、容量、并发编辑、一次写入计数），没有为 `deliver`、`failed_write`、`NativeEditor` 等内部函数建立测试入口，也没有断言线程数、临时文件命名或私有结构。

## RED → GREEN

5 个新测试一次性加入后先运行 `.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test`。首次结果为 **15 passed / 5 failed**：其中 4 个是真实行为缺口，第 5 个（重复停止/按住快捷键）失败于测试自身的第二次采集断言（把“另一个输入框”的原有草稿当成空框）。修正该断言后重跑为 **16 passed / 4 failed**；第 5 个测试在未改动的旧实现上直接通过，说明“同一段结果不重复输出”的旧行为已经存在。没有虚构 RED。

| 新测试 | 旧实现的实际失败（截取自 panic 输出的 Snapshot） | 行为缺口 |
| --- | --- | --- |
| `read_only_target_during_capture_keeps_the_complete_fallback_without_writing_it` | `detail: "The original target is read-only. The complete result is in the fallback text box. Check the original target before copying; output is not retried automatically."`，断言 `contains("nothing was written")` 失败 | 拒绝写入与“可能已部分写入”共用同一句提示，用户无法判断原框是否被改 |
| `disabled_target_at_start_is_reported_without_writing_any_control` | `detail: "The original target is disabled. The complete result is in the fallback text box. Review and copy it to the intended input."`，断言 `contains("No text was written to any control")` 失败 | 未明确说明任何控件都没有被写入 |
| `capacity_limited_target_is_reported_as_truncated_and_keeps_the_complete_fallback` | `detail: "The complete target output could not be confirmed. The complete result is in the fallback text box. Check the original target before copying; output is not retried automatically."`，断言 `contains("truncated")` 失败 | 截断与普通超时混为一类，无法告知目标只保存了前缀 |
| `concurrent_target_edit_during_output_is_reported_unconfirmed_and_never_reverted` | `detail: "The target draft changed during output; no replacement was attempted. The complete result is in the fallback text box. Check the original target before copying; output is not retried automatically."`，断言 `contains("could not be confirmed")` 与 `contains("may be partially written")` 失败 | 并发变化没有明确标为“未确认、可能部分写入” |
| `repeated_stop_held_hotkey_and_processing_controls_deliver_each_result_once` | 首次失败是测试自身的第二次采集断言写错（把“另一个输入框”的原有草稿当成空框），不是产品行为缺口；修正断言后旧实现直接通过 | 已有一次性交付行为，本任务补充其流程级回归覆盖 |

实现后同一条命令为 **20 passed，0 failed，1 ignored**（`capture_to_copy`）；`long_sessions` 8 passed；`local_quality` 2 ignored（需显式模型和语料）。

RED 失败细节来自 2026-11-11 本机运行，错误文本为旧实现实际输出，未改写。

## 实现

### 应用层：一次写入尝试的诊断结果

`app/listen/src/lib.rs` 的 Stop 完成路径改为：保存当前结果 → 读取目标最新草稿 → 只调用一次 `replace` → 失败后**只再读一次**目标并生成用户可见状态。新增私有函数 `deliver`、`not_delivered`、`failed_write`，不改变公开类型。

失败后的读回分类（均归一化 CRLF/LF）：

| 读回内容 | 用户可见状态（节选） | 含义 |
| --- | --- | --- |
| 无法读回 | `The write to the original target could not be confirmed (...); the target could not be read back (...)` | 写入未确认，提示检查原框 |
| 等于写入前草稿 | `The original target still holds its previous content, so nothing was written to it (...)` | 目标未被改，完整结果在备用文本框 |
| 等于完整目标文本 | `... could not be confirmed (...), but it now contains the complete appended text` | 迟到的完整写入也未确认，不重复追加 |
| 是目标文本的非空真前缀 | `The original target holds only N of M characters ... truncated; this is not a complete delivery` | 容量截断，绝不报告为完整成功 |
| 其他 | `... could not be confirmed (...); the target content changed and may be partially written` | 可能部分写入 |

保存失败、没有可用目标、开始时绑定失败、写入前读草稿失败时，完全不会调用 `replace`，状态为 `... No text was written to any control. The complete result is in the fallback text box. ...`。所有失败分支的 `Delivery` 仍为 `Fallback`（成功才是 `Appended`）；截断与未确认的区分放在用户可见的 `detail`，因为 `Delivery` 表达的是“完整结果在哪里”，而不是目标里的副本是否完整。没有重试、回滚、键盘注入或发送路径；`Target::replace` 的契约注释补充了“失败后允许读取用于状态报告，但不得自动重试或恢复原文”。

### 原生目标：只读/禁用检查与截断识别

`app/listen/src/target.rs` 把原来的 `validate` 拆为：

- `validate_identity`：销毁事件、根窗口身份属性、窗口/线程/进程、UIA runtime ID、祖先归属。**读取**也用它，因此只读或禁用后仍能读回内容做诊断。
- `validate_writable`：在身份检查之后确认控件类型、`IsEnabled`、非密码框、`CurrentIsReadOnly`，并给出具体原因（`The original target is read-only` / `... is disabled` / `... is a password field` / `... is no longer a supported editor`）。

`replace` 仍只在全部检查通过后调用一次 `ValuePattern.SetValue`。写后确认循环新增截断快速判断：当读回值**不等于写入前草稿**、非空、且是目标文本的真前缀，并连续 3 次（20 ms 间隔）保持同一长度时，返回 `The original target accepted only N of M characters; the appended text was truncated`；否则沿用 2 秒确认期限后报“无法确认”。这个稳定前缀条件避免把“provider 仍显示旧草稿”或“异步写入尚未生效”误判成截断。读回轮询只读不写。

## 验收条件对应的证据

| 验收条件 | 自动行为证据（`Application::dispatch` 完整流程） | 真实原生控件证据 |
| --- | --- | --- |
| 目标关闭、被重建、只读或禁用：不向替代控件输出，完整结果进入备用文本框 | `destroyed_original_target_...`（已有）；新增 `read_only_target_...`、`disabled_target_at_start_...`。断言 `delivery == Some(Fallback)`、`detail` 含具体原因与“nothing was written / No text was written to any control”、`result.text` 与 `current-result.txt` 为完整转写、两个编辑区草稿逐字不变、`replace_calls` 分别为 1/0（无重试）。开始后把焦点切到另一输入框，原目标仍不被重新选择。 | 只读场景：采集中对真实多行 EDIT 调用 `EM_SETREADONLY`，停止时状态为 `The original target still holds its previous content, so nothing was written to it (The original target is read-only). ...`；`WM_GETTEXT` 读回目标仍为 `capacity draft: Rust 123.`，邻居与另一窗口逐字不变。 |
| 容量限制导致截断：不得报告为完整成功，备用文本框保留完整转写 | `capacity_limited_target_...`：容量 30、目标文本 60 字符。断言 `delivery == Some(Fallback)`、`detail` **不含** `The complete result was appended`、含 `truncated`、`not a complete delivery`、`only 30 of 60 characters`、`Check the original target`、`not retried automatically`；目标只保存前 30 字符；备用结果和 `current-result.txt` 为完整转写；`replace_calls == 1`。 | 容量场景：真实多行 EDIT 的宿主在 `WM_SETTEXT` 上执行 40 字符上限。目标最终为 `capacity draft: Rust 123.\r\n今天測試耳機音頻請保留這段`（40 个 UTF-16 单元），意图文本 52 字符；状态为 `The original target holds only 39 of 51 characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. ...`；完整转写 `今天測試耳機音頻請保留這段發言由用戶檢查並手動發送` 在备用文本框与 `current-result.txt` 中逐字一致。 |
| 写后内容不符、超时、并发变化或可能部分写入：明确“写入未确认”，提示先检查原框；不自动重试、不回滚 | `unconfirmed_output_...`（已有，完整写入但确认失败）断言 `could not be confirmed`、`Check the original target`、`not retried automatically`；新增 `concurrent_target_edit_...`（写入前用户并发输入）断言 `could not be confirmed`、`may be partially written`、操作者输入被保留且未被追加或回滚、`replace_calls == 1`。 | 只读原生控件走“写入被拒绝后读回”的诊断路径；读回失败分支、2 秒超时后的迟到完整写入分支只在自动测试中覆盖（见已知限制）。 |
| 重复停止、按住快捷键重复事件、处理中操作：状态明确，同一段结果不重复，不串目标/结果 | `repeated_stop_held_hotkey_and_processing_controls_deliver_each_result_once`：连续 4 次 `Toggle` 时只有第一次停止，其余显示 `Still processing`；处理中 `Start`/`Stop` 不改变阶段；结果后连续 3 次 `Stop` 不追加、正文不变、`replace_calls` 仍为 1、状态含 `Not capturing`；随后一次新采集只写入新焦点控件，旧结果只出现一次，新结果不进入旧目标。既有 `busy_operations_...` 同样通过。 | 容量场景结果后执行两次真实 `--control stop`：目标文本逐字不变，`status.txt` 出现 `Not capturing`。开始后把焦点切到邻居控件再停止，写入仍只进入开始时绑定的目标。 |
| 任何失败分支不自动发送、不修改非目标输入框；备用文本可查看复制 | 所有失败测试都断言非目标草稿逐字不变；`Target` 契约只有读取和整值替换，产品没有 Enter/发送/粘贴代码。备用文本框内容在每次失败后仍为完整转写，`Stop` 后仍可读。 | 夹具的发送按钮与编辑框 Enter 计数均为 0；邻居与另一窗口逐字不变；备用文本框通过 `WM_GETTEXT` 与结果文件逐字一致；失败后“复制完整结果”按钮为 enabled。没有点击复制以避免覆盖用户剪贴板（见已知限制）。 |
| 从同一开始/停止入口、用可控目标故障验证，并补充真实原生控件场景 | 全部断言都从 `Application::dispatch(Control::Start/Stop/Toggle)` 进入。 | 两次真实完整流程均由正式 `listen.exe --control start|stop` 驱动：真实 WASAPI loopback 采集 8.35 秒默认播放、真实本地 whisper.cpp CPU 识别，目标为任务自有原生多行 EDIT，监听进程没有收到任何窗口句柄。 |

## 真实原生控件集成场景

环境：Windows 11，工作树 release 构建 `app\listen\target\release\listen.exe`，whisper.cpp v1.8.2 CPU、`ggml-base.bin`、中文模式、6 线程，默认多媒体播放设备。夹具与驱动只操作本任务启动的 Listen 进程和本任务创建的两个 WinForms 宿主窗口；结束后 Listen 通过正式 `--control quit` 退出，夹具关闭。

命令：

```powershell
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-5-integration\accept-native-failures.ps1 -Binary E:\luyin\.worktrees\issue-5\app\listen\target\release\listen.exe
```

驱动源码：`.artifacts\issue-5-integration\accept-native-failures.ps1`、`NativeTargetFixture5.cs`；控件定位与截图复用 `.artifacts\issue-3-integration\ListenNativeQa.cs` 中本任务自有的窗口/进程校验。

### 平台事实与场景构造

先写了一个一次性 Rust 探针（`app/listen/examples/target_limit_probe.rs`，运行后从工作树删除，副本与输出保存在 `.artifacts\issue-5-integration\`），用与产品相同的 COM UIA API 直接验证：

- `EM_SETLIMITTEXT(20)` **不会**限制多行 EDIT 的 `ValuePattern.SetValue` / `WM_SETTEXT`：写 70 字符后原生与 UIA 读回都是 70。单行 EDIT 同样不受限。文本上限只限制用户输入，不限制程序化整值写入。因此任务描述中“`EM_SETLIMITTEXT` 可造容量上限”的假设在本机不成立。
- 真实原生多行 EDIT 可以由宿主在 `WM_SETTEXT` 上实施容量上限：UIA `SetValue` 仍返回成功，原生与 UIA 读回都是截断后的前缀。验收夹具使用这种方式（等价于限制长度的应用字段），控件、消息和 UIA 读写全部是真实的。
- 只读多行 EDIT（`EM_SETREADONLY`）报告 `IsReadOnly=true`，UIA `SetValue` 返回 `HRESULT(0x80131509)` 且文本不变；产品在调用前已检查只读，正常流程不会发出该调用。

构造夹具时还发现并修正了一个夹具问题（不是产品问题）：用 `SetWindowLongPtrA` / 非 W 的 `SendMessage`、`CreateWindowEx` 声明会把原生 EDIT 变成 ANSI 窗口，`WM_SETTEXT` 会按 ANSI 码页转换，最初因此观察不到截断。最终夹具显式使用 `CreateWindowExW`、`SendMessageW`、`SetWindowLongPtrW`、`CallWindowProcW`，控件保持 Unicode。证据中的 `settext_calls=3`、`settext_truncated=2`、`last_incoming_length=52` 对应三次写入：驱动自检（52→40）、夹具复位（容量临时关闭）和产品写入（52→40）；因此产品的真实 UIA 写入确实经过夹具的容量入口并被截断。

### 实测结果

两次流程都播放同一已知 8 秒语音，识别结果均为 `今天測試耳機音頻請保留這段發言由用戶檢查並手動發送`（完整保留发言，繁体与标点差异属于 #9 范围），成功后临时 WAV 为 0。

| 场景 | 实际状态与结果 |
| --- | --- |
| 容量 40，开始时焦点在目标，采集中把焦点切到邻居控件，随后真实停止 | `phase=Result`；`detail=The original target holds only 39 of 51 characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. Check the original target before copying; output is not retried automatically and the original text is not reverted. The complete result is in the fallback text box.` 目标全文为 `capacity draft: Rust 123.\r\n今天測試耳機音頻請保留這段`（`WM_GETTEXT` 长度 40，意图文本 52）；备用文本框与 `current-result.txt` 为完整转写；邻居与另一窗口逐字不变；发送 0、Enter 0；两次真实 `--control stop` 后目标逐字不变且状态含 `Not capturing`；复制按钮 enabled。截图 `capacity-truncated.png` 已目视核对。 |
| 采集中把原目标改为只读，再把焦点切到另一窗口，真实停止 | `phase=Result`；`detail=The original target still holds its previous content, so nothing was written to it (The original target is read-only). The complete result is in the fallback text box. Review and copy it to the intended input.` 目标全文仍为 `capacity draft: Rust 123.`；备用文本框与结果文件完整一致；邻居与另一窗口逐字不变；发送 0、Enter 0；复制按钮 enabled。截图 `readonly-fallback.png` 已目视核对。 |

注：状态里的 `39 of 51` 是归一化 CRLF/LF 后按字符计数（换行记 1 个字符）；原生控件实际保存 `\r\n`，所以 `WM_GETTEXT` 长度是 40。二者描述的是同一段前缀。

证据：`.artifacts\issue-5-integration\native-evidence.json`（完整状态、目标文本、转写、计数、截图文件名）、`native-41564-639300088687140274\status.txt` 与 `current-result.txt`、`capacity-truncated.png`、`readonly-fallback.png`。全部为 Git 忽略的本机文件。

## 已知限制与未覆盖边界

- **容量场景的构造方式**：Windows 的 `EM_SETLIMITTEXT` 不限制程序化整值写入，原生场景由真实控件的宿主在 `WM_SETTEXT` 上实施上限。控件与 UIA 往返是真的，但这个上限是夹具宿主的策略，不等同于某个真实第三方应用自己的字段限制；本任务没有对真实浏览器/客户端的 `maxlength` 或自研编辑区做逐个验收。
- **截断识别的条件**：只把“非空、不等于写入前草稿、且为意图文本真前缀”的稳定读回判为截断。若容量恰好只能容纳写入前草稿（新文本一个字也放不下），会落入“仍保持原内容，未写入”分支——仍然不是完整成功、仍保留完整备用结果，但不会显示截断计数。读回不是稳定前缀（例如用户同时改动）则显示“可能部分写入”。
- **非事务写入**：UIA 读取、检查与 `SetValue` 不是跨进程原子操作；最后检查与写入之间的并发编辑仍可能发生。失败后只做诊断读回，不重试、不回滚；这继承 #3/#4 的平台限制。
- **超时后迟到写入**：2 秒确认期限后的“目标其实已完整”分支只有自动测试覆盖；原生场景没有人为制造 2 秒以上的 provider 延迟。
- **禁用原生控件**：只读用真实原生控件覆盖；禁用（`EnableWindow(FALSE)`）只有自动测试覆盖，因为禁用控件的 UIA 读取行为在不同 provider 上可能不同。若读回失败，状态仍会带上 `replace` 返回的具体原因（如 disabled），但会显示“无法读回”。
- **错误文本语言**：新增状态仍为英文，与现有 UI 状态一致；没有引入本地化。
- **剪贴板复制**：只验证了备用文本框完整可读和复制按钮 enabled，没有点击复制按钮读取剪贴板，以免覆盖用户当前剪贴板；复制实现本身未被本任务修改，沿用 #2 已验收的行为。
- **识别质量与延迟**：原生场景每次只播放一个 8.3 秒样本，只断言已知发言被保留；识别质量、实时性和长时间采集仍属于 #9 等任务。
- **状态文件写入**：应用以非原子方式写 `status.txt`（T1 既有行为，本任务未改）；验收驱动在读取时对共享冲突做了重试。这是驱动适配，不是产品行为变化。

## 构建与静态检查

在 `E:\luyin\.worktrees\issue-5`：

- `.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test`：`capture_to_copy` **20 passed / 0 failed / 1 ignored**（需操作者的原生适配器测试），`long_sessions` 8 passed，`local_quality` 2 ignored（需显式模型与语料），单元/文档测试 0 failed。
- 同一脚本 `-Task CheckFormat`：通过；`git diff --check`：通过。
- 同一脚本 `-Task Build`：离线锁定依赖 release 构建通过（格式整理前完整构建 39.08 秒，整理后增量 9.26 秒），产物为本工作树 `app\listen\target\release\listen.exe` 与相邻 `libunwind.dll`。

## 复现

自动行为：

```powershell
cd E:\luyin\.worktrees\issue-5
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
```

真实原生控件场景（会播放 8 秒已知语音并占用默认播放设备；只操作本任务创建的两个夹具窗口和本次启动的 Listen）：

```powershell
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-5-integration\accept-native-failures.ps1 -Binary E:\luyin\.worktrees\issue-5\app\listen\target\release\listen.exe
```

容量/只读行为基准（探针源码在 `.artifacts\issue-5-integration\target_limit_probe.rs`，输出记录在相邻 `probe-observations.txt`）。探针当时作为 `app/listen/examples/` 临时文件构建，验证后已从工作树删除，不属于交付实现。

## 与 #4 粘贴降级的合并复核（2026-11-11 深夜）

合并 `implement/issue-5-write-failures` 到集成分支时，`app/listen/src/target.rs` 与 #4 的粘贴降级（`f54889e`）有一处内容冲突。合并后的语义：

- 读取用 `validate_identity`（允许只读/禁用目标被诊断），**写入前**才用 `validate_writable`。
- `SetValue` 只调用一次；确认阶段区分三种结果：已确认完整、**稳定截断**（返回截断错误并给出字符数）、未确认。
- **截断优先于降级**：一旦确认目标只接受了前缀，直接报告截断错误，绝不向有容量上限的编辑区粘贴补齐。
- 只有"未确认"（写被提供程序丢弃）才进入 #4 的粘贴降级；降级前再次校验身份，且只追加、可读回校验。

合并后重新执行：

- `cargo test --offline --locked`：`capture_to_copy` **20 passed**、`long_sessions` **8 passed**、2 项需显式模型的本地质量用例保持 ignored；`cargo fmt --check` 通过；离线锁定依赖 release 构建通过。
- 原生故障场景在**合并后的根构建**上重跑 `accept-native-failures.ps1`：容量截断与只读两个场景均 PASS（截断目标 40 字符、意图 51 字符；`settext_calls=3`、`settext_truncated=2`、`last_incoming_length=52`；只读目标内容未变），两场景自动发送 0、Enter 0、旁框与另一窗口均未变，备用文本框完整、复制按钮可用。证据已覆盖为 `native-evidence.json`。
- DeepSeek Harness 输入框的粘贴降级在同一合并构建上重跑两次（mandarin、mixed-technical），两次都是 `The complete result was appended to the original target`，输入框逐字等于当前结果。