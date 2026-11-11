@echo off
setlocal
cd /d "%~dp0"
if not exist "%~dp0listen.exe" (
  echo Listen executable is missing: %~dp0listen.exe
  pause
  exit /b 1
)
if not exist "%~dp0libunwind.dll" (
  echo The required runtime library libunwind.dll is missing next to listen.exe.
  echo Keep listen.exe, libunwind.dll, engine and models together in the extracted folder.
  pause
  exit /b 1
)
if not exist "%~dp0engine\whisper-cli.exe" goto engine_missing
if not exist "%~dp0engine\vad-speech-segments.exe" goto engine_missing
if not exist "%~dp0models\ggml-small.bin" goto model_missing
if not exist "%~dp0models\ggml-silero-v5.1.2.bin" goto model_missing
start "" "%~dp0listen.exe" --engine "%~dp0engine\whisper-cli.exe" --model "%~dp0models\ggml-small.bin" --vad-model "%~dp0models\ggml-silero-v5.1.2.bin" --runtime "%~dp0runtime"
exit /b 0

:engine_missing
echo The local recognition engine is incomplete.
echo Expected engine\whisper-cli.exe, engine\vad-speech-segments.exe and their adjacent DLLs.
pause
exit /b 1

:model_missing
echo The local speech models are not prepared.
echo Put ggml-small.bin and ggml-silero-v5.1.2.bin into the models folder, then run this file again.
echo See README.md for the model sources and SHA-256 values.
pause
exit /b 1
