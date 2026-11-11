# Issue tracker: GitHub

仓库：https://github.com/xiaolin624/listen
需求、规格和实施任务保存在该仓库的 GitHub Issues。

使用已登录的 GitHub CLI（`gh`）读取、创建和更新 Issues。
所有操作通过 `--repo xiaolin624/listen` 显式指定目标仓库；使用 `gh api` 时在端点中指定 `repos/xiaolin624/listen/`。

每个实施任务单独创建 Issue，并关联所属规格。
优先使用 GitHub 原生子任务和阻塞关系。
接口不支持时，在正文顶部使用：

- `Part of #<父任务编号>`
- `Blocked by: #<阻塞任务编号>`

所有阻塞任务关闭后，任务才可开始。
完成时记录结果和验证证据，再关闭任务。
多行正文通过结构化参数提交；使用 CLI 时通过 `--body-file` 传递。

PRs as a request surface: no.
