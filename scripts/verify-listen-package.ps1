param(
    [string]$PackageRoot = '',
    [string]$Runtime = '',
    [string]$Evidence = '',
    [int]$TimeoutSeconds = 90,
    [switch]$SkipHash
)
# Check a packaged Listen directory, then start it through the daily cloud default
# and through the explicitly labelled local fallback.
#
# The default launch must select the cloud path without any --engine/--cloud-trial
# argument: a missing local configuration is refused with the fixed vendor/model/
# region visible, and a synthetic local credential makes the packaged build Ready
# without starting a capture. The local fallback launch must report a local backend
# when models are included and an explicit model error when they are not. No real
# key is read, no paid request is sent, and the ledger must stay empty.
#
# The guard checks fail the package if it contains key material, test audio/private
# runtime data or host-specific test-environment paths. It never replaces or closes
# another Listen instance.
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

# The delivered daily default must be the accepted cloud path; local recognition is
# an explicit fallback and may not become the default by accident.
if ([string]$manifest.default_mode -ne 'cloud') {
    throw ('The packaged default mode is not cloud: ' + [string]$manifest.default_mode)
}
if (-not [bool]$manifest.cloud_trial.selected_by_default) {
    throw 'The packaged cloud trial is not selected by default; #40 requires the accepted cloud path as the daily default.'
}
if (-not [bool]$manifest.local_fallback.explicit_only) {
    throw 'The packaged local fallback is missing or is not marked explicit-only.'
}
if ([string]$manifest.cloud_trial.provider -ne 'Alibaba Model Studio' -or
    [string]$manifest.cloud_trial.model -ne 'qwen-audio-3.1-asr-flash-message' -or
    [string]$manifest.cloud_trial.region -ne 'cn-beijing') {
    throw 'The packaged cloud-trial vendor/model/region does not match the fixed selection.'
}
if ([string]$manifest.cloud_trial.configure -ne 'scripts/configure-cloud-trial.ps1' -or
    [string]$manifest.cloud_trial.guide -ne 'CLOUD-TRIAL.md') {
    throw 'The packaged cloud-trial configuration path is missing from the manifest.'
}

