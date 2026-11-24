# #22 真实会议语料基准与启用闸门

状态：**已完成，闸门结论「继续替换」**。测量日期 2026-11-24，代码 `7d727e4`（分支 `integration/sensevoice-v0.2`，与 `integration/cloud-recognition` 同点）。

本文只报告实测数字与其来源。原始结果、逐段文本和复跑命令都在下面；不看结果的结论不写。

## 语料：复用 #28 的 20 段真人录音

工单原文要求「10 句真实腾讯会议链路 WAV」。实际复用 [#28](https://github.com/xiaolin624/listen/issues/28) 已冻结的 20 段真人语料 —— 它是 10 句要求的**超集**：`docs/issue-22-recording-guide.md` 列的 10 个 id 与文本是 20 段计划的前 10 条，逐字相同（`quality/meeting-corpus.json` 与 `quality/cloud-meeting-corpus.json` 前 10 条比对一致）。

口径变更由用户于 2026-11-24 明确选择「复用现有 20 段真人语料」，理由是不必重新录音、且已有听回核对与哈希链。本次**没有**新录任何音频。

| 项 | 实测值 |
| --- | --- |
| 冻结清单 | `.artifacts\cloud-evaluation\frozen-v1-20261115-125129\manifest.json` |
| `evidence_kind` | `real-human-speech` |
| `recording_chain` | `human speaker on phone in Tencent Meeting -> PC in the same meeting -> default multimedia headphones -> capture_only loopback; no recognition` |
| 段数 / 时长 | 20 / 249.14 s |
| 端点 | `耳机 (WH-1000XM4)`，20/20 相同 |
| 音频在位 + 哈希 | 20/20 存在，20/20 SHA-256 匹配 |
| 参考原文 | 20/20 与录音清单 `spoken` 逐字相同，`reference_sha256` 20/20 复算一致 |
| 人工核对 | `reviewer=User self-review, confirmed in conversation`；`human_recording_confirmed/listened_to_audio/reference_verified` 20/20 为 `true`（2026-11-15） |

**证据边界**：这是用户自证级证据，不是第三方旁听；单说话人（`speaker-01`）、单设备、口音与背景未记录。`docs/issue-28-real-corpus-baseline.md` 已自认「工具只能核验文件与人工确认记录，不能仅凭 WAV 判断发声者是否为真人」。所以本次结论只覆盖「同一说话人、同一耳机链路」的真实会议音频，不能外推到多人多设备。

## 测量方法

两个引擎跑**同一份录音清单**、同一台机器、同一次连续会话，唯一的差别是引擎参数。用当前构建（`7d727e4` release）测，不用 2026-11-15 的旧基线二进制，这样两边代码同代。

`-Corpus` 用 `recording\manifest.json` 而不是 `frozen-v1` 目录：后者带 `baseline.file`，会让基准脚本自动切进 `-Baseline` 固定基线模式并拒绝 `-ListenArgs`，从而无法跑 whisper。两份清单指向同一批 20 个 WAV、同一参考原文、同一哈希。

```powershell
cd E:\luyin

# whisper
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus .artifacts\cloud-evaluation\recording\manifest.json `
  -Runtime .artifacts\meeting-bench\whisper-runtime `
  -Engine whisper `
  -ListenArgs @('--engine',"$PWD\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe",'--model',"$PWD\.tools\models\ggml-small.bin",'--vad-model',"$PWD\.tools\models\ggml-silero-v5.1.2.bin",'--backend','host','--threads','6','--glossary',"$PWD\.artifacts\cloud-evaluation\frozen-v1-20261115-125129\glossary.txt",'--hidden')

# SenseVoice
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus .artifacts\cloud-evaluation\recording\manifest.json `
  -Runtime .artifacts\meeting-bench\sensevoice-runtime `
  -Engine sensevoice `
  -ListenArgs @('--engine','sensevoice','--sherpa-lib',"$PWD\.tools\sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib",'--sensevoice-model',"$PWD\.tools\models\sense-voice\model.int8.onnx",'--sensevoice-tokens',"$PWD\.tools\models\sense-voice\tokens.txt",'--sensevoice-vad',"$PWD\.tools\models\sense-voice\silero_vad.onnx",'--sensevoice-language','auto','--threads','6','--glossary',"$PWD\.artifacts\cloud-evaluation\frozen-v1-20261115-125129\glossary.txt",'--hidden')
```

两条命令都显式传引擎参数。**不要**照抄 `docs/issue-22-recording-guide.md` 里的 `-ListenArgs @()`：那是 #40 之前的写法，今天不带 `--engine` 会走云端试用默认（本机凭据已配置），结果却仍被标成 `sensevoice`。

产物：`.artifacts\meeting-corpus-bench\whisper.json`、`.artifacts\meeting-corpus-bench\sensevoice.json`，以及脚本自动生成的比较文档 `docs/issue-22-benchmark.md`（含 20 段逐段文本）。

## 结果

| 指标 | whisper small | SenseVoice q8 | 判读 |
| --- | ---: | ---: | --- |
| 中文准确率（1 − micro CER，894 参考字符） | 66.4%（错 300 字） | **75.8%**（错 216 字） | SenseVoice 高 9.4 个百分点，错误字符少 28% |
| 平均单句准确率 | 64.6% | **80.8%** | 同上 |
| 停止→结果 中位 | 6,775 ms | **861 ms** | SenseVoice 快 7.9 倍 |
| 停止→结果 均值 | 6,812 ms | **952 ms** | 同上；whisper 全部 20 段都超过 5.8 s |
| 停止→结果 最快 / 最慢（SenseVoice） | — | 423 / 3,236 ms | 3.2 s 那段是 `wasapi-loopback-tokens`，见下 |
| 专名召回（5 词硬编码） | 40%（2/5：WASAPI、SenseVoice） | 20%（1/5：SenseVoice） | 见「专名」一节 |
| 识别 / 无语音 / 失败 | 20 / 0 / 0 | 19 / 1 / 0 | SenseVoice 的 1 段是 `english-qualified` |

### 与历史基线的对照

2026-11-15 同一批音频、固定基线二进制 `4b8a487` 的 SenseVoice 结果是准确率 75.39%、停止→结果均值 365 ms（`.artifacts\cloud-evaluation\baseline-run-20261115-125129.json`）。本次当前构建是 75.8% / 均值 952 ms。

准确率基本一致（+0.4 个百分点），**延迟明显变慢**（均值 365 → 952 ms）。原因是本次是 launch 模式、带着完整应用与设备路径在同一会话里连续跑 20 段，而历史基线的 `timing_stop_to_result_ms` 只覆盖最后一段的识别到结果；两者不是同一测量口径，不能直接相减。本次两引擎互相对比才是同口径，跨版本绝对值不作为结论。

### 专名

| 词 | whisper | SenseVoice | 说明 |
| --- | ---: | ---: | --- |
| `WASAPI` | 命中 | 漏（输出 `WAPI back`） | whisper 在英文缩写上确实更强 |
| `SenseVoice` | 命中 | 命中 | 两者都能原样保留 |
| `DeepSeek Harness` | 漏（`對不起,個哈奈斯`） | 漏（整句开头丢词） | 两个引擎都错，靠术语表校正 |
| `CUDA` | 漏（`褲打加速`） | 漏（`酷加速`） | 同上 |
| `sherpa-onnx` | 漏（`shifa ONX`） | 漏（空） | 同上 |

专名召回**whisper 更高**，这是 ADR 0001 已记录的设计取舍：SenseVoice 是纯 CTC，没有解码期热词/提示词偏置，专名只能靠识别后的本地术语表校正；whisper 可以用 prompt 补，但代价是慢 7 倍且中文输出繁体。本次沿用词表 `frozen-v1\glossary.txt`，两项都没命中说明**当前术语表没有覆盖这两个词**（`DeepSeek Harness`、`sherpa-onnx` 在 `packaging\glossary.txt` 里的写法需要复核），属于 #18 范围，不是引擎闸门的判据。

### 纯英文段

`english-qualified`（92 字符纯英文）被 SenseVoice 判为 `NoSpeech`，按口径记全错；whisper 在这段是 0% CER。这与 2026-11-15 基线完全一致，是**既有已知限制**：VAD + 非语音门禁在纯英文短段上会整段丢弃。它让 SenseVoice 的准确率被扣掉约 10 个百分点（92/894），也就是说 75.8% 是**低估**后的数字。工单范围是中文会议场景，本条记录为风险，不作为否决理由；若后续真实使用里纯英文发言变多，需要单独处理。

## 闸门判定

规格 #15 的闸门原文：SenseVoice 若在「中文准确率**或**停止延迟」上没有明显优于 whisper small，就暂停替换并重新评估。

**结论：继续替换。**

理由（只引用上表实测）：

1. 中文准确率 75.8% vs 66.4%，高 9.4 个百分点、错误字符少 28% —— 明显优于，且中文是这个工具的主场景。
2. 停止→结果 中位 861 ms vs 6,775 ms —— 快 7.9 倍；whisper 的 20 段全部落在 5.8–8.9 s，直接违背「停止到结果 <1 s」的产品目标；SenseVoice 19/20 段在 1 秒左右（唯一 3.2 s 的异常段见下）。
3. 两项判据是「或」关系，这里两项都明显占优，不需要在专名召回上做取舍判断。

需要如实带走的限制：

- 专名召回 whisper 更高（2/5 vs 1/5）。替换后 `WASAPI` 这类缩写会更容易漏，必须靠术语表补；本次词表对 `DeepSeek Harness`、`sherpa-onnx` 没生效，说明词表覆盖不足。
- 纯英文段 SenseVoice 会判 `NoSpeech`（既有行为，与基线一致）。
- `wasapi-loopback-tokens` 这段 SenseVoice 停止→结果 3,236 ms，明显高于其余 19 段的 423–1,009 ms。原因未查（该段 15.93 s、含多个 HTTP/JSON 短词，可能是 VAD 切出多段导致多次解码）。不改变闸门结论，但记录在案，若真实会议里长句常拖过 2 s 需要单独查。
- 证据是单说话人、单耳机链路、用户自证；不能外推到多人会议或其它设备。

## 反例与偏差

1. **10 句 → 20 段**：见文首。工单验收第一条「10 句录制完成」按「20 段真人录音的超集，且 10 句内容逐字包含其中」满足，没有新录音。
2. **`docs/issue-22-recording-guide.md` 当时的命令已过时**：它的「SenseVoice（默认参数，`-ListenArgs @()`）」在 #40 之后会走云端试用。该文档的待填比较表已改为指向本文与 `docs/issue-22-benchmark.md`；2026-11-24 复审又把「跑基准」两段命令换成本文「测量方法」里的当前命令（`-ListenArgs` 显式传引擎参数）。数字未改。
3. **比较文档由脚本生成**：`docs/issue-22-benchmark.md` 是 `benchmark-meeting-corpus.ps1` 在两份结果同 manifest 时的产物，重新跑基准会覆盖它，不要手改。
4. **本次未跑固定基线模式**（`-Baseline`）：那一模式测的是 `4b8a487` 旧二进制，与「今天两个引擎谁更好」无关；历史结果保留在 `.artifacts\cloud-evaluation\baseline-run-20261115-125129.json` 供追溯。

## 归档与复跑

| 产物 | 路径 | 是否入库 |
| --- | --- | --- |
| 20 段音频 + 录音清单 | `.artifacts\cloud-evaluation\recording\` | 否（gitignore） |
| 冻结清单 + 术语表 + 基线凭据 | `.artifacts\cloud-evaluation\frozen-v1-20261115-125129\` | 否 |
| 两引擎结果 JSON | `.artifacts\meeting-corpus-bench\{whisper,sensevoice}.json` | 否 |
| 逐段比较表 | `docs\issue-22-benchmark.md` | 是 |
| 本文 | `docs\issue-22-verification.md` | 是 |

复跑方式与上面两条命令相同；只要两份结果指向同一 manifest，脚本会重写 `docs\issue-22-benchmark.md`。音频、参考原文与词表都有 SHA-256 冻结，换设备或换说话人需要重新录音并冻结新版本，不能与本次成绩混用。
