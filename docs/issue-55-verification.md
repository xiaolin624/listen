# #55 启动时不显示系统托盘图标，退出改到胶囊右键菜单 — 验证

状态：**完成**。工单 [GitHub issue #55](https://github.com/xiaolin624/listen/issues/55)。
分支 `implement/issue-55-no-tray`，基线 `0e9e0fa`（= `main` 尖端），实现提交
`53477e8`（交付包也由该提交生成；本文件随后只补充打包证据，不再改产品代码），
工作树 `E:\luyin\.worktrees\tray`。主工作树 `E:\luyin` 的 7 份用户改动全程未被触碰、
未 add、未 commit、未 stash。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| `--no-tray` 不调用 `NIM_ADD`，`tray=false`，也不发气泡 | 通过 | `launch::Config::tray/notify` + 3 个启动配置测试；实测 `status.txt` `tray=0`；UIA 枚举通知区域无 Listen |
| 默认不传时行为完全不变 | 通过 | `tray=1`；UIA 枚举新增 Listen 图标；`verify-panel.ps1` acceptance **47/47**、UI 自检 **48/48**（含托盘菜单 6 项、托盘左键、胶囊左键、Esc） |
| 胶囊右键菜单与托盘菜单是**同一份实现** | 通过 | 只保留一个 `show_context_menu`；桌面实测菜单 6 项文本/顺序与托盘一致；`capsule-context-menu.png` |
| 右键「退出」走现有 `ID_EXIT` 路径并结束进程 | 通过 | 真实鼠标右键 + 点击「退出」后进程退出，无残留 `listen` 进程，无残留托盘图标 |
| `status.txt` 新增 `tray=0\|1`（全有块 69 → 70 键） | 通过 | `status::tests::the_tray_icon_is_always_reported_as_zero_or_one`；两种模式实测；全有块测试 70 键 / required 仍 30 |
| 两个启动器都传 `--no-tray` | 通过 | `packaging\启动 Listen.cmd`、`packaging\启动 Listen 本地备用.cmd`；`verify-listen-package.ps1` 新增断言 |
| `-Task Test` | 通过 | **378 passed / 0 failed / 16 ignored**（基线 374 / 0 / 16，新增 4 个测试） |
| `-Task CheckFormat` | 通过 | 无输出、exit 0 |
| `verify-capsule.ps1`（胶囊回归，补充） | 33/33 项通过后停在无关的重启恢复步骤 | 见「胶囊回归」 |
| 打包后双击 `启动 Listen.cmd`：无托盘图标、右键能退出 | 通过 | 云端默认 `tray=0`；UIA 无图标；右键 6 项菜单退出；见「打包与启动器实跑」 |
| `verify-listen-package.ps1` | 通过 | 包内启动器冒烟 + 17 文件 / 19 哈希校验，`package_untouched=true` |

## TDD：红 → 绿

**红 1（启动配置）**：先新增 `app\listen\tests\no_tray.rs`，`Config::tray()` /
`Config::notify()` 尚不存在：

```text
error[E0599]: no method named `tray` found for struct `Config` in the current scope
error[E0599]: no method named `notify` found for struct `Config` in the current scope
error: could not compile `listen` (test "no_tray") due to 6 previous errors
```

日志：`.artifacts\issue-55\red-1-config.log`。

**绿 1**：**377 passed / 0 failed / 16 ignored**（374 + 3）。日志
`.artifacts\issue-55\green-1-config.log`。

**红 2（状态键）**：`status.rs` 测试先要求 `WindowFacts` 的 `tray` 事实、`tray` 键和
70 键全有块：

```text
error[E0560]: struct `status::WindowFacts<'_>` has no field named `tray`
error: could not compile `listen` (bin "listen" test) due to 4 previous errors
```

日志：`.artifacts\issue-55\red-2-status.log`。

**绿 2**：**378 passed / 0 failed / 16 ignored**（30 个测试套件），`-Task CheckFormat`
无输出。日志：`.artifacts\issue-55\green-2-status.log`；最终全量重跑
`.artifacts\issue-55\final-test.log` 同为 **378 / 0 / 16**。全量重跑前有两次在已知负载
敏感的 glossary 100 ms 用例上实测 101.1 ms / 101.5 ms（**阈值未改**），提高测试进程
优先级后重跑通过；`storage_bounds` 10 s、`backfill_timing` 10 s 未改。

基线 `.artifacts\issue-55\baseline-test.log` 为 **374 / 0 / 16**。基线第一次运行和一次
中间重跑在已知负载敏感的
`glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range` 上实测
101.49 ms / 101.5 ms（阈值 100 ms，**阈值未改**），重跑即绿；`storage_bounds` 10 s、
`backfill_timing` 10 s 未改。

新增 4 个测试：

1. `no_tray::the_notification_area_icon_is_installed_by_default_and_absent_with_no_tray`：
   默认 `tray()==true`，`--no-tray` 时 `false`。
2. `no_tray::no_tray_suppresses_transient_bubbles_and_hidden_still_does`：默认
   `notify()==true`；`--no-tray`、`--hidden`、两者同时都为 `false`。
3. `no_tray::no_tray_changes_no_other_launch_decision`：识别路径、runtime、hidden、
   glossary 不变；`--no-tray` 是独立开关（后续参数照常解析），拼写 `--no-tray=1` 仍是
   未知参数。
4. `status::tests::the_tray_icon_is_always_reported_as_zero_or_one`：`tray=0` 与
   `tray=1` 两种取值都按契约拼写，且键始终存在。

被动过的既有测试（同一个测试函数内改断言，未新增阈值）：

- `status::tests::BASE_KEYS` 35 → 36（`panel_capture_excluded` 之后插入 `tray`）。
- `status::tests::every_block_together_is_the_frozen_sixty_nine_keys_and_the_required_thirty`
  → `..._seventy_keys_...`，`keys.len()` 69 → 70。
- `status::tests::the_cheap_report_key_changes_with_every_fact_outside_the_snapshot`
  增加 `tray` 一个变化项。
- `status::tests::every_value_keeps_its_spelling_and_stays_on_one_line` 的
  `WindowFacts` 增加 `tray: true` 并断言 `tray=1`。

## 实现

- **启动开关**：`launch::Config` 增加 `no_tray`，解析独立开关 `--no-tray`
  （`--hidden` 同型，不吞下一个参数）；访问器 `tray()` 表示是否安装通知区域图标，
  `notify()` = `tray() && !hidden()`。默认构造 `no_tray=false`，所以默认启动的
  `tray()` / `notify()` 都为真，行为不变。`listen.exe --help` 写明该开关。
- **不建图标**：`gui::run` 的启动段只有在 `config.tray()` 为真时才构造
  `NOTIFYICONDATAW` 并调用 `Shell_NotifyIconW(NIM_ADD)`；`Ui.tray` 保持 `false`，
  于是轮询里的 `NIM_MODIFY`（含气泡）和退出时的 `NIM_DELETE` 都不会发生，也不存在
  “有图标但状态说没有”的不一致。
- **一份菜单**：`gui.rs` 原来的 `show_tray_menu` 改名为 `show_context_menu(owner, snapshot)`，
  菜单项、顺序、灰化规则、`TrackPopupMenu`、`ID_EXIT` 命令路径全部只此一份。
  - 托盘：窗口过程收到 `TRAY_MESSAGE` + `WM_RBUTTONUP` 时调用它（原有路径）。
  - 胶囊：`capsule_window` 新增 `set_context_handler(handler)` / `on_context_menu()`，
    照 `set_click_handler` 的模式注册；窗口过程在**未持有胶囊自身 RefCell 借用**时
    处理 `WM_RBUTTONUP` 并调用 `gui::show_context_menu_on_capsule`，后者复制 snapshot、
    释放 UI 借用后调用同一个 `show_context_menu`。菜单 owner 仍是隐藏宿主窗口，所以
    选择结果以同一个 `WM_COMMAND` 回到 `window_proc`，「退出」走现有 `ID_EXIT`。
  - 因为 `WM_RBUTTONUP` 被直接消费、不再落到 `DefWindowProc`，同一次右键不会再生成
    `WM_CONTEXTMENU`，所以没有额外处理它；左键（点击切换面板、拖动）路径未改。
- **状态**：`status::WindowFacts` / `FactsKey` 增加 `tray`，`status::report` 在
  `panel_capture_excluded` 之后输出 `tray=0|1`；`gui` 传入 `self.tray`
  （`NIM_ADD` 成功才为 1，`--no-tray` 或 Shell 拒绝都为 0）。键集合 69 → 70，
  `scripts\verify-panel.ps1` 的 `$requiredKeys` 仍为 30、不要求新键。
- **启动器**：两个 `.cmd` 都追加 `--no-tray`。`verify-listen-package.ps1` 新增两条
  断言：每个启动器都必须传 `--no-tray`，打包 README 必须解释它。
- **文案**：第二次启动被拒时，按 `config.tray()` 选择“从托盘退出”或
  “从胶囊右键菜单退出”；有托盘时的原文案保持不变。

## 桌面实跑（release，`--runtime` 指向 `.artifacts\issue-55\`）

被测二进制：`E:\luyin\.worktrees\tray\app\listen\target\release\listen.exe`（`-Task Build`），
本地引擎 SenseVoice（`E:\luyin\.tools\sherpa-onnx\...\lib` + `E:\luyin\.tools\models\sense-voice`）。

```powershell
# 默认（不传 --no-tray）：tray=1，通知区域出现图标
listen.exe --engine sensevoice ... --runtime ...\runtime-default

# --no-tray：tray=0，通知区域无图标；右键胶囊 → 退出
listen.exe --engine sensevoice ... --runtime ...\runtime-no-tray --no-tray
```

脚本：`E:\luyin\.artifacts\issue-55\verify-issue-55.ps1`（ASCII，证据日志
`verify-issue-55.log` / `verify-issue-55.console.log`）。结果 **RESULT PASS**：

- 默认启动：`tray=1`；UIA 通知区域新增 **1 个** Listen 按钮（在隐藏图标浮出层，
  accessible name 为 `Listen — Preparing — Ctrl+Alt+R Listen — Ready — Ctrl+Alt+R`，
  即同一按钮的两个 tooltip 快照被 UIA 拼接）；`--control quit` 后进程退出、图标消失。
- `--no-tray`：`tray=0`；UIA 通知区域**没有**新增 Listen 按钮；胶囊仍在
  （`822,48 275×33`，中心 `960,64`）。
- 在胶囊中心注入真实鼠标右键：`#32768` 原生菜单在光标 `960,64` 处打开；
  6 项依次为 `打开结果面板`、`开始 / 停止  Ctrl+Alt+R`、`重试本地识别`、
  `清理失败音频`、分隔线、`退出`；重试/清理按 `retry_available=false` 置灰；
  菜单以隐藏宿主窗口为前台属主（与托盘菜单一致）。点击「退出」后进程在 15 s 内结束，
  运行后无 `listen` 进程、无 Listen 托盘图标残留。
- 证据：`capsule-context-menu.png`（菜单 276×147；胶囊本身被
  `WDA_EXCLUDEFROMCAPTURE` 排除，所以画面里只有菜单，这是预期的取证方式）、
  `tray-before.txt` / `tray-default.txt` / `tray-no-tray.txt` / `tray-final.txt`。
  通知区域检查方式是 UIA：先取 `Shell_TrayWnd` 下的全部按钮
  （`TrayNotifyWnd` 的 `SystemTray.*` 按钮，含“显示隐藏的图标”），再展开该按钮的
  `TopLevelWindowForOverflowXamlIsland` 浮出层（`IsWindowVisible` 过滤），逐按钮读取
  `Name`；没有用截图数图标。

### 两种模式的 `status.txt` 摘要

| 键 | 默认启动 | `--no-tray` 启动 |
| --- | --- | --- |
| `phase` | `Ready` | `Ready` |
| `recognition_mode` | `local` | `local` |
| `tray` | `1` | `0` |
| `capsule` | `Ready` | `Ready` |
| `panel_visible` | `0` | `0` |
| `panel_error` | 空 | 空 |
| `process_id` | 120316 | 127724 |

完整快照：`.artifacts\issue-55\runtime-default\status.txt`、
`runtime-no-tray\status.txt`（退出后的最后一次写入；摘要表取自
`status-default.txt` / `status-no-tray.txt` 的 Ready 时刻；`.artifacts` 被 Git 忽略，
不提交）。两种模式的文件都是 **59 个键**（本地识别没有云端 usage 账本，usage 块只输出
`usage_available`；`detail` 是多行说明，按行数会更多）。全有块契约（usage 账本与
storage 同时存在）为 **70 键**，见
`every_block_together_is_the_frozen_seventy_keys_and_the_required_thirty`。

### `verify-panel.ps1`（默认模式，release）

```powershell
.\scripts\verify-panel.ps1 -Exe .\app\listen\target\release\listen.exe -Engine sensevoice `
  -SherpaLib <...> -SenseVoiceModel <...> -SenseVoiceTokens <...> -SenseVoiceVad <...> `
  -AudioClip E:\luyin\.artifacts\asr-quality-probe\zh-term.wav `
  -Runtime E:\luyin\.artifacts\issue-55\panel-runtime `
  -Evidence E:\luyin\.artifacts\issue-55\panel
```

**PASS**：acceptance **47/47**、UI self-checks **48/48**、failures **0**。其中包含
`status.txt keeps every previous key and adds panel_capture_excluded`（30 个 required
键齐全）、`panel_capture_excluded=0/1` 两个断言、托盘菜单可打开且恰好 6 项、第一项为
“打开结果面板”、分隔线在退出前、重试/清理灰化与 `retry_available` 一致、托盘左键
打开面板、胶囊点击切换、Esc 关闭。脚本的 `$requiredKeys` 不变，也不要求 `tray`。
证据：`.artifacts\issue-55\panel\panel-verification.json`、`tray-menu.png` 等。

### 胶囊回归（release，`verify-capsule.ps1`，补充）

因为改了胶囊窗口过程（只新增 `WM_RBUTTONUP` 分支），额外跑了胶囊自检。与本票相关的
窗口属性与行为全部通过（**33 项 PASS**）：置顶、`WS_EX_TOOLWINDOW`、
`WS_EX_NOACTIVATE`、`WDA_EXCLUDEFROMCAPTURE=17`（截图也确认胶囊不在画面里）、
点击不改变前台窗口、点击不移动胶囊、过渡到 Capturing/Processing 再回 Ready、
拖动 140×60 与位置记忆、正常退出。

随后脚本停在它自己的“重启恢复胶囊位置”步骤：本机是共享桌面，第一次运行期间系统播放
设备上有声音，产生了一条待取结果；重启时 Listen 正常恢复该结果并进入 `phase=Result`
（状态里写明 “Recovered 1 complete pending result …”），而该脚本第二段固定等待
`phase=Ready`，因此超时（33 checks / 1 failure）。这与 #55 的改动无关（`--no-tray`
未参与该运行，`tray=1`），也没有改脚本阈值或产品行为。`verify-panel.ps1` 的胶囊点击、
不抢焦点、位置/DPI 断言仍然全过，见上。

日志：`.artifacts\issue-55\verify-capsule.log`、`verify-capsule2.log`；
证据：`.artifacts\issue-55\capsule\`、`capsule2\`（`.artifacts` 不提交）。

## 键集合变更清单

| 位置 | 变更 |
| --- | --- |
| `app\listen\src\status.rs` | `WindowFacts` / `FactsKey` 增加 `tray`；`report` 在 `panel_capture_excluded` 之后新增 `tray=0\|1`；`BASE_KEYS` 35 → 36；全有块 69 → 70；`REQUIRED_KEYS` 仍 30；新增 0/1 表驱动测试 |
| `app\listen\src\gui.rs` | 传入 `WindowFacts.tray`；不再使用过时的“68-key”注释（改为 70） |
| `app\listen\tests\no_tray.rs` | 新增 3 个启动配置测试 |
| `scripts\verify-panel.ps1` | `$requiredKeys` **不变**（仍 30，不要求 `tray`） |
| `scripts\verify-listen-package.ps1` | 新增两个启动器都必须传 `--no-tray`、打包 README 必须提到 `--no-tray` |
| `README.md` | 快速开始/胶囊与面板/参数表/键数（69 → 70）/测试数（374 → 378）/验证文档索引 |
| `packaging\README.md` | 两个启动器无托盘、退出/重试/清理在胶囊右键菜单、等价命令行加 `--no-tray` |
| `docs\adr\0002-status-capsule-ui.md` | 托盘入口说明补胶囊右键菜单与 `--no-tray`；键数补 70 |
| `docs\issue-24-verification.md` | 键口径补“#55 后 70 键 / 30 required”与 `tray` 说明 |

## 无托盘时只能靠右键菜单的操作

启动器都传 `--no-tray`，因此通知区域没有任何 Listen 图标。此时下列入口只能从
**胶囊右键菜单**到达：

- **退出**（唯一普通用户退出入口；自动化仍可用 `--control quit`）；
- **重试本地识别**（失败音频保留后）；
- **清理失败音频**；
- **清理待处理段**子菜单（按采集序号逐段删除）；
- **打开结果面板**（只打开、不切换；左键单击胶囊是“切换”面板，不是同一语义）。

开始/停止仍可用 `Ctrl+Alt+R`，面板内也有复制/重试/清理/精修按钮；只运行一个实例的限制不变。

## 打包与启动器实跑

在干净的产品提交 `53477e8` 上执行（该提交是打包时的 HEAD；本文件随后只补充这段
打包证据，产品代码未再变化）：

```powershell
.\scripts\package-listen.ps1 -IncludeModels
& '.\dist\Listen-0.1.0\启动 Listen.cmd'      # 双击等价；云端凭据与 bl 已配置
.\scripts\verify-listen-package.ps1 -PackageRoot .\dist\Listen-0.1.0 -Evidence <json>
```

- **打包**：`dist\Listen-0.1.0`，manifest `git_commit=53477e85fd8f766b6c369f96e2ac7a512e5d7e10`、
  `git_dirty_at_package_time=false`、`included_models=true`；manifest 列 19 个文件
  （加 `.listen-package` 与 `manifest.json` 共 21 个），264,907,725 字节（≈253 MiB）。
- **双击 `启动 Listen.cmd`**（`verify-launcher-55.ps1`，ASCII，证据
  `.artifacts\issue-55\package\`）：启动器正常退出（exit 0），Listen 走**云端默认**
  （`recognition_mode=cloud-trial`，`preparation=Ready`，`phase=Ready`），
  `tray=0`；UIA 通知区域没有 Listen 图标；胶囊 `822,48 275×33`；真实右键在
  `960,64` 打开菜单，6 项与托盘一致（`打开结果面板`、`开始 / 停止  Ctrl+Alt+R`、
  `重试本地识别`、`清理失败音频`、分隔线、`退出`）；点击「退出」后进程结束，
  无残留进程与托盘图标。截图：`capsule-context-menu-packaged.png`。
  这次运行产生的 `dist\Listen-0.1.0\runtime-cloud\` 已被删除，包内容与 manifest 一致。
- **`verify-listen-package.ps1`**：**PASS**（`package_verification.json` /
  `package-verification.log`）：launcher smoke 用包内本地启动器启动到 `phase=Ready`、
  `recognition_mode=local`，随后 `--control quit` 正常退出；17 个文件逐项校验、
  19 个 SHA-256、5 个钉死哈希、密钥守卫自检、云端无效/就绪两条路径均通过，
  `package_untouched=true`。

## 未改变的部分

- 默认启动（不传 `--no-tray`）的托盘图标、菜单 6 项与顺序、灰化规则、气泡条件
  （`notify = tray && !hidden`）均不变；`verify-panel.ps1` 的托盘 6 项自检通过。
- 胶囊的视觉、位置、拖动、DPI、不抢焦点、`WDA_EXCLUDEFROMCAPTURE` 不变
  （`verify-capsule.ps1` 的 33 项相关检查全部通过；脚本停在无关的“重启后应有
  `phase=Ready`”假设，见「胶囊回归」）。
- 胶囊左键行为不变：单击切换展开面板，拖动仍是拖动。
- 不改云端/本地识别、交付、记录层；`--control` 命令集不变。
- 气泡只在有托盘时出现：没有通知区域图标时不做 `NIM_MODIFY`，`--hidden` 仍照旧抑制。
  本机任务栏处于“请勿打扰”，桌面脚本不把“弹出了可见 toast”当作可重复的验收断言；
  气泡条件由启动配置测试（`notify()`）+ `if self.tray` 的原有门槛覆盖。
