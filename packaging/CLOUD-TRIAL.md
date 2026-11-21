# Listen 云端试用配置（阿里云百炼，显式选择）

本包**没有**把云端识别设为日常默认：日常启动 `启动 Listen.cmd` 仍走本地识别；只有双击
`启动 Listen 云端试用.cmd`（或显式传入 `--cloud-trial ali`）才会选择云端试用。云端与本地
使用不同的运行目录，云端待取录音不会被本地模式自动读取，也不会自动改用本地结果冒充云端结果。

固定选择（不会自动换厂商、型号或地域）：

| 项目 | 值 |
| --- | --- |
| 厂商 | 阿里云百炼 Model Studio |
| 型号 | `qwen-audio-3.1-asr-flash-message` |
| 地域 | `cn-beijing`（北京） |
| 识别路径 | 北京 Message 流式识别；采集的连续音频 + 开始时固定的术语表 |

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

3. **双击 `启动 Listen 云端试用.cmd`。** 胶囊变为就绪后，按 **Ctrl+Alt+R** 开始，再按一次
   停止；使用方式与本地默认完全一致。

## 运行时显示的实际来源

- `runtime-cloud\status.txt` 的 `backend=` 行和展开结果面板会写出实际选择：
  `Cloud trial: Alibaba Model Studio; model=qwen-audio-3.1-asr-flash-message; region=cn-beijing; ...`。
- 型号或地域被本机配置改成不支持的值时，显示 `unsupported model (check local settings)` 或
  `unsupported region (check local settings)`，并在开始前拒绝，不会悄悄换成别的型号/地域。
- 原生/网页编辑区交付、原目标绑定、手改保护、结果面板与复制规则与本地模式相同；**永不自动
  发送、不自动摘要、不自动入会**。

## 缺凭据、免费保护失败、网络波动时

- 缺 Key：提示使用本机配置助手，本次不发起云端请求。
- 工作空间/型号/地域不合法：提示检查本机配置，本次不采集、不发送。
- 免费保护无法核验：提示检查 CLI 登录、额度和保护设置；本次不发送模型任务和音频，已收到
  的录音保留。
- 网络短暂中断：继续采集并自动重连、补传；停止后仍会等待整段确认。**不会**因为云端失败而
  自动调用本地识别，也不会把未确认文字交付。
- 账号、余额、型号未开通等无法自动恢复的问题：明确提示并保留录音；不会反复高频重试。

需要本地备用时，关闭云端模式并双击 `启动 Listen.cmd` 进行**新的**本机识别采集。这是显式
选择，不是失败后的自动降级；云端运行目录里的待处理录音仍保留，下次打开云端试用时可继续。

## 费用与数据

- 首轮试用硬上限 **CNY 50**：所有已结算估计与未决预留之和加新预留不得超过该值。每次尝试
  （包括重试）先在共享账本 `%LOCALAPPDATA%\Listen\cloud-trial\ledger.json` 记录费用准入。
- 面板会显示本工具累计请求、已确认/保守估算/未知用量与金额；未知不会显示成 0。
- **CNY 100/月是提醒线，不是停用上限**：达到或超过时只提示，不中止采集、不取消录音、不自动
  购买套餐或提高额度。
- 账本只是本工具的估算；**实际现金账单需要你自己在阿里云控制台核对**。本交付包没有做过
  真实账单对账，也不把估算金额当作已核对账单。
- 只发送本次采集的音频和开始时固定的术语表，不发送目标草稿、其他窗口、截图或剪贴板。Key
  不进入状态文件、日志或恢复数据。
- 录音的留存、训练使用及例外条款取决于你的账号、地域和型号。本包**不宣称零留存**，也不
  替代你在阿里云控制台核实的条款。

## 本包尚未证明什么

本交付包只完成离线可验证的交付与配置检查。**真实付费网络调用、真实编辑器的 20 段准确率与
“停止后 1 秒内完整回填”验收、以及实际账单对账都尚未运行**，因此：

- 不声称任何云端候选已通过 #26 的验收门槛；
- 不把云端设为日常默认（日常默认仍是本地识别）；
- 不提供准确率、延迟或费用的承诺数字。

验收进展与阻塞条件见仓库 `docs/issue-40-delivery.md`。
