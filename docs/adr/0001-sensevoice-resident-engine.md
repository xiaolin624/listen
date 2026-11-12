# 默认识别引擎改用 SenseVoice（sherpa-onnx 常驻）

状态：accepted（2026-11-12）

whisper.cpp small 在本机 CPU 上每条 10 秒音频要 6–10 秒，中文输出偏繁体、标点弱；同一批音频上 SenseVoice q8 只要 0.4–1.2 秒，普通话长句几乎全对，但纯 CTC 没有 prompt/热词偏置，英文专名更差（DeepSeek→“deep sea”），静音会吐 “The.”。决定：默认识别改为 SenseVoice，通过 sherpa-onnx 常驻应用进程（C API / Rust crate），窗口仍由 Silero VAD 切分并做静音门禁；专名用本地术语表做拼音/模糊校正；whisper 从交付包移除，只保留代码回退。为达到“停止→结果 <1 秒（10 秒语音）”，采集中持续识别（用 CPU 换延迟）；模型在后台加载，加载完成前允许开始采集并先缓冲音频。

## 考虑过的替代方案

- whisper small + prompt：英文术语更准，但慢 10 倍、中文变繁体、实测会丢整句。
- whisper medium / large-v3-turbo：中文 CER 仍 20% 以上，CPU 上更慢。
- SenseVoice GGUF CLI：单次更快，但每次识别起一个进程、模型不能常驻。
- Paraformer + 解码期热词：能原生偏置专名，但 Python/GGUF 两条路都没有可用的 hotword 入口。

## 后果

- 包体从约 493 MB 降到约 260 MB；需要 AVX2（自用机器满足）。
- 发布必须保留 FunASR/SenseVoice 的模型署名（FunASR MODEL_LICENSE）。
- 启用前仍以真实会议链路语料 benchmark 为闸门；术语规模按几十到几百设计，上千且每天新增则需要重新考虑热词引擎。
- 没有词级时间戳，只有 VAD 段级边界；现有按窗口提交的流程不受影响。
