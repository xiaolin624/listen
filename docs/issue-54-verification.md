# #54 展开面板对屏幕共享隐身（与胶囊一致）— 验证

状态：**完成**。工单 [GitHub issue #54](https://github.com/xiaolin624/listen/issues/54)。
分支 `implement/issue-54-panel-capture-exclusion`，基线 `82c46c7`（= `main` 尖端），
工作树 `E:\luyin\.worktrees\panel-exclusion`。主工作树 `E:\luyin` 的 7 份用户改动全程未被触碰、
未 add、未 commit、未 stash。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 面板窗口创建后设 `WDA_EXCLUDEFROMCAPTURE` | 通过 | `panel_window.rs` 的 `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`；桌面实测 `GetWindowDisplayAffinity=17` |
| 失败不静默 | 通过 | `panel_capture_excluded=0` + `Notice::PanelCaptureExcluded`（面板与 `status.txt` 的 `notice`）；本机无法诱导失败，见「未诱导项」 |
| `status.txt` 新增 `panel_capture_excluded=0\|1` | 通过 | 面板不存在/打开前 `0`，面板可见时 `1`；表驱动测试覆盖两个值 |
| 键集合同步（29 → 30） | 通过 | `status.rs` 的 `REQUIRED_KEYS` 与 `scripts\verify-panel.ps1` 的 `$requiredKeys` 均为 30；全有块测试为 69 键 |
| 帧取证通道 | 通过 | `PANEL_FRAME_MESSAGE = WM_APP + 21` → `runtime\panel-frame.bmp` → `panel-*.png`；`panel-ready.png` 是 800×600 的面板本体 |
| 全桌面截图里看不到面板 | 通过 | `desktop-panel-excluded.png`（1920×1080）与 `desktop-panel-region.png`（面板 800×600 区域）都只有桌面内容，没有面板 |
| `-Task Test` | 通过 | **362 passed / 0 failed / 16 ignored**（29 个测试套件）；基线 **359 / 0 / 16**，新增 3 个测试 |
| `-Task CheckFormat` | 通过 | 无输出、exit 0 |
| `scripts\verify-panel.ps1` 实跑 | 通过 | **47/47 acceptance + 48/48 UI self-checks / 0 失败** |
| 外观、位置、DPI、不抢焦点、按钮与托盘 | 不变 | 位置 560,91 1360×691 在工作区 0,0 1920×1020 内；面板/胶囊 DPI 均 120；打开面板前后前台窗口不变；按钮、胶囊点击、Esc、托盘菜单 6 项与置灰规则全部通过 |

## TDD：红 → 绿

**红**（先写测试，键还没实现）：

```powershell
.\scripts\build-listen.ps1 -Task Test
```

`status::tests::the_panel_capture_exclusion_is_always_reported_as_zero_or_one` 失败：

```
thread 'status::tests::the_panel_capture_exclusion_is_always_reported_as_zero_or_one' panicked at src\status.rs:798:9:
the panel exclusion key is always present: phase=Ready
...
test result: FAILED. 62 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

日志：`.artifacts\issue-54-red-test.log`。

**绿**（实现隐身、键、帧通道后）：

```
362 passed; 0 failed; 16 ignored（29 个测试套件）
```

日志：`.artifacts\issue-54-green-test.log` 与最终重跑 `.artifacts\issue-54-final-test2.log`；
基线日志 `.artifacts\issue-54-baseline-test.log`（359 / 0 / 16）。最终重跑前有一次全量重跑
在已知负载敏感的
`glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range` 上实测
109.8 ms（阈值 100 ms；**阈值未改**），其余 92 项全绿；随后重跑通过。新增 3 个测试：

1. `status::tests::the_panel_capture_exclusion_is_always_reported_as_zero_or_one`：
   `panel_capture_excluded` 必须存在，且 `false → 0`、`true → 1` 两种取值都按契约拼写。
2. `panel_window::tests::the_exported_frame_is_a_top_down_opaque_32_bit_bmp`：
   帧导出是 `BM` 头、负高度（top-down）、32 bpp，像素按 BGRA 写入且 alpha 强制 255，
   否则 `System.Drawing` 读出的 PNG 会全透明，取证等于没取。
3. `tests\ui_notices.rs::a_panel_that_cannot_hide_from_screen_capture_reports_the_full_transcript_risk`：
   平台拒绝时 `Notice::PanelCaptureExcluded` 说明“完整转写会出现在共享/录屏里”，保留平台原因，
   且成功复制不会清掉这条隐私警告。

键集合测试同时更新：`BASE_KEYS` 34 → 35、`REQUIRED_KEYS` 29 → 30、全有块 68 → 69，
测试名改为 `every_block_together_is_the_frozen_sixty_nine_keys_and_the_required_thirty`；
`the_cheap_report_key_changes_with_every_fact_outside_the_snapshot` 增加
`panel_capture_excluded` 一个变化项；`every_value_keeps_its_spelling_and_stays_on_one_line`
增加 `panel_capture_excluded=1` 断言。负载敏感阈值（glossary 100 ms、`storage_bounds` 10 s、
`backfill_timing` 10 s）未改。

## 实现

- **隐身**：`panel_window::Panel::create` 在 `CreateWindowExW` 之后立即
  `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`，与胶囊同型；成功/失败结果保存在
  `Panel::capture_excluded()` / `Panel::capture_exclusion_error()`。
- **状态**：`status.txt` 在 `capsule_capture_excluded` 之后新增 `panel_capture_excluded=0|1`。
  面板未创建时（没有窗口可隐藏）报 `0`，创建成功后报 `1`；`FactsKey` 也带上该事实，
  静止 tick 的“是否重写”判断不会漏掉它。
- **失败处理**：`gui.rs` 在面板创建成功但显示排除失败时调用
  `Application::report_panel_capture_exclusion`，生成
  `Notice::PanelCaptureExcluded`；`status.txt` 的 `notice` 与面板状态块都写明
  “完整转写会出现在共享/录屏里”，不会只留下一个没人看的 `0`。
- **帧通道**：`PANEL_FRAME_MESSAGE = WM_APP + 21`（与胶囊 `WM_APP + 20` 同型、不同号）。
  面板窗口过程收到后，用 `WM_PRINT`（`PRF_NONCLIENT|PRF_CLIENT|PRF_ERASEBKGND|PRF_CHILDREN`）
  让窗口和子控件画进同一进程创建的 32 位 DIB，再把当前帧写成
  `runtime\panel-frame.bmp`；脚本转成 PNG。这不是绕过显示排除：屏幕捕获 API 仍然看不到面板，
  只是同一进程把自己当前渲染的像素原样导出。产品路径从不发送这条消息，它不改变外观、
  位置、焦点或按钮状态。
- **脚本**：`scripts\verify-panel.ps1` 的 `$requiredKeys` 29 → 30；新增两项验收断言
  （打开前 `panel_capture_excluded=0`、面板可见时 `panel_capture_excluded=1`）与
  `WDA_EXCLUDEFROMCAPTURE`、帧证据、帧尺寸、全桌面截图、面板区域裁剪等自检；
  面板的 ready/capturing/fallback/failure 四张外观图全部改为帧导出，
  `panel-ready.png` 不再来自 `CopyFromScreen`。

## 桌面实跑

```powershell
$pkg = 'E:\luyin\dist\Listen-0.1.0'   # 只借包内 engine/models；listen.exe 是本工作树的构建
& .\scripts\verify-panel.ps1 `
  -Exe .\app\listen\target\release\listen.exe -Engine sensevoice `
  -SherpaLib "$pkg\engine\sherpa" `
  -SenseVoiceModel "$pkg\models\sense-voice\model.int8.onnx" `
  -SenseVoiceTokens "$pkg\models\sense-voice\tokens.txt" `
  -SenseVoiceVad "$pkg\models\sense-voice\silero_vad.onnx" `
  -Runtime E:\luyin\.artifacts\issue-54\runtime `
  -Evidence E:\luyin\.artifacts\issue-54
```

结果：**PASS**，acceptance **47/47**、UI self-checks **48/48**、failures 0
（此前 #20 脚本是 45 + 42；新增 2 项验收与 6 项自检）。关键实测：

- `panel_capture_excluded=0`（面板还没打开）→ `1`（`--control show` 后）。
- `GetWindowDisplayAffinity(panel) = 17`（`WDA_EXCLUDEFROMCAPTURE`）。
- 帧导出 `panel-ready.png`：800×600，与面板窗口尺寸一致；能看到标题、状态块、结果区与五个按钮。
- 全桌面截图 `desktop-panel-excluded.png`：1920×1080；面板与胶囊都不在画面里。
- 面板区域裁剪 `desktop-panel-region.png`：800×600，显示的是面板背后的 DSH 窗口内容
  （标题“接下来”、正文与输入框），不是面板。这就是“共享/录屏里看不到面板”的直接证据。
- 其它状态帧：`panel-capturing.png`、`panel-fallback.png`（含完整转写）、`panel-failure.png`。
- 既有行为全部通过：面板在其监视器工作区内、DPI 与胶囊一致、打开不抢焦点、不移动胶囊、
  复制按钮/重试/清理置灰规则、胶囊点击、Esc、托盘菜单 6 项与托盘左键。

日志与证据：`E:\luyin\.artifacts\issue-54\verify-panel.log`、
`panel-verification.json`、`panel-{ready,capturing,fallback,failure}.png`、
`desktop-panel-excluded.png`、`desktop-panel-region.png`、`tray-menu.png`
（`.artifacts` 被 Git 忽略，不提交）。

## 未诱导项（如实记录）

`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 在本机（Windows 11）对面板**成功**，
实测亲和度 17。要让面板失败需要关闭 DWM/合成、使用非顶层窗口或旧系统；产品运行路径无法在
不改代码/不注入的情况下制造该条件，因此**没有**失败态真机截图。失败态的证据是：

- 单元/集成测试：面板拒绝时 `panel_capture_excluded=0` 由状态测试覆盖，
  `Notice::PanelCaptureExcluded` 的文案与“成功复制不清除”由 `ui_notices.rs` 覆盖；
- 成功态真机证据（`1`、亲和度 17、帧与桌面截图）不能当成失败态证据。

## 键集合与文档同步清单

| 位置 | 变更 |
| --- | --- |
| `app\listen\src\status.rs` | `WindowFacts`/`FactsKey` 增加 `panel_capture_excluded`；`report` 增加键；`BASE_KEYS` 35、`REQUIRED_KEYS` 30、全有块 69；新增 0/1 表驱动测试 |
| `scripts\verify-panel.ps1` | `$requiredKeys` 29 → 30；+2 验收断言；+帧通道、桌面截图、区域裁剪、亲和度、帧尺寸断言；四张面板外观图改为帧导出 |
| `docs\issue-24-verification.md` | 键口径改为“当时 68/29；#54 后 69/30”；补 `panel_capture_excluded` 说明与面板隐身断言行 |
| `docs\adr\0002-status-capsule-ui.md` | 决定句与后果段写明面板同样隐身、失败不静默、帧通道与键集合变化 |
| `README.md` | 界面说明、结果面板说明、`status.txt` 键清单加入面板隐身/`panel_capture_excluded` |
| `docs\issue-20-verification.md` | 标注“面板截图来自真实屏幕”已被 #54 取代，指向本文件 |

## 未改变的部分

- 面板外观、尺寸、位置、DPI、控件与按钮布局、`IsDialogMessageW` 的 Esc/Tab 行为均未改。
- 面板仍不抢焦点、不进入任务栏/Alt+Tab、置顶方式不变。
- 胶囊的 `WDA_EXCLUDEFROMCAPTURE`、`capsule_capture_excluded`、`CAPSULE_FRAME_MESSAGE`
  与 `verify-capsule.ps1` 未改。
- 产品路径不发送 `PANEL_FRAME_MESSAGE`；帧导出只在验收脚本请求时发生。
- `status.txt` 既有键的名称、顺序、含义与条件块不变；只有新增键。
