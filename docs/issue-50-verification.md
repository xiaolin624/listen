# #50 重构验证：胶囊视觉规格单一表示（W8）

分支 `implement/w8-capsule-visual`，基线 `f1eec92`（= `integration/refactor-20261124` 尖端，W1–W7 已合入）。
票：GitHub issue #50（Speculative）；计划：`docs/refactor-program-2026-11-24.md` 第 8 波；证据：
`%TEMP%\listen-arch-20261124\surface.md` 的「把胶囊的动画集合与像素映射从 Win32 窗口 module 里提出来」候选；
ADR 0002（胶囊只用颜色/形状/动效表达状态、`WDA_EXCLUDEFROMCAPTURE` 隐身）。

## 结论

| 判据 | 结果 | 证据 |
| --- | --- | --- |
| 视觉规格只有一处 | 通过 | `CapsuleState::style`（`capsule.rs:58`）是**唯一**一处把状态变成画面/动效的 `match`；`animates()`（`capsule.rs:49`）只读 `style().motion.animates()`（`capsule.rs:159`），不再列状态名 |
| 新增 `CapsuleState` 变体会编译失败而不是静默静止 | 通过 | 探针 `probe-1.log`：`error[E0004]: non-exhaustive patterns: CapsuleState::Paused not covered --> src\capsule.rs:62:15`（正是 `style` 的 `match`）。旧 `animates` 的 `matches!` 在同样探针下会编译通过并把它当静止 |
| 删掉两处 Preparing/Capturing/Processing 编码 | 通过 | `capsule_window.rs` 删掉 `animates()`（旧 737-742）与 `state_layer()`（旧 891-944）；全仓 `Preparing | Capturing | Processing` 的三态并列只剩 `capsule.rs:58` 的 `style` 与它的数据 |
| 至少 3 个纯测试（Ready 静 / Capturing 呼吸 / Failure 暖色且不动） | 通过 | `ready_is_still`（`capsule.rs:463`）、`capturing_breathes`（`:476`）、`failure_is_warm_and_still`（`:502`），另有 `only_the_working_states_declare_motion`（`:519`）钉住整张动效表 |
| 外观逐像素等价 | 通过 | 30 个「状态 × 时间 × DPI」帧的像素哈希，期望值在**改动前**从旧 `capsule_window::paint_pixels` 捕获；重构后 `capsule::paint` 全部相等（`capsule.rs:614` 的金标记测试） |
| 窗口 module 只剩 DIB / 定时器 / 消息循环 / DPI / 拖动 / WDA | 通过 | `capsule_window.rs` 现在只有 `Surface`（DIB，`:560`）、定时器与 `paint`（`:311`/`:354`）、`window_proc`（`:656`）、`pixel_size` 与 `handle_dpi_change`（`:740`/`:509`）、拖动与位置、`WDA_EXCLUDEFROMCAPTURE`（`:201`）。纯像素/颜色/动效代码 0 行 |
| `scripts\verify-capsule.ps1` 不改松 | 通过 | `git diff f1eec92 -- scripts/` 为空；本波没有改任何断言 |
| 桌面验收实跑（基线对照） | 通过（33/33 项；唯一超时与基线逐字相同） | 见「桌面验收」一节：基线与重构后同为 33 PASS + 同一条 `phase=Ready` 超时；ready 帧 SHA-256 相同 |
| `-Task Test` 全绿且不减少 | 通过 | **353 passed / 0 failed / 16 ignored**（基线 **348 / 0 / 16**，+5 个 bin 测试） |
| `-Task CheckFormat` 无输出 | 通过 | `checkformat-1.log` 长度 0、exit 0 |
| `status.txt` 29 个 required 键不变 | 通过 | 未改 `status.rs` / `gui.rs`；`status::tests::every_block_together_is_the_frozen_sixty_eight_keys_and_the_required_twenty_nine` 仍通过 |
| `scripts/` 0 diff | 通过 | `git status --porcelain scripts/` 空 |

结论：**完成**（Speculative 但没有触发「成本超预期」的停下条件）。

