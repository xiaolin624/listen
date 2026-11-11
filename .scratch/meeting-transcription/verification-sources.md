# 原生 Windows 技术验证：官方依据与验收建议

资料核对日期：2026-11-10。本文件记录文档及源码依据；已进行的本机实验另见 [验证报告](verification-results.md)。下文的建议实验不代表均已完成。
固定环境：Windows 11 / i5-11400H / 16 GiB / RTX 3050 Laptop 4 GB / NVIDIA driver 546.33。
范围：耳机整个播放设备音频，不采麦克风；用户明确选定目标输入框后开始；停止后追加转写文本；失败显示备用文本框；不自动发送消息。

## 1. 保存目标输入框、追加和验证

- `IUIAutomation::GetFocusedElement` 返回当前有输入焦点的元素接口，可在用户选定时保存；元素也可能在调用返回前已被移除。保存引用不能保证目标录音结束时仍存在。[微软 GetFocusedElement](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation-getfocusedelement)
- `GetRuntimeId` 只保证生成时所在桌面的唯一性，ID 随时间可能被复用；不能把保存的 ID、名称或坐标当作永久目标定位器。[微软 GetRuntimeId](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationelement-getruntimeid)
- 建议在一个持续存活的非 UI、COM MTA 线程中捕获并使用目标；微软说明 UIA 与 UI 线程混用可能卡死，原 apartment 结束也可能影响远端对象寿命。[微软 UIA 线程模型](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-threading)
- 对实际支持 `ValuePattern` 的可编辑控件，`CurrentValue` 可读全文，`SetValue` 设置整个值；写入前必须 `CurrentIsEnabled == TRUE`、`CurrentIsReadOnly == FALSE`。这是读后拼接再整值写回，不是原生 append API。[微软 CurrentValue](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationvaluepattern-get_currentvalue)、[微软 SetValue](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationvaluepattern-setvalue)
- 建议流程：停止 → 检查保存目标仍可用及模式可写 → **此时**读取 `before` → 构造 `expected = before + 分隔符 + 转写文本` → `SetValue(expected)` → 有限等待并重新读取 `CurrentValue` → 与 `expected` 比较。开始时保存的正文快照不能用于覆盖，避免丢失录音期间用户新写的文字。
- `S_OK` 只代表接口调用成功；写后值比对、目标应用是否保留内容及是否产生发送动作须实测。读取与写入并非原子事务：并发编辑仍可能在两次调用之间发生；发现变化或结果不一致时保留转写文本、报告“写入未确认”，不得盲目重试或自动回滚。
- `UIA_E_ELEMENTNOTAVAILABLE = 0x80040201` 表示元素已不存在或被虚拟化；`UIA_E_NOTSUPPORTED` 表示模式/属性明确不支持；`UIA_E_TIMEOUT` 表示操作超时。失效、禁用、只读、读写失败或写后不一致时进入备用文本框；不自动换成另一个“看起来相同”的输入框。[微软 UIA 错误码](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-error-codes)

### 没有 ValuePattern 时的键盘回填

