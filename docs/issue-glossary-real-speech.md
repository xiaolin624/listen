# 术语表在真实语音上失效：默认未加载与匹配过严

2026-11-12。分支 `ticket/glossary-real-speech`（工作树 `E:\luyin\.worktrees\glossary-real-speech`），基线 `integration/v0.2` 的 `aa78bcc`。

来源：用户第一次真实运行的十句会议语料（`quality/meeting-corpus.json`，SenseVoice q8，运行时 `E:\luyin\.artifacts\listen`；十句识别文本在 `.artifacts/user-run-cer.py`）。整段 micro CER 13.3%，单句 2.6%–32.6%。术语表是产品里专名与技术词的唯一防线，但这次运行实际上**没有加载术语表**；即使加载，匹配器也几乎改不动真实语音的变形。

修复后：**171 passed / 0 failed / 15 ignored**，`-Task CheckFormat` 退出码 0；十句里的第 2、3、4 句恢复出 `SenseVoice`、`DeepSeek Harness`/`CUDA`、`WASAPI loopback`/`JSON`，其余句子和已知普通词负例保持不动。真机 A/B（同一 WAV、同一引擎、同一二进制，只差 `--glossary`）也证明校正链路真的生效，见第 6 节。

## 1. 两个缺陷

### A. 术语表根本没有加载

- `main.rs` 默认把术语表定为 `<runtime>\glossary.txt`；用户运行时该文件不存在。
- 开发启动器 `scripts/run-listen.ps1` 只传 `--runtime`，从不传 `--glossary`，仓库里的 `packaging/glossary.txt` 因此从未进入应用。
- `Glossary::load` 对缺失文件返回空表，于是整场运行没有任何校正；`status.txt` 也没有任何字段能看出术语表是否加载。

结果就是：十句里只有 `ku打加速` → `CUDA打加速` 一处变化（来自已有的 `CUDA | ku` 短别名），与用户观察一致。

### B. 即使加载，匹配器也处理不了真实的词长变形

旧规则要求逐 token 对齐：单 token 的 key 只与单 token 的窗口比较；多 token 的 key 要求每个 key token 至少覆盖一个 span token，且分组长度差不超过 40%、相似度不低于 0.6。真实语音恰好破坏这些前提：

| 识别片段 | 规范化 span | 规范化 key | 旧规则为什么没中 |
| --- | --- | --- | --- |
| `sens voice` | `sensvoice`（2 token） | `sensevoice`（1 token） | 单 token key 只接受单 token 窗口；整串相似度其实是 0.90 |
| `de harness` | `deharness`（2 token） | `deepseekharness`（2 token） | `de` vs `deepseek` 长度差 75%，连比较都不会发生 |
| `waSAPpilook back` | `wasappilookback`（2 token） | `wasapiloopback`（2 token） | `wasappilook` vs `wasapi` 长度差 45%，整串相似度其实有 0.867 |
| `jason` | `jason` | — | `JSON` 不在术语表，且 4 字母 key 低于旧的最小 key 长度 |

旧匹配器针对 TTS 语料调参，默认“词数不变、只错几个字母”；用户自己的会议语音会把一个词拆成两个、截断首词、把缩写念成一个长音，token 级一致性因此最先失效。

## 2. 缺陷 A 的修复

1. **开发启动器默认使用随包术语表**：`scripts/run-listen.ps1` 新增 `-Glossary` 参数，默认 `packaging\glossary.txt`（相对工作树根目录，`--control` 调用不受影响）；传入的路径按 Windows 命令行规则加引号，并在启动输出里打印 `Glossary: ...`。
2. **运行期默认回退**：`glossary::default_paths` 按优先级返回候选文件：
   - 显式 `--glossary`：只用它，不回落；
   - 否则 `<runtime>\glossary.txt`；
   - 再否则 `listen.exe` 旁边的 `glossary.txt`（复制出去的便携构建也能找到随包文件）；
   - 都不存在：加载 runtime 路径，仍是“缺失 = 空表、无警告”。
   应用在每次开始采集、重新读取术语表时重新选择第一个存在的候选，所以运行中新建 `<runtime>\glossary.txt` 仍会生效。
3. **status.txt 可诊断**：新增 `glossary=<path|none>`，写明本次实际使用的文件；文件缺失时为 `none`。格式错误的文件仍显示路径，并像以前一样在 `detail` 里写明 “Glossary correction is unavailable: <path>: line N ...”，本次转写照常交付。
4. **`--glossary` 仍然最优先**：`main.rs` 只在显式传入 `--glossary` 时设置进程级默认路径，因此显式路径不会静默回落到其它文件。

## 3. 缺陷 B 的修复：三条匹配规则

