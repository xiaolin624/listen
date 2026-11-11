# Issue #3 实现与验证记录

范围：[T2：向预先选定的原生输入框追加转写文本](https://github.com/xiaolin624/listen/issues/3)。实现工作树 `E:\luyin\.worktrees\issue-3`，分支 `implement/issue-3-native-target`，开始基线与集成分支均为 `e4f93bf511d160cd356813bc1d8f50b41a0d0b63`。#2 已完成，本任务保留其采集、托盘、控制命令、备用文本框和复制行为。

主流程已完成下述三条真实 Windows 桌面路径：原目标追加、开始无目标和原目标关闭。均通过实际点击、系统全局快捷键、真实 WASAPI 采集和本地 whisper.cpp 识别；没有向正式应用提供测试控件句柄。自动行为验证为 12/12 通过。规范与需求双轴审查结果将在本记录补充后再关闭任务。

## 自动行为验证与 RED → GREEN

按用户已确认的唯一主要测试边界：通过与全局快捷键相同的 `Application::dispatch(Control)` 驱动一次开始、停止、本地转写和交付。测试替换播放设备、本地识别引擎以及外部目标编辑区，保留真实临时文件和异步应用控制。没有给原生焦点、COM、消息线程或内部辅助函数新增业务测试入口。

每条测试按纵向顺序加入后执行，未先批量编写测试。命令均为：

```powershell
.\scripts\build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test
```

| 新增行为测试 | 实际 RED 或首次结果 | GREEN |
| --- | --- | --- |
| 原目标绑定、切窗、采集中新增草稿、完整多行中英/数字/emoji 追加 | 首次 8 个测试中此测试失败：实际草稿仍是 `原有草稿\n采集中新增 Rust 123`，没有预期完整转写；其余 7 个通过 | 加入开始时绑定和停止后读取/追加，8/8 通过 |
| 开始无有效目标，之后把焦点切到另一个输入框 | 已有错误回退处理上首次直接通过；没有虚构 RED | 9/9 通过，两个输入框不变，完整结果可读且已保存 |
| 原目标销毁，出现相似替代输入框 | 已有错误回退处理上首次直接通过 | 10/10 通过，不改写替代输入框，结果进入备用文本框 |
| 可能已写入但无法确认；重复开始/停止及处理中控制 | 已有一次性交付处理上首次直接通过 | 11/11 通过，完整结果保留，原目标只含一份追加，提示先检查且不自动重试 |
| 结果文件无法保存时保留完整文本与音频，避免先修改目标 | 首次 12 个测试中此测试失败：`snapshot.text` 实际为空，预期为完整转写；其余 11 个通过 | 保存失败也携带完整文本返回，保留可恢复音频，目标不变，12/12 通过 |

原有 7 项 T1 流程测试继续通过。最后一项使用真实文件系统，把 `current-result.txt` 路径预先置为目录以触发写入失败，未通过内部模拟伪造结果。确定的多行文本在完整流程中始终留在草稿或备用结果中。外部 `Target` 契约只提供读取和完整值替换，并要求写后核对；没有键盘或发送操作。自动替身没有真实发送系统，故不以恒为空的“发送计数”冒充没有发送的证据；实际没有 Enter / 发送事件须由桌面验收观察。

## 原生路径

- 正式 GUI 注入 `NativeTargets`，开始操作首先同步读取前台 GUI 线程的实际 `hwndFocus`。不按坐标、固定句柄、控件名称或应用白名单寻找目标，也不改变焦点。
- 当前能力限于具有 UIA Value 模式的原生 Edit / RichEdit 控件。自身进程、非编辑区、只读、禁用、密码框及无法访问的控件不成为可写目标。未支持编辑区继续本地转写并使用备用文本框。
- 同步给本次焦点控件添加唯一身份属性并再次确认焦点；原生线程绑定前和每次使用前都检查该属性。控件销毁后，即使 HWND 数值被复用，也不会取得本次属性。会话释放时只移除本次匹配的属性；身份属性权限不足时回退。
- 专用无窗口 MTA 线程创建、使用和释放保存的 UIA element / Value pattern 引用。该线程安装并处理原目标和原根窗口的销毁事件，不在停止时重新选择或搜索控件。COM 对象不跨线程传递。
- 输出时读取最新草稿，非空时加入一个换行，再追加完整转写。原生替换前再次读取草稿并检查是否改变；随后以 `SetValue` 写入，重新读取全文逐字核对，仅将 CRLF / LF 作为等价换行处理。
- 目标请求等待上限为 2 秒；未确认时保留完整结果，不重试。若写调用已经进入提供程序，超时后仍可能完成，因此明确提示检查原目标后再复制。该超时是原生请求保护，不是识别性能 SLA。
- 自动输出不用剪贴板、按键注入、焦点恢复或发送模式。只有用户点击既有“复制完整结果”按钮才触发备用文本框的 `WM_COPY`。成功追加后不打开 Listen 抢占前台；失败或备用结果仍沿用 T1 的可见窗口反馈。
- 完整转写先保存到当前结果文件，再尝试目标输出；保存失败不修改目标，内存中的完整结果和临时音频仍保留。目标回退本身不算识别失败，成功转写的临时音频按原有规则清理。

UIA 的读取、检查和 `SetValue` 并非跨进程原子事务；同时发生在最后检查与写入之间的用户编辑仍需更强的编辑器能力支持。这里完成采集期间编辑保留和写前检查，不声称所有并发编辑或所有控件的异常写入均已解决。通用编辑区和更广的异常输出覆盖分别属于后续任务。

## 构建与静态检查

最终 `scripts/build-listen.ps1 -ToolsRoot E:\luyin\.tools -Task Test` 为 **12/12 通过**，同一脚本的 `-Task CheckFormat` 通过，`git diff --check` 通过。`-Task Build` 使用离线锁定依赖，release 构建通过（29.84 秒）；产物为本工作树的 `app\listen\target\release\listen.exe` 和相邻 `libunwind.dll`。以上行为测试不连接桌面，也不运行真实 WASAPI 或识别引擎。

合并后主工作区也完成离线锁定依赖的 release 构建（25.17 秒），产物位于 `E:\luyin\app\listen\target\release\`。

## 主流程真实 Windows 桌面验收

2026-11-11，主流程使用交接记录中用户已指定的 Windows 本地脚本方式。被测 release 来自实现提交 `981696c8c6ef735ca7f05f3670e6c1eef97caca0`；集成合并 `f6ac378969a8ddc00aa6b8e07982cf7fb733c504` 的文件树与它一致。脚本只操作本次启动的 Listen 与本次创建的原生 EDIT 测试窗口，退出时两者均正常关闭。

```powershell
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File E:\luyin\.artifacts\issue-3-integration\accept-native-target.ps1 -Binary E:\luyin\.worktrees\issue-3\app\listen\target\release\listen.exe
```

环境沿用 #2：Windows 11、默认多媒体播放路径、whisper.cpp v1.8.2 CPU、多语言 base、中文模式、6 线程。实际播放已知的 8.08 秒语音：`今天测试耳机音频。请保留这段发言，由用户检查并手动发送。`。三次采集均得到 `今天測試耳機音頻請保留這段發言由用戶檢查並手動發送`；保留发言，存在繁体与标点差异。这是流程验收，最终识别质量选择属于 #9。

| 真实完整路径 | 观察结果 |
| --- | --- |
| 实际点击原生编辑框，按 Ctrl+Alt+R 开始；采集中用真实 Unicode 键盘输入添加 `采集中新增：English 456。`；切到另一窗口的输入框，再按系统快捷键停止 | 原目标完整保留 `原始草稿：Rust 123。`、第二行草稿和采集中新增编辑，在末尾加换行及一份转写；旁框和另一窗口的文字逐字不变。开始和停止各由系统接受 6 个按键事件。 |
| 在普通按钮上开始，采集中再选中有效输入框，停止 | 未向后来选中的输入框或此前原框写入；备用文本框与 UTF-8 当前结果文件完整一致。 |
| 选中另一窗口的原生编辑框开始，采集中关闭该窗口，随后在其他有效输入框上停止 | 显示原目标已销毁并使用备用文本框，不改写仍存在的两个输入框，也不搜索替代目标。 |
| 原目标追加完成后再次执行正式 Stop 控制命令 | 原目标没有第二份追加，当前结果保持不变。 |
| 三次流程均观察测试窗口的真实发送按钮与编辑控件 Enter 键事件 | 发送计数为 0，Enter 计数为 0。真实多行草稿仍保留；完整中英、数字、emoji、多行转写另由确定性完整流程测试覆盖，不把本次单行 ASR 输出当作真实多行识别证据。 |
| 三次成功识别后的数据和可见结果 | 备用文本框与当前结果文件逐字一致（等价处理 CRLF/LF）；临时 WAV 均为 0。T1 复制处理代码保留，本轮没有覆盖用户剪贴板；实际复制和禁网证据沿用独立的 #2 记录。 |

首个正常场景从停止到最终状态为 4.184 秒，仅为本次观测；后台同时有本地识别评测，不作为性能对照或 SLA。

本机证据位于 `.artifacts/issue-3-integration/native-target-evidence.json`，包含完整已知测试草稿、识别文本、目标全文、状态与检查结果。驱动源码为相邻的 `accept-native-target.ps1`、`ListenNativeQa.cs` 与 `NativeTargetFixture.cs`。`appended-native-target.png`、`no-target-fallback.png` 和 `closed-target-fallback.png` 均已目视检查，文字与状态清楚可见。音频、可执行文件和临时截图不作为生产代码或会议历史提交。

本轮实际全局快捷键和本地模型验证替代了仅靠测试句柄定位控件的旧实验；控件重建、容量限制和更多写入异常由 #5 扩展，不声称它们已全部完成。

## 原生接口依据

- [Microsoft：GetGUIThreadInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getguithreadinfo)：读取指定前台线程的焦点，支持其他进程且无需附加输入线程。
- [Microsoft：UI Automation threading](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-threading)：使用独立的无窗口 MTA 线程调用 UIA，保持 apartment 生命周期。
- [Microsoft：SetValue](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationvaluepattern-setvalue)：设置值前检查 enabled 和 readonly，写后由客户端读取确认。
- [Microsoft：SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook) 和 [事件常量](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)：out-of-context 事件通过安装线程的消息循环交付，销毁后不再使用原元素。
- [Microsoft：GetRuntimeId](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationelement-getruntimeid)：原生标识可能随时间复用，因此不能靠重新取得相同标识来替代原目标。
- [Microsoft：SetPropW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setpropw) 和 [RemovePropW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-removepropw)：为本次选定窗口保留独立身份数据，只移除自身添加的属性，UIPI 拒绝时回退。
