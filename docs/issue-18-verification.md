# Issue #18：术语表校正专名与技术词

2026-11-12。工作树 `E:\luyin\.worktrees\issue-18`，分支 `ticket/issue-18`，基线 `integration/v0.2` @ `eea9109`。本记录只描述本分支实际运行过的检查；未 push、未直接改动 `integration/v0.2`。

## 结论摘要

- 新增本地**术语表**（`Glossary`）：UTF-8 行文本，一个词一条，正式写法在前，可选别名（拼音、中文写法、顽固误写）放在 `|` 后。文件在**每次开始采集时重新读取**，改完下一次采集生效，不需要重启或改代码。
- 校正是**近似匹配**而不是穷举黑名单：实测的 `deep sea harnessness`、`deep seek harness`、`DeepSek harnessness`、`deepsek`、`waapp loop back` 都由 `DeepSeek Harness` / `WASAPI` / `WASAPI loopback` 一条条目召回；普通句子、`are/there/result/re-do/Re:`、`wasabi`、`deep water harness` 等负例保持不变。
- 校正只替换命中的 span，文本其余字节、空格、换行、标点都保持原样；只作用在**最终合并文本**上，不进入 `streaming.rs` 的合并逻辑，因此时间戳与分段不受影响。
- 应用边界三条路径（整段识别、流式分段识别、失败音频重试）都经过同一校正步骤；术语表文件缺失时不做校正，文件格式错误时状态说明会报告错误，本次采集仍交付未校正文本。
- 300 条术语、300 处替换、300 行转写的单次 `correct` 调用在本机 debug 测试配置下约 **28–30 ms**（单元测试上限 100 ms；release 会更快）。
- 全部测试：本 ticket 新增 12 个 `glossary.rs` 单元测试、6 个应用边界测试；未合并前 **82 passed, 3 ignored**，合入当前 `integration/v0.2`（含 #16、#17、#22 工具）后 **96 passed, 11 ignored**（多出的 ignored 是 #17 的真引擎测试，默认不跑）。

## 交付内容

| 文件 | 内容 |
| --- | --- |
| `app/listen/src/glossary.rs` | 术语表解析、近似校正、malformed 处理、单元测试 |
| `app/listen/src/lib.rs` | `Application` 接入：采集开始重读、三条路径校正、错误说明 |
| `app/listen/src/main.rs` | `--glossary <path>`，默认 `<runtime>\glossary.txt`；帮助文本 |
| `app/listen/tests/glossary_correction.rs` | 应用边界测试（交付、当前结果、重读、重试、流式、malformed、缺失） |
| `packaging/glossary.txt` | 随交付包发布的默认术语表（格式说明写在文件头注释里） |
| `scripts/package-listen.ps1`、`scripts/verify-listen-package.ps1` | 把 `glossary.txt` 打进交付目录并纳入必检文件 |
| `packaging\启动 Listen.cmd`、`packaging/README.md` | 启动参数 `--glossary`，以及用户可见的术语表说明 |

## 术语表文件格式

UTF-8 文本，按行处理：

```text
# 以 # 开头的行是注释；空行忽略
DeepSeek Harness | shen du qiu suo, 迪普西克
WASAPI | waapp
WASAPI loopback
CUDA | ku da, ku
GPU
```

- 正式写法在前，`|` 后是可选别名，逗号分隔；只写正式写法也合法。
- 2026-11-12 审查修复后，打包术语表只保留算法**无法推断**的别名：`wa-sa-pi` 归一化后等于正式写法 `WASAPI`，`waapp loop back` 由 `WASAPI loopback` 近似匹配召回，二者已删除；`shen du qiu suo`、`迪普西克`（拼音/汉字）与 `re`、`ku`、`ku da`（短别名/中文邻接）保留。证明见 `glossary::tests::the_shipped_canonical_forms_alone_catch_the_removed_variants` 与 [审查修复记录](issue-review-fixes.md)。
- 解析时会去掉 UTF-8 BOM、行首尾空白和行尾的 `\r`，因此 Windows 记事本保存的 CRLF 文件可用。
- 文件缺失：空术语表，文本原样交付，不算错误。
- 文件存在但不可读或格式错误（缺正式写法、多个 `|`、没有字母/数字、同一行混用拉丁与 CJK）：`load` 返回带**文件路径和行号**的错误；应用把它写进快照 `detail`，采集继续，交付未校正文本。
- 同一正式写法可重复出现，实际匹配以文件中靠前的条目为准。

