param(
    [string]$PackageRoot = '',
    [string]$Runtime = '',
    [string]$Evidence = '',
    [int]$TimeoutSeconds = 90,
    [switch]$SkipHash
)
# Check a packaged Listen directory and start it with the packaged engine/model paths.
# The launch check expects Ready when models are included and an explicit model error
# when they are not. It never replaces or closes another Listen instance.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $PackageRoot) {
    $cargo = [IO.File]::ReadAllText((Join-Path $projectRoot 'app\listen\Cargo.toml'))
    $versionMatch = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) { throw 'Cannot read the Listen version from app\listen\Cargo.toml.' }
    $PackageRoot = Join-Path $projectRoot ('dist\Listen-' + $versionMatch.Groups[1].Value)
}
elseif (-not [IO.Path]::IsPathRooted($PackageRoot)) {
    $PackageRoot = Join-Path $PWD $PackageRoot
}
$PackageRoot = [IO.Path]::GetFullPath($PackageRoot)
if (-not (Test-Path -LiteralPath $PackageRoot -PathType Container)) {
    throw "Package directory is missing: $PackageRoot. Run scripts\package-listen.ps1 first."
}
$manifestPath = Join-Path $PackageRoot 'manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "Package manifest is missing: $manifestPath"
}
$manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
if ($manifest.product -ne 'Listen') { throw 'The manifest is not a Listen package manifest.' }

$required = @(
    'listen.exe',
    'libunwind.dll',
    'README.md',
    'glossary.txt',
    'VERSION.txt',
    'engine\whisper-cli.exe',
    'engine\whisper.dll',
    'engine\ggml.dll',
    'engine\ggml-cpu.dll',
    'engine\ggml-base.dll',
    'engine\vad-speech-segments.exe'
)
if ($manifest.included_models) {
    $required += @('models\ggml-small.bin', 'models\ggml-silero-v5.1.2.bin')
}
else {
    $required += 'models\README.txt'
}
$missing = @($required | Where-Object { -not (Test-Path -LiteralPath (Join-Path $PackageRoot $_) -PathType Leaf) })
if ($missing.Count -gt 0) {
    throw ('Package files are missing: ' + ($missing -join ', '))
}
$launchers = @(Get-ChildItem -LiteralPath $PackageRoot -Filter '*.cmd' -File)
if ($launchers.Count -ne 1) {
    throw ('The package must contain exactly one double-click launcher (.cmd), found ' + $launchers.Count + '.')
}
$launcherPath = $launchers[0].FullName

$hashesVerified = 0
if (-not $SkipHash) {
    foreach ($entry in @($manifest.files)) {
        $path = Join-Path $PackageRoot $entry.path
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw ('Manifest entry is missing: ' + $entry.path) }
        $item = Get-Item -LiteralPath $path
        if ($item.Length -ne [long]$entry.bytes) {
            throw ('Package file size does not match the manifest: ' + $entry.path)
        }
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($actual -ne [string]$entry.sha256) {
            throw ('Package file hash does not match the manifest: ' + $entry.path)
        }
        $hashesVerified++
    }
}

$launcher = [IO.File]::ReadAllText($launcherPath)
if ($launcher -notmatch 'listen\.exe' -or $launcher -notmatch '--engine') {
    throw 'The launcher does not start listen.exe with the packaged engine path.'
}
$readme = [IO.File]::ReadAllText((Join-Path $PackageRoot 'README.md'))
foreach ($phrase in @('Ctrl+Alt+R', 'libunwind.dll', 'runtime', 'whisper.cpp')) {
    if ($readme.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The packaged README does not explain: ' + $phrase)
    }
}

$existing = @(Get-Process -Name listen -ErrorAction SilentlyContinue)
if ($existing.Count -gt 0) {
    throw 'A Listen instance is already running; this check will not replace or close it.'
}
if (-not $Runtime) {
    $Runtime = Join-Path $env:TEMP ('listen-package-verify-' + [guid]::NewGuid().ToString('N'))
}
elseif (-not [IO.Path]::IsPathRooted($Runtime)) {
    $Runtime = Join-Path $PWD $Runtime
}
$Runtime = [IO.Path]::GetFullPath($Runtime)
New-Item -ItemType Directory -Force -Path $Runtime | Out-Null
$statusPath = Join-Path $Runtime 'status.txt'
$exe = Join-Path $PackageRoot 'listen.exe'

function ConvertTo-QuotedWindowsPath([string]$Path) {
    return '"' + ([IO.Path]::GetFullPath($Path) -replace '(\\+)$', '$1$1') + '"'
}
function Read-Status([string]$Path) {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try { return [IO.File]::ReadAllText($Path) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw 'The status file stayed locked by Listen.'
}
function New-ControlProcess([string]$Command) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $exe
    $info.Arguments = '--control ' + $Command
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    return [Diagnostics.Process]::Start($info)
}

