param(
    [string]$ToolsRoot = '',
    [string]$Runtime = '',
    # sensevoice (default) or whisper select the engine; a path selects whisper.cpp
    # and uses that executable. Only the whisper paths are passed to listen.exe, so
    # the default launch exercises the same SenseVoice engine the product ships.
    [string]$Engine = 'sensevoice',
    [string]$Model = '',
    [string]$VadModel = '',
    [ValidateSet('host', 'cli')][string]$Backend = 'host',
    # The dev run uses the glossary shipped in the repository; pass -Glossary to
    # try a different file. A missing file still means "no correction", never a
    # failed launch.
    [string]$Glossary = '',
    [switch]$Hidden,
    [ValidateSet('', 'start', 'stop', 'toggle', 'retry', 'cleanup', 'show', 'panel', 'copy', 'refine', 'quit')][string]$Control = ''
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$binary = Join-Path $projectRoot 'app\listen\target\release\listen.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw 'Build first with .\scripts\build-listen.ps1.'
}
if ($Control) {
    # A control invocation must never hang on a modal dialog. The application
    # reports control failures on stderr and a non-zero exit code; enforce a bound
    # and kill a wedged control process instead of waiting forever.
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.Arguments = '--control ' + $Control
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) {
            try { $process.Kill() } catch { }
            throw "Listen control '$Control' did not exit within 15 seconds."
        }
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) {
            throw "Listen control failed ($($process.ExitCode)): $stderr"
        }
        if ($stderr) { Write-Output $stderr }
    }
    finally {
        $process.Dispose()
    }
    return
}
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
if (-not $ToolsRoot) { $ToolsRoot = Join-Path $repositoryRoot '.tools' }
if (-not $Runtime) { $Runtime = Join-Path $repositoryRoot '.artifacts\listen' }
if (-not $Glossary) { $Glossary = Join-Path $projectRoot 'packaging\glossary.txt' }
$engineKind = $Engine.Trim()
$whisperPath = $false
if ($engineKind -ieq 'whisper') {
    $whisperPath = $true
    if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
    if (-not $VadModel) { $VadModel = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin' }
} elseif ($engineKind -ieq 'sensevoice' -or $engineKind -ieq 'sense-voice' -or $engineKind -ieq 'sense_voice') {
    # The default engine needs no flags; passing none is what keeps this launcher
    # honest about the shipped default.
    $engineKind = 'sensevoice'
    if ($Model -or $VadModel) {
        throw 'The SenseVoice engine does not take --model or --vad-model; select -Engine whisper to use the whisper.cpp fallback.'
    }
} else {
    $whisperPath = $true
    if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
    if (-not $VadModel) { $VadModel = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin' }
}
# Start-Process joins ArgumentList; quote literal Windows paths containing spaces.
$quotedPaths = @($Runtime, $Glossary)
if ($whisperPath) {
    if (-not $Model) { throw 'The whisper.cpp fallback needs -Model.' }
    if (-not $VadModel) { throw 'The whisper.cpp fallback needs -VadModel.' }
    $quotedPaths += @($Model, $VadModel)
    if ($engineKind -ieq 'whisper') {
        $whisperExecutable = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe'
    } else {
        $whisperExecutable = $engineKind
    }
    $quotedPaths += $whisperExecutable
}
foreach ($path in $quotedPaths) {
    if ($path.Contains('"')) { throw 'Paths must not contain a double quote.' }
}
function ConvertTo-QuotedWindowsPath([string]$Path) {
    # A trailing backslash must be doubled before the closing command-line quote.
    '"' + ([IO.Path]::GetFullPath($Path) -replace '(\\+)$', '$1$1') + '"'
}
$launchArguments = @('--runtime', (ConvertTo-QuotedWindowsPath $Runtime), '--glossary', (ConvertTo-QuotedWindowsPath $Glossary))
if ($whisperPath) {
    $launchArguments += @('--engine', (ConvertTo-QuotedWindowsPath $whisperExecutable), '--model', (ConvertTo-QuotedWindowsPath $Model), '--vad-model', (ConvertTo-QuotedWindowsPath $VadModel), '--backend', $Backend)
}
if ($Hidden) { $launchArguments += '--hidden' }
$process = Start-Process -FilePath $binary -ArgumentList $launchArguments -WindowStyle Hidden -PassThru
Write-Output "Listen PID: $($process.Id)"
Write-Output "Engine: $(if ($whisperPath) { $whisperExecutable } else { 'SenseVoice (default resident engine, sherpa-onnx)' })"
Write-Output "Current status: $(Join-Path $Runtime 'status.txt')"
Write-Output "Glossary: $Glossary"
Write-Output "Panel text: $(Join-Path $Runtime 'panel.txt')"
Write-Output 'Ctrl+Alt+R starts/stops capture. Click the capsule or use --control show to open the result panel; Listen stays in the tray.'