## 校正算法

### 归一化与分词

- 每一行文本单独校正，行与行之间用原来的 `\n` 重新拼接，所以换行结构不变。
- 拉丁 token：连续字母/数字，转小写；CJK 字符一个字符一个 token（这样中文写法也能在连续中文里被找到）；其他字符（空格、标点、`-`、`·`）一律是分隔符。
- 比较时把 token 的归一化文本直接拼接，因此 `deep seek`、`deep-seek`、`DeepSeek` 归一化后都是 `deepseek`；大小写不影响匹配。
- CJK 与拉丁不会混在同一个候选 span 里；也拒绝同一行的正式写法/别名混用两种文字（见 malformed）。

### 匹配顺序（每行从左到右）

1. 从当前 token 开始，先试最长候选 span（该文字种类下最长 key 的 token 数 + 1），再依次缩短。命中后整段替换并跳过该 span；不命中则前进一个 token。这样“长短语优先于其中的短词”，不会把 `deep seek harnessness` 拆成多个部分。
2. **精确匹配**：span 归一化后与某个 key（正式写法、别名，或下文的前导词）完全相等，直接命中，不看阈值。这覆盖 `deepseek harness`、`wa-sa-pi`、`sense voice` 这类只差空格/大小写的写法。
3. **近似匹配**：按下面的阈值和保护条件比较。

### 阈值与保护条件

| 常量 | 值 | 作用 |
| --- | ---: | --- |
| `MIN_APPROXIMATE_LEN` | 5 | 归一化后 key 和 span 都至少 5 个字符才做近似；更短（`Rust`、`CUDA`、`GPU`、`re`、`ku`）只做精确匹配 |
| `SINGLE_KEY_THRESHOLD` | 0.85 | 单 token key 的编辑距离相似度下限；宁可漏掉，也不把普通词改成专名 |
| `MULTI_KEY_THRESHOLD` | 0.6 | 多 token key 的对齐下限；多个 token 的上下文使误报风险显著降低 |
| `EDGE_THRESHOLD` | 0.6 | span token 多于 key token 时，span 首 token 必须匹配首 key token 的前缀、末 token 必须匹配末 key token 的后缀 |
| `MAX_LENGTH_DIFFERENCE` | 0.4 | 归一化长度差 / 较长者，超过则直接拒绝 |

相似度是 `1 - Levenshtein / max(长度)`，实现用可复用的两行 DP（`Scratch`），不在候选循环里分配。

近似匹配还必须同时满足：

- 首字符和末字符相同（误识别几乎不会同时改首尾字母；这也是候选索引的 key）；
- 多 token key：候选 span 的 token 数不少于 key 的 token 数；把 key 的每个 token 按顺序对齐到 span 的一段**连续 token 组**，每个 token 组长度差不超过 0.4，得分取各组相似度的最小值，要求 ≥ 0.6。这样 `deep sea harnessness` 对齐为 `deepsea`(0.75) + `harnessness`(0.636)，而普通短语 `deep water harness` 的首组只有 0.556，被拒绝；
- 候选 span 的 token 多于 key 时，才额外检查首尾 token 的“前缀/后缀”相似度，防止把紧邻的普通词吞进替换（如 `DeepSeek Harness and` 的 `and`）。

补充规则：

