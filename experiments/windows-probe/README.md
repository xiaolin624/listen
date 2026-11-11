# Windows 技术验证程序

这是一次性技术验证程序，尚非产品。它用于检验原生音频采集和目标输入框写入的基础能力，并配合本地识别引擎做性能基准。

## 已实现的实验

- 从默认多媒体播放设备进行 WASAPI loopback 采集，输出单声道 16 kHz PCM WAV；不打开麦克风。
- 对自建的原生测试输入框保存 UI Automation 引用，读取最新内容后追加中英文，并比对写后结果。
- 验证采集期间新增的编辑得以保留、旁边的输入框未改变，以及目标销毁后写入返回失败。
- 用离线合成语音作为已知样本，测试采集及本地识别；不调用云端识别服务。

## 构建

本机的 Rust 1.99.1 gnullvm 工具链位于 `.tools/`。纯 Rust 构建使用该工具链的 rust-mingw 和 rust-lld，无需现有的 VC98 编译器。构建脚本会在可执行文件旁放置必需的 libunwind.dll。

在项目根目录运行：

```powershell
.\scripts\build-probe.ps1
.\experiments\windows-probe\target\release\listen-windows-probe.exe target-self-test
```

## 音频测试

下一组命令会通过当前默认播放设备播放约 26 秒合成中文语音，并同时采集该设备的混合声音。测试时请暂停其他声音，避免将其他播放内容混入测试文件。

```powershell
.\scripts\make-test-speech.ps1
.\experiments\windows-probe\target\release\listen-windows-probe.exe audio 28 .\.artifacts\verification\loopback-zh.wav .\.artifacts\verification\synthetic-zh.wav
```

音频参数 `28` 是这次实验的运行时间，不是产品的时长限制。程序逐包写入文件、按帧重采样；本次短测未验证长时间采集、设备切换或生产环境的无丢音保证。实验只有一个普通 WAV 文件，正式产品需要分段临时存储，避免单文件格式及内存积累成为长录音限制。

## 本地识别基准

已准备的引擎：whisper.cpp v1.8.2 CPU 版本，以及 CUDA 11.8 版本。后者还需 NVIDIA 官方 libcublas 11.11.3.6 运行库。下载来源、校验值和兼容依据记录在 [资料核验](../../.scratch/meeting-transcription/verification-sources.md)。模型为多语言 base，文件 SHA-256：`60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`。

```powershell
python .\scripts\benchmark-local-asr.py --engine .\.tools\whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe --model .\.tools\models\ggml-base.bin --audio .\.artifacts\verification\loopback-zh.wav --output .\.artifacts\verification\asr-base --backend cpu
python .\scripts\benchmark-local-asr.py --engine .\.tools\whisper-v1.8.2\bin\Release\whisper-cli.exe --model .\.tools\models\ggml-base.bin --audio .\.artifacts\verification\loopback-zh.wav --output .\.artifacts\verification\asr-base --backend cuda
```

脚本记录包含模型加载的整次进程耗时。识别过程只使用本地文件；测试日志和结果位于 `.artifacts/`，已被 Git 忽略。

## 结论边界

实际测量结果见 [验证报告](../../.scratch/meeting-transcription/verification-results.md)。本程序未实现全局快捷键、托盘界面、正式的备用文本框、长期后台分段识别或通用输入框适配。自建原生控件测试不代表 Codex 和网页编辑器已经通过兼容性测试。正式实现应在无法确认目标或写入结果时展示备用文本框，避免自动改换目标或盲目重试。
