# Listen 云端默认配置（阿里云百炼）

本包的**日常默认是已通过验收的云端识别**。双击 `启动 Listen.cmd` 就使用阿里云百炼北京
Message 流式识别；本地识别是明确标注的备用选择，只有双击 `启动 Listen 本地备用.cmd` 才会
运行。云端与本地使用不同的运行目录（`runtime-cloud\` / `runtime\`），云端待取录音不会被
本地模式自动读取，也不会在云端失败后自动改用本地结果冒充云端结果。

固定选择（不会自动换厂商、型号或地域）：

| 项目 | 值 |
| --- | --- |
| 厂商 | 阿里云百炼 Model Studio |
| 型号 | `qwen-audio-3.1-asr-flash-message` |
| 地域 | `cn-beijing`（北京） |
| 识别路径 | 北京 Message 流式识别；采集的连续音频 + 开始时固定的术语表 |
| 验收证据 | 2026-11-21 同一 20 段真实音频：交付 20/20，最慢 844 ms，准确率 95.86%（基线 75.39%）；见仓库 `docs/issue-40-acceptance-20261121.md` |

## 为什么云端成为日常默认

#40 的放行依据是 `docs/issue-40-acceptance-20261121.md` 记录的实测：固定 20 段真实会议链路
音频、真实应用工作流、单厂商。停止到结果可见 20/20 在 1 秒内（中位 656 ms，最慢 844 ms），
聚合准确率 95.86%，相对同一批音频的本地基线 75.39% 错误下降 83.2%。未达到门槛前不切换；
现在已经达到，因此默认切到云端。

## 第一次使用前

1. **本机准备阿里云百炼 CLI。** 云端每次开始采集时会先调用本机 PATH 上的 `bl.cmd` 做只读
   核验：配置的工作空间必须是当前 CLI 身份下唯一的默认工作空间；该型号的免费额度保护必须
   已开启（`autoStop`）、额度为正且未临近到期。交付包**不包含该 CLI，也不会替你登录或修改
   保护设置**。未安装、未登录、输出未知或保护不满足时，本次不会发送音频，已收到的录音保留在
   本机。
2. **在本机配置 API Key 与工作空间。** 在解压后的本目录打开 PowerShell：

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\scripts\configure-cloud-trial.ps1
   ```

   先输入北京业务空间 Workspace ID（不是 API Key），再以隐藏输入输入 API Key。Key 存入当前
   Windows 用户的 Credential Manager 通用凭据 `Listen/CloudTrial/Alibaba/cn-beijing`；本机
   非秘密文件 `%LOCALAPPDATA%\Listen\cloud-trial\ali.json` 只保存 `workspace_id`、`model`、
   `region`。不要把 Key 写进命令行、聊天、日志或脚本。

   只读检查与维护：

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\scripts\configure-cloud-trial.ps1 -ShowStatus
   powershell -ExecutionPolicy Bypass -File .\scripts\configure-cloud-trial.ps1 -SelfTest
   powershell -ExecutionPolicy Bypass -File .\scripts\configure-cloud-trial.ps1 -RemoveKey
   ```

   `-ShowStatus` 只显示是否存在凭据、固定厂商/型号/地域、共享账本占用，不显示 Key；
   `-SelfTest` 用临时假凭据执行一次真实 Credential Manager 写入/读回/删除，不联网；
   `-RemoveKey` 只删除应用凭据，保留配置与账本。修改后需要重新启动 Listen。

3. **双击 `启动 Listen.cmd`（日常默认）。** 胶囊变为就绪后，按 **Ctrl+Alt+R** 开始，再按一次
   停止；使用方式与以前的本地默认完全一致。

## 运行时显示的实际来源

- `runtime-cloud\status.txt` 的 `recognition_mode=cloud-trial`、`backend=` 行和展开结果面板
  会写出实际选择：`Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message;
  region=cn-beijing; ...`。
- 型号或地域被本机配置改成不支持的值时，显示 `unsupported model (check local settings)` 或
  `unsupported region (check local settings)`，并在开始前拒绝，不会悄悄换成别的型号/地域。
- 原生/网页编辑区交付、原目标绑定、手改保护、结果面板与复制规则与本地模式相同；**永不自动
  发送、不自动摘要、不自动入会**。

## 缺凭据、免费保护失败、网络波动时

- 缺 Key：提示 `No Alibaba cloud API key is configured. Use scripts/configure-cloud-trial.ps1
  locally ... no cloud request was sent`；本次不发起云端请求，也不改用本地识别。
- 工作空间/型号/地域不合法：提示 `Cloud trial requires a valid Alibaba workspace ...`；本次
  不采集、不发送，不写任何目标。
- 免费保护无法核验：提示检查 CLI 登录、额度和保护设置；本次不发送模型任务和音频，已收到
  的录音保留。
- 网络短暂中断：继续采集并自动重连、补传；停止后仍会等待整段确认。**不会**因为云端失败而
  自动调用本地识别，也不会把未确认文字交付。
- 账号、余额、型号未开通等无法自动恢复的问题：明确提示并保留录音；不会反复高频重试。
- 云端准备失败后仍可显式改用本地：关闭 Listen，双击 `启动 Listen 本地备用.cmd` 做**新的**
  本机识别采集。这是显式选择，不是失败后的自动降级；云端运行目录里的待处理音频仍保留，
  下次打开默认启动器时可继续。

## 费用与免费额度（重要）

- 该型号的免费额度**到 2026-12-21 为止**；之后按厂商实际计费转为**付费**。费用取决于你的
  账号、地域、实际音频时长和用量，本包不提供固定金额承诺。
- 首轮试用共享硬上限 **CNY 50**：所有已结算估计与未决预留之和加新预留不得超过该值。每次尝试
  （包括重试）先在共享账本 `%LOCALAPPDATA%\Listen\cloud-trial\ledger.json` 记录费用准入。
  不会自动购买套餐、自动提高预算或扩大免费额度。
- **CNY 100/月是提醒线，不是停用上限**：本月合计达到该线的 90%（CNY 90）时就提前提示，
  超过 100 元仍只提示；不会中止采集、取消录音或自动提高额度。
- **每次请求前都做实时免费保护核验**：唯一默认业务空间、精确型号、额度 > 0、`autoStop=true`、
  到期日。核验不通过就不发请求；核验通过也不等于“现金账单一定是 0 元”。
- 面板会显示本工具累计请求、已确认/保守估算/未知用量与金额；未知不会显示成 0，自动重试的
  用量同样计入。
- 账本只是本工具的估算；**实际现金账单需要你自己在阿里云控制台核对**。本交付包没有做过
  真实账单对账，也不把估算金额当作已核对账单。

## 数据与单一厂商

- 只发送本次采集的音频和开始时固定的术语表，不发送目标草稿、其他窗口、截图或剪贴板。Key
  不进入状态文件、日志或恢复数据。
- 日常只向阿里云一家发送录音，不常规双发，不在失败时跨厂商切换，也不用本地结果冒充云端结果。
- 录音的留存、训练使用及例外条款取决于你的账号、地域和型号。本包**不宣称零留存**，也不
  替代你在阿里云控制台核实的条款。

## 显式本地备用

需要不发送音频、或云端不可用时：

1. 准备本地模型（见 `README.md` 的“模型准备”）。
2. 关闭云端 Listen，双击 **`启动 Listen 本地备用.cmd`**。
3. `runtime\status.txt` 的 `recognition_mode=local`、`backend=` 和面板会写明本机引擎；音频
   不出本机。

本地备用不读取 `runtime-cloud\` 的待处理音频；云端恢复只由云端模式继续。

## 本包尚未证明什么

- 验收的现金账单**没有对账**；验收只证明免费保护使额外现金扣费路径不成立。
- 数据处理条款（留存、训练用途）只有官方文档引用，没有做独立核实。
- 验收批次中 1 段未返回最终用量（如实记为未知，未记为 0）；其余 19 段用量与免费额度扣减精确吻合。
- 弱网、断网恢复的真实网络演练未在验收批次执行；应用级恢复由离线可控测试覆盖。
- 延迟含最多约 50 ms 的测试轮询滞后；交付目标为受控原生编辑框夹具，不是所有第三方编辑器。