- **前导词**：多 token 的正式写法，如果第一个 token 归一化后至少 6 个字符，则它本身也作为一个 key（阈值仍按单 key 0.85）。因此 `deep seek`、`deepsek` 会得到完整的 `DeepSeek Harness`，不需要把每个错误写法都写成别名。`WASAPI loopback` 也生成前导词 `wasapi`，但文件里 `WASAPI` 条目在前，所以单独的 `WASAPI` 仍然只输出 `WASAPI`。
- **短别名限制**：归一化后长度 ≤ 2 且来自别名（非正式写法）的命中，只有当 token 紧邻中文（跳过空格后前/后字符是 CJK）时才生效。这是为实测的 `我们用re实现`（`re` → `Rust`）和 `使用CPU和ku进行对照`（`ku` → `CUDA`）保留召回，同时避免英文 `Re:`、`re-do`、`are`、`there`、`result`、`The variable re` 被误替换。正式写法本身不受此限制。
- 候选查询使用精确 HashMap + 按“首字符、末字符”分桶；没有对全部 300×300 条目做两两比较。

### 替换

命中的 span 从第一个 token 的字节起点替换到最后一个 token 的字节终点，写入术语表里的正式写法原文；span 前后的文本（包括中文、空格、标点、`\r`）原样追加。替换可能改变原 span 内部的空格数量（例如 `deep seek harness` → `DeepSeek Harness`），但不会在 span 外添加空格，也不会改动大小写以外的其他内容。

## 拼音与中文写法

- 拼音就是普通别名，按用户写的样子比较：`shen du qiu suo` 归一化为 `shenduqiusuo`，与识别文本里的同名 token span 相等即命中；带空格、连字符、大小写差异都按分隔符处理。
- **不内置汉字→拼音表**：不会把 `深度求索` 自动转成 `shen du qiu suo`，也不会把中文发音映射到汉字；用户要两种写法都命中，就在别名里把两种都写上。
- 中文写法别名按“一字符一 token”匹配，可以出现在连续中文句子中间：`我们使用迪普西克。` → `我们使用DeepSeek Harness。`。
- 中文负例：`他深入研究了这个深度问题，并探索了求解思路。` 不包含连续的“深度求索”，因此不会被改写。

## 应用接入

- `Application::new` 的签名未变；默认术语表路径是 `<runtime>\glossary.txt`。可执行文件从 `--glossary` 解析出绝对路径后，在启动 GUI 前调用 `listen::glossary::set_default_path(...)`，因为窗口模块负责构造 `Application`。
- 测试注入用新增的 `Application::with_glossary(path)` builder（与 `with_targets` 同风格），不改变既有构造调用。
- `Control::Start` 在真正开始采集前重新 `load` 一次术语表；`Control::Stop` 的识别线程、流式识别 worker 的合并结果、`Control::Retry` 的保留音频识别，都在得到**最终文本**后调用 `glossary.correct(&text)`。`streaming.rs`、`Timing` 和分段时间戳完全没有改动。
- 术语表错误：`glossary_note` 会附加在采集中的 `detail` 和最终 `detail` 上（例如 `Glossary correction is unavailable: <path>: line 2: ...`），结果仍保存、仍交付、仍可复制，只是不校正。
- 文件缺失：空术语表，`detail` 不出现错误，文本原样交付。
- 重试沿用本次采集开始时加载的术语表（重试不是新的采集），因此“编辑后下一次采集生效”的语义不变。

## RED → GREEN 证据