所有新规则都继续要求首尾字符一致、key 至少 5 个字符、长度差不超过 40%，并且只在**结构化 token 对齐失败后**才执行，因此不改变已经命中的 TTS 变体，也不改变句子的顺序、空格和分段。

### 3.1 整串比较（whole-span fallback）

当 token 数不同，或者 token 级分组对不上时，把整个规范化的 span 与整个规范化的 key 比较：

- 只对 Latin key，且 key 长度 ≥ 6；
- `ratio >= 0.85`，首尾字符必须相同；
- 阈值定在 0.85 是因为实测 `sensvoice`/`sensevoice` = 0.90、`wasappilookback`/`wasapiloopback` = 0.867；而 `wasabi`/`wasapi` = 0.833 和 `deepwaterharness`/`deepseekharness` = 0.75 都被挡在门外。
- 中文 key 不走整串规则，继续使用更严格的逐字路径，避免把普通中文句子整段替换；没有引入汉字→拼音表。

### 3.2 缩写的单次元音增删（acronym rule）

对全大写缩写（至少 4 个字符，如 `JSON`、`HTTP`、`CUDA`、`WASAPI`），允许 span 与 key 相差一个**元音**的插入或删除，且首尾字符一致：

- `jason` → `JSON`（去掉一个 `a`，相似度 0.80）；
- `waspi` → `WASAPI`（少一个 `a`）；
- **不接受替换**：`wasabi` → `WASAPI`（b/p 替换）、`coda` → `CUDA`（o/u 替换）都保持不动；
- 3 字母缩写（`API`、`MVP`、`SLA`、`QPS`）和 2 字母缩写（`PR`）只做精确匹配。`QOS` 与 `QPS` 只差一个替换，3 字母放宽会把 QoS 误改；`PRR`→`PR`、`SL`→`SLA` 同理不可靠（见第 5 节）。

### 3.3 首词截断（truncated leading word）

多词 key 的首词被截成前缀时，只要：

- key 至少两个 token，首词长度 ≥ 6；
- span 首词是 key 首词的**真前缀**，长度 ≥ 2 且严格短于 key 首词；
- 其余每个 token 首尾字符一致且相似度 ≥ 0.85；

就接受整段替换。例如 `de harness` → `DeepSeek Harness`、`wa loopback` → `WASAPI loopback`。而 `the harness`、`do harness`、`deployment harness`、`deep water harness` 都不满足“首词前缀 + 其余词精确”而保持原样。

### 3.4 新增的术语

`packaging/glossary.txt` 仍保持“一个词一条”，只增加用户会议里真实出现的技术词，不枚举误写：

```text
JSON
HTTP
API
MVP
SLA
QPS
PR
```

`CUDA` 原本已在文件中。4 个字母以上的 `JSON`/`HTTP`/`CUDA` 还能吃到 3.2 的元音规则；`API`/`MVP`/`SLA`/`QPS`/`PR` 只恢复大小写（精确匹配），不为它们放宽近似阈值。`PR` 是 2 字母 key，命中时走已有的 exact 路径，不会压到普通单词。

### 3.5 性能

300 条术语、300 句转写的既有预算测试仍通过；整串回退只在 token 对齐失败后发生，普通文本和 TTS 变体不增加比较。`Test` 全量运行中该测试为 40.1ms（门槛 100ms；单独运行时约 18.6ms，并行测试时会有调度抖动，但未再超限）。

## 4. 十句的前后结果

“修复前”是旧匹配器配当前 `packaging/glossary.txt` 时的 `Glossary::correct()` 输出；“修复后”是新匹配器配同一个文件的输出。识别文本来自用户的真实运行（`.artifacts/user-run-cer.py`），不是重新朗读。

| # | 修复前（关键片段） | 修复后（关键片段） | 结果 |
| ---: | --- | --- | --- |
| 1 | `我先同步一下今天的进展，…` | 同左 | 无术语，未变 |
| 2 | `…从w换成sens voice，现在用 ON X…` | `…从w换成SenseVoice，现在用 ON X…` | **sens voice → SenseVoice**；`w`、`ON X` 见缺口 |
| 3 | `de harness的接口…ku打加速…` | `DeepSeek Harness的接口…CUDA打加速…` | **de harness → DeepSeek Harness**；CUDA 旧规则已能修 |
| 4 | `…waSAPpilook back…能看到jason…` | `…WASAPI loopback…能看到JSON…` | **两条都修好** |
| 5 | `…1.8.2…8080…` | 同左 | 无术语，未变 |
| 6 | `…要在本季部署…` | 同左 | 本季/本机 是中文同音词，见缺口 |
| 7 | `这个 PRR…retryed…d stay unit test.` | 同左 | 短缩写与普通英文，见缺口 |
| 8 | `…回度比例悬空制在5%…` | 同左 | 识别错误，不是专名 |
| 9 | `地方在腾讯会议里说话…` | 同左 | 无术语，未变 |
| 10 | `今天的de先MVPAPI文档…SL…` | 同左 | 粘连/短缩写，见缺口 |

