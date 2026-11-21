@echo off
rem Daily default (issue #40): the accepted Alibaba cloud path. If the local
rem cloud credential, workspace, model or region is missing or invalid, Listen
rem refuses the capture and explains why; it never falls back to local recognition.
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
  echo The cloud configuration helper is missing: %~dp0scripts\configure-cloud-trial.ps1
  echo Extract the complete package again before starting Listen.
  pause
  exit /b 1
)
if not exist "%~dp0CLOUD-TRIAL.md" (
  echo The cloud configuration guide is missing: %~dp0CLOUD-TRIAL.md
  echo Extract the complete package again before starting Listen.
  pause
  exit /b 1
)
start "" "%~dp0listen.exe" --runtime "%~dp0runtime-cloud" --glossary "%~dp0glossary.txt"
exit /b 0
