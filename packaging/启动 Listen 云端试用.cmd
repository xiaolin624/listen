@echo off
rem Explicit Alibaba cloud trial. The plain launcher next to this file remains the
rem local daily default; this launcher never replaces it and never switches back
rem to local automatically.
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
if not exist "%~dp0scripts\configure-cloud-trial.ps1" (
  echo The cloud trial configuration helper is missing: %~dp0scripts\configure-cloud-trial.ps1
  echo Extract the complete package again before starting the cloud trial.
  pause
  exit /b 1
)
if not exist "%~dp0CLOUD-TRIAL.md" (
  echo The cloud trial guide is missing: %~dp0CLOUD-TRIAL.md
  echo Extract the complete package again before starting the cloud trial.
  pause
  exit /b 1
)
start "" "%~dp0listen.exe" --cloud-trial ali --runtime "%~dp0runtime-cloud" --glossary "%~dp0glossary.txt"
exit /b 0
