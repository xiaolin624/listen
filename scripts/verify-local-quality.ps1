param(
    [string]$ToolsRoot = '',
    [string]$Corpus = '',
    [string]$Model = ''
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
if (-not $ToolsRoot) { $ToolsRoot = Join-Path $repositoryRoot '.tools' }
if (-not $Corpus) { $Corpus = Join-Path $repositoryRoot '.artifacts\quality-corpus' }
$env:LISTEN_WHISPER = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe'
if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
$env:LISTEN_MODEL = [IO.Path]::GetFullPath($Model)
$env:LISTEN_VAD_MODEL = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin'
$env:LISTEN_QUALITY_CORPUS = [IO.Path]::GetFullPath($Corpus)
& (Join-Path $PSScriptRoot 'build-listen.ps1') -ToolsRoot $ToolsRoot -Task Quality
