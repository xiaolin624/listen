# Issue #25 验证：回填延迟（3.6 s → 1 s 以内）

2026-11-12。分支 `ticket/backfill-latency`（工作树 `E:\luyin\.worktrees\backfill-latency`），
基线 `integration/v0.2` 的 `8b237f9`。机器：11th Gen Intel Core i5-11400H，Windows 11 26200，
x64，SenseVoice q8 常驻引擎（`backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s)`）。

本文所有延迟数字都是**真实测量**：真实 release 二进制、真实 WASAPI 回环采集、真实 WAV 播放、
真实 UI Automation 目标；目标是一个会丢弃 `ValuePattern.SetValue` 的原生编辑框（见“复现目标”）。
没有一项是从常量推算出来的。

## 结论

| 场景 | 回填 `delivery_ms` | 停止→结果 `stop_to_result_ms` |
| --- | ---: | ---: |
| 修复前，第一次交付（真实目标，3.8 s 语音） | **3612** | 3758 |
| 修复前，第一次交付（真实目标，10.8 s 语音） | **3633** | 3961 |
| 修复后，同一目标的第一次交付（仍按旧顺序探测一次） | **3014 / 3002** | 3149 / 3340 |
| 修复后，同一目标的后续交付（跳过 SetValue） | **828 / 830 / 831 / 833**（中位 830） | 968 / 1141 |
| 不丢写目标（接受 `SetValue`）的直写路径 | 8 / 8（每次交付都直写，未粘贴） | 142 / 147 |

修复前 3612/3633 ms 与 issue 里用户本机的 3600–3668 ms 一致；修复前 10.8 s 的
`stop_to_result_ms=3961` 与用户记录的 3861–4114 ms 一致。修复后重复交付的回填进入 1 秒以内
（约 0.83 s，摘要文本长度与用户场景相同），残差主要是两次按键稳定等待与一次粘贴本身。

## 复现目标

用户的目标是一个“受控富文本编辑器”：`SetValue` 返回成功，但编辑器按自己的状态恢复内容，写入
被静默丢弃。仓库现有 `app/listen/tests/fixtures/generic-editors.html` 的 contenteditable
**不会**丢写（Chrome 接受 UIA 写入，`scripts/verify-generic-editor.ps1` 的 rich 场景与其
邻居不变断言都依赖这一点），所以本轮按 issue 的要求另建了最小真实目标：

`.artifacts\backfill-latency\ControlledEdit.cs`

- 一个普通顶层窗口 + 一个多行原生 `EDIT` 子控件（不是 WinForms，没有别的可聚焦控件）；
- 子类化窗口过程：`WM_SETTEXT` 返回成功但**不改文本**，即“provider 接受写入、目标保留自己的状态”；
- 记录收到的按键与 `WM_PASTE` 计数（用于证明“第二次交付没有再尝试写”）；
- 自己实现 `Ctrl+A` 全选。裸 Win32 `EDIT` 类本身不处理 `Ctrl+A`，而 WinForms 文本框、浏览器
  contenteditable、富文本编辑器都处理；目标必须像真实编辑器一样响应全选，否则粘贴降级无法被测量。

测量脚本（任务私有，未入库）：`.artifacts\backfill-latency\measure-controlled.ps1`。它启动
真实 `listen.exe`，把真实 WAV 播放到默认多媒体端点（`mandarin.wav` 10.8 s / `zh-term.wav`
3.83 s），在停止前把键盘焦点还给同一个绑定目标（绑定本身固定在采集开始时），读取
`status.txt` 的 `timing_delivery_ms`、`timing_stop_to_result_ms`、`latency.jsonl` 与
`LISTEN_DELIVERY_TRACE` 的分段记录。桌面被其它自动化抢占前台时会丢弃样本并重试；每个被采纳的
样本都要求 `Delivery=Some(Appended)`、目标文本严格等于“原草稿 + 每条转写各一次”、剪贴板原样
恢复、未收到 Enter。

## 插桩：3.6 s 的准确分解

`target.rs` 新增可选的本地分段记录：设置 `LISTEN_DELIVERY_TRACE=<文件>` 后，每次交付尝试追加
一行 JSON，记录各阶段墙钟毫秒数与编辑器身份（进程号 + UIA runtime id）。只写数字与身份，不写
草稿或转写文本；不设置该环境变量时不产生任何 I/O。

