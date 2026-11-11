# Issue #4 实现与验证记录

范围：[T3：通用桌面及网页编辑区的自动回填](https://github.com/xiaolin624/listen/issues/4)。工作树 `E:\luyin\.worktrees\issue-4`，分支 `implement/issue-4-generic-editors`。实现基于集成分支最新提交 `4b9eb56`（合并提交 `dc7ef50`），实现提交 `6d45dce`。本任务保留 #2 的采集、托盘、备用文本框与复制行为，并扩展 #3 的单一目标输出路径。

结论：浏览器普通 `<textarea>` 和 `contenteditable` 富文本编辑区都已通过真实 `Application::dispatch` → 真实 UIA 目标适配器 → 结果追加的完整流程；无法确认身份或可编辑状态时仍进入备用文本框，不向重建控件或当前焦点下的另一个控件写入。Codex 桌面端尚无用户确认的入口，本记录不把 CLI 或其它客户端算作该项验收。

## 测试入口与环境

- 机器：Windows 11，Windows PowerShell 5.1；工具链与模型缓存 `E:\luyin\.tools`；2026-11-11。
- 浏览器：Google Chrome 157.0.8103.39（`C:\Program Files\Google\Chrome\Application\chrome.exe`），只使用独立配置目录 `E:\luyin\.artifacts\issue-4-profile`，测试结束删除配置目录并关闭本测试的窗口。
- 夹具：`app/listen/tests/fixtures/generic-editors.html`。`#plain` 为 `<textarea aria-label="Listen plain target">`，`#rich` 为 `contenteditable="true" role="textbox"`（名称“网页富文本编辑区”），`#other` 为 `<textarea aria-label="Listen non-target">`；页面把 `Enter`、`Submit`、`input` 次数和三个编辑区的实时值渲染为 JSON 证据。
- 完整流程测试：`app/listen/tests/capture_to_copy.rs` 末尾被 `#[ignore]` 的 `selected_generic_editor_receives_complete_output_through_the_real_target_adapter`。它通过 `Application::dispatch(Control::Start/Stop)`、真实 `listen::target::NativeTargets`、真实临时文件和异步结果追加运行；音频与识别由确定性测试替身提供（不是真实 WASAPI / whisper.cpp），所以本记录验收的是**目标绑定与输出路径**，不是语音识别质量。
- 操作者驱动：`scripts/verify-generic-editor.ps1`。测试写出 `ready` 后，驱动用 Win32 `SetForegroundWindow` + UIA `SetFocus()` 把焦点放到夹具目标并写 `focused`；测试进入 `Capturing` 后，驱动在采集中修改目标草稿、切焦点到 `#other`，再写 `stop`；测试完成断言后写 `finished`。驱动随后用 UIA 读取三个编辑区的实时值，并断言 `Enter` / `Submit` 为 0、`#other` 未变化、焦点仍在 `#other`。
- 测试命令：

```powershell
.\scripts\verify-generic-editor.ps1 -Target plain -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\green-plain
.\scripts\verify-generic-editor.ps1 -Target rich  -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\green-rich
```

驱动内部调用任务指定的入口 `scripts/test-focused-editor.ps1 -ToolsRoot E:\luyin\.tools -Handshake <目录>`。该入口先执行完整测试套件，再运行被忽略的完整流程测试一次。

## 实现要点

- **开始时绑定**：`TargetSelection::focused_target` 仍由 `Application::dispatch(Control::Start)` 在任何就绪检查与 UI 反馈之前调用。读取真实前台窗口、根窗口、进程、线程和 `GetGUIThreadInfo` 的 `hwndFocus`，再在专用无窗口 MTA 线程内调用 `IUIAutomation::GetFocusedElement()`；确认元素进程与开始时选定的前台进程一致，并且其最近的非空 HWND 祖先属于开始时选定的根窗口。绑定后不再重新读取前台或重新选择控件。
- **不按应用/控件类型分流**：不再要求焦点 HWND 的类名为 `Edit`/`richedit*`。由 UIA 判断焦点元素是否为 `ControlType.Edit` 且具备可写 Value 模式，因此浏览器普通编辑框、`contenteditable` 和原生 UIA 编辑区走同一路径。焦点是文档、按钮、地址栏或无 Value 模式的控件时进入备用文本框。
- **身份确认**：保存 `IUIAutomationElement::GetRuntimeId()` 返回的整数序列，每次读取/写入前重新获取并逐项比较；元素重建后不再匹配。辅以根窗口 HWND、进程、线程、`IsWindow`、`#3` 的 `SetPropW` 窗口属性（UIPI 拒绝时不作为唯一依据），以及从目标元素向上走到实际 HWND 祖先的根窗口归属检查。浏览器同一 HWND 承载整页 DOM，所以运行时 ID 是区分页面元素的主要身份。
- **读取**：优先 `ValuePattern.CurrentValue`；失败时才使用 `TextPattern.GetDocumentRange().GetText(-1)`。`TextPattern` 没有插入能力。
- **写入**：只用 `ValuePattern.SetValue`。写入前检查 `CurrentIsEnabled`、`ValuePattern.CurrentIsReadOnly`、`ControlType == Edit`（不是整个 Document）、非密码框、进程未变；`SetValue` 只调用一次。
- **读回确认**：先把 `\r\n` 统一为 `\n` 再比较。Chromium 的 `SetValue` 在 UI 线程异步生效（本机实测立即读取仍返回旧值，约 9ms 后才一致），所以写后以 20ms 间隔轮询可访问值，最多 2 秒；只轮询读取，不重复写入。`TextPattern` 读回也走同一比较。
- **不改变焦点**：UIA 的 `IUIAutomation2::AutoSetFocus` 默认会让 `SetValue` 等方法先把焦点移到目标元素。实现创建 `CUIAutomation8` 后把 `AutoSetFocus` 设为 false，避免追加时把焦点从用户当前编辑位置抢走；本记录的各次运行都验证焦点仍在 `#other`。参考 [Microsoft: IUIAutomation2::get_AutoSetFocus](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation2-get_autosetfocus)。
- **保留既有约束**：不注入键盘、不使用剪贴板、不调用 `SetFocus`/`SetForegroundWindow`、不自动发送、不自动重试；读取最新草稿后只追加一次，无法确认时保留完整结果到备用文本框并说明状态。失败分支仍由 `Application` 统一生成备用结果与提示。

## RED：实现前的真实完整流程

在实现前的提交 `dc7ef50`（其 `target.rs` 与 `fd4ea4d` 相同）上运行同一夹具与驱动，并加 `-ExpectFallback`。旧实现要求焦点 HWND 类名为 `Edit`/`richedit*`；Chrome 的焦点 HWND 是承载整页的 `Chrome_RenderWidgetHostHWND`，因此开始采集时没有可绑定目标，停止后走备用文本框。这是预期的 RED 形态，不是测试崩溃；被忽略测试在 `-ExpectFallback` 下通过，说明完整流程确实观察到 Fallback。

实际输出：

```text
RESULT target=plain delivery=Some(Fallback) enter=0 submit=0 input=1 active=other
DETAIL No supported editable target was selected at capture start. The complete result is in the fallback text box. Review and copy it to the intended input.
FINAL_TARGET 网页原有草稿：Rust 123。\n第二行 draft。\n采集中新增 Rust 123。
```

- `delivery.txt`：`Some(Fallback)`。
- `transcript.txt`：`第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀`。
- `#plain` 只保留操作者在采集中新增的草稿，没有转写；`#rich` 保持 `富文本原有草稿：Rust 123。\n第二行 draft。`；`#other` 保持 `旁边输入框必须保持不变。`；`Enter=0`、`Submit=0`。
- 证据：`E:\luyin\.artifacts\issue-4\red-plain\`（`driver-evidence.json`、`detail.txt`、`transcript.txt`、`delivery.txt`、`page.png`）及 `E:\luyin\.artifacts\issue-4\red-plain.log`。

## GREEN：普通浏览器编辑框（textarea）

实现提交 `6d45dce` 上运行 `-Target plain`：

```text
RESULT target=plain delivery=Some(Appended) enter=0 submit=0 input=2 active=Listen non-target
DETAIL The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.
FINAL_TARGET 网页原有草稿：Rust 123。\n第二行 draft。\n采集中新增 Rust 123。\n第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀
```

- 被忽略测试的 `assert_eq!(result.delivery, Some(expected), "{}", result.detail)` 通过；完整流程测试 `1 passed`。
- 最终全文逐字包含：原有草稿两行、采集中新增的 `采集中新增 Rust 123。`、一个换行、完整中英文多行转写；原有内容没有被覆盖。
- `#other` 仍为 `旁边输入框必须保持不变。`；UIA 读取 `focused_is_other=true`，焦点仍在旁边输入框，没有被追加操作抢走。
- `Enter=0`、`Submit=0`；`input=2`（操作者修改草稿与工具追加各触发一次普通 textarea 的 `input` 事件，属于平台正常行为）。
- `#rich` 未被修改；`#plain` 的页面渲染证据与 UIA 实时值一致。
- 证据：`E:\luyin\.artifacts\issue-4\green-plain\`。

## GREEN：网页 contenteditable 富文本编辑区

同一实现上运行 `-Target rich`：

```text
RESULT target=rich delivery=Some(Appended) enter=0 submit=0 input=0 active=Listen non-target
DETAIL The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.
FINAL_TARGET 富文本原有草稿：Rust 123。\n第二行 draft。\n采集中新增 Rust 123。\n第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀
```

- 被忽略测试同样 `1 passed`；最终可访问值包含最新草稿、采集中新增编辑和完整转写。
- `#other` 未修改，焦点仍在 `#other`；`Enter=0`、`Submit=0`。
- `input=0`：对 `contenteditable` 的 `ValuePattern.SetValue` **没有派发 `input` 或 `change` 事件**，与交接实测和 Chromium 源码一致。
- 夹具自己渲染的 JSON 在追加后仍停在操作者编辑后的内容（`rendered_evidence_target` 缺少转写），因为 `contenteditable` 写入不触发页面事件，也没有 `focusin`；驱动因此用 UIA 实时值核对最终全文，而不是把页面渲染 JSON 当作写入结果。
- 证据：`E:\luyin\.artifacts\issue-4\green-rich\`；`page.png` 中目标区已显示完整追加文本，且原有 `<b>` 粗体已变成普通文本，可直接看到富文本结构被拍平。

## 真实不可确认分支

除 RED 的平台回退外，又在真实 Chrome 页面上覆盖两个运行中失效的分支；两者都通过完整流程测试并进入 Fallback：

| 采集中操作 | 实际 `delivery` / `detail` | 目标最终内容 |
| --- | --- | --- |
| 点击夹具的 “Rebuild selected target”，把原目标替换为同名、重建的文本框，再把焦点移到 `#other` | `Some(Fallback)`；`The original target is no longer available. The complete result is in the fallback text box. Review and copy it to the intended input.` | 重建控件保持 `重建的相似输入框`，没有收到转写 |
| 点击 “Make selected target read-only”，再把焦点移到 `#other` | `Some(Fallback)`；`The original target is not an enabled writable editor. The complete result is in the fallback text box. Review and copy it to the intended input.` | 原目标保持原有两行草稿，没有收到转写 |

两次均为 `enter=0`、`submit=0`、`input=0`、焦点仍在 `Listen non-target`、`#other` 未修改。重建场景直接验证了运行时 ID 失效后不会向同名替代控件写入；只读场景验证了写前可编辑状态检查。证据：`E:\luyin\.artifacts\issue-4\rebuild-plain\`、`E:\luyin\.artifacts\issue-4\readonly-plain\`。

## 平台限制与已知风险

- **contenteditable 不派发编辑事件**：Chromium 的 `AXNodeObject::OnNativeSetValueAction` 对 `<input>`/`<textarea>` 调用带 `kDispatchInputAndChangeEvent` 的 `SetValue`，对任意 `contenteditable` 只调用 `setInnerText`。因此工具只能确认**可访问值**已写入并读回，不能确认页面自己的状态已接受；React/Vue 等受控编辑器若以内部 state 为准，可能在下次渲染时覆盖回去。
- **富文本结构被拍平**：`setInnerText` 会替换 DOM 文本节点并移除原有标记；本记录中 `#rich` 原有 `<b>` 粗体在追加后消失。只读的 TextPattern 无法恢复或保留结构。
- **写入是异步生效**：Chromium 的 `ValuePattern.SetValue` 返回时页面尚未更新；本机实测立即读回仍为旧值，约 9ms 后才一致。实现以最多 2 秒的读回轮询确认，不重复写入。
- **换行规范化**：Chrome 会把写入的 CRLF 规范化为 LF；比较前统一 `\r\n`→`\n`，否则会把成功写入误判为失败。
- **UIA 默认抢焦点**：不设置 `AutoSetFocus=false` 时，UIA 的 `SetValue` 会把焦点移到目标。实现已在 `CUIAutomation8` 上关闭该行为；测试的焦点断言只证明本机 Chrome 页面上有效，不能证明所有第三方提供程序都没有内部焦点副作用。
- **唯一的非注入写入路径**：`TextPattern` 只能读，不能插入。没有 Value 模式的编辑区（包括部分只提供 TextPattern 的原生控件）仍只能进入备用文本框；这是平台能力边界，不是按应用类型分类。
- **主动读取的范围**：UIA 读取、检查与 `SetValue` 不是跨进程原子事务；用户在最后检查与写入之间的并发编辑仍可能发生。写前会重新读取草稿并检查是否改变，但不声称覆盖所有并发编辑。
- **没有真实 ASR**：本流程使用确定性播放与识别替身，因此不能把本记录的文本当作 whisper.cpp 的识别质量证据；识别质量见 [Issue #9 验证记录](issue-9-verification.md)。

## 原生编辑区的回归与控件类型修正（2026-11-11 深夜补测）

第一次真实 WASAPI 采集验收（8 个固定样本、原生 Win32 多行 EDIT 目标）暴露了本任务重写 `target.rs` 后的一处回归：**所有语音样本都进入了备用文本框**，状态的说明是 `No supported editable target was selected at capture start`。

用同一套 `windows` COM 绑定写了临时探针（已删除）确认原因：UIA 把**原生多行 EDIT** 暴露成 `UIA_DocumentControlTypeId`（50030），而不是 `UIA_EditControlTypeId`（50004），但它同时提供可写的 `ValuePattern` 与 `TextPattern`、`IsReadOnly=false`。首版实现只接受 `Edit`，因此把原生多行编辑区误判为不可写。

修正（`app/listen/src/target.rs` 的 `writable_editor_type`）：接受 `Edit` **或** `Document`，可写性仍由 `ValuePattern` 存在、`IsReadOnly=false`、`IsEnabled`、非密码框决定。整页 `Document`（Chrome 的 web area）本身是只读的，仍会被排除，所以放宽控制类型没有把"整页文档"变成目标。

修正后同一驱动重跑 8 个样本全部符合预期：

| 样本 | 阶段 | 目标追加 | 停止到结果 |
| --- | --- | --- | --- |
| mandarin | Result | 是 | 6.94 s |
| mixed-technical | Result | 是 | 7.20 s |
| english-numbers | Result | 是 | 6.72 s |
| homophones | Result | 是 | 6.61 s |
| multiline | Result | 是 | 7.10 s |
| silence | NoSpeech | 否（目标不变） | 1.14 s |
| instrumental | NoSpeech | 否（目标不变） | 1.14 s |
| notification | NoSpeech | 否（目标不变） | 1.15 s |

`automatic_send_count=0`、`enter_key_count=0`；目标文本逐次增长且与"旧文本 + 换行 + 本次转写"逐字一致（21→63→136→246→278→335 字）。这次运行同时是 **#3 原生目标路径在本任务重写后的真实回归**，也补上了本记录"原生桌面目标的新路径未验证"的缺口。

驱动与证据：`E:\luyin\.artifacts\quality-live-driver\accept-quality-live.ps1`、`E:\luyin\.artifacts\quality-capture\live-evidence.json` 及每个样本的 `<sample>.json`。这是一次**真实 WASAPI 采集 + 真实 whisper.cpp** 的运行，目标由真实焦点选定，没有向工具传递任何窗口句柄。

## 尚未验证

- **Codex 桌面端**：本机未发现已确认的 Codex 桌面 GUI 入口，只有 CLI；按任务要求没有用 CLI 冒充桌面端输入框。该项验收仍缺用户确认的实际样例。
- **浏览器插件/扩展内的编辑区**：本轮验证的是普通 Chrome 页面中的 `contenteditable`；没有单独启动并验证某个扩展自己的输入区。它们若使用同一 Chromium UIA 暴露方式，理论上走同一路径，但缺少真实样例证据。
- **受控 React/Vue 富文本编辑器**：夹具的 `#rich` 是非受控 `contenteditable`。受控组件在无 `input` 事件时是否覆盖、何时覆盖，尚未用真实页面验证。
- **原生编辑区的路径**：已在上节用 8 个真实 WASAPI 样本回归通过（多行 EDIT 被 UIA 报成 `Document`，现在按可写 `ValuePattern` 判定）。单行 EDIT、RichEdit 及其他第三方 UIA 提供程序仍只有通用契约，没有逐个实测。
- **真实 GUI 的备用文本框截图**：本记录通过 `detail.txt` / `transcript.txt` / `delivery.txt` 和夹具页面证明 Fallback 分支，没有重新截取 Listen 自身窗口；备用文本框和复制逻辑沿用 #2 已有测试。
- **长时间/真实耳机采集**：不属于 #4；本任务的播放和异步线程边界与 #2/#9 一致。

## 构建与静态检查

- `scripts/build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task CheckFormat`：通过。
- `scripts/build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test`：完整套件 **15 passed、0 failed、1 ignored**（目标完整流程测试）+ **2 ignored**（需显式模型/语料的本地质量测试）；被忽略的目标测试在每次操作者运行中单独 **1 passed**。
- `scripts/build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Build`：离线锁定依赖 release 构建通过（39.95 秒），产物为 `app\listen\target\release\listen.exe` 与相邻 `libunwind.dll`。
- `git diff --check`：通过。

## 证据位置与复现

本机证据位于 `E:\luyin\.artifacts\`（已被 Git 忽略）：

- `issue-4\red-plain\`：RED；`issue-4\red-plain.log` 为完整命令日志。
- `issue-4\green-plain\`：普通 textarea GREEN。
- `issue-4\green-rich\`：contenteditable GREEN。
- `issue-4\rebuild-plain\`：目标重建回退。
- `issue-4\readonly-plain\`：目标只读回退。

每个目录中的 `driver-evidence.json` 包含目标、场景、`delivery`、`detail`、完整转写、操作者编辑、最终三个字段、焦点归属和三个页面计数；`page.png` 为最终页面截图。驱动在结束时只关闭标题为 `Listen owned editor verification*` 的窗口，并按 `--user-data-dir` 匹配清理自己的 Chrome 进程，随后删除测试配置目录。

RED 需要在实现前的 `target.rs` 上运行；当前实现上再加 `-ExpectFallback` 会如预期失败，不能用来复现 RED。复现 GREEN：

```powershell
.\scripts\verify-generic-editor.ps1 -Target plain -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\green-plain
.\scripts\verify-generic-editor.ps1 -Target rich  -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\green-rich
.\scripts\verify-generic-editor.ps1 -Target plain -Scenario rebuild  -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\rebuild-plain
.\scripts\verify-generic-editor.ps1 -Target plain -Scenario readonly -ToolsRoot E:\luyin\.tools -Handshake E:\luyin\.artifacts\issue-4\readonly-plain
```
