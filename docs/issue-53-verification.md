# #53 剪贴板回退在 CEF/Electron 宿主上不可靠：确认重试与焦点恢复 — 验证

状态：**完成**。工作卡 [GitHub issue #53](https://github.com/xiaolin624/listen/issues/53)。
分支 `implement/issue-53-cef-fallback`，基线 `391dd8a`（= `main` 尖端），
工作树 `E:\luyin\.worktrees\cef-fallback`。主工作树 `E:\luyin` 的 7 份用户改动全程未触碰：
未 add、未 commit、未 stash、未 reset --hard。改动只有一个文件：
`app/listen/src/target.rs`（+688/−38），`scripts/` 与 `status.rs` 无 diff。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 粘贴确认改有界重试，520 ms UIA 确认保留 | 通过 | `PASTE_CONFIRM_TIMEOUT=520 ms` 不变；超时后 `confirm_read_back` 在 1 s / 150 ms 的窗口里反复读回，而不是只读一次；单测 3 个 |
| 焦点恢复而不是直接放弃 | 通过 | `ensure_foreground_focus`：入口严格校验 → 只对绑定窗口 `SetForegroundWindow` + 只对绑定 element `SetFocus` → 300 ms 内复验；仍失败保留原措辞；单测 3 个 |
| 恢复后仍失败按现有措辞拒绝 | 通过 | `a_focus_that_cannot_be_restored_keeps_the_original_refusal` 断言错误字符串与恢复前完全一致 |
| 目标身份抗重渲染 | 通过（判据 + 单测） | `same_logical_editor`：同进程 + 同顶层窗口 + 同 ControlType + ClassName/AutomationId（至少一个非空且相等，双方都发布时不得矛盾）+ 值仍属于本次交付；匿名控件保持严格 runtime id；单测 5 个 |
| 真实复测 5/5 `appended` | 通过 | `accept-run2`：5 段全部 `appended`，`delivery.json` 5 条 `appended`，每条 transcript 恰好 1 次、原草稿恰好 1 次 |
| 输入框原文恢复 | 通过 | 验收结束 `composerRestored=true`；最终截图只有 placeholder 文本，无可访问文本（见「输入框恢复」） |
| 无残留 `listen` 进程 | 通过 | `residualListenProcesses=0`，复查 `Get-Process listen` 为空 |
| `-Task Test` | 通过 | **374 passed / 0 failed / 16 ignored**（29 个测试套件）；基线 **362 / 0 / 16**，新增 12 个测试 |
| `-Task CheckFormat` | 通过 | 无输出、exit 0 |
| `scripts/` 0 diff；`status.txt` 键集合不变 | 通过 | `git diff --stat -- scripts` 为空；`status.rs` 未改，`every_block_together_is_the_frozen_sixty_nine_keys_and_the_required_thirty` 仍绿（69 键 / 30 required） |

## TDD：红 → 绿

**红 1：读回重试**（先写测试，函数还是桩）：

`.\scripts\build-listen.ps1 -Task Test` 里 `target::tests::a_read_back_that_is_too_early_is_retried_until_the_pasted_text_appears`、
`the_read_back_retries_stop_inside_the_bounded_window`、`a_read_back_that_cannot_read_the_clipboard_fails_with_the_last_error`
均失败；日志 `.artifacts\issue-53\red-read-back.log`：

```
test result: FAILED. 15 passed; 3 failed; 0 ignored; 0 measured; 79 filtered out
```

**红 2：焦点恢复**（日志 `.artifacts\issue-53\red-focus.log`）：

```
test result: FAILED. 19 passed; 2 failed; 0 ignored; 0 measured; 79 filtered out
```

**红 3：身份判据**（日志 `.artifacts\issue-53\red-identity.log`）：

```
test result: FAILED. 25 passed; 1 failed; 0 ignored; 0 measured; 79 filtered out
```

**红 4：真实桌面**（`accept-run1`，修复前，日志 `.artifacts\issue-53\accept-run1.log`）：
第 1–4 段 `appended`，第 5 段 **`uncertain`**，detail 原文：

```
The write to the original target could not be confirmed (The complete target output could not
be confirmed after the fallback write); the target content changed and may be partially
written. ... The complete result is in the expanded result panel.
```

这次真实失败还暴露了 issue 未写出的第二个根因（见下节）：trace 里第 5 段是
`paste_only`，`read_back_accessibility=531`（520 ms 确认窗口读满）、`read_back=1316`
（有界重试确实执行了 3–4 次）、`collapse=147`，目标里其实已经出现完整转写，但确认值多了一个
换行，所以永远匹配不上。

**绿**：
- 单元测试逐片变绿；最终 `target::tests` 27 个（基线 15 + 新增 12）。
- 全量 `-Task Test`：**374 passed / 0 failed / 16 ignored**，日志 `.artifacts\issue-53\green-test2.log`。
- 真实 5 段：`accept-run2`，日志 `.artifacts\issue-53\accept-run2.log` + `.artifacts\issue-53\accept\accept-results.json`。

> 一次中间态全量重跑（与桌面 UIA/焦点探针并行）在
> `latency_and_preparation::backfill_to_the_bound_target_stays_inside_the_three_hundred_millisecond_budget`
> 上测到 623 ms（阈值 300 ms，**未改**）；该 suite 单独重跑 0.17 s 全绿，最终空闲下全量重跑也
> 全绿。它同样是对同时运行的桌面自动化敏感的计时测试，本次没有改任何阈值或测试。

## 真实复测（本地 SenseVoice，不花云端额度）

启动方式（`--hidden` 不影响快捷键与 `--control`）：

```
app\listen\target\release\listen.exe --engine sensevoice ^
  --sherpa-lib E:\luyin\.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib ^
  --sensevoice-model E:\luyin\.tools\models\sense-voice\model.int8.onnx ^
  --sensevoice-tokens E:\luyin\.tools\models\sense-voice\tokens.txt ^
  --sensevoice-vad E:\luyin\.tools\models\sense-voice\silero_vad.onnx ^
  --runtime E:\luyin\.artifacts\issue-53\runtime --hidden
```

驱动是任务私有的 `.artifacts\issue-53\accept-issue53.ps1`（不入库）：真实 UIA 选定目标、真实
`SendInput` 聚焦/打字/移焦点、`System.Media.SoundPlayer` 播放
`E:\luyin\.artifacts\quality-corpus\mandarin.wav`（10.8 s），先 `--control start` 再播放；
**全程没有按过 Enter**，也没有发送任何消息。目标就是当前 DSH 宿主输入框：
`[Edit] class='GRgSLq_input' name='发消息或创建任务, / 调用指令, @ 文件或对话'`，支持
ValuePattern、非只读、非密码。每段开始前清空输入框（干净段）或写入一个带唯一标记的草稿
（第 2/3/4 段），采集中用真实按键追加文字或把焦点移到桌面再点回输入框。

### 5 段结果

| # | 场景 | `delivery.json` | trace 风格 | 交付总耗时 | 输入框校验 |
| --- | --- | --- | --- | ---: | --- |
| 1 | 干净输入框 | `appended` | `direct`（SetValue 被丢弃，2023 ms 确认后走剪贴板粘贴） | 2755 ms | transcript 1 次、原草稿 0 次 |
| 2 | 采集中真实打字 | `appended` | `paste_only` | 495 ms | 原草稿 + 采集中的输入各 1 次、transcript 1 次 |
| 3 | 同 2 | `appended` | `paste_only` | 494 ms | 同上 |
| 4 | 停止前焦点移到桌面再移回 | `appended` | `paste_only` | 486 ms | 原草稿 1 次、transcript 1 次；`focus.refocused=true` |
| 5 | 干净输入框 | `appended` | `paste_only`（空段落草稿走真实剪贴板读回） | 683 ms | transcript 1 次 |

`delivery.json`（`.artifacts\issue-53\accept\delivery.json`）关键行：

```json
{"sequence":1,"state":"appended","baseline":"\n","before":"\n","after":"\n\n这段语音只在本机处理，…","detail":"The complete result was appended to the original target and read back in full."}
{"sequence":2,"state":"appended","baseline":"SEG2-ORIGINAL-DRAFT PLUS-CAPTURE-2","before":"SEG2-ORIGINAL-DRAFT PLUS-CAPTURE-2","after":"SEG2-ORIGINAL-DRAFT PLUS-CAPTURE-2\n这段语音只在本机处理，…"}
{"sequence":3,"state":"appended","baseline":"SEG3-ORIGINAL-DRAFT PLUS-CAPTURE-3","before":…}
{"sequence":4,"state":"appended","baseline":"SEG4-ORIGINAL-DRAFT","before":"SEG4-ORIGINAL-DRAFT","after":"SEG4-ORIGINAL-DRAFT\n这段语音只在本机处理，…"}
{"sequence":5,"state":"appended","baseline":"\n","before":"\n","after":"\n\n这段语音只在本机处理，…"}
```

没有 `uncertain` / `fallback`。每段交付的累计内容与「段前草稿 + 一个分隔符 + 完整转写」逐字一致
（`composerMatchesExpected=true`，`transcriptCount=1`，`draftMarkerCount=1`），没有重复段落。
第 4 段的焦点证据：`SetForegroundWindow(Progman)` 被接受（前台窗口 65912），随后真实点击把
Harness 窗口与输入框焦点拿回，`refocused=true`。

### 本次真实运行覆盖到什么、没覆盖到什么

- **覆盖**：直接写被 CEF 宿主丢弃 → 剪贴板回退；会话记忆后的 `paste_only`；采集中真实按键修改
  草稿；焦点离开/回到目标；空输入框的 UIA 表示（见下节）；剪贴板内容被回退路径备份并原样恢复。
- **未触发**：本宿主的 composer `runtime_id` 五次完全相同
  （trace 的 `editor` 恒为 `15080/42.199078.4.4.1.1363868`），所以「重渲染等价身份」这次没有被
  真实触发；焦点恢复分支也没有被触发（第 4 段回到交付时焦点已经正确）。这两条由单元测试覆盖
  （成功/失败分支与身份正反例），没有把单测当成真实桌面证据。
- **未触发但已修**：可访问值确认秒过的段不会走到剪贴板重试；`accept-run1` 的第 5 段是重试路径的
  真实执行证据（3–4 次读回），但它当时的结论是 `uncertain`，根因是下一节的空段落问题。

## 真实复测中发现并修掉的第二个根因：空输入框的 UIA 表示

DSH（CEF）输入框被清空后，UIA `ValuePattern` 报 `"\n"`（DOM 里剩一个空段落），而
`Ctrl+A / Ctrl+C` 复制出来的是空字符串。`paste_only` 路径原本信任 UIA 的 `known_draft="\n"`，
于是 `plan_paste` 认为草稿非空、追加一个 `"\n"` 分隔符，`wanted` 变成 `"\n\n"+转写`；真实粘贴
落下的可访问值是 `"\n"+转写`（或者干净一次时直接是转写），精确确认永远不成立 → `uncertain`。

修复（只影响这一种草稿）：`line_breaks_only(draft)` 为真时，`paste_only` 路径改用真实剪贴板读回
（`read_draft`）得到真正的草稿，再据此规划粘贴与确认。它每次多一次 `draft_read`（第 5 段
683 ms 中的 463 ms），只在「草稿只有换行」时发生；有文字/空格的草稿仍走原来的零额外读回路径。
这不是放宽确认：读回值仍必须与本次粘贴的完整期望值逐字相等。

## 身份判据与焦点恢复的取舍

### 身份判据（`same_logical_editor`）

runtime id 相等时仍然直接通过（最快、最严格）。不相等时，只有同时满足下列全部条件才视为同一
个逻辑输入框，否则照旧拒绝：

1. 同一个进程、同一个顶层窗口（浏览器整个 DOM 共用一个 HWND，窗口相同本身不足以识别）；
2. 同一个 UIA ControlType（`Edit`/`Document`），Edit 不能顶替别的控件；
3. `ClassName` / `AutomationId` 至少一个非空且与绑定值相等；双方都发布同一个属性时不得矛盾；
   两者都匿名的控件保持严格 runtime id，不做等价推断；
4. 候选元素的实时值仍属于这次交付：等于草稿、以草稿为前缀（用户在其后打字），或等于本次粘贴
   的完整期望值（`wanted`）。值已经偏离，说明该控件被别的写者/用户改成别的内容，不发任何键。

不猜位置、不猜当前焦点、不按「现在前台是谁」选目标。入口处允许一次焦点恢复，但每个按键前的
guard 仍是严格的同一套校验；回退开始后焦点再变化只会中止，不会把用户焦点抢回来。

### 焦点恢复（`ensure_foreground_focus`）

只在回退入口尝试一次：`SetForegroundWindow` 只针对采集开始时绑定的顶层窗口，`SetFocus` 只针对
绑定的那个 UIA element，然后在 300 ms 内（20 ms 一次）复验；恢复自身的错误不会替换拒绝措辞。
`SetForegroundWindow` 可能被 Windows 前台锁拒绝（后台进程常见），这时仍按原措辞拒绝并进入
备用面板，不会改为「对当前焦点粘贴」。`accept-run1/2` 的脚本证明了后台进程可以对桌面窗口
`SetForegroundWindow`，但反向抢回 Harness 前台并不可靠（脚本用真实点击完成），所以产品侧保留
「恢复失败就拒绝」的保守方向。

## 没改的东西

- 不引入自动重发：失败仍是 `fallback`/`uncertain`，`uncertain` 的语义、措辞和「永不再自动写」不变。
- 直接写仍是**一次**尝试，不重试、不缩短 2 s `CONFIRM_TIMEOUT`；粘贴确认的 520 ms 也不变。
- 晚到结果（late）与精修（refine）的规则、ADR 0005 的 `started`/`appended` 记录顺序不变。
- 没有改 `paste.rs` 的 `KEY_SETTLE`、没有改任何测试阈值（`glossary` 100 ms、`storage_bounds` 10 s、
  `backfill_timing` 10 s 原样）；`scripts/` 0 diff；`status.rs` 未改。
- 没有修改或删除任何既有测试；只新增 12 个（读回重试 3、焦点恢复 3、身份判据 5、空段落判定 1）。
- 没有改交付记录里的 `before`/`after` 生成规则。注意：对空段落宿主，记录里的 `after` 是
  「`\n\n` + 转写」这一**意图值**，真实可访问值是「转写」；后续 late/refine 若拿它比对会因不一致
  而保守拒绝（不会写错目标）。这是刻意保留的保守方向，不在本任务内改记录语义。

## 输入框恢复（如实记录）

用户原始输入框是空的（首次探测 `ValuePattern.Current.Value == ""`）。这个 CEF 输入框**忽略**
`ValuePattern.SetValue`（实测调用成功但值不变），所以验收脚本改用真实按键读写：
`Ctrl+A` + `Delete` 清空、`SendInput` Unicode 打字。被编辑过一次后，清空的输入框在 UIA 里表示
为 `"\n"`（一个空段落），无法回到最初的 `""`；视觉上始终只有 placeholder
（`.artifacts\issue-53\accept\composer-final.png`）。验收运行保存并恢复了运行开始时的值
（`composerRestored=true`），最终截图确认没有任何可见文字。没有按 Enter，也没有发出任何消息。

## 仍会保守地判 `uncertain` 的情况

- 焦点恢复失败（例如 `SetForegroundWindow` 被前台锁拒绝、目标窗口被最小化/销毁），或恢复后仍
  不是绑定目标 → 原措辞拒绝，进备用面板；
- 重渲染后的焦点节点与绑定控件在进程/窗口/ControlType/Class/AutomationId 任一项对不上，或两者
  都匿名（没有可证明的静态身份）→ 不粘贴；
- 该控件值已经不等于草稿、不以草稿为前缀、也不等于本次粘贴的完整期望值（例如用户在开头改了
  字）→ 不发键；
- 粘贴后 1 s 内剪贴板读回仍不等于完整期望值（目标更深层的异步、剪贴板被其他进程占用、读回
  反复失败）→ `uncertain`，不重试、不回滚；
- 直接写晚到 + 回退粘贴同时发生、或目标内容在交付中途被第三方改写 → 现有 `failed_write` 判据
  照旧保守；
- 空段落宿主的记录 `after` 与真实可访问值的换行差异：只影响后续 late/refine 的证明，方向是
  拒绝而不是误写。

## 证据文件

任务私有，均在 `.artifacts\issue-53\`（未提交）：

- `baseline-test.log`（基线 362 / 0 / 16）、`green-test2.log`（最终 374 / 0 / 16）
- `red-read-back.log`、`red-focus.log`、`red-identity.log`、`accept-run1.log`（真实红）
- `accept-run2.log`、`accept\accept-results.json`、`accept\delivery.json`、`accept\status.txt`、
  `accept\current-result.txt`、`accept\delivery-trace.jsonl`、`accept\composer-segment-1..5.png`、
  `accept\composer-final.png`
- 驱动与探针：`accept-issue53.ps1`、`probe-composer.ps1`、`probe-focus.ps1`、`probe-type.ps1`
