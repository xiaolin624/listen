# #29 阿里云识别试用：实现与验收状态

> **2026-11-21 更新：** 本文件记录试用阶段的历史状态。后续 [#40 单厂商验收](issue-40-acceptance-20261121.md) 已通过，云端识别现在是日常默认，本地识别改为显式备用；当前交付状态、费用与限制见 [issue-40-delivery.md](issue-40-delivery.md) 与 [ADR 0006](adr/0006-cloud-default-after-acceptance.md)。下文“日常默认仍是 SenseVoice”等描述仅指当时状态。

本任务属于 [#26](https://github.com/xiaolin624/listen/issues/26)。**当前是可审查、可离线验证的部分实现，#29 必须保持开放。两个文件识别模型已有真实调用成功的证据；原生 Message 流式应用验收、20 段云端准确率及真实目标的一秒回填验收均未运行。**

原生适配器已经接入北京 Message WebSocket、连续 WASAPI PCM、结果保存与原目标交付。当前没有已核实的整任务输入/输出 Token 上界；原生入口改用独立的供应商免费保护政策：**每次开始后，先保存开头音频，在后台只读核验当前 CLI 身份的默认业务空间、准确型号额度和“用完即停”，再持久化共享账本记录，通过固定业务空间的 WSS 鉴权后才发模型任务和音频**。任何核验未知或失败均停止上传并保留已收音频。配置助手本身不授予云端准入；没有允许用户填写 `verified=true`、估算倍率或强制绕过保护的选项。日常默认仍是 SenseVoice。

## 已就绪的前提与验证范围（2026-11-15）

**2026-11-16 最新只读复查：现有 CLI 仍为 2.10.1，但控制台会话返回 code 3：未登录或登录已过期。** 11 月 15 日的额度及保护快照不能作为新请求的准入依据；当前原生尝试会因无法重新核验免费保护而保留收到的录音、不发模型任务或音频。CLI 会话与 Windows Credential Manager 中的应用凭据分开，这个结果不证明应用密钥失效。本工作不自动登录或调整任何保护设置，真实 Message 调用仍未运行。

- **真实语料与本地基线已验收。** 20 段真人耳机音频、固定参考原文及本地基线实跑已完成；用户对全部 75 项关键表达判定回复“全部认可”，包括其中的识别失败项。[#28 已关闭为 CLOSED / COMPLETED](https://github.com/xiaolin624/listen/issues/28#issuecomment-6890812891)。不用重录或重复确认这批基线。正式记录在本机 `.artifacts/cloud-evaluation/human-acceptance-20261115-150232/`，固定集合见 `.artifacts/cloud-evaluation/frozen-v1-20261115-125129/manifest.json`。
- **应用凭据已配置。** 既有配置助手已设置 Windows Credential Manager 及本机非秘密配置；2026-11-15 的 `-ShowStatus` 只读核对确认凭据存在、没有进程密钥覆盖、配置使用支持的 Message 型号/北京地域及合法工作空间格式，共享账本尚无任务预留。此检查不读取或显示密钥值，也不验证真实 Message 权限、余额或计费。
- **CLI 与文件识别已验证。** 沿用用户安装并登录的百炼 CLI 2.10.1，`qwen3-asr-flash` 和 `qwen-audio-3.1-asr-flash` 使用已冻结短录音的文件识别调用成功。CLI 登录与 Listen 应用凭据是两套入口；这些结果不证明 Message WebSocket、应用开始/停止、原目标安全回填或一秒指标通过。完整报告、原始返回及调用后额度复查在本机 `.artifacts/cloud-evaluation/bailian-cli-check-20261115-143529/`。文件调用的现金账单未核对，不能将未知现金扣款记为零。

## 本机配置入口

在项目目录使用 PowerShell。密钥只在隐藏输入中填写；助手不接受密钥命令行参数。

```powershell
.\scripts\configure-cloud-trial.ps1
.\scripts\configure-cloud-trial.ps1 -ShowStatus
```

第一个命令先询问北京业务空间 Workspace ID，再以隐藏输入询问北京 API Key。密钥存入当前 Windows 用户的 Credential Manager 通用凭据 `Listen/CloudTrial/Alibaba/cn-beijing`，采用本机持久化；不是 DPAPI 密文文件。非秘密配置在 `%LOCALAPPDATA%\Listen\cloud-trial\ali.json`，只有 `workspace_id`、`model`、`region`。助手不检查账号权限或余额，也不发起网络请求。修改后需重启 Listen。

`-ShowStatus` 只显示凭据是否存在、配置位置、固定厂商/型号/地域、共享账本占用和是否冻结；不显示密钥。`-RemoveKey` 只删除上述应用凭据，保留配置与费用账本。`-SelfTest` 使用唯一的临时假凭据执行真实 Windows 凭据写入/读回/删除，不接触应用凭据、不联网；本次已通过。

测试进程可以注入 `LISTEN_ALI_API_KEY`、`LISTEN_ALI_WORKSPACE_ID`，以及显式的 `LISTEN_ALI_MODEL`、`LISTEN_ALI_REGION`。这是 Listen 自己的变量名。进程密钥优先于 Credential Manager，设置为空或格式错误会拒绝，不回退到另一密钥。不要在命令行、聊天、日志或脚本源码里写真实密钥；推荐使用上面的隐藏输入助手。未提供环境配置时读取本机 JSON；只支持 `qwen-audio-3.1-asr-flash-message` / `cn-beijing`，不自动换型号或地域。

## 原生免费保护准入

沿用已有、已登录的百炼 CLI；本次核验版本为 2.10.1。每次尝试在后台依次执行以下只读能力，固定使用 `--console-region cn-beijing --console-site domestic --output json --timeout 10`，不传密钥、不切换登录身份、不修改任何保护设置：

- `bl workspace list`：配置中的工作空间必须恰好出现一次，并且是返回结果中唯一的 `default=true` 工作空间。首版不接受非默认空间。
- `bl usage free --model qwen-audio-3.1-asr-flash-message`：返回必须精确匹配型号、Audio 类型、开启的 `autoStop`、有效正数额度，以及至少距当前北京时间日期两天的合法到期日。日期粒度不足以安全接纳次日到期额度。

命令缺失、未登录、输出未知或超过 64 KiB、每条命令超过 12 秒、保护关闭、额度失效或工作空间不匹配均拒绝模型任务。两次子进程的总等待有界；查询期间开始、停止和界面轮询不等待 CLI，收到的 PCM 继续留在本机。子进程隐藏窗口，先挂起并绑定 Job 后才恢复唯一主线程，标准错误不回显；退出时终止命令子树，再收尾输出管道。CLI 查询只产生脱敏政策说明及核验时间，不把工作空间 ID、密钥或原始响应写入费用记录。

业务空间专属域名只接受所属 API Key，Message 在 WebSocket 握手时验证 Authorization。应用始终连接已核验空间的固定北京域名，取得 101 后才发 `run-task` 和音频。因此把同一 CLI 身份下默认空间的型号免费保护用于这条原生调用，是结合官方免费保护、专属域名和握手鉴权契约的推论；不是 Token 上界，也不证明模型权限、最终用量或实际现金账单。

11 月 15 日只读核验曾取得数小时一致的“用完即停”开启证据，本工作未改设置；该历史记录不替代每次尝试的现场查询。官方未提供开关 activation timestamp 或数值传播上界；代码不会假装检测出任意外部刚发生的开关变更。若在外部刚调整过设置，应按官方说明等待生效后再试。应用不读取外部研究笔记或用户填写的“已验证”JSON 作为放行凭据，也不设强制绕过开关。

## 显式选择及现在能验证的行为

先执行离线构建；不需要下载新 crate 或模型：

```powershell
.\scripts\build-listen.ps1 -Task Build
.\scripts\run-listen.ps1 -Control quit
.\scripts\run-listen.ps1 -CloudTrial ali -Runtime .\.artifacts\cloud-trial
.\scripts\run-listen.ps1 -Control show
```

也可直接启动 `app\listen\target\release\listen.exe --cloud-trial ali --runtime <目录>`。`--cloud-trial ali` 不能与 `--engine` 组合。省略试用选择则沿用日常本地引擎；试用失败不会调用本地引擎或其它厂商。正常 Ctrl+Alt+R、开始/停止、展开结果面板及 `--control` 仍是同一应用入口。

以上命令显式选择原生试用；**开始后只有免费保护核验、账本持久化及业务空间握手鉴权均通过才会发送云端模型任务和音频**。无配置、缺密钥、非法型号/地域给出本机修正提示；保护核验失败会说明本次未发送音频，并保留收到的本地音频。面板及 `status.txt` 的 `backend` 能查看厂商、实际受支持型号、地域和首轮合计上限。助手和应用不会自动购买套餐、增加预算或改变日常默认。

## 已完成的实现边界

- 同一默认多媒体播放端点的 WASAPI 采集，16 kHz、单声道、有符号 16-bit little-endian PCM。连续暂存没有本地重叠窗口、VAD、短句阈值或幻觉黑名单。每 100 ms 左右的 PCM 持久化后才可发送，尚未连接时的开头保留在本机。停止采集独立于握手/网络等待。
- 每段独立 Session ID，每次云尝试独立 UUID task ID。升级到准确业务空间的 HTTPS/WSS 域名，发送 `run-task`，匹配 `task-started` 后才发送 PCM。稳定分句按句子 ID 更新并按顺序组合；重复句子不会重复追加。停止后排完尾音频再发 `finish-task`，仅匹配的 `task-finished` 可完成。错误任务 ID、提前确认、连接关闭和仅稳定分句均不能代替整段确认；仍有非空未定稿句子的异常确认会拒绝交付。
- 开始时固定术语快照，只提供规范词形，权重 4；不附历史转写、目标草稿、截图或剪贴板。显式关闭口头语去除和中间结果，保留方言表达。术语超过厂商限制或文件无法读取时不上传。模型是否逐字保留重复、数字、否定等仍需真实语料核验。
- 完整非空文字先写临时文件、同步并原子替换 `current-result.txt`，再使用现有原目标追加/读回确认。保存失败不写目标、不删 PCM，完整文字在本次面板中保留；原目标不可写则保留完整备用结果，不重新选择当前焦点，不发送提交键。确认无语音保留上一次可复制结果，不追加空行。
- 请求只获得本次 PCM、固定术语及所选账号配置；目标正文不会进入识别适配器。凭据不进入状态、费用记录和恢复文件。远端错误正文不回显，只显示有限的认证、型号、计费/额度、限流或协议分类；无法区分的 HTTP 429 明确提示可能是限流或账号预算，不自动重试。
- 原生 WinHTTP 使用异步完成回调：握手共用 15 秒等待期限，单次发送最多等待 5 秒；界面轮询不等待网络。关闭中间结果时，正常长句可能持续上传而没有应用帧，因此录音中的每次接收不设独立计时器，由下述 FreeOnly 整任务时限约束。应用仍在启动确认、停止后尾音频尚未排完及 `finish-task` 后的结束确认阶段执行 15 秒限界。超时或退出直接取消实际挂起的子句柄，回调上下文与读写缓冲区保留至最终 `HANDLE_CLOSING`；不并发执行 `WebSocketClose`，也不留下阻塞的接收或关闭线程。发送必须取得完整写入完成，接收按完成回调拼接 UTF-8 分片。这些是故障处理限界，不代表正常回填的一秒指标已验收。

连续 PCM 单段暂存上限为 1 GiB；FreeOnly 会话另有 `1 GiB / 32,000 字节每秒 + 60 秒` 派生的墙钟上限，覆盖长期空闲或慢速发送，达到上限关闭连接、保留音频且不交付部分文字。这是与既有留存上限一致的操作保护，不是 Token 计费换算。网络接收和转写文字也有内存上限。失败只保留本段 PCM/格式资料及费用预留，**本票没有交付自动网络重连、补传、跨重启恢复、多段队列或迟到自动回填**；这些属于后续任务。重启不会自动识别或展示旧录音，保留路径需从失败面板或该运行目录查看。

## 50 元共享账本

固定位置：`%LOCALAPPDATA%\Listen\cloud-trial\ledger.json`，独立于 `--runtime`。阿里、重试和最终验收必须复用这份账本，不能各建一份 50 元额度。账本不自动按月重置；日常 100 元/月提醒没有在本票实现。

每次请求先在跨进程排他锁内持久化费用准入及最高现金费用预留，再允许连接和建立模型任务。所有已结算估计与未决预留之和加新预留不得超过 50 元。金额用整数纳元；每次尝试记录 provider/model/region、segment/task ID、费率或免费保护来源、核验时间、累计用量、是否取得最终用量、估计费用与未知原因。账单实际扣款单独为未知，未填成 0。

`result-generated` 用量是累计值，重复及较旧值不会反复相加或回退。旧版 TokenBounds 记录保持兼容：最终累计用量完整、一致且未超过已核实边界才释放预留差额；缺字段、连接失败或缺最终确认时整笔预留继续占用，超过 Token 边界会冻结新试用。每条持久化记录必须恰好有一种费用政策，预留金额与政策一致；缺失、双重政策或金额矛盾均按损坏账本拒绝。FreeOnly 记录不填虚构 Token 上界，现金预留为 0 的依据是已核验的供应商免费保护；未知输入/输出和未核对实扣仍各自为未知，不因现金上限为 0 就声称没有使用量或账单已核验。累计用量矛盾仍会冻结新试用，等待复核。锁竞争、账本损坏或写入失败均拒绝新请求，不创建替代账本。不要删除或修改账本来重置额度。

原生 Message 使用上述独立 FreeOnly 政策；受控离线适配器的有界 Token 只验证旧预算逻辑，不能复制为真实模型输出上界。官方月度预算停用存在收费延迟，不能独立充当 50 元硬封顶；它与已生效的型号免费保护是不同功能，不把前者的延迟推到后者。

## 复现离线验证

```powershell
.\scripts\build-listen.ps1 -Task Test
.\scripts\build-listen.ps1 -Task CheckFormat
.\scripts\configure-cloud-trial.ps1 -SelfTest
```

`app/listen/tests/cloud_trial.rs` 从 `Application::dispatch/poll/snapshot` 断言完整结果、状态、原草稿和保留文件。操作系统播放设备、凭据、CLI 状态输出和远端 socket 可以被替换；并非直接把“成功结果”注入应用的假云实现。覆盖采集中发送、连接前开头、握手未返回时停止、固定术语、短答/无语音、未定稿及缺失/错误确认、重复/乱序、保存失败、只读目标备用结果、账号错误脱敏、缺密钥/未知成本拒绝、免费保护核验期间开始/停止/轮询不被阻塞、准确型号及默认空间准入、累计用量、超界冻结、不同运行目录/重启及四个独立进程竞争同一预算。

全量测试里的一个 ignored 项是由跨进程预算用例显式启动四次的离线子进程 actor；其余现有 ignored 项需要真实模型、编辑器或付费条件。它们不计为通过。离线用例不证明真实识别质量、厂商计费正确性或一秒回填。

本机诊断另以真实 WinHTTP 和 loopback 服务核对静默握手、非阻塞的静默接收轮询、发送背压、挂起接收的退出，以及正常文本/二进制/跨 UTF-8 分片和结束消息。握手和发送使用 200 ms 诊断期限，在约 200–220 ms 返回；静默接收验证轮询和取消，不要求录音中的接收超时。长句诊断使用生产默认期限，对端持续接收 18 秒、180 帧合成 PCM，期间不发应用帧，直到收尾才返回完整结果和结束确认；修复前在第 15 秒错误超时，修复后完成。各场景的句柄退出约 0–2 ms，最终关闭回调释放全部上下文；这是本机观测，不是 WinHTTP 对最终取消回调时长的保证。此前假 CLI 诊断覆盖正常、登录过期、输出超限和 12 秒超时，均确认命令及后代被 Job 终止。诊断不含 API Key、不读播放设备、不发 ASR 或互联网请求；证据保存在本机外部实现笔记中，不把 loopback 成功记为真实云验收。

## #29 自身仍未满足的条件

1. 独立 FreeOnly 费用政策已实现，仍需完成本次实现审查及真实原生流程核验。不接受经验 Token/秒或手填数字为证明，不能借用其他型号的 25 Token/秒；若免费保护条件不满足，仍拒绝发送。
2. 凭据存在及配置格式、CLI 登录和两个文件型号调用已核对；原生 Message 的对应型号权限、实际额度和计费适用性仍需在具备费用准入政策后核验。不重新要求用户配置已有凭据。先小批量核对使用量和账单，再在同一个 50 元账本内扩大。
3. 用已就绪真实语料中的短发言在正常网络下做小批应用功能验证：从当前开始/停止入口持续发送本次耳机音频，匹配结束确认，保存完整文字并在真实原目标安全回填；核对这次 task ID、用量、预算预留和实际账单。原生 Message 路径尚无音频上传、应用功能或计费核验的调用证据，两个文件识别结果不替代此项验收。

因此 #29 目前保持开放。关闭 #29 不要求先完成依赖它的后续质量、时延或默认选择任务；本票先补齐自己的真实账号、预算保护和小批正常流式功能验收。

## 后续正式验收与日常启用

#28 的真实语料及本地基线已完成并正式验收；后续 #31、#38、#39、#40 工作按依赖和规格推进，不作为反向阻塞 #29 的条件。后续云端质量比较复用这批固定 20 段真实耳机音频、参考原文和术语，按 [#28 真实语料基线指南](issue-28-real-corpus-baseline.md) 做同音频比较；当前尚无这批 20 段的云端完整评测结果，独立的两次文件识别小样本不能代替正式评测。

后续正式质量门槛是编辑错误不高于基线一半，数字、术语、否定和不确定表达逐项复核；正常网络下每家分别 20 次，至少 19/20 在停止后 1 秒内完成真实目标完整读回，失败不能重跑替换分母。还需核实实际账号/型号的数据留存及训练条款，代码不宣称零留存。这些门槛及默认选择由后续任务验收，未达标前不切日常默认。

## 官方契约来源（2026-11-15 复核）

[Message WebSocket](https://help.aliyun.com/zh/model-studio/qwen-asr-message-websocket-api)、[客户端事件及参数](https://help.aliyun.com/zh/model-studio/qwen-asr-message-client-events)、[服务端事件及累计用量](https://help.aliyun.com/zh/model-studio/qwen-asr-message-server-events)、[即时术语限制](https://help.aliyun.com/zh/model-studio/improve-asr-accuracy)、[型号信息](https://help.aliyun.com/zh/model-studio/qwen-audio-3-1-asr-flash-message)、[目录价格](https://help.aliyun.com/zh/model-studio/model-pricing)、[免费额度及用完即停](https://help.aliyun.com/zh/model-studio/new-free-quota/)、[账单及免费保护](https://help.aliyun.com/zh/model-studio/bill-query-and-cost-management)、[业务空间专属域名](https://help.aliyun.com/zh/model-studio/base-url)、[地域和鉴权范围](https://help.aliyun.com/zh/model-studio/regions)、[月度预算停止及延迟](https://help.aliyun.com/zh/model-studio/budget-management)、[错误分类](https://help.aliyun.com/en/model-studio/error-code-start)。完整协议研究保存在本次实现的外部证据笔记中。