$required = @(
    'listen.exe',
    'libunwind.dll',
    'README.md',
    'CLOUD-TRIAL.md',
    'glossary.txt',
    'VERSION.txt',
    'scripts\configure-cloud-trial.ps1',
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

# Exactly two launchers: the daily cloud default and the explicitly labelled local
# fallback.
$launchers = @(Get-ChildItem -LiteralPath $PackageRoot -Filter '*.cmd' -File)
if ($launchers.Count -ne 2) {
    throw ('The package must contain exactly two launchers (.cmd: daily cloud default and explicit local fallback), found ' + $launchers.Count + '.')
}
$launcherInfo = @($launchers | ForEach-Object {
        [pscustomobject]@{ File = $_; Text = [IO.File]::ReadAllText($_.FullName) }
    })
$localLaunchers = @($launcherInfo | Where-Object { $_.Text -match '--engine' -and $_.Text -notmatch '--cloud-trial' })
$defaultLaunchers = @($launcherInfo | Where-Object { $_.Text -notmatch '--engine' })
if ($localLaunchers.Count -ne 1 -or $defaultLaunchers.Count -ne 1) {
    throw 'The package must contain exactly one default launcher (no --engine) and exactly one explicit local fallback launcher (--engine, no --cloud-trial).'
}
$defaultLauncher = $defaultLaunchers[0]
$localLauncher = $localLaunchers[0]
foreach ($phrase in @('listen.exe', '--runtime', 'runtime-cloud', '--glossary', 'scripts\configure-cloud-trial.ps1', 'CLOUD-TRIAL.md')) {
    if ($defaultLauncher.Text.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The default launcher does not contain: ' + $phrase)
    }
}
if ($defaultLauncher.Text -match '--engine') {
    throw 'The default launcher must not pass --engine; the daily default is the cloud path, not a local fallback.'
}
if ($defaultLauncher.Text -match '--cloud-trial') {
    throw 'The default launcher must not pass --cloud-trial; the packaged binary default itself must select the accepted cloud path.'
}
foreach ($phrase in @('listen.exe', '--engine', '--model', '--vad-model', '--runtime', '--glossary')) {
    if ($localLauncher.Text.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The local fallback launcher does not pass: ' + $phrase)
    }
}
if ($localLauncher.Text -notmatch '--runtime\s+"%~dp0runtime"') {
    throw 'The local fallback launcher does not use the packaged local runtime directory.'
}
if ($localLauncher.Text -match '--cloud-trial') {
    throw 'The local fallback launcher must not pass --cloud-trial; local recognition is an explicit fallback.'
}
if ([string]$manifest.local_fallback.launcher -ne $localLauncher.File.Name) {
    throw 'The manifest local fallback launcher does not name the packaged local launcher.'
}
foreach ($phrase in @('whisper')) {
    if ([string]$manifest.local_fallback.engine -notmatch $phrase -or [string]$manifest.local_fallback.runtime -ne 'runtime') {
        throw 'The manifest local fallback engine/runtime is missing or inconsistent.'
    }
}

# The machine-local configuration surface is a real helper, not a stub.
$helper = [IO.File]::ReadAllText((Join-Path $PackageRoot 'scripts\configure-cloud-trial.ps1'))
foreach ($phrase in @('Listen/CloudTrial/Alibaba/cn-beijing', 'SecureString', 'LISTEN_ALI_API_KEY', 'qwen-audio-3.1-asr-flash-message', 'cn-beijing')) {
    if ($helper.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The packaged configuration helper does not contain: ' + $phrase)
    }
}

$readme = [IO.File]::ReadAllText((Join-Path $PackageRoot 'README.md'))
foreach ($phrase in @('Ctrl+Alt+R', 'libunwind.dll', 'runtime', 'runtime-cloud', 'whisper.cpp', '--engine', 'qwen-audio-3.1-asr-flash-message', 'cn-beijing', 'configure-cloud-trial.ps1', 'CLOUD-TRIAL.md')) {
    if ($readme.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The packaged README does not explain: ' + $phrase)
    }
}
$cloudGuide = [IO.File]::ReadAllText((Join-Path $PackageRoot 'CLOUD-TRIAL.md'))
foreach ($phrase in @('qwen-audio-3.1-asr-flash-message', 'cn-beijing', 'configure-cloud-trial.ps1', 'runtime-cloud', 'runtime', 'CNY 50', 'CNY 100')) {
    if ($cloudGuide.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The packaged cloud guide does not explain: ' + $phrase)
    }
}

$versionText = [IO.File]::ReadAllText((Join-Path $PackageRoot 'VERSION.txt'))
foreach ($phrase in @('Daily default: cloud', 'Local fallback (explicit only)', 'Alibaba Model Studio', 'qwen-audio-3.1-asr-flash-message', 'cn-beijing')) {
    if ($versionText.IndexOf($phrase, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The packaged VERSION.txt does not record: ' + $phrase)
    }
}
foreach ($hostDocument in @(@{ Path = $manifestPath; Text = [IO.File]::ReadAllText($manifestPath) }, @{ Path = (Join-Path $PackageRoot 'VERSION.txt'); Text = $versionText })) {
    if ($hostDocument.Text -match '(?i)\b[A-Z]:\\') {
        throw ('The delivered ' + (Split-Path $hostDocument.Path -Leaf) + ' contains a host-specific absolute path.')
    }
}

$forbiddenExtensions = @('.wav', '.pcm', '.mp3', '.m4a', '.flac', '.ogg', '.opus', '.log', '.jsonl', '.env', '.pem', '.pfx', '.key', '.tmp')
$forbiddenNames = @('status.txt', 'panel.txt', 'current-result.txt', 'segments.json', 'delivery.json', 'latency.jsonl', 'ali.json', 'ledger.json', 'ledger.lock', '.env', 'credentials.json')
$forbiddenDirectories = @('runtime', 'runtime-cloud', 'target', 'tests', '.artifacts', '.tools', '.git')
$textExtensions = @('.md', '.txt', '.json', '.cmd', '.ps1', '.bat', '.cfg', '.ini', '.yml', '.yaml')
$highConfidenceSecretPatterns = @(
    '(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{20,}',
    '(?i)\bsk-[A-Za-z0-9_-]{20,}',
    '(?i)\bLTAI[A-Za-z0-9]{12,}'
)
$textSecretPatterns = @(
    '(?i)LISTEN_ALI_API_KEY\s*=\s*[^\s"''<]',
    '(?i)\bapi[_-]?key\s*[:=]\s*[A-Za-z0-9._~+/=-]{16,}',
    '(?i)Authorization\s*[:=]\s*[A-Za-z0-9._~+/=-]{12,}'
)
$hostTestPatterns = @(
    '(?i)\.tools\\',
    '(?i)\.artifacts\\',
    '(?i)cargo-target',
    '(?i)listen-worktrees',
    '(?i)\\luyin\\'
)
# Large files are not exempt from scanning by size: only these exact pinned
# binaries may skip the byte scan, and the manifest hash check still verifies their
# content. Any other file over the limit is a package error, so a large log renamed
# as an arbitrary file cannot bypass key detection.
$contentScanAllowlist = @('models\ggml-small.bin')
function Read-Latin1([string]$Path) {
    return [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes($Path))
}
# Prove the guard is not vacuous before trusting a clean scan.
$guardProbe = Join-Path $env:TEMP ('listen-package-guard-' + [guid]::NewGuid().ToString('N') + '.txt')
try {
    $probeText = 'LISTEN_ALI_API_KEY=' + ('sk-' + ('a' * 24))
    [IO.File]::WriteAllText($guardProbe, $probeText, [Text.UTF8Encoding]::new($false))
    $detected = $false
    foreach ($pattern in ($highConfidenceSecretPatterns + $textSecretPatterns)) {
        if ([regex]::IsMatch($probeText, $pattern)) { $detected = $true }
    }
    if (-not $detected) { throw 'The package secret guard self-test did not detect its synthetic key.' }
}
finally {
    Remove-Item -LiteralPath $guardProbe -Force -ErrorAction SilentlyContinue
}

$scannedFiles = 0
$packageFiles = @(Get-ChildItem -LiteralPath $PackageRoot -Recurse -File -Force)
foreach ($file in $packageFiles) {
    $relative = $file.FullName.Substring($PackageRoot.Length + 1)
    $segments = $relative -split '\\'
    if ($segments.Count -gt 1) {
        foreach ($segment in $segments[0..($segments.Count - 2)]) {
            if ($forbiddenDirectories -contains $segment -or $segment -like 'session-*') {
                throw ('The package contains a forbidden directory: ' + $relative)
            }
        }
    }
    if ($forbiddenExtensions -contains $file.Extension.ToLowerInvariant()) {
        throw ('The package contains a forbidden file type: ' + $relative)
    }
    if ($forbiddenNames -contains $file.Name.ToLowerInvariant()) {
        throw ('The package contains a test/runtime file: ' + $relative)
    }
    if ($file.Length -gt 8MB) {
        if ($contentScanAllowlist -notcontains $relative) {
            throw ('The package contains a file over 8 MiB that is not an allowlisted pinned binary: ' + $relative)
        }
        continue
    }
    $scannedFiles++
    $blob = Read-Latin1 $file.FullName
    foreach ($pattern in $highConfidenceSecretPatterns) {
        if ([regex]::IsMatch($blob, $pattern)) {
            throw ('The package contains key-like material in: ' + $relative)
        }
    }
    if ($textExtensions -contains $file.Extension.ToLowerInvariant()) {
        foreach ($pattern in $textSecretPatterns) {
            if ([regex]::IsMatch($blob, $pattern)) {
                throw ('The package contains a key assignment in: ' + $relative)
            }
        }
        foreach ($pattern in $hostTestPatterns) {
            if ([regex]::IsMatch($blob, $pattern)) {
                throw ('The package contains a host-specific test path in: ' + $relative)
            }
        }
    }
}

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
function New-ListenProcess([string]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $exe
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    return [Diagnostics.Process]::Start($info)
}
function Stop-ListenGracefully([Diagnostics.Process]$App, [string]$Label) {
    $control = $null
    try {
        $control = New-ControlProcess 'quit'
        if (-not $control.WaitForExit(10000)) {
            throw ($Label + ' control process did not exit within 10 seconds.')
        }
        if ($control.ExitCode -ne 0) {
            throw ($Label + ' did not accept --control quit (exit ' + $control.ExitCode + ').')
        }
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
    if (-not $App.WaitForExit(10000)) { throw ($Label + ' did not exit after --control quit.') }
    if ($App.ExitCode -ne 0) { throw ($Label + ' exited with ' + $App.ExitCode + '.') }
}
function Invoke-ListenLaunch {
    param(
        [string]$Label,
        [string[]]$Arguments,
        [hashtable]$Environment,
        [string]$RunDirectory
    )
    New-Item -ItemType Directory -Force -Path $RunDirectory | Out-Null
    $statusPath = Join-Path $RunDirectory 'status.txt'
    Remove-Item -LiteralPath $statusPath -Force -ErrorAction SilentlyContinue
    $app = $null
    $phases = [Collections.Generic.List[string]]::new()
    $started = [DateTime]::UtcNow
    # Windows PowerShell 5.1 exposes a null ProcessStartInfo.EnvironmentVariables
    # through property chaining on this host, so scope the child environment with
    # the process-level API and restore it in finally instead.
    $savedEnvironment = @{}
    foreach ($name in $Environment.Keys) {
        $key = [string]$name
        $savedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, [string]$Environment[$key], 'Process')
    }
    try {
        $app = New-ListenProcess ($Arguments -join ' ')
        $listenPid = $app.Id
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $finalStatus = ''
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($app.HasExited) { throw ($Label + ' exited before reporting a state (exit ' + $app.ExitCode + ').') }
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
        if (-not $finalStatus) { throw ($Label + ' did not report Ready or Failure within ' + $TimeoutSeconds + ' seconds.') }
        $startToState = [int]([DateTime]::UtcNow - $started).TotalMilliseconds
        Stop-ListenGracefully $app $Label
        return [pscustomobject]@{
            label           = $Label
            status          = $finalStatus
            phases          = @($phases.ToArray())
            start_to_state  = $startToState
            process_id      = $listenPid
            runtime         = $RunDirectory
        }
    }
    finally {
        foreach ($name in $savedEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable([string]$name, $savedEnvironment[$name], 'Process')
        }
        if ($null -ne $app -and -not $app.HasExited) {
            try { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue } catch { }
            try { $app.WaitForExit(5000) } catch { }
        }
        if ($null -ne $app) { $app.Dispose() }
    }
}
function Get-StatusField([string]$Status, [string]$Name) {
    if ($Status -match ('(?m)^' + [regex]::Escape($Name) + '=(.*)$')) { return $Matches[1] }
    return ''
}

$engine = Join-Path $PackageRoot 'engine\whisper-cli.exe'
$model = Join-Path $PackageRoot 'models\ggml-small.bin'
$vad = Join-Path $PackageRoot 'models\ggml-silero-v5.1.2.bin'
$localArguments = @(
    '--engine', (ConvertTo-QuotedWindowsPath $engine),
    '--model', (ConvertTo-QuotedWindowsPath $model),
    '--vad-model', (ConvertTo-QuotedWindowsPath $vad),
    '--runtime', (ConvertTo-QuotedWindowsPath $Runtime),
    '--hidden'
)
$local = Invoke-ListenLaunch -Label 'explicit local fallback package launch' -Arguments $localArguments -Environment @{} -RunDirectory $Runtime
$localPhase = Get-StatusField $local.status 'phase'
$localBackend = Get-StatusField $local.status 'backend'
$localDetail = Get-StatusField $local.status 'detail'
$localMode = Get-StatusField $local.status 'recognition_mode'
if ($localMode -ne 'local') {
    throw ('The explicit local fallback did not report recognition_mode=local:' + [Environment]::NewLine + $local.status)
}
if ($manifest.included_models) {
    if ($localPhase -ne 'Ready') {
        throw ('The package with models did not become Ready:' + [Environment]::NewLine + $local.status)
    }
    if (-not $localBackend) { throw 'The local fallback did not report a recognition backend.' }
    if ($localBackend -match '(?i)Cloud trial') { throw 'The local fallback launch reported a cloud backend.' }
}
else {
    if ($localPhase -ne 'Failure') {
        throw ('The package without models did not report the expected failure:' + [Environment]::NewLine + $local.status)
    }
    if ($localDetail -notmatch 'Local model is unavailable|Local whisper\.cpp executable is missing') {
        throw ('The missing-model failure is not explicit:' + [Environment]::NewLine + $local.status)
    }
}

# Daily cloud default: the packaged binary is started with no --engine and no
# --cloud-trial, exactly like the default launcher. No local configuration must be
# refused with the fixed source displayed, and a synthetic valid local configuration
# must become Ready. Neither launch starts a capture, so no free-only check,
# connection or paid request happens.
$fakeKey = 'package-verification-fake-key-0000'
$cloudRoot = Join-Path $Runtime 'cloud'
$invalidHome = Join-Path $cloudRoot 'invalid-home'
$invalidRuntime = Join-Path $cloudRoot 'invalid-runtime'
$readyHome = Join-Path $cloudRoot 'ready-home'
$readyRuntime = Join-Path $cloudRoot 'ready-runtime'
New-Item -ItemType Directory -Force -Path $invalidHome, $readyHome | Out-Null
$baseCloudEnvironment = @{
    LISTEN_ALI_MODEL  = 'qwen-audio-3.1-asr-flash-message'
    LISTEN_ALI_REGION = 'cn-beijing'
}
$invalidEnvironment = $baseCloudEnvironment.Clone()
$invalidEnvironment['LOCALAPPDATA'] = $invalidHome
$invalidEnvironment['LISTEN_ALI_API_KEY'] = ''
$invalidEnvironment['LISTEN_ALI_WORKSPACE_ID'] = ''
$readyEnvironment = $baseCloudEnvironment.Clone()
$readyEnvironment['LOCALAPPDATA'] = $readyHome
$readyEnvironment['LISTEN_ALI_API_KEY'] = $fakeKey
$readyEnvironment['LISTEN_ALI_WORKSPACE_ID'] = 'package-verification-workspace'
$glossaryPath = Join-Path $PackageRoot 'glossary.txt'
# Deliberately no --engine and no --cloud-trial: this is the packaged default.
$defaultArguments = @(
    '--runtime', (ConvertTo-QuotedWindowsPath $invalidRuntime),
    '--glossary', (ConvertTo-QuotedWindowsPath $glossaryPath),
    '--hidden'
)
$cloudInvalid = Invoke-ListenLaunch -Label 'daily cloud default launch without local configuration' -Arguments $defaultArguments -Environment $invalidEnvironment -RunDirectory $invalidRuntime
$invalidPhase = Get-StatusField $cloudInvalid.status 'phase'
$invalidBackend = Get-StatusField $cloudInvalid.status 'backend'
$invalidDetail = Get-StatusField $cloudInvalid.status 'detail'
$invalidMode = Get-StatusField $cloudInvalid.status 'recognition_mode'
if ($invalidMode -ne 'cloud-trial') {
    throw ('The unconfigured default launch did not select the cloud path:' + [Environment]::NewLine + $cloudInvalid.status)
}
if ($invalidPhase -ne 'Failure') {
    throw ('The unconfigured cloud default did not report the expected failure:' + [Environment]::NewLine + $cloudInvalid.status)
}
foreach ($expected in @('Alibaba Model Studio', 'qwen-audio-3.1-asr-flash-message', 'cn-beijing')) {
    if ($invalidBackend.IndexOf($expected, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('The refused cloud default did not show the actual source: ' + $expected)
    }
}
if ($invalidDetail -notmatch 'valid Alibaba workspace') {
    throw ('The unconfigured cloud default did not explain the local configuration:' + [Environment]::NewLine + $cloudInvalid.status)
}
if ((Get-StatusField $cloudInvalid.status 'recognition') -match '(?i)capturing') {
    throw 'The refused cloud default started a capture.'
}

$readyArguments = @(
    '--runtime', (ConvertTo-QuotedWindowsPath $readyRuntime),
    '--glossary', (ConvertTo-QuotedWindowsPath $glossaryPath),
    '--hidden'
)
$cloudReady = Invoke-ListenLaunch -Label 'daily cloud default launch with synthetic local configuration' -Arguments $readyArguments -Environment $readyEnvironment -RunDirectory $readyRuntime
$readyPhase = Get-StatusField $cloudReady.status 'phase'
$readyBackend = Get-StatusField $cloudReady.status 'backend'
$readyDetail = Get-StatusField $cloudReady.status 'detail'
$readyMode = Get-StatusField $cloudReady.status 'recognition_mode'
if ($readyMode -ne 'cloud-trial') {
    throw ('The configured default launch did not select the cloud path:' + [Environment]::NewLine + $cloudReady.status)
}
if ($readyPhase -ne 'Ready') {
    throw ('The packaged cloud default did not become Ready with a valid local configuration:' + [Environment]::NewLine + $cloudReady.status)
}
foreach ($expected in @('Alibaba Model Studio', 'qwen-audio-3.1-asr-flash-message', 'cn-beijing')) {
    if ($readyBackend.IndexOf($expected, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
        throw ('Ready did not show the actual cloud source: ' + $expected)
    }
}
if ($readyBackend -match '(?i)two-provider') {
    throw 'The cloud backend still claims a second provider.'
}
foreach ($leakedPath in @($cloudInvalid.runtime, $invalidHome, $cloudReady.runtime, $readyHome)) {
    foreach ($file in @(Get-ChildItem -LiteralPath $leakedPath -Recurse -File -ErrorAction SilentlyContinue)) {
        $blob = Read-Latin1 $file.FullName
        if ($blob.Contains($fakeKey)) { throw ('The synthetic cloud key reached a runtime file: ' + $file.FullName) }
    }
}
$ledgerPath = Join-Path $readyHome 'Listen\cloud-trial\ledger.json'
$cloudPaidRequests = 0
if (Test-Path -LiteralPath $ledgerPath -PathType Leaf) {
    $ledger = [IO.File]::ReadAllText($ledgerPath) | ConvertFrom-Json
    $cloudPaidRequests = @($ledger.attempts).Count
}
if ($cloudPaidRequests -ne 0) {
    throw ('The package cloud check reserved a paid request (' + $cloudPaidRequests + ' attempts).')
}

$summary = [ordered]@{
    checked_utc              = [DateTime]::UtcNow.ToString('o')
    package_root             = $PackageRoot
    version                  = [string]$manifest.version
    git_commit               = [string]$manifest.git_commit
    included_models          = [bool]$manifest.included_models
    default_mode             = [string]$manifest.default_mode
    cloud_trial_by_default   = [bool]$manifest.cloud_trial.selected_by_default
    cloud_trial_model        = [string]$manifest.cloud_trial.model
    cloud_trial_region       = [string]$manifest.cloud_trial.region
    local_fallback_launcher  = [string]$manifest.local_fallback.launcher
    files_checked            = $required.Count
    hashes_verified          = $hashesVerified
    launchers_checked        = $launcherInfo.Count
    scanned_files            = $scannedFiles
    secret_guard_self_test   = $true
    local_fallback_phase     = $localPhase
    local_fallback_detail    = $localDetail
    local_fallback_backend   = $localBackend
    local_fallback_mode      = $localMode
    local_fallback_phases    = @($local.phases)
    cloud_invalid_phase      = $invalidPhase
    cloud_invalid_detail     = $invalidDetail
    cloud_invalid_backend    = $invalidBackend
    cloud_invalid_mode       = $invalidMode
    cloud_ready_phase        = $readyPhase
    cloud_ready_detail       = $readyDetail
    cloud_ready_backend      = $readyBackend
    cloud_ready_mode         = $readyMode
    cloud_paid_requests      = $cloudPaidRequests
    cloud_ready_phases       = @($cloudReady.phases)
    process_id               = $local.process_id
    runtime                  = $local.runtime
    start_to_state_ms        = $local.start_to_state
    control_quit_ok          = $true
}
if ($Evidence) {
    if (-not [IO.Path]::IsPathRooted($Evidence)) { $Evidence = Join-Path $PWD $Evidence }
    $evidencePath = [IO.Path]::GetFullPath($Evidence)
    New-Item -ItemType Directory -Force -Path (Split-Path $evidencePath -Parent) | Out-Null
    [IO.File]::WriteAllText($evidencePath, ($summary | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
}
$summary | ConvertTo-Json -Depth 5
