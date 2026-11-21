# #40 单厂商真实验收（2026-11-21）

同一批冻结语料、真实应用工作流、真实云端识别，产出此前缺失的验收证据。**本文件只记录实测结果，不改变任何默认设置。**

## 方法与口径

- 语料：`.artifacts/cloud-evaluation/frozen-v1-20261115-125129/manifest.json`，20 段真人会议链路录音，合计 249.14 秒，逐段 SHA-256 校验。
- 驱动：`verify-live-cloud-workflow.ps1`，每段独立起一个 Listen 实例（`--cloud-trial ali`），用应用自己的开始/停止控制，先把音频按真实节奏播放到默认播放端点，由 WASAPI loopback 采集；目标是一个受控的原生编辑框夹具，交付后校验内容与读回。
- 厂商/型号/地域：阿里云百炼 `qwen-audio-3.1-asr-flash-message` / `cn-beijing`，单厂商，无第二家。
- 免费保护：每次开始前由应用现场只读核验（唯一默认业务空间、精确型号、额度 > 0、`autoStop=true`、到期日）。20 次全部通过核验后才发出请求。
- 准确率口径与基线一致：去掉标点、符号与空白后，按字符 Levenshtein 计算；聚合 CER = 总错距 / 总参考字符。空转写计为全错。
- 延迟口径：从应用"停止"控制到 `phase=Result`（该状态在完整文字落盘并完成目标读回确认之后才出现），测试端以 50 ms 轮询观测，因此实测值**含最多约 50 ms 的轮询滞后**，真实值不高于此。

## 结果

| 指标 | 实测 | 门槛 | 结论 |
| --- | --- | --- | --- |
| 交付成功 | 20 / 20 | — | 通过 |
| 停止到结果可见 | 中位 656 ms，均值 668 ms，**最慢 844 ms** | 19/20 ≤ 1000 ms | **20/20 通过** |
| 聚合准确率 | **95.86%**（CER 4.14%，37/894 字符） | ≥ 87.70%（错误 ≤ 基线一半） | **通过，余量 8.2 个百分点** |
| 相对基线错误下降 | 错距 220 → 37，**下降 83.2%** | ≥ 50% | 通过 |

基线（同一 20 段、同一口径，SenseVoice 本地引擎）：准确率 75.39%（CER 24.61%，220/894）。

逐段：8/20 完全无错（`wasapi-loopback-tokens`、`mixed-chinese-english`、`meeting-scene`、`version-comparison`、`filler-repetition`、`long-qualified-plan`、`english-qualified`、`time-duration`）。最难的 5 段依次是 `numbers-units`（6/38）、`mandarin-long`（6/48）、`product-names-whisper-sensevoice`（5/46）、`product-names-deepseek-cuda`（4/45）、`negation-scope`（3/35）。错误集中在数字与专有名词，与本地引擎的弱点方向一致但幅度小得多。

## 费用

- 全程走模型级免费额度，无现金扣费路径（`autoStop=true` 在每次开始前现场核验）。
- 20 段上报 total tokens 合计 **4036**（input/output 由厂商逐段返回，19 段有最终用量、1 段未返回最终用量）。
- 免费额度实测扣减：本批 **3850** tokens，与 19 段最终用量之和精确相等；未返回最终用量的那 1 段（186 tokens）如实保持未知，**没有记为 0**。
- 从会话开始至今累计消耗 4691 / 1,000,000 tokens（0.47%）。
- **现金账单仍未核对**，本文件不声称"账单为 0 元"，只声称免费保护使额外现金扣费路径不成立。

## 未覆盖与限制

- 只测单厂商；原双厂商对照任务已随第二厂商退出删除。
- 数据处理条款（留存、训练用途）仅有官方文档引用，未做独立核实。
- 弱网、断网恢复的真实网络演练未在本批执行（应用级恢复由离线可控测试覆盖）。
- 1 段未返回最终用量；其余 19 段用量与额度扣减精确吻合。
- 延迟含 ≤50 ms 测试轮询滞后；这不是"目标内部确认时刻"的精确测量。
- 交付目标为受控原生编辑框夹具，不是用户日常使用的具体第三方编辑器。

## 复现

```
# 逐段运行（每段独立证据目录）
verify-live-cloud-workflow.ps1 -Sample <id>
# 汇总与评分
run-acceptance-20.ps1 ; score-acceptance.ps1
```

证据：`%TEMP%\listen-acceptance-20261121\acceptance-20.json`、`score-20.json`；每段目录 `E:\luyin\.artifacts\cloud-evaluation\ali-live-workflow-*\`。
