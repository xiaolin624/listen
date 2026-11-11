param(
    [string]$ToolsRoot = '',
    [string]$Runtime = '',
    [string]$Engine = '',
    [string]$Model = '',
    [switch]$Hidden,
    [ValidateSet('', 'start', 'stop', 'toggle', 'show', 'quit')][string]$Control = ''
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$binary = Join-Path $projectRoot 'app\listen\target\release\listen.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw 'Build first with .\scripts\build-listen.ps1.'
}
if ($Control) {
    $process = Start-Process -FilePath $binary -ArgumentList @('--control', $Control) -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Listen control failed ($($process.ExitCode))" }
    return
}
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
if (-not $ToolsRoot) { $ToolsRoot = Join-Path $repositoryRoot '.tools' }
if (-not $Runtime) { $Runtime = Join-Path $repositoryRoot '.artifacts\listen' }
if (-not $Engine) { $Engine = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe' }
if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-base.bin' }
# Start-Process joins ArgumentList; quote literal Windows paths containing spaces.
foreach ($path in @($Engine, $Model, $Runtime)) {
    if ($path.Contains('"')) { throw 'Paths must not contain a double quote.' }
}
function ConvertTo-QuotedWindowsPath([string]$Path) {
    # A trailing backslash must be doubled before the closing command-line quote.
    '"' + ([IO.Path]::GetFullPath($Path) -replace '(\\+)$', '$1$1') + '"'
}
$launchArguments = @('--engine', (ConvertTo-QuotedWindowsPath $Engine), '--model', (ConvertTo-QuotedWindowsPath $Model), '--runtime', (ConvertTo-QuotedWindowsPath $Runtime))
if ($Hidden) { $launchArguments += '--hidden' }
$process = Start-Process -FilePath $binary -ArgumentList $launchArguments -WindowStyle Hidden -PassThru
Write-Output "Listen PID: $($process.Id)"
Write-Output "Current status: $(Join-Path $Runtime 'status.txt')"
Write-Output 'Ctrl+Alt+R starts/stops capture. Closing the window keeps Listen in the tray.'