### 修复前（`listen-before.exe`，基线 + 仅插桩）

3.83 s 语音（`zh-term.wav`），真实目标，第一次交付：

```text
{"editor":"32008/42.19008064","outcome":"appended","total_ms":3610,
 "read":0,"validate":0,"set_value":0,"confirm":2000,"preflight":1,"backup":0,
 "draft_read":466,"collapse":147,"paste":519,"read_back":320,"collapse":151}
```

| 阶段 | 毫秒 | 说明 |
| --- | ---: | --- |
| `read` / `validate` / `set_value` | 0 / 0 / 0 | UIA 读草稿、可写性校验、`SetValue` 调用本身都很快 |
| `confirm` | **2000** | provider 丢弃写入，轮询满 `CONFIRM_TIMEOUT` |
| `preflight` | 1 | 粘贴降级前再读一次“晚到的写入” |
| `backup` | 0 | 剪贴板备份（文本） |
| `draft_read` | 466 | `Ctrl+A` + `Ctrl+C` + 读剪贴板 + `Right`，含 `KEY_SETTLE` |
| `collapse` | 147 | 又发一次 `Right`（**与上一步重复**） |
| `paste` | 519 | 写剪贴板 + `Ctrl+V` + 两次固定等待 |
| `read_back` | 320 | `Ctrl+A` + `Ctrl+C` 读回 |
| `collapse` | 151 | 读回后把全选折叠成光标 |
| 合计 | **3610** | 其中确认窗口 2000 ms、键盘回退 1603 ms |

10.8 s 语音（`mandarin.wav`）的同一路径：`delivery_ms=3633`，
`confirm=2004, draft_read=472, collapse=146, paste=526, read_back=322, collapse=151`，
`recognition_ms=301`，`stop_to_result_ms=3961`。

这证实 issue 的假设：**2 s 确认窗口 + 约 1.6 s 键盘回退**；其中 298 ms 是同一光标折叠做了两遍。

### 修复后（同一目标、同一会话，两次交付）

第一次交付仍走旧顺序探测一次（设计如此），第二次交付使用会话记忆直接走粘贴：

```text
capture 1: {"style":"direct","outcome":"appended","total_ms":3013,
 "read":0,"validate":1,"write":0,"confirm":2019,"preflight":1,"backup":0,
 "draft_read":467,"paste":518,"read_back_accessibility":1}
capture 2: {"style":"paste_only","outcome":"appended_after_paste_only","total_ms":827,
 "read":0,"validate":0,"preflight":2,"backup":0,
 "select_all":154,"collapse":150,"paste":518,"read_back_accessibility":0}
```

| 阶段 | 第一次（ms） | 第二次（ms） |
| --- | ---: | ---: |
| `read` / `validate` / `write` | 0 / 1 / 0（`SetValue` 调用 1 次） | 0 / 0 / —（**未调用**） |
| `confirm` | 2019 | —（不进入确认窗口） |
| `preflight` / `backup` | 1 / 0 | 2 / 0 |
| 草稿读取 | `draft_read` 467（`Ctrl+A`+`Ctrl+C`+`Right`） | `select_all` 154（`Ctrl+A`）+ `collapse` 150（`Right`） |
| `paste` | 518 | 518 |
| 读回确认 | `read_back_accessibility` 1 | `read_back_accessibility` 0 |
| `delivery_ms`（应用边界） | **3014 / 3002** | **828 / 830 / 831 / 833** |

目标侧计数同时证明语义：第一次 `settext_calls=1`（有一次写尝试）、第二次 `settext_calls=0`
（没有写尝试），两次都 `paste_calls=1`、`return_keys=0`，最终文本严格等于
“原草稿 + 转写 + 转写”，原草稿只出现一次，剪贴板哨兵值恢复。

第二次交付的残差 830 ms 里：`Ctrl+A` 154 + `Right` 150 + 粘贴 518 = 822，读回确认 0–1 ms。
这些是真实的按键稳定等待（`KEY_SETTLE = 140 ms`）与目标处理时间，不是可以“证明冗余”的等待，
本轮不再压缩。

## 根因

