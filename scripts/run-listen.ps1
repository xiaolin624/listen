param(
    [string]$ToolsRoot = '',
    [string]$Runtime = '',
    [string]$Engine = '',
    [string]$Model = '',
    [string]$VadModel = '',
    [ValidateSet('host', 'cli')][string]$Backend = 'host',
    [switch]$Hidden,
    [ValidateSet('', 'start', 'stop', 'toggle', 'retry', 'cleanup', 'show', 'quit')][string]$Control = ''
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
if (-not $Engine) { $Engine = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe' }
if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
if (-not $VadModel) { $VadModel = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin' }
# Start-Process joins ArgumentList; quote literal Windows paths containing spaces.
foreach ($path in @($Engine, $Model, $VadModel, $Runtime)) {
    if ($path.Contains('"')) { throw 'Paths must not contain a double quote.' }
}
function ConvertTo-QuotedWindowsPath([string]$Path) {
    # A trailing backslash must be doubled before the closing command-line quote.
    '"' + ([IO.Path]::GetFullPath($Path) -replace '(\\+)$', '$1$1') + '"'
}
$launchArguments = @('--engine', (ConvertTo-QuotedWindowsPath $Engine), '--model', (ConvertTo-QuotedWindowsPath $Model), '--vad-model', (ConvertTo-QuotedWindowsPath $VadModel), '--runtime', (ConvertTo-QuotedWindowsPath $Runtime), '--backend', $Backend)
if ($Hidden) { $launchArguments += '--hidden' }
$process = Start-Process -FilePath $binary -ArgumentList $launchArguments -WindowStyle Hidden -PassThru
Write-Output "Listen PID: $($process.Id)"
Write-Output "Current status: $(Join-Path $Runtime 'status.txt')"
Write-Output 'Ctrl+Alt+R starts/stops capture. Closing the window keeps Listen in the tray.'