## 形状（各一句）

- **状态 → 视觉**：`CapsuleState::style` 一次穷尽 `match`，每个状态给出一个 `Style { body, motion }`；
  `Body` 是颜色 + 垂直不透明度衰减，`Motion` 是 `Still` / `Breathe { 周期, 下限, 幅度, 虚线 }` /
  `Sweep { 周期, 颜色增亮, 辉光, 上限 }`。动效参数也在这张表里，所以「哪些状态会动」和「怎么动」是同一份数据。
- **派生问题**：`animates()` = `style().motion.animates()`；`Motion::animates` 自己也是穷尽 `match`
  （`Still → false`，其余 → true），新增一种运动也必须显式表态。
- **像素映射**：`capsule::paint(pixels, width, height, scale, state, time)` 纯函数（`capsule.rs:286`），
  `paint` 每帧只取一次 `state.style()`，然后逐点 `style.layer(x, y, width, height, time) -> Layer`。
  窗口只把 DIB 的 `&mut [u32]` 传进来。

## 视觉等价证据（逐像素）

**方法**：帧哈希 = 对整帧 premultiplied ARGB 像素做 FNV-1a 64；任何一字节变化都会改变哈希。期望值是在
**任何重构之前**用临时测试调用旧的 `capsule_window::paint_pixels` 打印出来的（`baseline-hashes.log`），
不是重构后重算公式——所以它不是同义反复，而是「旧实现的输出」这一独立事实。

样本 30 个：

- 5 个状态 × 5 个时间点（0.0 / 0.275 / 0.55 / 1.15 / 2.5 s）× 96 DPI 的 220×26 帧 = 25 帧；
- 每个状态再各 1 个缩放帧：Ready@1.25（275×33）、Preparing@1.5（330×39）、Capturing@2.0（440×52）、
  Processing@1.25（275×33）、Failure@1.5（330×39）= 5 帧。

重构后 `capsule::paint` 的 30 个哈希与基线**全部逐一相等**（测试
`every_state_renders_exactly_the_frame_the_window_module_drew_before_the_move`，`capsule.rs:614`）。
两个副产品同时说明哈希不是空转：静止状态在 5 个时间点的哈希相同（Ready / Failure），
Capturing 在 0.0 与 0.55 s 相同（1.1 s 周期的一半），Processing 在 0.0 与 1.15 s 相同（一个周期）。

**f32 顺序**：本波只搬动代码并参数化常量，没有重排浮点运算。`alpha` 的原式
`(0.22 + 0.18*vertical) * dash * (0.55 + 0.45*pulse)` 在数据里写成
`(base * dash) * (floor + depth*pulse)`，结合顺序一致；无虚线的 Capturing 走 `dash = 1.0`，
`base * 1.0` 是精确运算；Sweep 的 `base + glow*band` 与原 `(0.18 + 0.24*vertical) + 0.55*band` 同序。
金标记测试就是这条断言的机器检查。

## 红 / 绿

命令统一为 `.\scripts\build-listen.ps1 -Task Test|CheckFormat`；日志在
`E:\luyin\.worktrees\w8\.artifacts\`（未提交、已被 `.gitignore` 忽略）。

1. **基线** — `baseline-test.log`（23:25）：exit 0，**348 / 0 / 16**，29 个 target。
2. **红（编译红）** — `red-1.log`：只先写 5 个新测试，尚无实现。`error[E0425]: cannot find type Layer`、
   `error[E0599]: no method named style`、`error[E0599]: no method named animates`（3 处）、
   `error[E0425]: cannot find function paint`（2 处）；`Listen Test failed (101)`。
3. **绿 #1** — `green-1.log`：全量在机器 ~79% 负载下，唯一失败是计划文档记录的已知负载假红
   `glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（100 ms 预算）；
   本波没改 `glossary.rs`，**未改阈值**。同日志里 bin target 的 5 个新测试全部 ok 之前 cargo 已中止，
   因此单跑 bin target 复验（`capsule-bin.log`）：**11 passed / 0 failed**，含金标记测试。
