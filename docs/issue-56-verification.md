# #56 云端型号可切换：`--ali-model` 与 message/streaming 的 A/B 实测 — 验证

状态：**开关与测量完成；默认型号未切换**（是否切换交回父级决定）。
工单 [GitHub issue #56](https://github.com/xiaolin624/listen/issues/56)。
分支 `implement/issue-56-ali-model-a-b`，基线 `8cea066`（= `main` 尖端），
实现提交 `a1c4129`（本文件随后单独提交，不再改产品代码）。
工作树 `E:\luyin\.worktrees\ab-model`。主工作树 `E:\luyin` 的 7 份用户改动全程未被
触碰、未 add、未 commit、未 stash。

## 结论（草案）

| 判据 | 对照 `message` | 候选 `streaming` | 谁更好 |
| --- | ---: | ---: | --- |
| 内容 CER（`chinese_accuracy` = 1 − 微平均 CER） | **96.09%**（35/894 字错） | **94.30%**（51/894 字错） | message，+1.79 pp |
| 停止→结果 中位 / 最大 | **1,010 ms / 3,296 ms** | **893 ms / 3,125 ms** | streaming，中位快 117 ms |
| 识别 / 无语音 / 失败（段） | 20 / 0 / 0 | 20 / 0 / 0 | 平 |
| 专名与数字保真 | `1.8.2`、`Listen`、`WASAPI`、`本机` 均正确 | `1.8.2`→`一点八点二`、`Listen`→`Ethan`、`WASAPI`→`Wasaapi`、`本机`→`本期` | message |

**结论：本票不建议切换默认型号。** 以 20 段冻结语料和「每段一次 start/stop、每次一个
独立任务」的现有采集形态，`streaming` 只把停止→结果中位改善约 0.12 s（相对 1.01 s 约
11.6%），却让内容 CER 退步 1.79 pp，并在**版本号/数字写法和项目名**上出现实质性退化
（`1.8.2`→`一点八点二` 单独一段就让该段 CER 从 0% 变成 41.18%）。对「把转写文本填进
目标输入框」的产品目标，数字与专名的保真比 0.1–0.3 s 的收尾延迟更重要。

`streaming` 并非全面更差：在长句完整性（`mandarin-long` 12.50%→4.17%）、
`numbers-units`（21.05%→18.42%）、`long-qualified-plan`（3.17%→0%）、
`sherpa-onnx` 近似拼写（`Shiva-ONNX`→`sherpa-omninx`）上更好。它同时修好了
message 在 `mandarin-long` 漏掉「接口已经联调完成」的问题。这是「长会议连续转写」与
「短段精确转写」的定位差异，不是单纯的好坏——但本票的 8.83–21.45 s 短段语料和当前
`intermediate_result_enabled:false` 的请求参数，正是现产品日常使用的形态。

若父级希望继续验证「长会议」方向，应另开一票：加 5–10 分钟的连续会议语料、评估
`intermediate_result_enabled:true` 的差别、并复核数字/专名策略。本票不切换默认值。

## 开关实现

| 位置 | 内容 |
| --- | --- |
| `app\listen\src\cloud\mod.rs` | `ALI_MODEL`（现役默认，值不变）、`ALI_MODEL_STREAMING`（候选）、`ALI_MODELS`（闭集白名单）、`is_allowed_model()` |
| `app\listen\src\cloud\mod.rs` | `AliSettings::with_model(Option<&str>)`：显式进程级选择覆盖本地设置与 `LISTEN_ALI_MODEL`；`None` 时原值不动 |
| `app\listen\src\cloud\mod.rs` | `AliSettings::validate()`：型号判定从「等于 `ALI_MODEL`」改为「在白名单内」 |
| `app\listen\src\cloud\mod.rs` | `AliSettings::description()`：白名单内型号原样显示；白名单外仍是 `unsupported model (check local settings)`，不回显任意输入 |
| `app\listen\src\cloud\mod.rs` | `CloudSessions::from_local_with_model()`：把 `--ali-model` 应用到载入的本地设置；`from_local()` 保留为「无选择」入口 |
| `app\listen\src\launch.rs` | `Config` 新增 `ali_model: Option<String>`、`--ali-model <id>` 解析、`Config::ali_model()`；白名单外**解析期**拒绝；`--engine` + `--ali-model` 明确拒绝 |
| `app\listen\src\cloud\free_only.rs` | `FreeOnlyStatusSource::quota(model)` 与 `verify()` 的 `quota["model"] == settings.model`：核验的型号 = 任务实际发送的型号 |
| `app\listen\src\main.rs` | `--help` 增补 `--ali-model` |

**默认行为零变化**：不传 `--ali-model` 时 `ali_model=None`，`with_model(None)` 不改动任何
设置，默认仍是 `qwen-audio-3.1-asr-flash-message`，`--cloud-trial` / `--engine` 的既有
互斥语义未改。新增的只是「`--ali-model` 不能与 `--engine` 同时给」这一条显式拒绝
（本地路径没有云端型号，不能被静默忽略）。

白名单（闭集，`ALI_MODELS`）：

- `qwen-audio-3.1-asr-flash-message`（现役；默认）
- `qwen-audio-3.1-asr-flash-streaming`（候选）

白名单外拒绝原文（解析期；此时未打开设备、未读凭据、未联网）：

```text
--ali-model must be one of qwen-audio-3.1-asr-flash-message, qwen-audio-3.1-asr-flash-streaming; refusing to fall back to the default model
```

未静默回落：解析失败即进程退出，既不换成默认型号，也不改走本地识别。拒绝信息不回显
任意型号输入。用 A/B 同一个 release 二进制做 CLI 冒烟（`--control` 模式不弹 GUI 对话框）：

```text
listen.exe --ali-model whisper-large --control start   -> exit 2
```

该进程在解析阶段退出（exit 2），未进入控制分发。

`status.txt` 的键集合未改（仍 70 键，`app\listen\src\status.rs` 无 diff），只是
`backend=` 的**值**随所选型号变化；交付记录/持久化格式未改。

## 前置检查（`bl` CLI，2.10.1）

- `bl auth status`：`authenticated=true`，API key 与 console 均来自本地 config，
  console region `cn-beijing`、site `domestic`。**会话有效，未代登录、未重装 CLI。**
  （输出里的 key/console 都是 masked，本文件不复述任何账号 ID、key、工作空间 ID 原文。）
- `bl workspace list`：恰好 1 个 workspace 且 `default=true`，与本地
  `%LOCALAPPDATA%\Listen\cloud-trial\ali.json` 的 `workspace_id` 一致——满足
  `free_only.rs` 的「唯一默认工作空间」前置条件（不一致会在核验阶段被拒绝）。
- `bl usage free --model <id>` 两个型号的返回（A/B 之前，`type` 均为 `Audio`、
  `autoStop` 均为 `true`、`expires` 均为 `2026-12-21`）：

| 型号 | remaining / total | 口径 |
| --- | ---: | --- |
| `qwen-audio-3.1-asr-flash-message` | 993,363 / 1,000,000 | 同一个 JSON 形状：`model`、`type=Audio`、数值 `remaining`/`total`、日期 `expires`、布尔 `autoStop` |
| `qwen-audio-3.1-asr-flash-streaming` | 1,000,000 / 1,000,000 | 同上，**不是**按秒计费的另一种口径 |

**判断：`free_only.rs` 的解析不需要为 `streaming` 做兼容改动。** 两型号返回同一个
`type=Audio` 额度记录形状；额度是按型号**各自独立**计的一份额度（message 已用掉一部分，
streaming 在 A/B 前是满额）。核验仍保持「exact-model」：CLI 查询用所选型号，返回记录的
`model` 必须等于所选型号，跨型号的记录不被接受（见测试 3）。

## 测试（TDD：红 → 绿）

基线 **378 passed / 0 failed / 16 ignored**（父级给定）。新增
`app\listen\tests\cloud_model_switch.rs`（3 个测试），最终 **381 / 0 / 16**。
`-Task CheckFormat` 无输出、exit 0。日志在 `.artifacts\issue-56\`（Git 忽略）。

| 切片 | 红（新测试先失败） | 绿（实现后全量） |
| --- | --- | --- |
| 1. `--ali-model` 解析与白名单 | `red-1-launch.log`：`E0432 unresolved import listen::cloud::ALI_MODEL_STREAMING`、4×`E0599 no method named ali_model` | `green-1-launch.log`：**379 / 0 / 16** |
| 2. 设置守卫与 backend 值 | `red-2-settings.log`：5×`E0599 no method named with_model` | `green-2-settings.log`：**380 / 0 / 16** |
| 3. 免费保护按所选型号核验 | `red-3-free-only.log`：`E0050 method quota has 2 parameters but the declaration in trait quota has 1` + 3×`E0308` | `green-3-free-only.log`：**381 / 0 / 16** |

三个测试分别覆盖：

1. `the_launch_flag_selects_a_whitelisted_model_and_refuses_an_unlisted_one`：
   默认 `None`（行为不变）、两个白名单型号可选、白名单外拒绝且信息含两个合法 id 与
   「fall back」、不回显任意输入、`--engine` + `--ali-model` 被拒。
2. `the_settings_guard_reports_a_whitelisted_model_and_still_rejects_a_stranger`：
   白名单型号在 `description()` 中显示、白名单外仍是 `unsupported model` 且不回显；
   `with_model(None)` 不动既有值；`Application` 快照的 `backend`（即 `status.txt` 的
   `backend=`）显示所选型号，且 preparation 为 `Ready`（说明 `validate()` 接受该型号）。
3. `the_live_free_only_check_queries_and_requires_the_selected_model`：
   选 streaming 时 CLI 被问的是 streaming 且其自身额度记录通过；返回 message 的记录时
   拒绝（不跨型号放开）；不选时仍问/接受 message。

已知负载敏感用例（`glossary` 100 ms、`storage_bounds` 10 s、`backfill_timing` 10 s）的
阈值未改，三次全量运行均通过。

## A/B 实测

### 环境与命令

- 语料：`E:\luyin\.artifacts\cloud-evaluation\recording\manifest.json`（20 段真实会议
  链路录音，manifest SHA-256 `9d97ab51…3fe46`；脚本逐段复核 WAV SHA-256，未改语料）。
  每轮音频合计 **249.14 s**（4.15 min）；两轮合计 498.28 s（8.30 min），在「不超过约
  10 分钟音频费用」的约束内。
- 二进制：`app\listen\target\release\listen.exe`（`scripts\build-listen.ps1 -Task Build`），
  SHA-256 `7c6842e3…31db`。
- 驱动：`scripts\benchmark-meeting-corpus.ps1` 的 launch 模式，`-ListenArgs` 传
  `--cloud-trial ali --ali-model <id> --runtime <绝对路径> --hidden`，`-Runtime` / `-Output`
  用绝对路径（见下方运维备注）。脚本自行追加 `--runtime`，与本参数值相同。
- **`-Engine` 只是结果文件标签，不代表真实引擎**：脚本的 `ValidateSet` 只有
  `whisper|sensevoice`，本轮未传，故两份 JSON 的 `engine` 字段都是默认标签 `whisper`，
  `engine_label_source=launch arguments`。真实型号以每段 `backend=` 与 `listen_args` 为准。
- 两轮命令（等价于工单示例，只把相对路径改成绝对路径）：

```powershell
.\scripts\benchmark-meeting-corpus.ps1 `
  -Corpus E:\luyin\.artifacts\cloud-evaluation\recording\manifest.json `
  -Runtime  E:\luyin\.worktrees\ab-model\.artifacts\ab-ali-model\runtime-message `
  -ListenArgs @('--cloud-trial','ali','--ali-model','qwen-audio-3.1-asr-flash-message',
                '--runtime','E:\luyin\.worktrees\ab-model\.artifacts\ab-ali-model\runtime-message','--hidden') `
  -Output   E:\luyin\.worktrees\ab-model\.artifacts\ab-ali-model\message.json

# 候选轮：型号/目录/输出换成 streaming
```

- 原始结果（Git 忽略，保留未删）：
  - `.artifacts\ab-ali-model\message.json`（SHA-256 `6112b183…98c3f`），
    运行日志 `.artifacts\ab-ali-model\message-run.log`
  - `.artifacts\ab-ali-model\streaming.json`（SHA-256 `4711e90d…5f11d`），
    运行日志 `.artifacts\ab-ali-model\streaming-run.log`
  - 汇总 `.artifacts\ab-ali-model\comparison.md` / `analysis.log`
- `manifest.json` 没有 `baseline` 块，因此脚本不会写 `-evaluation.json`、也不会碰
  `docs\issue-22-benchmark.md`。

**运维备注（未计入成绩）**：第一次 message 轮用相对 `-Runtime/-Output` 启动，脚本用
`[IO.Path]::GetFullPath` 按 shell 进程 CWD（`E:\luyin`）解析，把运行目录写进了主工作树的
`.artifacts`。发现后立即终止（第 1 段已完成、第 2 段未开始），删除该目录并用绝对路径
重跑。那次尝试的一段音频不计入两轮结果（共享账本里仍保留它的请求记录）；两轮正式结果
都是完整 20/20。主工作树仍只有 7 份用户 dirty 文档。

### 汇总

| 指标 | message（对照） | streaming（候选） |
| --- | ---: | ---: |
| 段数 | 20 | 20 |
| 识别 / 无语音 / 失败 | 20 / 0 / 0 | 20 / 0 / 0 |
| 内容 CER（`chinese_accuracy`） | **96.09%**（35/894 字错） | **94.30%**（51/894 字错） |
| 平均句准确率 | 95.85% | 93.66% |
| 停止→结果 中位 | **1,010 ms** | **893 ms** |
| 停止→结果 平均 | 1,260 ms | 979 ms |
| 停止→结果 最大 | 3,296 ms | 3,125 ms |
| 墙钟停止→结果 平均（含控制进程） | 1,365 ms | 1,068 ms |
| 每段 `backend` | 20/20 段 `model=qwen-audio-3.1-asr-flash-message` | 20/20 段 `model=qwen-audio-3.1-asr-flash-streaming` |
| fatal | none | none |

逐段 CER 差值（message − streaming，正数 = streaming 更好）：`mandarin-long` +8.33、
`product-names-whisper-sensevoice` +2.17、`numbers-units` +2.63、`long-qualified-plan`
+3.17 这 4 段 streaming 更好；`homophones` −5.26、`version-comparison` −41.18、
`project-listen` −10.42、`english-qualified` −3.26 这 4 段更差；其余 12 段相同。

**每段 `backend` 证明真的切换了型号**：两轮各 20 段的 `status.txt` `backend=` 都被脚本
逐段抄进 JSON 的 `samples[*].backend`；上方逐段表的「backend（型号）」列是从该字符串
取 `model=` 后的型号，每轮表后的代码块给出完整原始字符串。没有任何一段同时出现两个
型号，也没有回落到默认型号。收尾后两轮的 `status.txt` 仍分别保留
`backend=…model=qwen-audio-3.1-asr-flash-message…` 与 `…-streaming…`。

### 逐段结果

#### message（`.artifacts\ab-ali-model\message.json`）

| # | id | phase | CER | 停止→结果 ms | backend（型号） | 识别文本 |
| ---: | --- | --- | ---: | ---: | --- | --- |
| 1 | mandarin-long | Result | 12.50% | 3,296 | `qwen-audio-3.1-asr-flash-message` | 我先同步一下。今天的进展：后端最后已经连到完成，前端还在补最后的错误提示。明天下午可以走一遍验收流程。 |
| 2 | product-names-whisper-sensevoice | Result | 6.52% | 916 | `qwen-audio-3.1-asr-flash-message` | 我们把语音识别从Whisper换成SenseVoice，现在用Shiva-ONNX在本地跑推理。 |
| 3 | product-names-deepseek-cuda | Result | 0.00% | 889 | `qwen-audio-3.1-asr-flash-message` | DeepSeek-Harness的接口已经调通了，CUDA加速这一版先不开，先用CPU把流程跑稳。 |
| 4 | wasapi-loopback-tokens | Result | 1.85% | 1,102 | `qwen-audio-3.1-asr-flash-message` | 采集走的是。wasapi look back音频直接送进模型，接口偶尔返回http500，日志里能看到json解析失败。 |
| 5 | numbers-units | Result | 21.05% | 973 | `qwen-audio-3.1-asr-flash-message` | 这次发布的，这次发布的版本是1.8.2，服务端口改成8080，失败重试3次，间隔2秒，超时监测30秒。 |
| 6 | homophones | Result | 5.26% | 1,377 | `qwen-audio-3.1-asr-flash-message` | 本机环境已经配好了，本季度的目标不变，请确认是要在本机部署还是下个季度再部署？ |
| 7 | mixed-chinese-english | Result | 0.00% | 977 | `qwen-audio-3.1-asr-flash-message` | 这个PR我review过了，主要改了retry的逻辑。merge之前还需要再补一个unit test。 |
| 8 | digits-punctuation | Result | 4.88% | 1,624 | `qwen-audio-3.1-asr-flash-message` | 结论有三点。第一，灰度比例先控制在5%。第二，回滚窗口留30分钟。第三，明天10点半之前给我确认。 |
| 9 | meeting-scene | Result | 0.00% | 981 | `qwen-audio-3.1-asr-flash-message` | 对方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。 |
| 10 | english-acronyms | Result | 2.44% | 1,016 | `qwen-audio-3.1-asr-flash-message` | 今天的Demo先叫MVP，API文档我下午发到群里，SLA和QPS这两个指标下周再对齐。 |
| 11 | negation-scope | Result | 8.57% | 921 | `qwen-audio-3.1-asr-flash-message` | 这次不要删除旧录音。也不是所有结果都要重跑，只有没完成那几段才能继续处理。 |
| 12 | uncertainty | Result | 2.70% | 880 | `qwen-audio-3.1-asr-flash-message` | 这个现象可能跟网络有关，我大概下午再确认，目前还不能保证周五上线，也许要拖顺延一天。 |
| 13 | negative-decimal | Result | 3.33% | 954 | `qwen-audio-3.1-asr-flash-message` | 温度先设成零下3度，不是零上3度，电压读数3.5伏，误差大概0.2伏。 |
| 14 | storage-budget | Result | 4.44% | 1,108 | `qwen-audio-3.1-asr-flash-message` | 缓存上限是1GB。剩余空间低于1GB时，先停止采集。每月100元只是提醒线，不是自动停用的上限。 |
| 15 | version-comparison | Result | 0.00% | 2,152 | `qwen-audio-3.1-asr-flash-message` | 现在测试的是1.8.2，不要记成18.2，下一个补丁是1.8.3，端口仍然是8080。 |
| 16 | project-listen | Result | 6.25% | 1,004 | `qwen-audio-3.1-asr-flash-message` | Listen这个项目先支持Windows，用Rust写采集和恢复，精修仍由我手动触发，暂时不开自动Job。 |
| 17 | filler-repetition | Result | 0.00% | 1,375 | `qwen-audio-3.1-asr-flash-message` | 嗯，我我先补充一下，就是这个问题还没确认，还没确认，先别把它当成已经修好了。 |
| 18 | long-qualified-plan | Result | 3.17% | 928 | `qwen-audio-3.1-asr-flash-message` | 如果明天的回归没有发现新问题，我们可能周五下午开始灰度。不过这只是暂定时间，遇到数据不完整时还是要先停下来核对，不要为了赶进度效果检查。 |
| 19 | english-qualified | Result | 0.00% | 1,069 | `qwen-audio-3.1-asr-flash-message` | The api response is not ready yet. We may retry after two seconds, but please keep the original JSON and request ID. |
| 20 | time-duration | Result | 0.00% | 1,657 | `qwen-audio-3.1-asr-flash-message` | 会议改到下午3:15，预计持续45分钟，最晚4点结束。不是明天，是后天下午，我可能会晚到5分钟。 |

全 20 段 `backend=` 原始字符串逐段相同（JSON `samples[*].backend`）：

```text
Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message; region=cn-beijing; shared single-vendor trial cap=CNY 50
```

#### streaming（`.artifacts\ab-ali-model\streaming.json`）

| # | id | phase | CER | 停止→结果 ms | backend（型号） | 识别文本 |
| ---: | --- | --- | ---: | ---: | --- | --- |
| 1 | mandarin-long | Result | 4.17% | 3,125 | `qwen-audio-3.1-asr-flash-streaming` | 我先同步一下。今天的进展，后端接口已经联调完成，前端还在补最后的错误提示。明天下午可以走一遍验收流程。 |
| 2 | product-names-whisper-sensevoice | Result | 4.35% | 619 | `qwen-audio-3.1-asr-flash-streaming` | 我们把语音识别从 whisper 换成 sensevoice， 现在用 sherpa-omninx 在本地跑推理。 |
| 3 | product-names-deepseek-cuda | Result | 0.00% | 627 | `qwen-audio-3.1-asr-flash-streaming` | DeepSeek-Harness的接口已经调通了，CUDA加速这一版先不开，先用CPU把流程跑稳。 |
| 4 | wasapi-loopback-tokens | Result | 1.85% | 780 | `qwen-audio-3.1-asr-flash-streaming` | 采集走的是。Wasaapi loopback 音频直接送进模型，接口偶尔返回 http 500，日志里能看到 json 解析失败。 |
| 5 | numbers-units | Result | 18.42% | 696 | `qwen-audio-3.1-asr-flash-streaming` | 这次发布的。这次发布的版本是1.8.2，服务端口改成8080，失败重试3次，间隔2秒，超时先测30秒。 |
| 6 | homophones | Result | 10.53% | 909 | `qwen-audio-3.1-asr-flash-streaming` | 本期环境已经配好了，本季度的目标不变。请确认是要在本期部署，还是下个季度再部署？ |
| 7 | mixed-chinese-english | Result | 0.00% | 646 | `qwen-audio-3.1-asr-flash-streaming` | 这个PR我review过了，主要改了retry的逻辑。merge之前还需要再补一个unit test。 |
| 8 | digits-punctuation | Result | 4.88% | 975 | `qwen-audio-3.1-asr-flash-streaming` | 结论有三点：第一，灰度比例先控制在5%；第二，回滚窗口留30分钟；第三，明天10点半之前给我确认。 |
| 9 | meeting-scene | Result | 0.00% | 977 | `qwen-audio-3.1-asr-flash-streaming` | 对方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。 |
| 10 | english-acronyms | Result | 2.44% | 843 | `qwen-audio-3.1-asr-flash-streaming` | 今天的demo先叫mvp，api文档我下午发到群里，sla和qps这两个指标下周再对齐。 |
| 11 | negation-scope | Result | 8.57% | 877 | `qwen-audio-3.1-asr-flash-streaming` | 这次不要删除旧录音。也不是所有结果都要重跑，只有没完成那几段才能继续处理。 |
| 12 | uncertainty | Result | 2.70% | 1,400 | `qwen-audio-3.1-asr-flash-streaming` | 这个现象可能跟网络有关，我大概下午再确认。目前还不能保证周五上线，也许要拖顺延一天。 |
| 13 | negative-decimal | Result | 3.33% | 742 | `qwen-audio-3.1-asr-flash-streaming` | 温度先设成零下3度，不是零上3度。电压读数3.5伏，误差大概0.2伏。 |
| 14 | storage-budget | Result | 4.44% | 810 | `qwen-audio-3.1-asr-flash-streaming` | 缓存上限是1GB。剩余空间低于1gb时，先停止采集。每月100元只是提醒线，不是自动停用的上限。 |
| 15 | version-comparison | Result | 41.18% | 1,001 | `qwen-audio-3.1-asr-flash-streaming` | 现在测试的是一点八点二，不要记成十八点二。下一个补丁是一点八点三，端口仍然是8080。 |
| 16 | project-listen | Result | 16.67% | 929 | `qwen-audio-3.1-asr-flash-streaming` | Ethan这个项目先支持windows，用rust写采集和恢复，精修仍由我手动触发，暂时不开自动job。 |
| 17 | filler-repetition | Result | 0.00% | 1,088 | `qwen-audio-3.1-asr-flash-streaming` | 嗯，我我先补充一下，就是这个问题还没确认，还没确认，先别把它当成已经修好了。 |
| 18 | long-qualified-plan | Result | 0.00% | 640 | `qwen-audio-3.1-asr-flash-streaming` | 如果明天的回归没有发现新问题，我们可能周五下午开始灰度。不过这只是暂定时间，遇到数据不完整时还是要先停下来核对，不要为了赶进度跳过检查。 |
| 19 | english-qualified | Result | 3.26% | 969 | `qwen-audio-3.1-asr-flash-streaming` | The API response is not ready yet. You may retry after two seconds, but please keep the original JSON and request ID. |
| 20 | time-duration | Result | 0.00% | 925 | `qwen-audio-3.1-asr-flash-streaming` | 会议改到下午3:15，预计持续45分钟，最晚4点结束。不是明天，是后天下午，我可能会晚到5分钟。 |

全 20 段 `backend=` 原始字符串逐段相同（JSON `samples[*].backend`）：

```text
Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-streaming; region=cn-beijing; shared single-vendor trial cap=CNY 50
```


### 失败样本

两轮均 **0 失败、0 无语音**：20 段全部 `phase=Result`，没有失败样本需要保留，也没有
任何一段被删除或重跑（`sample_count=20`、`scored_count=20`，JSON 中 20 段齐全）。
唯一未进入正式结果的样本是上文运维备注中那次被终止的启动的第一段（属于驱动路径错误，
不是型号/识别失败）；它不在两份 JSON 里，也不计入任何 CER 或延迟。

### 专名与数字抽查

工单点名的 `P99`、`RAG`、`JVM`、`Kubernetes`、`embedding`、`goroutine` 在这 20 段
冻结语料里**没有出现**（逐段 reference 匹配为 0；`RAG` 只作为 `storage` 的子串命中，
不是这个词）。因此改抽查语料实际包含的专名/术语：

| term | 语料中出现（每轮） | message 命中 | streaming 命中 |
| --- | ---: | ---: | ---: |
| DeepSeek Harness | 1 | 1/1 | 1/1 |
| CUDA | 1 | 1/1 | 1/1 |
| Whisper | 1 | 1/1 | 1/1 |
| SenseVoice | 1 | 1/1 | 1/1 |
| sherpa-onnx | 1 | 0/1 | 0/1 |
| WASAPI | 1 | **1/1** | **0/1** |
| HTTP | 1 | 1/1 | 1/1 |
| JSON | 2 | 2/2 | 2/2 |
| Listen | 1 | **1/1** | **0/1** |
| Rust | 1 | 1/1 | 1/1 |
| Windows | 1 | 1/1 | 1/1 |
| MVP | 1 | 1/1 | 1/1 |
| API | 3 | 3/3 | 3/3 |
| SLA | 1 | 1/1 | 1/1 |
| QPS | 1 | 1/1 | 1/1 |
| PR | 1 | 1/1 | 1/1 |

四段关键差异的原文（reference 与两轮识别文本）：

| 段 | reference | message | streaming |
| --- | --- | --- | --- |
| `version-comparison` | 现在测试的是 1.8.2，不要记成 18.2；下一个补丁是 1.8.3，端口仍然是 8080。 | 现在测试的是1.8.2，不要记成18.2，下一个补丁是1.8.3，端口仍然是8080。（CER 0%） | 现在测试的是**一点八点二**，不要记成**十八点二**。下一个补丁是**一点八点三**，端口仍然是8080。（CER 41.18%） |
| `project-listen` | Listen 这个项目先支持 Windows，用 Rust 写采集和恢复；精修仍由我手动触发，暂时不开自动摘要。 | Listen这个项目先支持Windows，用Rust写采集和恢复，精修仍由我手动触发，暂时不开自动Job。（CER 6.25%，`Listen` 对） | **Ethan**这个项目先支持windows，用rust写采集和恢复，精修仍由我手动触发，暂时不开自动job。（CER 16.67%，`Listen`→`Ethan`） |
| `homophones` | 本机环境已经配好了，本季度的目标不变；请确认是要在本机部署，还是下个季度再部署一次。 | **本机**环境已经配好了，本季度的目标不变，请确认是要在**本机**部署还是下个季度再部署？（CER 5.26%） | **本期**环境已经配好了，本季度的目标不变。请确认是要在**本期**部署，还是下个季度再部署？（CER 10.53%） |
| `mandarin-long` | 我先同步一下今天的进展：后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以一起走一遍验收流程。 | 我先同步一下。今天的进展：**后端最后已经连到完成**，前端还在补最后的错误提示。明天下午可以走一遍验收流程。（CER 12.50%，漏/错「接口已经联调完成」） | 我先同步一下。今天的进展，**后端接口已经联调完成**，前端还在补最后的错误提示。明天下午可以走一遍验收流程。（CER 4.17%，长句更完整） |

`english-qualified` 还有一处：reference `We may retry after two seconds`，message 正确，
streaming 写成 `You may retry`（CER 0% → 3.26%）。

### 额度消耗

| 型号 | A/B 前 remaining | message 轮后 | A/B 后 remaining | 总消耗 |
| --- | ---: | ---: | ---: | ---: |
| `qwen-audio-3.1-asr-flash-message` | 993,363 | 991,082 | 990,233 | 3,130 |
| `qwen-audio-3.1-asr-flash-streaming` | 1,000,000 | 1,000,000 | 998,225 | 1,775 |

message 的 3,130 包含那次被终止尝试的 21.45 s 与正式轮 249.14 s；streaming 的 1,775
只对应正式轮 249.14 s。注意 message 计数在 message 轮结束后又下降 849——控制台用量
统计有分钟级延迟，因此**不要**把两次读数之差严格当作单轮用量；这里只记录事实读数。
两轮结束后两型号仍是 `autoStop=true`、`expires=2026-12-21`，额度按型号独立计数。
`bl usage free` 原文（不含任何账号标识）保存在
`.artifacts\issue-56\usage-before-streaming-*.json` 与 `usage-after-*.json`。

## 换型号还需要哪些连带改动（仅当父级决定换）

1. **型号常量与默认值**：`ALI_MODEL` 改为 streaming（或新增默认常量并调整
   `AliSettings::beijing`）；`ALI_MODELS` 保留两个型号还是只留一个，要同时定
   「白名单是否继续允许 message」。`CloudError::Configuration` 文案同步。
2. **核验口径**：`free_only.rs` 已参数化，无需改逻辑；但需在真实额度上复核 streaming
   的 `type/remaining/total/expires/autoStop` 语义是否长期与 message 一致（本票两次
   查询一致），并更新 `FreeOnlyPolicy.evidence` 里的模型语义说明。
3. **请求参数**：本票沿用 `intermediate_result_enabled:false`。若换 streaming，需另测
   `true` 对增量结果、句末标点、数字格式的影响；`streaming` 的 `heartbeat`/`finish-task`
   行为也应在长音频上复核。
4. **验收与语料**：新增一次换型号验收记录（`docs\issue-56-*.md` 或后续票）；`manifest.json`
   本身不需改（哈希冻结），但若要证明「长会议连续转写」优势，需新增 5–10 分钟连续
   会议语料并重新冻结，且**不能**用本票的短段语料下结论。
5. **文档/脚本里写死型号的地方**：除 `cloud\mod.rs` 的常量定义本身外，还有 17 个文件
   含 `qwen-audio-3.1-asr-flash-message` 字符串——
   当前说明性文档与脚本：`README.md`（引擎对照表）、`packaging\CLOUD-TRIAL.md`、
   `packaging\README.md`、`scripts\configure-cloud-trial.ps1`、
   `scripts\package-listen.ps1`、`scripts\run-listen.ps1`、
   `scripts\verify-listen-package.ps1`、`app\listen\src\main.rs`（HELP）；
   历史记录/决策记录：`docs\adr\0006-cloud-default-after-acceptance.md`、
   `docs\issue-23-verification.md`、`docs\issue-29-cloud-trial.md`、
   `docs\issue-40-acceptance-20261121.md`、`docs\issue-40-delivery.md`；
   测试 fixture：`app\listen\src\panel.rs`（2 处显示文案）与
   `app\listen\tests\{cloud_trial,delivery_config,usage_ledger}.rs`。
   **历史记录与 ADR 不应改写**（它们记录当时的事实/决定）：换型号应新增 ADR 取代
   `0006`、新增验收记录，并只更新说明性文档、脚本和 HELP。
   `CLAUDE.md` 在本仓库不存在；`AGENTS.md` 不含型号。若父级维护其它代理文档，需同步。
6. **验收记录**：`docs\issue-40-acceptance-20261121.md` 与 `issue-40-delivery.md` 是
   message 型号的验收历史，不能当成新默认型号的验收；换型号需要新的验收记录与
   （若涉及打包）`verify-listen-package.ps1` 的复核。
7. **本票的开关去留**：`--ali-model` 可作为长期诊断开关保留（默认不变即零风险）；若保留，
   建议在 README/打包文档中补一句「默认型号以外仅供测量」。

## 限制

- 单轮 20 段、每段一次 start/stop；语料是 8.83–21.45 s 的短段，不是 `streaming` 定位的
  长时间连续会议。结论只对「当前产品形态」成立。
- 每段只跑一次，未做多次重复；CER 与延迟差异没有置信区间。
- 延迟是本机（默认多媒体耳机、同一网络、同一 release 二进制）上的一次测量，绝对值
  依赖网络与机器；两轮之间只有型号不同。
- 两轮的最大停止→结果都出现在第 1 段（3,296 / 3,125 ms），与「本轮第一个任务」的建立
  有关。剔除第 1 段后的 19 段：message 中位 1,016 ms / 最大 2,152 ms，streaming
  中位 909 ms / 最大 1,400 ms——中位差 107 ms，streaming 的尾部更短；本节其它表格仍按
  含首段的 20 段口径报告。

## 收尾复查

- `git merge main`：**Already up to date**；`main` 仍是 `8cea066`，本分支在其上新增
  `a1c4129`（实现+测试）与本文件提交。
- 无残留 `listen` 进程（`Get-Process -Name listen` 为空）。
- 工作树干净：`git status --short` 无输出；`.artifacts`、`dist` 未提交（`.artifacts/`
  在 `.gitignore` 中），仓库内 `git status --short --untracked-files=all` 也为空。
- 主工作树 `E:\luyin` 仍是 **7 份用户 dirty**（3 个 `M` + 4 个 `??`），与开工时一致，
  全程未 add / commit / stash / reset。
- `status.txt` 键集合、交付记录格式、默认型号均未改变（`status.rs`、`delivery.rs`、
  `ALI_MODEL` 的值无 diff）。