1. `SetValue` 被受控编辑器静默丢弃，`confirmed()` 必须等满 `CONFIRM_TIMEOUT = 2 s` 才能区分
   “丢弃”与“晚到”，这是第一次交付的 2 s。
2. 随后的键盘回退把同一份草稿读了两次（`Ctrl+A/C` 读草稿 + 读回）、把光标折叠了两次，每一步
   都有 140 ms 稳定等待，合计约 1.6 s。
3. 每次采集都重新绑定目标，adapter 不记得“这个编辑器会丢写”，所以每次都要重付 2 s。

## 采取的规则与安全论证

**规则**：会话内按“编辑器身份（进程号 + UIA runtime id）”记住“这个编辑器丢弃 `ValuePattern.SetValue`”。
该记忆只由**真正发生了键盘粘贴并读回确认**的那次交付写入：写入被拒绝、截断、或晚到（确认窗口内
生效）都不写入记忆。命中记忆的后续交付**完全不再尝试 `SetValue`**，直接走粘贴降级。

为什么安全：

- **不存在“晚到写入”竞态**：命中记忆的交付没有发出任何写调用，因此没有“确认窗口关闭后才生效”
  的写入可以和粘贴竞争。这正是 `CONFIRM_TIMEOUT` 不能对未知目标缩短的原因：缩短窗口会让
  “窗口后生效的写入 + 粘贴”叠加成重复文本；而本方案是**完全不写**，不是缩短窗口。
- **写前仍读草稿**：命中记忆的交付先用 UIA 读当前值，并要求它等于 `deliver()` 刚读到的草稿
  （不等则报“draft changed”，不发任何键）。这个值同时作为粘贴计划的草稿，所以不会把整段旧草稿
  再粘一遍。
- **写后仍读回确认**：粘贴后用可访问值做**精确匹配**确认（默认 300 ms 内；该通道与直写路径的
  确认通道相同），不匹配时退回原来的剪贴板读回（`Ctrl+A/C`）再比较；仍不匹配就报错，绝不重试、
  绝不回滚。
- **截断仍拒绝**：`plan_paste` 发现目标只接受了意图文本的前缀时只报错，不粘贴；容量限制不会被
  粘贴绕过。
- **不覆盖用户手改**：草稿被用户改过时只追加转写文本（沿用既有 `plan_paste` 规则），不会写回
  旧草稿；`deliver()` 在写前发现草稿变化则不写。
- **不发提交键**：命中记忆的路径只发 `Ctrl+A`、`Right`、`Ctrl+V`（以及读回时的 `Ctrl+A/C`），
  从不发 Enter；桌面验收与夹具计数都断言 `return_keys=0`。
- **身份粒度**：按编辑器而不是按进程记忆。浏览器把普通输入框和富文本编辑器放在同一个进程里，
  按进程记忆会让另一个编辑器无谓地继承粘贴路径（粘贴路径要求目标持有前台焦点）；按
  “进程 + UIA runtime id”则只影响真正丢写的那个编辑器。runtime id 在同一元素存活期间稳定
  （`validate_identity` 本来就依赖这一点）。
- **有界**：记忆最多 128 个编辑器，超出后清空重来（回到“先直写”的安全侧）；进程退出即清空，
  新会话的第一次交付仍按旧顺序探测。

## 粘贴路径里删掉的“可证明冗余”

1. **重复的光标折叠**：`read_draft` 已经用 `Ctrl+A`、`Ctrl+C`、`Right` 把全选折叠到末尾，旧代码
   随后又发一次 `Right`。两次之间没有任何按键或选择变化，第二次是纯 no-op；删除后观测行为不变。
   实测省去 146–151 ms。
2. **命中记忆的交付不再重复读草稿**：调用方刚用 UIA 读过实时值，且这次没有发出写调用，所以不存在
   “晚到写入”需要用剪贴板二次确认；仍保留 `Ctrl+A` + `Right` 把光标放到文本末尾（粘贴必须落在
   末尾）。实测把 `draft_read` 466 ms 换成 `select_all` 154 + `collapse` 150 ms。
3. **读回确认优先走可访问值**：粘贴成功后先用 UIA 值做精确匹配（上限 300 ms），命中即成功；只有
   目标在窗口内没有发布粘贴结果时才退回原来的剪贴板读回。实测原生编辑器上为 0–2 ms，原来为
   `read_back` 320 + `collapse` 151 ms。退回路径仍会先 `Ctrl+A/C` 读回、再折叠光标，与旧行为一致。