按测试接缝分三轮红→绿，日志在 `E:\luyin\.artifacts\`：

1. **格式 + 精确校正**（`issue18-red1.log` → `issue18-green1.log`）。先写 `file_format_...` 测试，`correct` 只返回原文：
   ```text
   test glossary::tests::file_format_ignores_comments_and_blank_lines_and_restores_canonical_spelling ... FAILED
   assertion `left == right` failed
     left: "我们使用 deepseek harness 和 WASAPI，再用 cuda。"
    right: "我们使用 DeepSeek Harness 和 WASAPI，再用 CUDA。"
   ```
   实现解析、分词、精确匹配和 span 替换后转绿。

2. **近似匹配与负例**（`issue18-red2.log`）。`Deep sea harnessness.` 未被改写：
   ```text
   test glossary::tests::one_canonical_term_catches_its_measured_misrecognitions ... FAILED
   assertion `left == right` failed: input: "Deep sea harnessness."
     left: "Deep sea harnessness."
    right: "DeepSeek Harness."
   ```
   实现 Levenshtein 相似度、长度/首尾保护、token 组对齐和前导词后转绿；同一轮加入的负例一直保持通过。

3. **应用边界**（`issue18-app-boundary-red.log` → `issue18-green-all.log`）。先完成接入前的 `Application::with_glossary`，再临时关掉两处 `glossary.correct` 调用运行：
   ```text
   test corrected_text_is_delivered_and_saved_as_the_current_result ... FAILED
   test editing_the_glossary_applies_to_the_next_capture_without_a_restart ... FAILED
   test streamed_recognition_is_corrected_on_the_final_merged_text ... FAILED
   test a_retry_of_retained_audio_is_corrected_too ... FAILED
   test result: FAILED. 2 passed; 4 failed; 0 ignored
   ```
   恢复接入后 6 个应用边界测试全部通过；malformed 和缺失文件两个测试在红、绿两态都通过（它们验证“不丢采集”的另一半）。

## 测试与性能

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test -Manifest E:\luyin\.worktrees\issue-18\app\listen\Cargo.toml
& E:\luyin\.artifacts\run-cargo.ps1 -Task CheckFormat -Manifest E:\luyin\.worktrees\issue-18\app\listen\Cargo.toml
```

- 结果：未合并前 **82 passed, 3 ignored, 0 failed**（基线 64/3/0）；合入当前 `integration/v0.2` 后 **96 passed, 11 ignored, 0 failed**。新增测试没有修改既有断言。
- 新增 `glossary.rs` 单元测试 12 个：格式/注释/空行、缺失文件、一条条目多种实测误识别、普通句负例、别名/拼音/中文写法、短别名负例、默认打包术语表正例/负例、换行与标点保持、中文长句负例、malformed 行号与路径、性能。
- 新增应用边界测试 6 个：交付 + 当前结果、两次采集之间改文件、重试、流式合并文本、malformed 不丢采集、缺失文件。
- **性能测试**：300 条术语（每个词一条）、300 行转写、共 300 处替换，单次 `correct` 断言 < 100 ms；本机 debug 测试配置实测：
  ```text
   300-term correction took 28.5608ms
  ```
  多次运行（含合并 #17 后的完整套件）落在 26–37 ms。实现按“首/末字符桶 + 长度差 + token 数”先剪枝，候选只做短字符串 Levenshtein，且复用 DP 行缓冲；不对条目两两比较。

## 正例（输入来自本机 sherpa-onnx SenseVoice q8 实测）

| 识别原文 | 交付文本 |
| --- | --- |
| `我们使用deep seek harness来做测试。` | `我们使用DeepSeek Harness来做测试。` |
| `We use the DeepSek harnessness API for transcription.` | `We use the DeepSeek Harness API for transcription.` |
| `Deep sea harnessness.` | `DeepSeek Harness.` |
| `We use the Deepsek harnessness API.` | `We use the DeepSeek Harness API.` |
| `We use the deepse harnessness API.` | `We use the DeepSeek Harness API.` |
| `我们用re实现音频采集通过waapp loop back获取声音模型在本机运行使用CPU和进行对照。` | `我们用Rust实现音频采集通过WASAPI loopback获取声音模型在本机运行使用CPU和进行对照。` |
| `使用CPU和ku进行对照。` | `使用CPU和CUDA进行对照。` |
| `通过 wa-sa-pi 采集。` | `通过 WASAPI 采集。` |

