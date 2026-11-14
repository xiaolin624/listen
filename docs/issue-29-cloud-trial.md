# #29 阿里云识别试用：实现与验收状态

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)。**当前是可审查、可离线验证的部分实现，#29 必须保持开放。真实云端调用、20 段准确率及真实目标的一秒回填验收均未运行。**

原生适配器已经接入北京 Message WebSocket、连续 WASAPI PCM、结果保存与原目标交付。但当前厂商文档没有可证明的每任务输入/输出 Token 上界，应用不能保证一次调用最高费用，因此**生产试用会在联网之前拒绝所有调用，即使密钥、业务空间和余额已正确配置也一样**。配置助手不是已能运行付费验收的承诺；没有允许用户填写 `verified=true`、估算倍率或强制绕过保护的选项。日常默认仍是 SenseVoice。

## 本机配置入口

在项目目录使用 PowerShell。密钥只在隐藏输入中填写；助手不接受密钥命令行参数。

```powershell
.\scripts\configure-cloud-trial.ps1
.\scripts\configure-cloud-trial.ps1 -ShowStatus
```

第一个命令先询问北京业务空间 Workspace ID，再以隐藏输入询问北京 API Key。密钥存入当前 Windows 用户的 Credential Manager 通用凭据 `Listen/CloudTrial/Alibaba/cn-beijing`，采用本机持久化；不是 DPAPI 密文文件。非秘密配置在 `%LOCALAPPDATA%\Listen\cloud-trial\ali.json`，只有 `workspace_id`、`model`、`region`。助手不检查账号权限或余额，也不发起网络请求。修改后需重启 Listen。

`-ShowStatus` 只显示凭据是否存在、配置位置、固定厂商/型号/地域、共享账本占用和是否冻结；不显示密钥。`-RemoveKey` 只删除上述应用凭据，保留配置与费用账本。`-SelfTest` 使用唯一的临时假凭据执行真实 Windows 凭据写入/读回/删除，不接触应用凭据、不联网；本次已通过。

测试进程可以注入 `LISTEN_ALI_API_KEY`、`LISTEN_ALI_WORKSPACE_ID`，以及显式的 `LISTEN_ALI_MODEL`、`LISTEN_ALI_REGION`。这是 Listen 自己的变量名。进程密钥优先于 Credential Manager，设置为空或格式错误会拒绝，不回退到另一密钥。不要在命令行、聊天、日志或脚本源码里写真实密钥；推荐使用上面的隐藏输入助手。未提供环境配置时读取本机 JSON；只支持 `qwen-audio-3.1-asr-flash-message` / `cn-beijing`，不自动换型号或地域。

## 显式选择及现在能验证的行为

先执行离线构建；不需要下载新 crate 或模型：

```powershell
.\scripts\build-listen.ps1 -Task Build
.\scripts\run-listen.ps1 -Control quit
.\scripts\run-listen.ps1 -CloudTrial ali -Runtime .\.artifacts\cloud-trial
.\scripts\run-listen.ps1 -Control show
```

也可直接启动 `app\listen\target\release\listen.exe --cloud-trial ali --runtime <目录>`。`--cloud-trial ali` 不能与 `--engine` 组合。省略试用选择则沿用日常本地引擎；试用失败不会调用本地引擎或豆包。正常 Ctrl+Alt+R、开始/停止、展开结果面板及 `--control` 仍是同一应用入口。

**当前以上试用命令只用于查看拒绝状态，不会发出云请求。** 无配置、缺密钥、非法型号/地域会给出本机修正提示；配置完整后会显示“尚不能核实本次最高费用，试用未发送”的费用保护原因。面板及 `status.txt` 的 `backend` 能查看厂商、实际受支持型号、地域和首轮合计上限。助手和应用不会自动购买套餐、增加预算或改变日常默认。

## 已完成的实现边界

- 同一默认多媒体播放端点的 WASAPI 采集，16 kHz、单声道、有符号 16-bit little-endian PCM。连续暂存没有本地重叠窗口、VAD、短句阈值或幻觉黑名单。每 100 ms 左右的 PCM 持久化后才可发送，尚未连接时的开头保留在本机。停止采集独立于握手/网络等待。
- 每段独立 Session ID，每次云尝试独立 UUID task ID。升级到准确业务空间的 HTTPS/WSS 域名，发送 `run-task`，匹配 `task-started` 后才发送 PCM。稳定分句按句子 ID 更新并按顺序组合；重复句子不会重复追加。停止后排完尾音频再发 `finish-task`，仅匹配的 `task-finished` 可完成。错误任务 ID、提前确认、连接关闭和仅稳定分句均不能代替整段确认；仍有非空未定稿句子的异常确认会拒绝交付。
- 开始时固定术语快照，只提供规范词形，权重 4；不附历史转写、目标草稿、截图或剪贴板。显式关闭口头语去除和中间结果，保留方言表达。术语超过厂商限制或文件无法读取时不上传。模型是否逐字保留重复、数字、否定等仍需真实语料核验。
- 完整非空文字先写临时文件、同步并原子替换 `current-result.txt`，再使用现有原目标追加/读回确认。保存失败不写目标、不删 PCM，完整文字在本次面板中保留；原目标不可写则保留完整备用结果，不重新选择当前焦点，不发送提交键。确认无语音保留上一次可复制结果，不追加空行。
- 请求只获得本次 PCM、固定术语及所选账号配置；目标正文不会进入识别适配器。凭据不进入状态、费用记录和恢复文件。远端错误正文不回显，只显示有限的认证、型号、计费/额度、限流或协议分类；无法区分的 HTTP 429 明确提示可能是限流或账号预算，不自动重试。

