@echo off
rem Explicit local fallback. Use this launcher only when you choose local
rem recognition (SenseVoice q8 through sherpa-onnx on this CPU). It is not the
rem daily default, it is never started automatically, and it never replaces a
rem cloud result.
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
if not exist "%~dp0engine\sherpa\sherpa-onnx-c-api.dll" goto engine_missing
if not exist "%~dp0engine\sherpa\onnxruntime.dll" goto engine_missing
if not exist "%~dp0models\sense-voice\model.int8.onnx" goto model_missing
if not exist "%~dp0models\sense-voice\tokens.txt" goto model_missing
if not exist "%~dp0models\sense-voice\silero_vad.onnx" goto model_missing
start "" "%~dp0listen.exe" --engine sensevoice --sherpa-lib "%~dp0engine\sherpa" --sensevoice-model "%~dp0models\sense-voice\model.int8.onnx" --sensevoice-tokens "%~dp0models\sense-voice\tokens.txt" --sensevoice-vad "%~dp0models\sense-voice\silero_vad.onnx" --runtime "%~dp0runtime" --glossary "%~dp0glossary.txt"
exit /b 0

:engine_missing
echo The local recognition engine is incomplete.
echo Expected engine\sherpa\sherpa-onnx-c-api.dll, engine\sherpa\onnxruntime.dll and their provider DLL.
pause
exit /b 1

:model_missing
echo The local SenseVoice q8 model files are not prepared.
echo Put model.int8.onnx, tokens.txt and silero_vad.onnx into the models\sense-voice folder,
echo then run this file again. See README.md for the model sources and SHA-256 values.
pause
exit /b 1