说明：交付文本保持识别原文的空格。参考转写里的 `我们使用 DeepSeek Harness 来做测试。` 带空格，但本工具只在命中的英文 span 内替换，不会在中文与英文之间插入或删除空格，因此实际交付为 `我们使用DeepSeek Harness来做测试。`。这是“保留原句空格”的直接结果。

## 负例（保持原样）

- `The deep sea is calm, and we seek consensus.`（`deep`、`sea`、`seek` 各自普通词）
- `The safety harness is loose.`
- `The deep water harness is not our product.`（token 组对齐相似度 0.556 < 0.6）
- `Wasabi is a plant, not an audio API.`（0.833 < 0.85）
- `The result, there, and are ready; please re-do the review.`
- `Re: the meeting notes`、`The variable re is unused.`（短别名不在中文旁）
- `我们用 CPU 跑模型，本季的项目已经完成。`
- `请确认本机的音频设置，不要把本机写成本季。`
- `这段语音只在本机处理，不会上传到云端，请保留完整的发言内容，由我检查以后手动发送。`

## 交付包

- 默认术语表是 `packaging/glossary.txt`，`scripts/package-listen.ps1` 把它复制到交付目录根部；`scripts/verify-listen-package.ps1` 把 `glossary.txt` 加入必检清单。
- `packaging\启动 Listen.cmd` 增加 `--glossary "%~dp0glossary.txt"`，用户在包内直接编辑该文件即可；交付目录只读时删掉文件也不影响采集（空术语表）。
- `packaging/README.md` 增加了格式、生效时机和错误行为的简短说明。
- 本轮没有重新生成 `dist\Listen-*` 交付目录，也没有运行打包验收脚本；打包链路属于 #23，此处只保证文件被复制和必检。

## 故意不做 / 已知范围

- **不内置汉字→拼音表**：拼音别名与用户写法逐字符比较；同一词的拼音和汉字要分别写成别名。
- 不纠正单个字母（`W` → `Whisper`）和两字母缩写（`GP` → `GPU`）：这类 key 太容易误伤；实测契约只要求 `re`，而它被限制在中文上下文。需要时用户可在自己的术语表里加别名，但短别名仍受中文邻接规则约束。
- 不恢复引擎**完全丢掉**的词（例如实测 `使用CPU和进行对照` 里 `CUDA` 已消失，无法从文本恢复；只用 `ku`/`ku da` 覆盖还在文本里的情况）。
- 不处理标点、繁简、数字、同音字改写；这些不属于术语表校正范围。
- 不做解码期热词偏置：SenseVoice CTC 没有 hotword/prompt 入口，本层是识别后处理（与 ADR 0001、规格 #15 一致）。
- 不改时间戳、分段和窗口合并：校正只发生在最终文本上；`partial-result.txt` 检查点保持未校正原文。
- 术语规模按几十到几百设计；每次采集重读一次文件，不做文件监听或自动学习。
- 中文别名在连续中文里按字符窗口匹配，不依赖分词；极端情况下与更长中文词共享连续字符时可能命中（用户可删掉该别名缓解）。

## 合并记录

- `81cabf0`：合入 `integration/v0.2` 的 #16（准备期即可开始采集），冲突在 `lib.rs` 的采集 `detail` 组装处；保留双方内容（`preparation_note` + glossary note）。
- `ed93864`：合入 `integration/v0.2` 的 #17（SenseVoice 常驻引擎，`main.rs` 的 `Config` 增加引擎字段并把默认引擎改为 SenseVoice）与 #22 工具，冲突在 `main.rs` 的 `Config` 字段、HELP 和相对路径解析列表；保留 SenseVoice 配置并加入 `glossary` 字段/说明/路径。
- 合并后使用独立 target 目录重跑完整套件：`96 passed, 11 ignored, 0 failed`；`glossary_correction` 6 个应用边界测试和 `glossary` 12 个单元测试全绿，性能断言仍为 26.4 ms（本次合并后运行）。
