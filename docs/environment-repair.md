# 本地开发环境修复记录

日期：2026-11-10。

## 原因与证据

`setup refresh had errors` 发生在命令启动之前。当天的 Codex 沙箱日志记录：

```text
deny ACE failed on E:\luyin\.git:
open deny ACL target for update (READ_CONTROL | WRITE_DAC):
拒绝访问。 (os error 5)
```

原 `.git` 所有者为 `AI\CodexSandboxOffline`。当前用户为 `AI\19677`，
无法以 `READ_CONTROL | WRITE_DAC` 打开该目录。沙箱刷新必须先保护 `.git`，
因此权限刷新失败会阻断命令执行，也会影响依赖该环境的 Node 运行时。
Git 同时报告了 `detected dubious ownership`。

`gh` 是另一项独立问题：修复前 `Get-Command gh` 失败，检查的安装目录、
注册表和 WinGet 均未找到 GitHub CLI。即使可执行文件存在，安装目录也需要位于 PATH。

## 已完成的修复

- 确认原 Git 仓库没有 refs、objects、index 或 packed-refs。
- 将原 Git 元数据完整备份，再以当前用户初始化空仓库并保留原配置。
- 添加 `origin`：`https://github.com/xiaolin624/listen.git`。
- 从 `cli/cli` 官方发布下载 GitHub CLI 2.103.3，验证 SHA-256 后安装到
  `%LOCALAPPDATA%\Programs\GitHub CLI`。
- 将 CLI 的 `bin` 目录添加到用户 PATH，并保留修改前的 PATH 备份。
- 在已有用户命令目录 `%APPDATA%\npm` 添加 `gh.cmd`，
  将命令转发到官方 `gh.exe`，供仍保留旧 PATH 的应用使用。
- 写入 `AGENTS.md` 和 `docs/agents/` 下的三份工程配置。

原 Git 元数据位于：

```text
.scratch/environment-repair/backups/20261110-215040-038/git-metadata/
```

同目录的 `before.json` 保存修改前的用户 PATH 和 Git 所有者。
该备份目录已加入本仓库的 `.git/info/exclude`。

## 一键修复与检查

在项目根目录运行：

```powershell
& .\scripts\repair-environment.ps1
```

只检查，不修改环境：

```powershell
& .\scripts\repair-environment.ps1 -CheckOnly
```

脚本支持重复执行。若遇到有提交历史或暂存内容的权限异常仓库，会停止自动重建。

## 已验证

- 直接运行 `gh --version` 成功，旧 PATH 的命令环境也能解析 `gh`。
- `.git` 的所有者为 `AI\19677`，ACL 更新权限检查返回 `0`。
- `git status` 和 `git remote -v` 成功。
- `codex sandbox --permission-profile :workspace -C E:\luyin -- cmd.exe /d /c echo SANDBOX_OK`
  返回 `SANDBOX_OK`，沙箱日志记录 `errors=[]`。
- Node REPL 已恢复，并成功读取四份项目配置。
- 修复脚本通过 PowerShell 语法检查与重复执行检查。

GitHub CLI 的账号授权需由用户在 GitHub 官方页面完成。
登录后的状态可通过 `gh auth status --hostname github.com` 检查。

## 官方参考

- [OpenAI：Windows 沙箱](https://learn.chatgpt.com/docs/windows/windows-sandbox)
- [GitHub CLI：官方安装说明](https://github.com/cli/cli#installation)
- [GitHub CLI：登录说明](https://cli.github.com/manual/gh_auth_login)