连续 PCM 单段暂存上限为 1 GiB；网络接收和转写文字也有内存上限。失败只保留本段 PCM/格式资料及费用预留，**本票没有交付自动网络重连、补传、跨重启恢复、多段队列或迟到自动回填**；这些属于后续任务。重启不会自动识别或展示旧录音，保留路径需从失败面板或该运行目录查看。

## 50 元共享账本

固定位置：`%LOCALAPPDATA%\Listen\cloud-trial\ledger.json`，独立于 `--runtime`。阿里、后续第二厂商、重试和最终验收必须复用这份账本，不能各建一份 50 元额度。账本不自动按月重置；日常 100 元/月提醒没有在本票实现。

每次请求先在跨进程排他锁内持久化最高费用预留，再允许建立付费任务。所有已结算估计与未决预留之和加新预留不得超过 50 元。金额用整数纳元；每次尝试记录 provider/model/region、segment/task ID、费率及来源日期、输入/输出上界依据、累计用量、是否取得最终用量、估计费用与未知原因。账单实际扣款单独为未知，未填成 0。

`result-generated` 用量是累计值，重复及较旧值不会反复相加或回退。最终累计用量完整、一致且未超过已核实边界才释放预留差额；缺字段、连接失败或缺最终确认时整笔预留继续占用。用量超界或矛盾会冻结新试用，等待账单复核。锁竞争、账本损坏或写入失败均拒绝新请求，不创建替代账本。不要删除或修改账本来重置额度。

原生 Message 目前没有已核实的预留政策，因此不会产生新请求。受控离线适配器的有界 Token 只验证预算逻辑，不能复制为真实模型费率/输出上界。官方账号月度预算停止、额度用完即停等可作为额外防护，但停用存在延迟，不能替代本地可证明的请求上界。

## 复现离线验证

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\configure-cloud-trial.ps1 -SelfTest
```

`app/listen/tests/cloud_trial.rs` 从 `Application::dispatch/poll/snapshot` 断言完整结果、状态、原草稿和保留文件。只有操作系统播放设备、凭据和远端 socket 被替换；并非直接把“成功结果”注入应用的假云实现。覆盖采集中发送、连接前开头、握手未返回时停止、固定术语、短答/无语音、未定稿及缺失/错误确认、重复/乱序、保存失败、只读目标备用结果、账号错误脱敏、缺密钥/未知成本拒绝、累计用量、超界冻结、不同运行目录/重启及四个独立进程竞争同一预算。原生分帧重组另有离线支持测试。

全量测试里的一个 ignored 项是由跨进程预算用例显式启动四次的离线子进程 actor；其余现有 ignored 项需要真实模型、编辑器或付费条件。它们不计为通过。离线用例不证明真实识别质量、厂商计费正确性或一秒回填。

## #29 自身仍未满足的条件

1. 通过供应商契约/可执行限制证明 Message 输入、术语输入和输出的最高费用，并审查后把真正受支持的政策写进原生适配器。当前不接受经验 Token/秒或手填数字为证明，不能借用其他型号的 25 Token/秒。
2. 本机北京账号、业务空间、Message 权限、有效通用 API Key、余额/额度与计费适用性；本次没有核查真实账号。先小批量核对使用量和账单，再在同一个 50 元账本内扩大。
3. 用真实短发言在正常网络下做小批应用功能验证：从当前开始/停止入口持续发送本次耳机音频，匹配结束确认，保存完整文字并在真实原目标安全回填；核对这次 task ID、用量、预算预留和实际账单。本次没有真实音频上传或此项功能调用证据。

因此 #29 目前保持开放。关闭 #29 不要求先完成依赖它的后续质量、时延或默认选择任务；本票先补齐自己的真实账号、预算保护和小批正常流式功能验收。

## 后续正式验收与日常启用

#28 的真实语料基线及后续 #31、#38、#39、#40 工作继续按规格推进，不作为反向阻塞 #29 的条件。固定 20 段真实耳机音频、参考原文和术语，按 [#28 真实语料基线指南](issue-28-real-corpus-baseline.md) 做同音频比较；当前尚无这批真实实测结果。

后续正式质量门槛是编辑错误不高于基线一半，数字、术语、否定和不确定表达逐项复核；正常网络下每家分别 20 次，至少 19/20 在停止后 1 秒内完成真实目标完整读回，失败不能重跑替换分母。还需核实实际账号/型号的数据留存及训练条款，代码不宣称零留存。这些门槛及默认选择由后续任务验收，未达标前不切日常默认。

## 官方契约来源（2026-11-14 核验）

[Message WebSocket](https://help.aliyun.com/zh/model-studio/qwen-asr-message-websocket-api)、[客户端事件及参数](https://help.aliyun.com/zh/model-studio/qwen-asr-message-client-events)、[服务端事件及累计用量](https://help.aliyun.com/zh/model-studio/qwen-asr-message-server-events)、[即时术语限制](https://help.aliyun.com/zh/model-studio/improve-asr-accuracy)、[型号信息](https://help.aliyun.com/zh/model-studio/qwen-audio-3-1-asr-flash-message)、[目录价格](https://help.aliyun.com/zh/model-studio/model-pricing)、[预算停止及延迟说明](https://help.aliyun.com/zh/model-studio/budget-management)、[错误分类](https://help.aliyun.com/en/model-studio/error-code-start)。完整协议研究保存在本次实现的外部证据笔记中。