完整十句的前后输出见第 7 节日志；其中只有第 2、3、4 句发生变化。

## 5. 仍然存在的缺口与原因

这些不是“再调一下阈值”就能安全解决的，全部保留原样并在此记录：

| 缺口 | 为什么不改 |
| --- | --- |
| `本季` → `本机`（第 6 句） | 这是中文同音词，不是专名；`本季` 本身是正常词（“这个季度”），加入 `本机 \| 本季` 会在真正的“本季度”语境里误改。术语表不做上下文语义改写。 |
| `w` → `Whisper`（第 2 句） | 1 个字母，低于最小长度；任何近似规则都会把普通单字母替换掉。 |
| `ON X` → `sherpa-onnx`（第 2 句） | 规范化后是 `onx` 对 `sherpaonnx`，长度和内容都差太远；没有可靠的字形/读音对应。 |
| `PRR` → `PR`（第 7 句） | key 只有 2 个字母，`prr` 对 `pr` 的相似度 0.667；放宽到能命中就会误伤普通短词和其它缩写。 |
| `SL` → `SLA`（第 10 句） | key 3 个字母、span 2 个字母，相似度 0.667；且 `SLO` 等相邻缩写只差一个字符。 |
| `retryed` → `retry`（第 7 句） | `retry` 是普通英文动词，不是专名；把普通小写词加入术语表会扩大改写面，且该词在句中出现时大小写/位置都不可靠。 |
| `d stay` → 删除（第 7 句） | 术语表只能替换匹配到的 span，不能删除与目标词相邻的多余 token；`d stay unit test` 是插入错误，不是 `unit test` 的拼写变体。 |
| `de先` → `demo 先`（第 10 句） | `de` 只有 2 个字母，且 `demo` 是普通英文缩写；`de` 与 `demo` 的相似度 0.5，没有安全规则。 |
| `MVPAPI` 拆成 `MVP`、`API`（第 10 句） | 引擎把两个缩写粘成一个 token，术语表不做分词；为这一种粘连加 `MVPAPI` 别名属于枚举误写。 |
| `HTTP500` 的空格、`模型接口` 等（第 4 句） | 识别层的漏字/粘连/标点问题，不是专名替换；术语表不能用别名补空格或插入句子成分。 |

## 6. 验证

### 6.1 测试

```powershell
& E:\luyin\.artifacts\run-cargo.ps1 -Task Test `
    -Manifest E:\luyin\.worktrees\glossary-real-speech\app\listen\Cargo.toml `
    -TargetDir E:\luyin\.artifacts\cargo-target-glossary
```

结果：**171 passed / 0 failed / 15 ignored**（基线 163 passed / 0 failed / 14 ignored）。新增/覆盖：

- `the_users_real_meeting_transcripts_are_corrected_where_the_glossary_can`：十句前后回归夹具；
- `the_shipped_glossary_restores_the_corpus_acronyms`：`jason`→`JSON`、`http`→`HTTP` 等；
- `short_acronym_deformations_are_left_alone_instead_of_guessed`：`SL`、`PRR`、`SLO` 不动；
- `the_looser_whole_span_rule_leaves_ordinary_words_and_phrases_alone`：`we seek`、`deep sea is calm`、`safety harness`、`deep water harness`、`wasabi`、`rest`、`deck`、`sense`、`voice`、`device`、`service` 不动；
- `a_truncated_leading_word_is_recovered_only_with_the_rest_of_the_term`；
- `the_default_glossary_falls_back_to_the_copy_next_to_the_executable`；
- 应用边界 `the_active_glossary_file_is_reported_for_status_txt`、`no_glossary_file_is_reported_as_none_and_still_delivers`。

`-Task CheckFormat` 退出码 0。

### 6.2 匹配器前后诊断

诊断是仓库里的忽略测试，运行：

```powershell
cargo test --lib -- --ignored --nocapture diagnostic_real_meeting_lines
```

修复前（`aa78bcc`）只有第 3 句的 `ku打加速` → `CUDA打加速`，第 2、4 句完全没改；修复后第 2、3、4 句分别得到 `SenseVoice`、`DeepSeek Harness`、`WASAPI loopback` 和 `JSON`。原始输出保存在：

- `.artifacts/issue-glossary-real-speech/diagnostic-before.txt`
- `.artifacts/issue-glossary-real-speech/diagnostic-after.txt`