## 未改动的路径与刻意保留

- **`CONFIRM_TIMEOUT` 保持 2 s**：对任何尚未被会话证明“丢写”的目标，第一次交付仍是一次写入 +
  2 s 确认 + 剪贴板读草稿 + 粘贴 + 读回。这样接受写入的目标（包括写入很慢的目标）仍走直写快路径，
  不会被误粘贴。
- **直写目标的快路径**：代码路径与顺序没有变化（读草稿 → 校验 → 一次 `SetValue` → 确认），
  单元测试 `a_target_that_writes_directly_keeps_the_direct_path_on_every_delivery` 固定“每次
  交付都直写、从不粘贴”。既有真机记录为 274/280/294 ms（`docs/issue-review-fixes.md`）。
- **目标绑定**：仍在采集开始时固定；重试、精修、停止都使用同一绑定目标，不重新选择前台焦点。
- **失败语义**：写入失败不重试、不自动回滚、不自动发送；失败后仍读一次目标用于诊断。

## 测试

新增 6 个单元测试（`app/listen/src/target.rs`，用可记录的假编辑器测试写决策，不需要桌面）：

| 测试 | 固定的行为 |
| --- | --- |
| `the_first_delivery_to_a_target_that_drops_the_write_tries_once_then_pastes` | 第一次交付：读 → 校验 → 一次写 → 确认 → 粘贴，并记住该编辑器 |
| `a_second_delivery_to_the_same_dropping_target_skips_the_write` | 第二次交付不再写、不再读剪贴板草稿，只粘贴一次；转写不重复 |
| `a_target_that_writes_directly_keeps_the_direct_path_on_every_delivery` | 直写目标每次都直写，从不粘贴，也不写入记忆 |
| `a_late_write_is_confirmed_without_a_paste_and_teaches_nothing` | 晚到（确认窗口内生效）的写入不会被再粘贴，也不会被记成“丢写” |
| `a_truncated_write_is_reported_without_pasting_and_is_not_remembered` | 截断只报错、不粘贴、不记忆 |
| `a_changed_draft_blocks_a_repeat_delivery_before_any_key_is_sent` | 草稿变化时不发按键、不追加 |

红/绿证据：临时把记忆查询改成恒为 `false`（模拟“没有记忆”的旧行为）后，完整套件为
`60 passed / 2 failed`，失败的正是 `a_second_delivery_to_the_same_dropping_target_skips_the_write`
（第二次交付仍然写了）与 `the_first_delivery...`（“记住该编辑器”的断言）；恢复实现后
**177 passed / 0 failed / 15 ignored**，`cargo fmt --check` 退出码 0。

## 真实目标回归

- 真实目标测量（`measure-controlled.ps1`）：两次采集一个会话，第一次与第二次都
  `Delivery=Some(Appended)`；最终文本严格等于“原草稿 + 每条转写各一次”；原草稿只出现一次；
  剪贴板哨兵恢复；`return_keys=0`；第二次 `settext_calls=0`。证据
  `.artifacts\backfill-latency\after\measurement.json`、`before\measurement.json`、
  `after-ten\measurement.json`、`before-ten\measurement.json`，分段记录在同目录的
  `delivery-trace.jsonl`。
- `scripts\verify-generic-editor.ps1 -Target plain` / `-Target rich`：结果见“桌面验收”。
- `.artifacts\final-review-fixes\accept-paste-crlf.ps1` **不再适用**：它按 v0.1 的
  `ListenCaptureToCopyWindowV1` 查找窗口（`[ListenNativeQa]::FindWindow`），v0.2 的宿主窗口是
  `ListenHostWindowV2`，而且脚本依赖旧交付包里的 whisper 目录。该脚本断言的性质
  （粘贴降级只追加一次、不重复草稿、剪贴板恢复、不发送 Enter）已由本轮的真实目标测量与
  `verify-generic-editor.ps1` 覆盖。

## 桌面验收

在最终 release 构建（`listen-final.exe`，与测量用的是同一份代码）之后运行：

