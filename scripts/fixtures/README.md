# 桌面验收夹具（仓库内容）

这三个 C# 文件是 `scripts\verify-native-target.ps1`、`scripts\verify-package-retry.ps1`、
`scripts\verify-package-device.ps1` 用 `Add-Type -Path` 现场编译的验收夹具。它们原先只存在于
被 Git 忽略的 `.artifacts\issue-11\accept\`，干净检出无法运行那三个脚本；复制入库后，
三个脚本的 `-FixtureRoot` 默认值改为本目录，`-FixtureRoot` 仍可指回旧路径。

| 文件 | 用途 | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `PackageFixture.cs` | 操作者自有的 WinForms EDIT 夹具：目标、邻居、另一个窗口、发送按钮、容量/只读/丢弃写入模式；`Sends`/`ReturnKeys` 是真实的发送与回车计数 | 11,420 | `3901C5EF684564A79FCAB1D33B1EE2AE2A3B78D2719B5FD2640AF5CCFDE5A205` |
| `ListenNativeQa.cs` | 绑到被测 Listen 进程的 Win32 辅助：读窗口文本、点控件、发送真实输入 | 16,039 | `8292B4B65775F17C7BD34C5CA1BE8495B91D0F1BFB3005CA963E301E8189579C` |
| `AudioRoute.cs` | MMDeviceEnumerator + Windows 策略 COM：读/写真实默认播放端点，并把 PCM 播到指定端点 | 21,366 | `4E05967EA6A7B90A239F92978137F3BE4F72E1164EB63D19157E85C61A6375CA` |

- 内容与 `.artifacts\issue-11\accept\` 下的同名文件**逐字节相同**（上面的 SHA-256 可复核；按 **LF** 字节计算，
  与仓库索引一致。和许可证文本一样，`core.autocrlf=true` 的机器重新检出后工作树里会是 CRLF，哈希会不同）；
  只依赖 .NET 基类库（`System`、`System.Drawing`、`System.Windows.Forms`、`System.Runtime.InteropServices`），
  不需要同目录的其它文件。
- 接口不变：`PackageFixture.Start/ResetTarget/SetMode/SetCapacity/SetReadOnly/DestroyTarget/Text/Close`、
  `ListenNativeQa.ReadText/ClickControl`、`AudioRoute.Multimedia/Communications/Console/SetDefault/StartPlayback/WaitPlayback`。
- 这些夹具是**操作者侧**的窗口与设备驱动，不是产品代码；`docs\issue-24-verification.md` 的验收口径
  （只断言用户可观察行为）仍然成立：夹具只负责制造目标/邻居/端点场景并读回文本。