$engine = Join-Path $PackageRoot 'engine\whisper-cli.exe'
$model = Join-Path $PackageRoot 'models\ggml-small.bin'
$vad = Join-Path $PackageRoot 'models\ggml-silero-v5.1.2.bin'
$arguments = @(
    '--engine', (ConvertTo-QuotedWindowsPath $engine),
    '--model', (ConvertTo-QuotedWindowsPath $model),
    '--vad-model', (ConvertTo-QuotedWindowsPath $vad),
    '--runtime', (ConvertTo-QuotedWindowsPath $Runtime),
    '--hidden'
) -join ' '

$app = $null
$listenPid = 0
$phases = [Collections.Generic.List[string]]::new()
$finalStatus = ''
$started = [DateTime]::UtcNow
$quitOk = $false
try {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $exe
    $info.Arguments = $arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $app = [Diagnostics.Process]::Start($info)
    $listenPid = $app.Id
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($app.HasExited) { throw ('Packaged Listen exited before reporting a state (exit ' + $app.ExitCode + ').') }
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $text = Read-Status $statusPath
            if ($text -match '(?m)^phase=(\w+)') {
                $phase = $Matches[1]
                if (-not $phases.Contains($phase)) { $phases.Add($phase) }
                if ($phase -eq 'Ready' -or $phase -eq 'Failure') {
                    $finalStatus = $text
                    break
                }
            }
        }
        Start-Sleep -Milliseconds 50
    }
    if (-not $finalStatus) { throw ('Packaged Listen did not report Ready or Failure within ' + $TimeoutSeconds + ' seconds.') }
    if ($manifest.included_models) {
        if ($finalStatus -notmatch '(?m)^phase=Ready') {
            throw ('The package with models did not become Ready:' + [Environment]::NewLine + $finalStatus)
        }
        if ($finalStatus -match '(?m)^backend=$') { throw 'Ready did not report a recognition backend.' }
    }
    else {
        if ($finalStatus -notmatch '(?m)^phase=Failure') {
            throw ('The package without models did not report the expected failure:' + [Environment]::NewLine + $finalStatus)
        }
        if ($finalStatus -notmatch 'Local model is unavailable|Local whisper\.cpp executable is missing') {
            throw ('The missing-model failure is not explicit:' + [Environment]::NewLine + $finalStatus)
        }
    }

    $control = $null
    try {
        $control = New-ControlProcess 'quit'
        if (-not $control.WaitForExit(10000)) {
            throw 'The packaged Listen control process did not exit within 10 seconds.'
        }
        if ($control.ExitCode -ne 0) { throw ('The packaged Listen did not accept --control quit (exit ' + $control.ExitCode + ').') }
    }
    finally {
        if ($null -ne $control) {
            if (-not $control.HasExited) {
                try { $control.Kill() } catch { }
                try { $control.WaitForExit(5000) } catch { }
            }
            $control.Dispose()
        }
    }
    if (-not $app.WaitForExit(10000)) { throw 'Packaged Listen did not exit after --control quit.' }
    $quitOk = ($app.ExitCode -eq 0)
    if (-not $quitOk) { throw ('Packaged Listen exited with ' + $app.ExitCode + '.') }
}
finally {
    if ($null -ne $app -and -not $app.HasExited) {
        try { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue } catch { }
        try { $app.WaitForExit(5000) } catch { }
    }
    if ($null -ne $app) { $app.Dispose() }
}

$summary = [ordered]@{
    checked_utc = [DateTime]::UtcNow.ToString('o')
    package_root = $PackageRoot
    version = [string]$manifest.version
    git_commit = [string]$manifest.git_commit
    included_models = [bool]$manifest.included_models
    files_checked = $required.Count
    hashes_verified = $hashesVerified
    observed_phases = @($phases.ToArray())
    final_phase = if ($finalStatus -match '(?m)^phase=(\w+)') { $Matches[1] } else { '' }
    final_detail = if ($finalStatus -match '(?m)^detail=(.*)$') { $Matches[1] } else { '' }
    backend = if ($finalStatus -match '(?m)^backend=(.*)$') { $Matches[1] } else { '' }
    process_id = $listenPid
    runtime = $Runtime
    start_to_state_ms = [int]([DateTime]::UtcNow - $started).TotalMilliseconds
    control_quit_ok = $quitOk
}
if ($Evidence) {
    if (-not [IO.Path]::IsPathRooted($Evidence)) { $Evidence = Join-Path $PWD $Evidence }
    $evidencePath = [IO.Path]::GetFullPath($Evidence)
    New-Item -ItemType Directory -Force -Path (Split-Path $evidencePath -Parent) | Out-Null
    [IO.File]::WriteAllText($evidencePath, ($summary | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
}
$summary | ConvertTo-Json -Depth 5
