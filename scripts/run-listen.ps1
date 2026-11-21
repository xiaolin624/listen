param(
    [string]$ToolsRoot = '',
    [string]$Runtime = '',
    # Empty selects the daily default: the accepted Alibaba cloud trial. Pass
    # -Engine sensevoice (or whisper / a whisper-cli.exe path) to select the
    # explicit local fallback; -CloudTrial ali selects the cloud path explicitly.
    # The two selections cannot be combined.
    [string]$Engine = '',
    [ValidateSet('', 'ali')][string]$CloudTrial = '',
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
            # Stopping something that is not running is not a failure: a restart
            # sequence should not turn red just because the instance already exited.
            if ($Control -eq 'quit' -and $stderr -match 'Listen is not running') {
                Write-Output 'Listen is not running; nothing to stop.'
                return
            }
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
$engineKind = $Engine.Trim()
if ($CloudTrial -and $engineKind) {
    throw '-CloudTrial ali cannot be combined with -Engine; cloud trial has no local fallback. Omit -Engine for the cloud default, or omit -CloudTrial to select local recognition.'
}
$localMode = [bool]$engineKind
if (-not $Runtime) {
    # Cloud continuous PCM and local recognition windows must not share a runtime:
    # a recovery must never hand one mode's audio to the other adapter.
    $Runtime = if ($localMode) { Join-Path $repositoryRoot '.artifacts\listen-local' } else { Join-Path $repositoryRoot '.artifacts\listen' }
}
if (-not $Glossary) { $Glossary = Join-Path $projectRoot 'packaging\glossary.txt' }
$whisperPath = $false
if ($engineKind -ieq 'whisper') {
    $whisperPath = $true
    if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
    if (-not $VadModel) { $VadModel = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin' }
} elseif ($engineKind -ieq 'sensevoice' -or $engineKind -ieq 'sense-voice' -or $engineKind -ieq 'sense_voice') {
    # The local fallback is explicit even for the resident engine.
    $engineKind = 'sensevoice'
    if ($Model -or $VadModel) {
        throw 'The SenseVoice engine does not take --model or --vad-model; select -Engine whisper to use the whisper.cpp fallback.'
    }
} elseif ($engineKind) {
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
if ($CloudTrial) { $launchArguments += @('--cloud-trial', 'ali') }
if ($whisperPath) {
    $launchArguments += @('--engine', (ConvertTo-QuotedWindowsPath $whisperExecutable), '--model', (ConvertTo-QuotedWindowsPath $Model), '--vad-model', (ConvertTo-QuotedWindowsPath $VadModel), '--backend', $Backend)
}
elseif ($localMode) {
    # The local fallback is an explicit selection; the cloud default is never
    # silently replaced by local recognition.
    $launchArguments += @('--engine', 'sensevoice')
}
if ($Hidden) { $launchArguments += '--hidden' }
$process = Start-Process -FilePath $binary -ArgumentList $launchArguments -WindowStyle Hidden -PassThru
Write-Output "Listen PID: $($process.Id)"
Write-Output "Engine: $(if ($CloudTrial) { 'Alibaba cloud trial / qwen-audio-3.1-asr-flash-message / cn-beijing (explicit selection; budget protection applies)' } elseif ($whisperPath) { $whisperExecutable + ' (explicit local fallback)' } elseif ($localMode) { 'SenseVoice (explicit local fallback, sherpa-onnx)' } else { 'Alibaba cloud default / qwen-audio-3.1-asr-flash-message / cn-beijing (daily default; live free-only protection applies)' })"
Write-Output "Current status: $(Join-Path $Runtime 'status.txt')"
Write-Output "Glossary: $Glossary"
Write-Output "Panel text: $(Join-Path $Runtime 'panel.txt')"
Write-Output 'Ctrl+Alt+R starts/stops capture. Click the capsule or use --control show to open the result panel; Listen stays in the tray.'
