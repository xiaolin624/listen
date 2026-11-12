param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('mandarin', 'mixed-technical', 'english-numbers', 'homophones', 'multiline', 'silence', 'instrumental', 'notification')]
    [string]$Sample,
    [Parameter(Mandatory = $true)][string]$Runtime,
    [string]$Corpus = '',
    [string]$Output = '',
    [int]$TimeoutSeconds = 180
)
# This is an explicit hardware acceptance driver: it plays audio on the default
# render device and controls an already running Listen. It never selects a target
# or changes devices. See docs/issue-9-verification.md before running it.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
if (-not $Corpus) { $Corpus = Join-Path $repositoryRoot '.artifacts\quality-corpus' }
if (-not $Output) { $Output = Join-Path $repositoryRoot '.artifacts\quality-capture' }
$Runtime = [IO.Path]::GetFullPath($Runtime)
$statusPath = Join-Path $Runtime 'status.txt'
$resultPath = Join-Path $Runtime 'current-result.txt'
if (-not (Test-Path -LiteralPath $statusPath -PathType Leaf)) { throw "Running Listen must use this runtime: $Runtime" }
$initialStatus = [IO.File]::ReadAllText($statusPath)
if ($initialStatus -notmatch '(?m)^process_id=(\d+)\r?$') { throw 'Rebuild and start Listen so this runtime identifies its live process.' }
$listenProcess = Get-Process -Id ([int]$Matches[1]) -ErrorAction Stop
$expectedBinary = [IO.Path]::GetFullPath((Join-Path $projectRoot 'app\listen\target\release\listen.exe'))
if ($listenProcess.ProcessName -ne 'listen' -or $listenProcess.Path -ine $expectedBinary) { throw 'The runtime belongs to a different process or build.' }
if ($initialStatus -match '^phase=(Capturing|Processing)') { throw 'Finish the current Listen session before this acceptance run.' }
if ($initialStatus -match '^phase=Preparing') {
    # First-use model preparation must finish before the hotkey path accepts a
    # capture; never treat a preparing model as ready.
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    do {
        Start-Sleep -Milliseconds 100
        $initialStatus = [IO.File]::ReadAllText($statusPath)
        if ($initialStatus -match '^phase=Failure') { throw $initialStatus }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Listen did not finish preparing its local model within 120 seconds.' }
    } until ($initialStatus -notmatch '^phase=Preparing')
}
$manifest = Get-Content -LiteralPath (Join-Path $Corpus 'manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$fixture = $manifest.samples | Where-Object id -eq $Sample | Select-Object -First 1
if (-not $fixture) { throw "Sample missing from manifest: $Sample" }
if ((Get-FileHash -LiteralPath $fixture.audio -Algorithm SHA256).Hash.ToLowerInvariant() -ne $fixture.sha256) { throw 'Fixture checksum changed.' }
$before = if (Test-Path -LiteralPath $resultPath) { [IO.File]::ReadAllText($resultPath) } else { '' }
$player = New-Object System.Media.SoundPlayer($fixture.audio)
$player.Load()
$started = $false
$watch = [Diagnostics.Stopwatch]::StartNew()
try {
    & (Join-Path $PSScriptRoot 'run-listen.ps1') -Control start
    $started = $true
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 50
        $status = [IO.File]::ReadAllText($statusPath)
        if ($status -match '^phase=Failure') { throw $status }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Listen did not enter Capturing in this runtime.' }
    } until ($status -match '^phase=Capturing')
    $player.PlaySync()
    Start-Sleep -Milliseconds 250
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    & (Join-Path $PSScriptRoot 'run-listen.ps1') -Control stop
    $started = $false
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 100
        $status = [IO.File]::ReadAllText($statusPath)
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Timed out waiting for local recognition; inspect Listen without deleting its audio.' }
    } until ($status -match '^phase=(Result|NoSpeech|Failure)')
    $stopwatch.Stop()
    $after = if (Test-Path -LiteralPath $resultPath) { [IO.File]::ReadAllText($resultPath) } else { '' }
    $null = New-Item -ItemType Directory -Force -Path $Output
    $timing = [ordered]@{}
    foreach ($name in @('preparation', 'stop_to_result', 'finalize', 'recognition', 'last_window_wait', 'last_window_inference', 'save', 'delivery')) {
        if ($status -match ('(?m)^timing_' + $name + '_ms=(\d+)')) { $timing.Add($name + '_ms', [long]$Matches[1]) }
    }
    $backend = ''
    if ($status -match '(?m)^backend=(.*)$') { $backend = $Matches[1] }
    $evidence = [ordered]@{
        sample = $Sample; reference = $fixture.reference; fixture_sha256 = $fixture.sha256
        phase_and_detail = $status; transcript = $after
        previous_result_unchanged = ($before -ceq $after)
        seconds_after_stop = [Math]::Round($stopwatch.Elapsed.TotalSeconds, 3)
        elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
        backend = $backend
        timing = $timing
        real_wasapi_capture = $true; playback_source = 'System.Media.SoundPlayer'
        desktop_hotkey_tested = $false; target_or_clipboard_readback_tested = $false
    }
    $evidence | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $Output ($Sample + '.json')) -Encoding UTF8
    if ($status -match '^phase=Failure') { throw $status }
    if ($fixture.kind -ne 'speech' -and ($status -notmatch '^phase=NoSpeech' -or $before -cne $after)) {
        throw 'Non-speech acceptance failed: expected NoSpeech and the previous result unchanged.'
    }
    Write-Output "Captured $Sample; manually compare the reference and transcript: $(Join-Path $Output ($Sample + '.json'))"
} finally {
    if ($started) { & (Join-Path $PSScriptRoot 'run-listen.ps1') -Control stop }
    $player.Dispose()
}