4. **绿 #2（重跑全量）** — `green-2.log`：exit 0，**353 / 0 / 16**；bin target 55 → 60（+5）。
5. **`-Task CheckFormat`** — `checkformat-1.log` 长度 0、exit 0。
6. **提交前最终复跑** — 文档写完后先跑了两次全量，都被计划文档记录的已知负载假红挡住：
   `green-3-final.log` 与 `green-4-final.log` 的唯一失败都是
   `glossary::tests::correcting_hundreds_of_terms_stays_in_the_millisecond_range`（实测 **112.4306 ms**，
   预算 100 ms；当时机器 33–37% 负载，该测试与同进程 88 个 lib 测试并行）。按计划文档的口径**未改阈值**，
   单跑 3 次（`singlerun-glossary-1/2/3.log`）全部 **0.05–0.06 s 通过**；随后 `green-5-try1.log`：
   exit 0，**353 / 0 / 16**，29 个 target（提交用的最终绿）。
7. **`-Task CheckFormat`（提交前）** — `checkformat-3-final.log` 长度 0、exit 0。
8. **合并后复跑** — 见下「合并」。

## 探针：新增变体必须编译失败

临时加入 `CapsuleState::Paused`，并同时补上 `name()` 与 `panel.rs` 的文本分支（这样唯一的失败点只可能是
视觉规格）。`cargo check --bin listen`（`probe-1.log`）结果：

```text
error[E0004]: non-exhaustive patterns: `CapsuleState::Paused` not covered
   --> src\capsule.rs:62:15
    |               ^^^^ pattern `CapsuleState::Paused` not covered
error: could not compile `listen` (bin "listen") due to 1 previous error
```

`capsule.rs:62` 正是 `pub fn style` 里的 `match self {`。也就是说：新状态只要没在唯一那张表里决定
「长什么样 + 动不动」，构建就失败；旧实现的 `matches!` 会编译通过并把它静默当成静止。

探针已完整回退：`git status --porcelain` 只剩 `capsule.rs` / `capsule_window.rs` 两个源文件，
`grep 'Paused|PROBE'` 无残留（本文档是回退之后才写的）。

（过程中一次 `[IO.File]` 相对路径写到了主工作树 `E:\luyin\app\listen\src\capsule.rs`：已用
`git checkout -- app/listen/src/capsule.rs` 恢复，主工作树现在仍是用户原来的 7 份 dirty 文档，未 add /
未 commit / 未 stash / 未 reset。）

## 变异实验（证明新测试不是空转）

两次临时变异，跑 `--bin listen -- capsule::`，跑完都已回退（`git status` 只剩两个源文件 + 本文档）：

1. **颜色变异**（`mutation-colour.log`）：Capturing 的绿从 `0.86` 改成 `0.85`（其余不动）。
   金标记测试第一个 Capturing 帧就红：`state=Capturing time=0 scale=1 220x26`，
   `left: 1310146081412458349`（变异后）vs `right: 8574719489953684509`（= 基线 `0x76ff85f1ff4edc1d`），
   **10 passed / 1 failed**。说明 30 个哈希能抓住单个颜色通道 0.01 的变化。
2. **动效变异**（`mutation-motion.log`）：把 `Motion::Breathe | Motion::Sweep => true` 改成 `false`
   （等价于旧实现里 `matches!` 把会动的状态静默当静止）。`capturing_breathes` 在
   `assert!(CapsuleState::Capturing.animates())` 处红，`only_the_working_states_declare_motion`
   的动效表比较红（**9 passed / 2 failed**）。说明「哪些状态会动」这条事实不再是无人检查的 `matches!`。

## 新增测试（5 个，全部在 bin target 的 `capsule.rs`）