| 检查 | 结果 |
| --- | --- |
| `scripts\verify-generic-editor.ps1 -Target plain` | **PASS**：`RESULT target=plain delivery=Some(Appended) enter=0 submit=0`；最终文本 = 原草稿 + 采集中手工编辑 + 一条转写；邻居输入框不变，焦点仍在非目标输入框 |
| `scripts\verify-generic-editor.ps1 -Target rich` | **PASS**：`RESULT target=rich delivery=Some(Appended) enter=0 submit=0`；contenteditable 走直写路径，最终文本 = 富文本原草稿 + 采集中编辑 + 一条转写，没有重复草稿、没有 Enter |
| 真实目标 + 真实语音（`measure-controlled.ps1`，3.83 s / 10.8 s） | **PASS**（4 个 run）：两次交付都是 `Some(Appended)`；最终文本严格等于“原草稿 + 每条转写各一次”；原草稿只出现一次；剪贴板恢复；`return_keys=0`；第二次交付 `settext_calls=0`、`paste_calls=1` |
| 同上，夹具改为接受写入（直写路径） | **PASS**：两次交付 `delivery_ms=8 / 8`，`settext_calls=1 / 1`、`paste_calls=0`，目标文本正确 |
| `-Task Test`（合并 `integration/v0.2` 后） | **177 passed / 0 failed / 15 ignored**（基线 171/0/15，新增 6 个） |
| `-Task CheckFormat` | 退出码 0 |
| `-Task Build`（release） | 成功；真实二进制已用于上述真实目标测量 |

证据目录：`.artifacts\backfill-latency\`（`before\`、`after\`、`before-ten\`、`after-ten\`、
`after-fast\` 各自的 `measurement.json` 与 `delivery-trace.jsonl`；`accept-plain\`、
`accept-rich\` 的 `driver-evidence.json`）。全部未入库。

环境注意：本机只安装了 Windows PowerShell 5.1，它会把**无 BOM 的 UTF-8 脚本按 ANSI 解码**。
`scripts\verify-generic-editor.ps1` 含中文标签（`网页富文本编辑区`），用 5.1 直接运行会在解析阶段
失败（`意外的标记`）。验收时用同目录下的 UTF-8 BOM 副本运行，脚本内容未修改；这是工具链环境问题，
不是本票代码问题。另外 Chrome 只有在窗口被激活后才发布页面可访问性树，验收脚本在查找夹具窗口前
需要先激活该窗口（用 `warm-chrome.ps1` 辅助），否则会报 “The owned fixture window did not appear”。

## 环境噪声与限制

- 本机桌面同时被其它自动化使用，采集期间会被抢走前台焦点。测量脚本因此会在交付等待期间把焦点
  还给**同一个已绑定目标**（相当于用户一直停在输入框里），并在 `Delivery` 不是确认追加时丢弃
  样本、在可能已部分写入时重跑整个 run。所有被采纳样本都来自真实目标；被丢弃的尝试不计入数字。
- 复现目标自己实现了 `Ctrl+A` 全选：裸 Win32 `EDIT` 类不处理这个组合键，而真实编辑器（WinForms、
  浏览器、富文本编辑器）都处理。这是夹具对真实编辑器行为的补齐，不是产品特判。
- 测量使用真实 SenseVoice 识别（10.8 s 语音约 283–314 ms），但延迟对比主要看 `delivery_ms`；
  `stop_to_result_ms` 同时给出。
- `LISTEN_DELIVERY_TRACE` 是本地诊断开关，不联网、不写文本内容；正常使用不产生文件。

## 分支与提交

- 分支：`ticket/backfill-latency`（工作树 `E:\luyin\.worktrees\backfill-latency`），基线
  `8b237f9`。
- 提交：
  - `3791608` `fix: skip the dropped value write on repeat deliveries to the same target (#25)`
    —— adapter 记忆、写决策 seam、粘贴路径的三处冗余删除、6 个单元测试、`NativeTargets::new()`。
  - 本文所在的 `docs: record the backfill latency breakdown and the dropped-write fix (#25)` 提交。
- 交付前 `git merge integration/v0.2` 干净完成（`Already up to date`），合并后套件
  **177 passed / 0 failed / 15 ignored**，`-Task CheckFormat` 退出码 0，`-Task Build`（release）
  成功。
