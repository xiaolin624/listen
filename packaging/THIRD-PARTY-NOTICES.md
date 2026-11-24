# 第三方署名与许可证（Listen 交付包）

本文件是 Listen 交付包内第三方组件的署名与许可证说明。包内 `licenses\` 目录保存了
每个上游许可证的**原文**；本文件只做索引与来源记录，不替代原文，也不改写任何条款。

本包**不再包含** whisper.cpp 的模型或运行库（ticket #23 起换为 SenseVoice q8）。
本地识别使用 SenseVoiceSmall q8 模型，通过 sherpa-onnx C API 在应用进程内常驻运行。

## 1. FunASR / SenseVoice 模型权重

- 组件：`SenseVoiceSmall` 语音识别模型（本包内的 `models\sense-voice\model.int8.onnx`、
  `models\sense-voice\tokens.txt`、`models\sense-voice\silero_vad.onnx`）。
- 来源：FunASR / SenseVoice 项目（<https://github.com/modelscope/FunASR>、
  <https://github.com/FunAudioLLM/SenseVoice>）。本包内的 ONNX 文件是 sherpa-onnx
  对 SenseVoiceSmall 的 int8 转换产物，模型列表见
  <https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html>。
- 许可证：**FunASR MODEL_LICENSE**（FunASR Model Open Source License Agreement v1.1，
  版权所有 (C) [2023-2028] [Alibaba Group]）。原文：`licenses\FunASR-MODEL_LICENSE.txt`，
  取自 <https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE>。
- 该许可证 2.2 条要求：使用、复制、修改、分享时必须**注明出处以及作者信息，并保留
  相关模型名称**。因此本包在 `README.md`、`VERSION.txt` 和本文件中都保留
  “SenseVoiceSmall / FunASR” 字样与来源链接。
- 注意：模型权重与仓库源码的许可证不同。SenseVoice 仓库源码是 MIT；模型权重按各自的
  模型卡条款分发，本包按 FunASR MODEL_LICENSE 分发 SenseVoiceSmall 权重。

## 2. sherpa-onnx

- 组件：`engine\sherpa\sherpa-onnx-c-api.dll`（sherpa-onnx v2.0.1 Windows x64 shared MT
  构建的 C API）。
- 来源：<https://github.com/k2-fsa/sherpa-onnx>。
- 许可证：**Apache License, Version 2.0**。原文：`licenses\sherpa-onnx-LICENSE.txt`，
  取自 <https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE>。
- Apache License 2.0 第 4 条要求随分发提供许可证副本并保留署名声明；因此本包同时携带
  本文件与该许可证原文。

## 3. ONNX Runtime

- 组件：`engine\sherpa\onnxruntime.dll`、`engine\sherpa\onnxruntime_providers_shared.dll`
  （ONNX Runtime 1.28.2，Microsoft Corporation）。
- 来源：<https://github.com/microsoft/onnxruntime>。
- 许可证：**MIT License**（Copyright (c) Microsoft Corporation）。原文：
  `licenses\onnxruntime-LICENSE.txt`。上游规范路径是
  <https://github.com/microsoft/onnxruntime/blob/main/LICENSE>；本机打包环境无法直接
  取到该 URL（整个 `microsoft/onnxruntime` 仓库路径返回 HTTP 404），因此原文取自同仓库
  的官方镜像仓库 <https://raw.gitcode.com/microsoft/onnxruntime/raw/main/LICENSE>，
  并与另一份逐字副本（Hugging Face 上的 ONNX Runtime 许可证副本）比对一致。
- MIT 许可证要求随分发保留版权声明与许可声明；因此本包携带本文件与该许可证原文。

## 4. 包内许可证原文清单与校验

| 文件 | 上游来源 | 字节 | 文本 SHA-256 |
| --- | --- | ---: | --- |
| `licenses\FunASR-MODEL_LICENSE.txt` | <https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE> | 5306 | `7DBA975A2069691DB4992B0592D70828B330D2F8A30A71450F4E152A554E84F8` |
| `licenses\sherpa-onnx-LICENSE.txt` | <https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE> | 11358 | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |
| `licenses\onnxruntime-LICENSE.txt` | <https://github.com/microsoft/onnxruntime/blob/main/LICENSE>（经上述镜像取得） | 1073 | `2F07C72751AED99790B8A4869CF2311DF85A860B22DED05FA22803587A48922C` |

上表哈希是**文本文件本身的 SHA-256**，可用 PowerShell 复核：

```powershell
Get-FileHash .\licenses\FunASR-MODEL_LICENSE.txt -Algorithm SHA256
Get-FileHash .\licenses\sherpa-onnx-LICENSE.txt -Algorithm SHA256
Get-FileHash .\licenses\onnxruntime-LICENSE.txt -Algorithm SHA256
```

上表数值对应**以 LF 换行保存**的字节（本包内就是这一形式）。换行口径来自**本机 Git 的系统级
配置**：`git config --system core.autocrlf` 为 `true`；本仓库没有 `.gitattributes`，也不会把行尾
规范化写进仓库。换一台机器重新检出时，这些 `.txt` 在工作树里可能变成 CRLF，字节数与哈希随之
不同——只有行尾不同，条款文字不变。上表与包内哈希一律按 **LF** 计算；如果你用检出后的工作树
重新打包，请以**包内文件**重新计算哈希，不要拿上表去核对 CRLF 版本。

## 5. 已移除的组件

whisper.cpp 引擎（`whisper-cli.exe`、`whisper.dll`、`ggml*.dll`、`vad-speech-segments.exe`）
与 whisper 模型（`ggml-small.bin`、`ggml-silero-v5.1.2.bin`）自 ticket #23 起不再随本包
分发；`manifest.json` 的 `engine_source` 与 `engine_files` 也不含它们。