| 位置 | 测试 | 钉住什么 |
| --- | --- | --- |
| `capsule.rs:463` | `ready_is_still` | `Ready` 不动，且同一逻辑点的 `Layer` 在 t=0 / 0.275 / 1.15 / 60 s **完全相等** |
| `capsule.rs:476` | `capturing_breathes` | `Capturing` 会动：1/4 周期（0.275 s）比 t=0 更亮；呼吸只改不透明度、颜色三元组不变；1.1 s 后回到同一张图 |
| `capsule.rs:502` | `failure_is_warm_and_still` | `Failure` 不动（4 个时间点 `Layer` 相等）且是暖色（红 > 0.5、红 > 绿、红 > 蓝） |
| `capsule.rs:519` | `only_the_working_states_declare_motion` | 会动的恰好是 Preparing / Capturing / Processing 三个，表写成字面量 |
| `capsule.rs:614` | `every_state_renders_exactly_the_frame_the_window_module_drew_before_the_move` | 30 个金标记帧哈希 = 重构前实现 |

## 被动过的既有测试（1 条）

| 原位置 | 现位置 | 改动 | 类型 |
| --- | --- | --- | --- |
| `capsule_window.rs` 的 `the_capsule_is_transparent_outside_its_rounded_bar` | `capsule.rs:588` | 只把 `paint_pixels` 改名为 `paint`；3 条断言逐字未改 | 随被测函数搬 module |

其余 29 个 target 的 347 个测试：**0 处改动、0 删除**。

## 主动改动（非测试）

| 文件 | 改动 |
| --- | --- |
| `capsule.rs` | 新增视觉规格：`Style` / `Body` / `Motion` / `Layer`、`CapsuleState::style`、`CapsuleState::animates`、`capsule::paint`、`capsule_coverage`（后两者从 `capsule_window.rs` 搬入，算术未改）；模块文档改为「状态模型 + 视觉规格」 |
| `capsule_window.rs` | 删除 `animates()` / `state_layer()` / `paint_pixels()` / `capsule_coverage()`；3 处 cadence 判断改调 `self.state.animates()`；`paint` 改调 `capsule::paint`；模块文档改为「只剩 Windows 侧」 |

## 偏差（逐条）

1. **没有新开 module**：视觉规格放进既有的 `capsule.rs`（和 `capsule_state` 一起），不是 `capsule_visual.rs`。
   理由：票面写「和 `capsule_state` 放在一起」，且 `capsule.rs` 本来就是 Windows-free 的状态 module；
   多开一层只会多一个再导出。
2. **`Style` 是数据而不是每个状态一个闭包/函数**：为了让「会不会动」能从同一份值派生（`Motion`），
   参数化是必要的；代价是颜色/透明度常量从表达式变成字段，读代码时需要对照 `Style::layer` 的公式。
   已用金标记测试把「参数化没有改变任何一个像素」钉住。
3. **窗口 module 仍保留 `pixel_size` / `LOGICAL_WIDTH` / `LOGICAL_HEIGHT` / 位置文件 / `save_frame`**：
   这些是窗口尺寸与呈现（DIB）事实，不是状态 → 视觉规格。票面列出的「DIB、定时器、消息循环、DPI、拖动、
   WDA」都留着，`save_frame` 属于 DIB 呈现侧的验收钩子（`CAPSULE_FRAME_MESSAGE`），维持原样。
4. **`Style::layer` 保持私有**：外部只需要 `CapsuleState::animates()` 与 `capsule::paint`；`layer` 是
   规格的实现细节，测试在同一个 module 内可见。`Style` / `Layer` 本身因签名而公开，但 `capsule` 是
   二进制 crate 的私有 module，不扩大 crate 的公开面。
5. **没有改任何视觉常量**：颜色、透明度、周期、虚线、扫带全部逐字搬移（`0.36/0.62/0.55`、`1.8/0.55/0.45`、
   `14.0`、`1.1/0.80/0.20`、`1.15/13.0/56.0/28.0`、`0.45/0.32/0.55/0.94`、`0.93/0.24/0.27/0.52/0.32`）。
6. **`animates` 的调用频率**：`set_state` 每次状态变化一次、`tick` 每帧一次（动画态 60 Hz / 静止态 250 ms），
   现在每次会构造一个 6 字段的 `Style`（复制 f32），相对一次全帧光栅化（220×26 = 5720 点）可忽略；
   没有测到可见差异，也没有引入缓存。

