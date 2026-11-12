# Issue #22：真实会议语料录音与基准

这份文档只做两件事：让用户录完 10 句真实腾讯会议链路的语音；录完后用同一批音频对比 whisper 与 SenseVoice。录音工具只采集默认多媒体播放设备，**不做任何识别、不联网、不写目标输入框**。

## 一次性准备

1. 电脑和手机都加入**同一个腾讯会议**。手机开麦念句子；电脑端静音，不要用扬声器。
2. 确认 Windows 的默认播放设备是耳机（右键任务栏音量 → 声音设置 → 输出）。录音期间不要切换设备。
3. 构建录音器（只需一次；之后改代码才需要重跑）：

   ```powershell
   cd E:\luyin
   .\scripts\build-recorder.ps1
   ```

   成功后会打印 `...\app\listen\target\release\examples\capture_only.exe`。

> 如果提示“禁止运行脚本”，先执行 `Set-ExecutionPolicy -Scope Process Bypass`（只影响当前 PowerShell 窗口），再运行下面的命令。

## 录 10 句（大约 5 分钟）

先看一遍句子和每句要测什么（不会录音）：

```powershell
.\scripts\record-meeting-corpus.ps1 -ListOnly
```

开始录音：

```powershell
.\scripts\record-meeting-corpus.ps1
```

每句的流程：

1. 屏幕显示第 n 句和 `sentence-nn.txt`（中文可在这个文件里看）。
2. 按 Enter 开始；等 `RECORDING` 出现后按正常会议语速朗读，**不要刻意放慢**，每句控制在 12 秒内。
3. 读完按 Enter 停止，然后：
   - 直接按 Enter：接受这一句；
   - 输入 `r` 再按 Enter：重录这一句；
   - 输入 `q` 再按 Enter：先停下，已接受的句子不会丢。
4. 全部录完后脚本会打印 manifest 路径；缺任何一句都会以非零退出并列出缺的 id。

断点续录：再运行一次同一条命令，已录好且 SHA-256 匹配的句子会自动跳过。只想重录某一句：

```powershell
.\scripts\record-meeting-corpus.ps1 -Only numbers-units
```

常用参数：`-Output <目录>`（默认 `.artifacts\meeting-corpus`）、`-SentenceDelaySeconds 3`（按 Enter 后等 3 秒再开录）。

录音期间请只让会议声音从默认播放设备出来。Windows 在播放设备完全空闲时可能不产生 loopback 数据；如果脚本提示没有采到声音（no audio samples），先确认会议里有人在说话、默认播放设备正确，再重录这一句。

## 这 10 句分别在测什么

| # | id | 测什么 |
| ---: | --- | --- |
| 1 | `mandarin-long` | 普通话长句、多分句，简体输出与句读标点 |
| 2 | `product-names-whisper-sensevoice` | 产品名 Whisper / SenseVoice / sherpa-onnx 是否原样保留 |
| 3 | `product-names-deepseek-cuda` | DeepSeek Harness 连读、CUDA / CPU |
| 4 | `wasapi-loopback-tokens` | WASAPI loopback、HTTP 500、JSON 等短技术词 |
| 5 | `numbers-units` | 版本 1.8.2、端口 8080、3 次、2 秒、30 秒 |
| 6 | `homophones` | 本机 / 本季、在 / 再 |
| 7 | `mixed-chinese-english` | 中文里夹 PR / review / retry / merge / unit test |
| 8 | `digits-punctuation` | 冒号、分号、百分号、10:30 等数字与标点压力 |
| 9 | `meeting-scene` | 腾讯会议、耳机、只采集不识别等整句内容 |
| 10 | `english-acronyms` | 连续缩写 demo / MVP / API / SLA / QPS |

## 跑基准（同一批音频，分别跑两个引擎）

录音完成后，同一个 manifest 会同时用于两个引擎。默认结果写入 `.artifacts\meeting-corpus-bench\<engine>.json`。

whisper（显式启动参数）：

```powershell
cd E:\luyin
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus .artifacts\meeting-corpus `
  -Runtime .artifacts\meeting-bench\whisper-runtime `
  -Engine whisper `
  -ListenArgs @('--engine', "$PWD\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe", '--model', "$PWD\.tools\models\ggml-small.bin", '--vad-model', "$PWD\.tools\models\ggml-silero-v5.1.2.bin", '--backend', 'host')
```

SenseVoice（**#17 合并后**，默认参数；具体 flag 以 #17 为准）：

```powershell
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus .artifacts\meeting-corpus `
  -Runtime .artifacts\meeting-bench\sensevoice-runtime `
  -Engine sensevoice `
  -ListenArgs @()
```

说明：

- 传 `-ListenArgs` 表示由脚本启动 Listen，跑完自动退出；不传则附着到已经在跑、且 `-Runtime` 指向的 Listen 实例（此时 `-Engine` 标签由你声明，实际后端会写进结果 JSON 的 `backend` 字段）。
- 脚本逐句播放 WAV → 等 `phase=Capturing` → `--control stop` → 读 `runtime\status.txt` 的 `timing_stop_to_result_ms` 和 `runtime\current-result.txt`，并计算 CER、中文准确率、停止→结果、专名召回。
- `-Output` 可以指定结果文件；每句失败会记录 `phase=Failure` 与原因，不会编造文本或数字。
- 结果目录里同时存在 `whisper.json` 和 `sensevoice.json`（同一份 manifest）时，脚本才会生成 `docs\issue-22-benchmark.md`；只有一份时只打印部分表格并明确说缺哪个引擎。
- 跑基准时不要让焦点落在可编辑文本框上、也不要手动操作 Listen；脚本会驱动开始/停止。

## 之后发生什么

1. 两份结果 JSON 都存在后，`docs\issue-22-benchmark.md` 会由脚本生成，包含中文准确率、停止→结果、专名召回和逐句文本。
2. 根据结果在 issue #22 / #23 里记录启用闸门的结论与理由。**在看到真实录音数据之前不写结论。**
3. 语料 WAV、`sentence-*.txt` 和 `manifest.json` 留在 `.artifacts\meeting-corpus`，后续回归可直接重跑；manifest 的 SHA-256 会防止音频被替换。

## Comparison results (pending)

This section will hold the measured comparison once the ten sentences are recorded and both
engines have been run on the same manifest. Nothing below is a result yet; the harness never
fills in numbers that it did not read from `runtime\status.txt` and `runtime\current-result.txt`.

| Metric | whisper | SenseVoice |
| --- | ---: | ---: |
| Chinese accuracy (1 - micro CER) | pending | pending |
| Mean stop → result (ms) | pending | pending |
| Median stop → result (ms) | pending | pending |
| Proper-noun recall | pending | pending |

Gate decision: pending. It is recorded with its reasons after both result files exist and
`docs\issue-22-benchmark.md` has been reviewed; this guide does not choose an outcome in advance.