### 6.3 真机运行（构建出的 listen.exe + 开发启动器）

启动：

```powershell
.\scripts\run-listen.ps1 -Runtime E:\luyin\.artifacts\issue-glossary-real-speech\runtime -Hidden
```

输出包含：

```text
Engine: SenseVoice (default resident engine, sherpa-onnx)
Glossary: E:\luyin\.worktrees\glossary-real-speech\packaging\glossary.txt
```

`runtime\status.txt`（启动后 Ready）：

```text
backend=SenseVoice q8, resident (sherpa-onnx), CPU, 6 thread(s); ...
glossary=E:\luyin\.worktrees\glossary-real-speech\packaging\glossary.txt
preparation=Ready
```

然后用同一个构建、同一段 `quality-corpus/mixed-technical.wav`、同一个运行目录做 A/B：只改 `-Glossary`，一次用随包术语表，一次指向不存在的文件（`glossary=none`）。两次都真实采集、真实 SenseVoice 识别，结果：

```text
with glossary : 我们用 Rust实现音频采集通过 WASAPI loopback获取声音模型在本机运行使用 CPU和进行对照。
without       : 我们用 re实现音频采集通过 waapp loop back获取声音模型在本机运行使用 CPU和进行对照。
```

`re` → `Rust`、`waapp loop back` → `WASAPI loopback` 都是真实引擎输出被术语表改写，证明加载、匹配、交付链路都在真实二进制里生效。证据在：

- `.artifacts/issue-glossary-real-speech/launch.txt`
- `.artifacts/issue-glossary-real-speech/status.txt` / `status-after-capture.txt`
- `.artifacts/issue-glossary-real-speech/capture/mixed-technical.json`
- `.artifacts/issue-glossary-real-speech/capture-noglossary/mixed-technical.json`
- `.artifacts/issue-glossary-real-speech/capture-comparison.txt`

## 7. 十句诊断原始输出

修复前（旧匹配器；`MISMATCH` 表示与期望不一致）：

```text
line  1 [unchanged]
  recognized: 我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。
  corrected : 我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。
line  2 [MISMATCH]
  recognized: 我们把语音识别成w换成sens voice，现在用 ON X在本地跑推理。
  corrected : 我们把语音识别成w换成sens voice，现在用 ON X在本地跑推理。
line  3 [MISMATCH]
  recognized: de harness的接口已经调通了ku打加速这一版先不开，选用CPU把流程抛稳。
  corrected : de harness的接口已经调通了CUDA打加速这一版先不开，选用CPU把流程抛稳。
line  4 [MISMATCH]
  recognized: 采集走的是waSAPpilook back音频直接送进模接口偶尔返回HTTP500日志里能看到jason解析失败。
  corrected : 采集走的是waSAPpilook back音频直接送进模接口偶尔返回HTTP500日志里能看到jason解析失败。
line  5 [unchanged]
  recognized: 这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。
  corrected : 这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。
line  6 [unchanged]
  recognized: 本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。
  corrected : 本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。
line  7 [unchanged]
  recognized: 这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.
  corrected : 这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.
line  8 [unchanged]
  recognized: 结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。
  corrected : 结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。
line  9 [unchanged]
  recognized: 地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。
  corrected : 地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。
line 10 [unchanged]
  recognized: 今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。
  corrected : 今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。
```

修复后：

```text
line  1 [unchanged]
  recognized: 我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。
  corrected : 我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。
line  2 [fixed]
  recognized: 我们把语音识别成w换成sens voice，现在用 ON X在本地跑推理。
  corrected : 我们把语音识别成w换成SenseVoice，现在用 ON X在本地跑推理。
line  3 [fixed]
  recognized: de harness的接口已经调通了ku打加速这一版先不开，选用CPU把流程抛稳。
  corrected : DeepSeek Harness的接口已经调通了CUDA打加速这一版先不开，选用CPU把流程抛稳。
line  4 [fixed]
  recognized: 采集走的是waSAPpilook back音频直接送进模接口偶尔返回HTTP500日志里能看到jason解析失败。
  corrected : 采集走的是WASAPI loopback音频直接送进模接口偶尔返回HTTP500日志里能看到JSON解析失败。
line  5 [unchanged]
  recognized: 这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。
  corrected : 这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。
line  6 [unchanged]
  recognized: 本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。
  corrected : 本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。
line  7 [unchanged]
  recognized: 这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.
  corrected : 这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.
line  8 [unchanged]
  recognized: 结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。
  corrected : 结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。
line  9 [unchanged]
  recognized: 地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。
  corrected : 地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。
line 10 [unchanged]
  recognized: 今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。
  corrected : 今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。
```
