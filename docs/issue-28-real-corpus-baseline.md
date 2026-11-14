# #28：20 段真实语料与固定本地基线

状态：**工具就绪，验收前提未满足**。截至 2026-11-14，没有可核验的 20 段真实录音、听回核对的参考原文和本次基线实跑结果，不能关闭 #28，也不能据此选择云端默认厂商。本页不报告识别成绩。

权威要求：[规格 #26](https://github.com/xiaolin624/listen/issues/26)、[任务 #28](https://github.com/xiaolin624/listen/issues/28)。沿用现有 `record-meeting-corpus.ps1`、`benchmark-meeting-corpus.ps1` 的录音与实机重放能力，扩展其整批 CER 计算和结果来源核验。旧 #22 的十句流程继续可用，不是本任务的完成前提。

## 当前已经准备好的内容

- `quality/cloud-meeting-corpus.json` 是独立的 20 段录制计划，覆盖中文长句、中英技术词、产品/项目名、数字与单位、否定和不确定表达；它不是已存在的音频。
- 录音器支持 `-Corpus` 选择计划，每段保存稳定 id、PCM WAV、SHA-256、采样参数、时间和端点信息，断点续录；新计划会回放后再让人确认接受。不同计划不能混进同一输出目录。
- `prepare-meeting-baseline.ps1` 在独立工作树离线构建固定版本，保存精确参数、二进制/模型/库/术语表/构建日志哈希，不启动应用或录音。
- `evaluate-meeting-corpus.py review-template/freeze` 要求 20 段声音、逐段听回核对、真人来源确认、录制条件和关键表达，固定参考原文及术语快照；未核对、变更或重复音频不能算满 20 段。
- `benchmark-meeting-corpus.ps1 -Baseline` 从构建凭据启动固定二进制，核验文件哈希、实际后端及实际术语表，逐段重放同一 WAV 并保留全文、失败和耗时。它不触发精修或上传。
- 新评分器按整个固定集合计分，漏跑、失败、超时和 `NoSpeech` 都不会缩小分母；关键表达始终需要人工逐项判定，出现词表中的词不代表表达正确。

## 已有文件为何不算本次语料

只读检查了原工作区 `.artifacts` 和 `dist`。`meeting-corpus-smoke` 只有数秒的录音器冒烟文件，没有 20 段对应的参考原文和真人来源核验；`quality-corpus/manifest.json` 明确记载 `System.Speech` 生成器；`issue-glossary-real-speech/capture/mixed-technical.json` 的源哈希对应这份合成语音。其它保留音频是开发/恢复/回放夹具，没有形成可核验的本次 20 段集合。

旧用户十句的保存文字只能复算旧识别结果，13.3% / 10.5% 是背景，不能变成本次重新听原音的成绩。不得用 TTS、旧识别文字或脚本测试夹具填补缺失录音。本工具只能核验文件与人工确认记录，不能仅凭 WAV 判断发声者是否为真人。

## 固定本地版本

当前本地基线固定为 `4b8a487e5e6cb5aaedf8bfe8084291cc24540545`，即云端会话重构之前的本地实现。明确使用 SenseVoice q8、Silero VAD、CPU 6 线程、`auto` 语言，并传入从该版本 `packaging/glossary.txt` 复制的固定术语表。不能依靠运行目录中恰好存在的另一个词表。

准备命令（无音频采集、播放、识别或联网）：

```powershell
cd E:\luyin
.\scripts\prepare-meeting-baseline.ps1
```

默认产物在 `E:\luyin\.artifacts\cloud-evaluation\baseline-4b8a487`。此目录已经在本机成功构建；若 `baseline.json` 已存在，脚本拒绝覆盖。需要重新构建时传入一个新的 `-Output` 目录并重新冻结语料。

本机 2026-11-14 的构建记录（仅是构建证据，没有识别成绩）：

| 项目 | SHA-256 |
| --- | --- |
| `listen.exe` | `b86ae45ddbbbbbad2d2dfcf49b3bfca3b892815f5d4043869903277f2d24f42a` |
| 固定术语表 | `21f91cfb5699593b59f6ff516fac2d7c7f5d952ae15566dfeaa8f97cecc7b66c` |
| SenseVoice q8 | `c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51` |

其余哈希、完整启动参数、构建日志和干净源工作树位置在该目录的 `baseline.json`。保留这些文件和被引用的模型/库，不向日常启动器写入设置。

## 需要真人配合的具体步骤

用户需要约 10 分钟，手机和电脑各加入同一个腾讯会议。手机开麦读句子，电脑保持静音、声音从默认播放设备的耳机播放，避免扬声器回授。选用等价耳机链路时，先把实际链路写入计划的 `recording_chain` 并固定计划，再从新目录开始录；不要复用不符链路的说明。

先只预览（不会开录音设备）：

```powershell
cd E:\luyin
$evaluationRoot = 'E:\luyin\.artifacts\cloud-evaluation'
.\scripts\record-meeting-corpus.ps1 `
  -Corpus .\quality\cloud-meeting-corpus.json `
  -Output "$evaluationRoot\recordings" -ListOnly
```

准备好手机、会议、耳机并且可以发言后，再由用户运行：

```powershell
.\scripts\build-recorder.ps1
.\scripts\record-meeting-corpus.ps1 `
  -Corpus .\quality\cloud-meeting-corpus.json `
  -Output "$evaluationRoot\recordings"
```

每段按 Enter 开始，等出现 `RECORDING` 后正常语速朗读；读完按 Enter 停止，听回这一段。直接 Enter 接受，`r` 重录，`q` 暂停；下次同命令会跳过哈希匹配的已接受录音。只重录某一段可追加 `-Only uncertainty`。每段是独立发言，重复、口头语和限定语要保留，不要求把长句赶在 12 秒内读完。

用户必须确认这 20 段来自真人。参考原文要听声音后核对，不能拿模型输出当答案。记录说话人代号、口音、日期、实际链路、耳机和背景声即可，不记真实姓名、会议号或其它无关私人信息。

## 核对原文并冻结

录齐后生成核对表，已有核对表不会被覆盖：

```powershell
python .\scripts\evaluate-meeting-corpus.py review-template `
  --definition .\quality\cloud-meeting-corpus.json `
  --recordings "$evaluationRoot\recordings\manifest.json" `
  --output "$evaluationRoot\reference-review.json"
notepad "$evaluationRoot\reference-review.json"
```

逐段听回后填写：

- 顶层 `evidence_kind` 设为 `real-human-speech`；测试夹具只能用 `tooling-fixture`，不会满足质量验收条件。
- 每段确认 `human_recording_confirmed`、`listened_to_audio`、`reference_verified` 为 `true`，填写 `reviewer`、带时区的 `reviewed_at` 和 `recording_conditions`。
- `reference` 改成真正听到的完整原话，保留重复；同步核对 `expressions` 中的技术词、英文、数字、否定和不确定表达，`expected` 必须出现在实际原文里。
- 数字只在同值、同符号、同单位时可列为 `number_equivalences`。例如 `{"canonical":"2 秒","variants":["两秒","二秒"],"verified":true,"reason":"听回确认为两秒，书写等价"}`。在基线和两家候选比较前固定，不按识别结果临时调整。解析器不能证明的口语形式保留为内容差异并在人工检查中说明。

冻结到一个全新目录：

```powershell
python .\scripts\evaluate-meeting-corpus.py freeze `
  --definition .\quality\cloud-meeting-corpus.json `
  --recordings "$evaluationRoot\recordings\manifest.json" `
  --review "$evaluationRoot\reference-review.json" `
  --baseline "$evaluationRoot\baseline-4b8a487\baseline.json" `
  --output "$evaluationRoot\frozen-v1"
```

`frozen-v1/manifest.json` 是后续本地、百炼、豆包必须共用的固定集合；同目录保存术语表和基线凭据副本。音频按绝对路径引用并核验哈希，不复制进源码。重录或改参考原文后需要重新生成核对表、冻结新版本，并重跑全部候选；不能混用不同版本的成绩。

## 让基线实际听这 20 段声音

先退出日常 Listen，保持耳机和默认播放设备不变。运行期间不要播放其它声音，也不要把焦点停在可编辑输入框，以免基准驱动正常交付。此步骤通过真实 WASAPI 重放/采集路径识别，单独使用评测运行目录。

```powershell
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus "$evaluationRoot\frozen-v1\manifest.json" `
  -Baseline "$evaluationRoot\frozen-v1\baseline.json" `
  -Runtime "$evaluationRoot\baseline-runtime" `
  -Output "$evaluationRoot\baseline-run.json"
```

不传 `-ListenArgs` 或 `-ListenBinary`：它们来自已固定的凭据。脚本逐段记录来源音频哈希、完整文字、实际后端/术语表、失败原因、应用停止到结果时间和外部计时。若中途超时或致命失败，未跑的尾段也保存为 `NotRun`；不要删除失败记录来凑满 20 个成功结果。确需修复环境重跑时，保留原文件，使用新的运行输出文件名。

新口径的权威报告是 `baseline-run-evaluation.json` / `.md`。原始 JSON 中保留旧 #22 字段以兼容旧读取者，不能把旧归一化后的 `summary.chinese_accuracy` 当作 #26 内容成绩。脚本不会覆盖旧 `docs/issue-22-benchmark.md`。

## 逐项检查关键表达并复跑评分

首次评分自动生成 `baseline-run-evaluation-key-review.json`，所有判断默认为 `pending`。每项都列出参考表达和该段完整识别文字，`presence_hint` 只是定位帮助，不是通过判定。

听回并比对全文后填写核对人的 `reviewer`、`reviewed_at`；每一项填写 `verdict`（`pass` 或 `fail`）、`observed`（识别出的片段，漏失可写“漏失”）和必要说明。出现同一个词但否定、数字或语境错误时仍判失败。然后执行：

```powershell
python .\scripts\evaluate-meeting-corpus.py score `
  --manifest "$evaluationRoot\frozen-v1\manifest.json" `
  --results "$evaluationRoot\baseline-run.json" `
  --reviews "$evaluationRoot\baseline-run-evaluation-key-review.json" `
  --output "$evaluationRoot\baseline-reviewed.json"
```

关键表达核对绑定音频/参考集合和完整结果文件的哈希，不能套在另一轮结果上。`acceptance_prerequisites_met` 只表示这份基线证据资料齐备，不表示云端准确率或完整回填速度已经达标；基线本身的错误必须如实保留。

## 固定计分口径

内容 CER 是 `(累计替换 + 删除 + 插入) / 全部 20 段参考字符`，不是逐段百分比的平均。使用与旧评测相同的 Levenshtein 递推并保留确定的最短路径分类；最短对齐有歧义时单项分类可以依赖此固定规则，总编辑数不变。

统一 NFKC 与大小写，忽略空白、一般句读；数字间的点、冒号、逗号、斜线、数字负号，以及百分号、单位和有意义符号仍参与内容比较。因此 `1.8.2 ≠ 18.2`、`3:15 ≠ 315`、`-3 ≠ 3`、`5% ≠ 5`，也不会删除“不”“可能”。只有事前逐段核对且解析器能证明同值/同符号/同单位的数字写法才可等价，不能把 `30次` 或 `不三次` 归成 `3次`。

标点、大小写、空白和已核对的等价数字书写差异单列；完整参考与完整输出始终保存。“两三次”“一两次”表达范围，不能当成 23/12；“三百二”有省略歧义，不能当成 302。解析器只接受保守的正式数词和包含零的明确逐位读数（例如“八零八零”），其余书写保留为内容差异供人工说明。失败、超时、`NoSpeech` 或漏跑按空输出计删除，失败携带的旧结果或未完成草稿不作为最终文字。漏跑会让“验收前提”保持未满足，即使仍计算了保守的整批 CER。

这里记录的停止到结果时间不等于外部目标完整读回确认时间，不能用来声称满足“20 次至少 19 次一秒内完整回填”。双厂商比较、费用/条款、流式回填速度和恢复验收属于 #26 后续任务。

## 数据与验证边界

全部评测录音、参考核对、构建凭据、运行结果置于 `.artifacts/cloud-evaluation`，已由 Git 忽略；它与生产 `.artifacts/listen` 的待处理录音、待取结果和清理策略分开。保留评测原音以便复跑，不从生产目录自动采集、上传或删录音，不提交密钥或私人音频。

工具行为测试可运行：

```powershell
python -m unittest discover -s scripts/tests -v
```

这些测试创建带 `tooling-fixture` 标签的临时文件，只验证计分、来源核验和人工确认规则。Windows 录音器/重放入口仅测试 `-ListOnly` 与错误配置在开录前拒绝；没有把夹具当成准确率证据，也没有实际录音或播放。本次另外完成了固定版本的 `--offline --locked --release` 构建。