- 多行/富文本控件常用 `TextPattern` 读取；该模式不能设置文本。微软示例提供聚焦后模拟键盘作为替代，但示例包含删除原文的步骤，不能直接满足本项目追加要求；是否可用以具体控件实测为准。[微软文本框写入示例](https://learn.microsoft.com/en-us/dotnet/framework/ui-automation/add-content-to-a-text-box-using-ui-automation)
- 若验证键盘路径：`SetFocus` 后再次 `GetFocusedElement`，用 `CompareElements` 验证仍为保存目标；必须验证窗口切换、最小化、弹窗、录音期间焦点迁移和发送前瞬间失焦。焦点检查与键盘输入之间仍有竞态，不能据此承诺绝不误投。[微软 SetFocus](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomationelement-setfocus)、[微软 CompareElements](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation-compareelements)
- 必测：光标在中间及已有选区时仍追加到末尾；中英文输入法、中文/emoji/组合字符/多行完整；原文不变；Ctrl/Shift/Alt 被用户按住时不产生快捷键；高完整性级别目标明确失败。`SendInput` 返回输入事件数量而非文本写入证明，受 UIPI 限制，已有按键状态可能干扰。[微软 SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- 如用剪贴板粘贴，另测占用、原剪贴板恢复、用户同时复制与多行粘贴；如用 Unicode 键入，另测代理对与换行。均不注入发送快捷键、不调用发送按钮；须用测试控件和对应应用验明换行不会触发发送。
- 要求能通过 `TextPattern` 或应用提供的其他可验证读取通道比对写后全文；无法读回、无法确认焦点或存在未经验证的键盘行为时，默认备用文本框。已可能部分写入的失败须标明，禁止自动重试造成重复。

## 2. whisper.cpp v1.8.2、驱动及模型基准

- v1.8.2 官方资产确有 Windows x64 的 `whisper-cublas-11.8.0-bin-x64.zip`（58,926,878 bytes）；官方 SHA-256 为 `7def83d3a7c656f0a6d280dae1ca229719cfe665c2565e88aeff81a16c935d2c`。源码 CI 的 CUDA matrix 为 12.4.0 / 11.8.0，并启用 `GGML_CUDA`、将 CUDA DLL 打包。[官方资产清单](https://github.com/ggml-org/whisper.cpp/releases/expanded_assets/v1.8.2)、[v1.8.2 CI 源码](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/.github/workflows/build.yml#L729-L908)
- NVIDIA 的 CUDA 11.8 发布说明：Windows minor-version compatibility 最低 452.39，11.8 GA 对应 Toolkit 驱动为 522.06，并说明新驱动向后兼容旧 CUDA。**546.33 高于两者，满足 11.8 驱动版本条件**；这是兼容性判断，尚不能替代 DLL 加载、设备识别、分配和推理实测。[NVIDIA CUDA 11.8 release notes，Table 2/3](https://docs.nvidia.com/cuda/archive/11.8.0/cuda-toolkit-release-notes/)
- 使用 `base`、`small` 多语言模型，不能选择仅英语的 `base.en` / `small.en`。OpenAI 官方表列 base 74M、small 244M 参数；其参考实现标注约 1 GB / 2 GB VRAM，而 medium 约 5 GB。该表是参考实现估计，**不是 whisper.cpp 的显存测量**。[OpenAI Whisper 模型表](https://github.com/openai/whisper#available-models-and-languages)
- whisper.cpp v1.8.2 自己的表列 base：142 MiB 磁盘 / 约 388 MB Mem，small：466 MiB / 约 852 MB Mem；该 Mem 表未承诺 CUDA 显存峰值。4 GB 环境以 base 为起点、small 为第二基准，是为显存余量与质量比较所作的实验选择；需实际测 GPU 峰值、显示程序占用和 OOM，不承诺实时速度。[v1.8.2 内存表](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/README.md#memory-usage)

## 3. 长音频必须在采集期间分段处理

- v1.8.2 `read_audio_data` 会先获得总帧数，然后将 `pcmf32` resize 到整段长度；stdin 路径还会先把全部输入积入 `audio_data`。把无限录音留到停止后交给这一整文件路径，会使内存随时长增长。[v1.8.2 文件读取源码](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/examples/common-whisper.cpp#L39-L123)
- 官方 `stream` 示例有 step / length / keep 参数，复用有限音频窗口，并保留边界重叠；但它是麦克风概念验证，跟不上时会清空音频并报警，不能直接作为本项目无静默丢音的实现。[v1.8.2 stream 源码](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/examples/stream/stream.cpp#L15-L39)、[窗口及过载处理](https://github.com/ggml-org/whisper.cpp/blob/v1.8.2/examples/stream/stream.cpp#L235-L315)
- 建议实验参数：20–30 秒推理块、0.5–1 秒重叠、按绝对采样位置去重；这些是待验证参数，需检查跨块词句遗漏及重复。采集线程只入有界队列，后台线程持续转写；不能因模型忙阻塞 WASAPI 取包。
- 音频内存仅保留有限窗口/队列；积压可写分段临时文件但必须有磁盘上限、清理和错误反馈；队列满或磁盘满须显式提示缺失/停止，不静默丢弃。转写结果可逐段写磁盘，停止时只收尾并排空已记录任务。
- 容量算例（算术估计）：16 kHz 单声道 f32 为 64,000 bytes/s，约 220 MiB/小时；原始高采样率/多通道及重复解码会更大，因此即使模型内存固定，整段音频向量也不能无限增长。

## 4. 建议实验验收（均未执行）

1. 自建可控单行/多行测试窗：预填原文，选定后录音期间继续编辑、切窗；停止后仅追加一次且写后全文匹配。记录控件类型、模式、HRESULT、耗时；避免把完整隐私正文写日志。
2. 销毁并重建控件、关闭窗口、只读/禁用、UIA 超时、缺模式：全部保留转写文本到备用文本框；不误写新窗口、不自动发送；已部分写入的失败有明确状态。
3. 对计划支持的真实应用逐个验证 ValuePattern 或键盘路径，至少重复 20 次；含焦点抢占、已有选区、长中文/emoji/多行、IME、权限不匹配。任何误投、删原文或自动发送即该路径不通过。
4. 固定官方 zip/hash 和 base/small 模型，记录 GPU backend 日志、模型加载成功、GPU/进程内存峰值、每块音频时长与耗时、停止后收尾时间、人工校对误字；分别测安静语音、中英混合、静音及重叠发言。不把其他机器跑分当速度承诺。
5. 进行至少 60 分钟分段采集转写并注入慢推理：音频队列有界、进程音频缓存不随总时长线性增长；积压状态可见，尾块不丢，跨块不重复。另测短于一块即停止、停止连按、模型失败、队列满、磁盘满、设备断开。