## 桌面验收（`scripts\verify-capsule.ps1`）

票面说「不必实跑桌面验收（外观不变由像素等价或测试保证）」，但本波把 `paint` 的调用点搬了 module，
所以实跑一次，并把基线（`f1eec92` 的 release 构建）放在同一个包、同一个脚本、同样参数下对照：

| 运行 | 结果 |
| --- | --- |
| 重构后（`implement/w8-capsule-visual`） | **33 / 33 项 PASS**，随后在「重启恢复位置」处抛 `Timed out waiting for phase Ready` |
| 基线 `f1eec92` | **33 / 33 项 PASS**，同一条 `Timed out waiting for phase Ready` |

- 通过项包括：胶囊窗口类/扩展样式、`WDA_EXCLUDEFROMCAPTURE (17)`、topmost、点击不抢焦点、
  `status.txt` 的 `capsule=` 全程 Ready→Capturing→Processing→Ready、拖动与 `capsule-position.txt`、
  启动 125 ms（基线同一脚本测得 156 ms）。
- 唯一失败**基线与重构后逐字相同**：脚本在 `--control quit` 后重启，应用恢复上一段完整结果
  （`phase=Result`），而脚本的 `Wait-Phase 'Ready'` 只接受 `phase=Ready`。本波没有改恢复、`--control`
  或 `status.txt`，该超时在基线上同样复现，因此判定为既有的验收脚本/恢复交互问题，不是本波回归。
- 额外的桌面级等价证据：两次运行保存的 ready 帧（`capsule-ready.png`，125% DPI 的 275×33）
  **SHA-256 完全相同**（`E6A069E0AA1A1B44A68A76A8318065838ACE22F391CF02A15EA50446BEA75CB1`），逐像素相同。
- 证据目录：`E:\luyin\.worktrees\w8\.artifacts\verify-capsule\`（重构后）与
  `E:\luyin\.worktrees\w8\.artifacts\verify-capsule-baseline\`（基线 `f1eec92` 的同一脚本输出，工作树已删除）；
  两次运行结束后均无残留 `listen` 进程。

## 合并

`git merge integration/refactor-20261124`：`Already up to date.`（integration 仍是 `f1eec92`，没有新提交，
因此无冲突可解）。合并后复跑：

- `-Task Test`：`green-6-postmerge-try1.log`，exit 0，**353 / 0 / 16**，29 个 target（一次通过）；
- `-Task CheckFormat`：`checkformat-4-postmerge.log` 长度 0，exit 0；
- 工作树 `git status --porcelain` 空；`listen` 进程 0 个。

## 后续建议

1. **视觉规格的下一步是「像素测试之外的可读性」**：如果以后要加第六个状态（例如「等待网络」），
   现在必须一次决定 `Body` + `Motion`；若它其实只是「Processing 但更暗」，考虑给 `Motion` 加参数而不是
   复制一个状态。
2. **金标记测试会挡住有意的外观改动**：这是 ADR 0002 的验收资产，但以后若产品改外观，需要一次性重算 30 个
   哈希并在 PR 里说明「外观有意变化」；否则它会把「改外观」和「改坏」一起拦下。
3. **`animates` 与 `paint` 的一致性仍然是「同一份 `Style`」保证的，不是测试保证的**：现在的测试只钉住
   Ready / Capturing / Failure 三态与整张动效表；如果以后 `Motion` 增加新变体（比如 `Spin`），
   `Motion::animates` 的穷尽 `match` 会逼你表态，但「Spin 在 `layer` 里真的动了」还需要一条新测试。
4. **窗口 module 的遗留物**：`save_frame` 的 BMP 编码（约 30 行）仍是产品代码里的测试钩子；如果以后
   要更彻底地把验收资产与产品代码分开，可以把它移到 `#[cfg(test)]` 之外的一个专用 harness，但本波不动
   （票面要求视觉不变、范围最小）。
