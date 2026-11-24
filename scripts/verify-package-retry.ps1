# Desktop acceptance for issue #24: recognition failure, local retry and explicit
# cleanup, on a real packaged listen.exe running the local SenseVoice engine.
#
# What it checks:
#   1. a real recognition failure (the captured WAV cannot be read back) leaves the
#      capture's audio on disk, reports Failure with preparation=Ready, offers a retry,
#      writes nothing to the target and produces no result;
#   2. retrying while the engine is still broken keeps the SAME retained audio, keeps
#      the retry offer and still writes nothing;
#   3. repairing the fault and retrying the same audio reaches Result without replaying
#      the speech, appends the transcript to the target exactly once, clears the retry
#      offer and removes the temporary audio;
#   4. a later failure can be cleaned explicitly: --control cleanup deletes the retained
#      audio, disables retry, leaves the current result and the target unchanged, and a
#      following retry cannot resurrect the failed capture;
#   5. a broken model is a PREPARATION failure, not a capture failure: nothing is
#      captured, so retry and cleanup explain that there is nothing to recover. This
#      case is recorded so the difference is explicit in the evidence, not assumed.
#
# ORACLE: the acceptance oracle is the control channel plus the runtime files:
#   --control start|stop|retry|cleanup|quit
#   runtime\status.txt          phase, preparation, detail, retained_audio,
#                               retry_available, current_result
#   runtime\current-result.txt  the complete result
#   runtime\delivery.json       the durable delivery state (appended / fallback / ...)
# Window classes and control ids are never used. The only native window calls here
# create and read the operator-owned TARGET fixture; Listen is driven by --control.
#
# The recognition failure is induced without touching the product: the capture's own
# spooled playback.wav gets a deny-read ACE for the current user while the capture is
# running, so the engine cannot read the audio it just recorded. The write side of the
# spool is untouched, the ACE is removed again before the successful retry, and every
# ACE operation is recorded in the evidence. If the script exits early it removes the
# ACE in its finally block.
#
# ASCII only on purpose: this file must survive any code page.
#
# Example:
#   pwsh -File scripts\verify-package-retry.ps1 -PackageRoot <extracted package> `
#     -Output .artifacts\issue-24\retry

param(
    [Parameter(Mandatory = $true)][string]$PackageRoot,
    [Parameter(Mandatory = $true)][string]$Output,
    [string]$FixtureRoot = '',
    [string]$AudioClip = '',
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms

$repoRoot = Split-Path $PSScriptRoot -Parent
$sharedRoot = $repoRoot
try {
    $commonGit = & git -C $repoRoot rev-parse --path-format=absolute --git-common-dir 2>$null
    if ($LASTEXITCODE -eq 0 -and $commonGit) { $sharedRoot = Split-Path $commonGit -Parent }
} catch { }
$artifacts = Join-Path $sharedRoot '.artifacts'

$PackageRoot = [IO.Path]::GetFullPath($PackageRoot)
$Output = [IO.Path]::GetFullPath($Output)
if (-not $FixtureRoot) { $FixtureRoot = Join-Path $artifacts 'issue-11\accept' }
$FixtureRoot = [IO.Path]::GetFullPath($FixtureRoot)
if (-not $AudioClip) {
    foreach ($candidate in @((Join-Path $artifacts 'issue-2-integration\source-a.wav'), (Join-Path $artifacts 'asr-quality-probe\zh-term.wav'))) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $AudioClip = $candidate; break }
    }
}
if (-not $AudioClip) { throw 'No speech fixture WAV was found; pass -AudioClip.' }
$AudioClip = [IO.Path]::GetFullPath($AudioClip)
$binary = Join-Path $PackageRoot 'listen.exe'
$sherpaLib = Join-Path $PackageRoot 'engine\sherpa'
$model = Join-Path $PackageRoot 'models\sense-voice\model.int8.onnx'
$tokens = Join-Path $PackageRoot 'models\sense-voice\tokens.txt'
$vad = Join-Path $PackageRoot 'models\sense-voice\silero_vad.onnx'
$nativeQa = Join-Path $FixtureRoot 'ListenNativeQa.cs'
$packageFixture = Join-Path $FixtureRoot 'PackageFixture.cs'
foreach ($required in @($binary, $nativeQa, $packageFixture)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Required file is missing: $required" }
}
foreach ($required in @($model, $tokens, $vad)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "The packaged SenseVoice files are missing: $required" }
}
if (Get-Process -Name listen -ErrorAction SilentlyContinue) {
    throw 'A Listen instance is already running; this acceptance will not replace or close it.'
}

New-Item -ItemType Directory -Force -Path $Output | Out-Null
$runtime = Join-Path $Output 'runtime'
$brokenRuntime = Join-Path $Output 'runtime-broken-model'
$statusPath = Join-Path $runtime 'status.txt'
$resultPath = Join-Path $runtime 'current-result.txt'
$deliveryPath = Join-Path $runtime 'delivery.json'

Add-Type -Path $nativeQa -ReferencedAssemblies System.Drawing
Add-Type -Path $packageFixture -ReferencedAssemblies System.Drawing, System.Windows.Forms
$null = [ListenNativeQa]::SetProcessDPIAware()

$currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$app = $null
$fixtureStarted = $false
$phaseLog = [Collections.Generic.List[string]]::new()
$aclLog = [Collections.Generic.List[object]]::new()
$cases = [Collections.Generic.List[object]]::new()
$deniedPaths = [Collections.Generic.List[string]]::new()

function Read-StatusText([string]$Path) {
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        try { return [IO.File]::ReadAllText($Path) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw "The status file stayed locked by Listen: $Path"
}
function Get-Field([string]$Name, [string]$Path = '') {
    if (-not $Path) { $Path = $statusPath }
    $text = Read-StatusText $Path
    $match = [regex]::Match($text, ('(?m)^' + [regex]::Escape($Name) + '=(.*)$'))
    if ($match.Success) { return $match.Groups[1].Value.TrimEnd("`r") }
    return ''
}
function Get-PhaseText([string]$Text) {
    if ($Text -match '(?m)^phase=(\w+)') { return $Matches[1] }
    return 'unknown'
}
function Wait-PhaseOn([string]$StatusFile, [string]$Wanted, [int]$Seconds = $TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (Test-Path -LiteralPath $StatusFile -PathType Leaf) {
            $text = Read-StatusText $StatusFile
            $phase = Get-PhaseText $text
            if ($phaseLog.Count -eq 0 -or $phaseLog[$phaseLog.Count - 1] -ne $phase) { $phaseLog.Add($phase) }
            if ($phase -eq $Wanted) { return $text }
        }
        if ($app.HasExited) { throw 'Listen exited during acceptance.' }
        Start-Sleep -Milliseconds 50
    } until ([DateTime]::UtcNow -gt $deadline)
    throw ("Wait expired for phase $Wanted in ${StatusFile}:`n" + (Read-StatusText $StatusFile))
}
function Wait-Phase([string]$Wanted, [int]$Seconds = $TimeoutSeconds) { return Wait-PhaseOn $statusPath $Wanted $Seconds }
function Invoke-Control([string]$Command) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.Arguments = '--control ' + $Command
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) { throw "Listen control '$Command' did not exit within 15 seconds." }
        if ($process.ExitCode -ne 0) { throw "Listen control '$Command' failed ($($process.ExitCode))." }
    }
    finally { $process.Dispose() }
}
function Read-Text([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '' }
    return [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false))
}
function Normalize([string]$Value) { return ($Value -replace "`r`n", "`n" -replace "`r", "`n") }
function Assert-Equal([string]$Actual, [string]$Expected, [string]$Label) {
    if ((Normalize $Actual) -cne (Normalize $Expected)) {
        throw ($Label + ' differs.' + [Environment]::NewLine + 'ACTUAL=' + $Actual + [Environment]::NewLine + 'EXPECTED=' + $Expected)
    }
}
function Read-Target { return [ListenNativeQa]::ReadText([PackageFixture]::Target, $PID) }
function Read-Neighbor { return [ListenNativeQa]::ReadText([PackageFixture]::Neighbor, $PID) }
function Read-Other { return [ListenNativeQa]::ReadText([PackageFixture]::Other, $PID) }
function Play-Speech {
    $player = [System.Media.SoundPlayer]::new($AudioClip)
    try { $player.PlaySync() } finally { $player.Dispose() }
}
function Get-Wavs {
    return @(Get-ChildItem -LiteralPath $runtime -Recurse -Filter '*.wav' -ErrorAction SilentlyContinue)
}
function Get-DeliveryState([string]$Path) {
    $text = Read-Text $Path
    if (-not $text) { return '' }
    $match = [regex]::Match($text, '"state":"(\w+)"')
    if ($match.Success) { return $match.Groups[1].Value }
    return ''
}
function Set-DenyRead([string]$Path) {
    $output = & icacls.exe $Path /deny "$($currentUser):(RD)" 2>&1
    $aclLog.Add([ordered]@{ action = 'deny-read'; path = $Path; exit_code = $LASTEXITCODE; output = ($output -join ' ') })
    if ($LASTEXITCODE -ne 0) { throw ("Cannot deny read on the captured audio: " + ($output -join ' ')) }
    $deniedPaths.Add($Path)
}
function Clear-DenyRead([string]$Path) {
    $output = & icacls.exe $Path /remove:d $currentUser 2>&1
    $aclLog.Add([ordered]@{ action = 'allow-read'; path = $Path; exit_code = $LASTEXITCODE; output = ($output -join ' ') })
    if ($LASTEXITCODE -ne 0) { throw ("Cannot restore read access on the captured audio: " + ($output -join ' ')) }
    $deniedPaths.Remove($Path) | Out-Null
    $probe = [IO.File]::Open($Path, 'Open', 'Read', [IO.FileShare]::ReadWrite)
    $probe.Close()
}
function Wait-CapturedAudio([int]$Seconds = 20) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $wavs = @(Get-Wavs)
        if ($wavs.Count -gt 0) { return $wavs }
        Start-Sleep -Milliseconds 50
    }
    throw 'Listen did not spool the captured audio to a WAV file while capturing.'
}
function Assert-NoSendOrEnter {
    if ([PackageFixture]::Sends -ne 0) { throw 'An automatic send action occurred.' }
    if ([PackageFixture]::ReturnKeys -ne 0) { throw 'An automatic Enter key action occurred.' }
}
function Start-CaptureWithFailure {
    # Focus the operator-owned target, start through the control channel, play real
    # speech so the capture actually has samples, then deny read on the spooled WAV and
    # stop. The engine cannot read the capture it just made: a real recognition failure.
    # The speech is played before the deny because the spool file only exists once the
    # capture has received samples; nothing reads it before stop.
    [ListenNativeQa]::ClickControl([PackageFixture]::Primary, [PackageFixture]::Target, $PID)
    Invoke-Control 'start'
    $null = Wait-Phase 'Capturing' 30
    Play-Speech
    $wavs = Wait-CapturedAudio 15
    foreach ($wav in $wavs) { Set-DenyRead $wav.FullName }
    Invoke-Control 'stop'
    return , $wavs
}

try {
    if (Test-Path -LiteralPath $runtime) { Remove-Item -LiteralPath $runtime -Recurse -Force }
    if (Test-Path -LiteralPath $brokenRuntime) { Remove-Item -LiteralPath $brokenRuntime -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $runtime, $brokenRuntime | Out-Null

    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.Arguments = '--engine sensevoice --sherpa-lib "' + $sherpaLib + '" --sensevoice-model "' + $model + '" --sensevoice-tokens "' + $tokens + '" --sensevoice-vad "' + $vad + '" --runtime "' + $runtime + '" --hidden'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $app = [Diagnostics.Process]::Start($info)

    $ready = Wait-Phase 'Ready' 180
    if ((Get-Field 'preparation') -ne 'Ready') { throw ('The local engine is not ready: ' + (Get-Field 'detail')) }
    if ((Get-Field 'recognition_mode') -ne 'local') { throw ('The run is not on the local engine: ' + (Get-Field 'recognition_mode')) }
    if ((Get-Field 'backend') -notmatch 'SenseVoice q8') { throw ('The SenseVoice backend is not reported: ' + (Get-Field 'backend')) }
    if ((Get-Field 'retry_available') -ne 'false') { throw 'A fresh run already offers a retry.' }
    Write-Output ('Ready: ' + (Get-Field 'backend'))

    [PackageFixture]::Start()
    $fixtureStarted = $true
    Start-Sleep -Milliseconds 400
    [PackageFixture]::ResetTarget('retry draft: keep this draft.')
    $initialTarget = Read-Target
    $initialNeighbor = Read-Neighbor
    $initialOther = Read-Other

    # ---- 1. recognition failure retains the audio ------------------------
    $wavs1 = Start-CaptureWithFailure
    $failure1 = Wait-Phase 'Failure'
    $detail1 = Get-Field 'detail'
    $retained1 = Get-Field 'retained_audio'
    $result1 = Read-Text $resultPath
    if ((Get-Field 'preparation') -ne 'Ready') { throw ('A recognition failure must keep preparation=Ready: ' + (Get-Field 'preparation')) }
    if ((Get-Field 'retry_available') -ne 'true') { throw ('The failed capture did not offer a local retry: ' + $detail1) }
    if ($detail1 -notmatch 'cannot read the captured WAV') { throw ('The failure does not name the unreadable capture: ' + $detail1) }
    if ($detail1 -notmatch 'Audio retained for local recovery') { throw ('The failure does not retain its audio: ' + $detail1) }
    if (-not $retained1) { throw 'status.txt reports no retained audio after a recognition failure.' }
    if (@(Get-Wavs).Count -lt 1) { throw 'The failed capture did not keep its WAV on disk.' }
    if ($result1) { throw ('A failed recognition wrote a current result: ' + $result1) }
    Assert-Equal (Read-Target) $initialTarget 'Target after the first failure'
    Assert-NoSendOrEnter
    $cases.Add([ordered]@{
            case = 'recognition-failure-retains-audio'
            passed = $true
            detail = $detail1
            retained_audio = $retained1
            prepared = (Get-Field 'preparation')
            retry_available = (Get-Field 'retry_available')
            current_result_empty = ($result1 -eq '')
            target_unchanged = $true
            wav_count = @(Get-Wavs).Count
        })
    Write-Output 'PASS recognition failure: Failure with preparation=Ready, same audio kept, retry offered.'

    # ---- 2. retry while the engine is still broken -----------------------
    $retainedBeforeRetry = $retained1
    Invoke-Control 'retry'
    $failure2 = Wait-Phase 'Failure' 120
    $detail2 = Get-Field 'detail'
    $retained2 = Get-Field 'retained_audio'
    if ((Get-Field 'retry_available') -ne 'true') { throw ('A failed retry dropped the retry offer: ' + $detail2) }
    if ($retained2 -ne $retainedBeforeRetry) { throw ('The retry did not keep the same retained audio: ' + $retained2 + ' vs ' + $retainedBeforeRetry) }
    if ($detail2 -notmatch 'cannot read the captured WAV') { throw ('The failed retry does not name the unreadable capture: ' + $detail2) }
    if (@(Get-Wavs).Count -lt 1) { throw 'The failed retry deleted the retained audio.' }
    Assert-Equal (Read-Target) $initialTarget 'Target after the failed retry'
    $cases.Add([ordered]@{
            case = 'retry-still-broken-keeps-audio'
            passed = $true
            detail = $detail2
            retained_audio = $retained2
            same_audio = ($retained2 -eq $retainedBeforeRetry)
            retry_available = (Get-Field 'retry_available')
            target_unchanged = $true
        })
    Write-Output 'PASS failed retry: the same audio is still retryable, the target is untouched.'

    # ---- 3. repair the fault and retry the same capture ------------------
    foreach ($wav in @(Get-Wavs)) { if ($deniedPaths.Contains($wav.FullName)) { Clear-DenyRead $wav.FullName } }
        Invoke-Control 'retry'
    $result3 = Wait-Phase 'Result'
    $transcript = Read-Text $resultPath
    $target3 = Read-Target
    if (-not $transcript) { throw 'The recovered retry produced no result.' }
    if ((Get-Field 'retry_available') -ne 'false') { throw 'A successful retry still offers a retry.' }
    if ($retained1 -and (Get-Field 'retained_audio')) { throw ('A successful retry kept its retained audio: ' + (Get-Field 'retained_audio')) }
    if (@(Get-Wavs).Count -ne 0) { throw 'A successful retry left temporary audio behind.' }
    Assert-Equal $target3 ($initialTarget + "`n" + $transcript) 'The recovered retry was not appended exactly once'
    $delivery3 = Get-DeliveryState $deliveryPath
    if ($delivery3 -ne 'appended') { throw ('The recovered retry is not recorded as an append: ' + $delivery3) }
    Assert-Equal (Read-Neighbor) $initialNeighbor 'Neighbour after the recovered retry'
    Assert-Equal (Read-Other) $initialOther 'Other window after the recovered retry'
    Assert-NoSendOrEnter
    $cases.Add([ordered]@{
            case = 'repaired-retry-reaches-result'
            passed = $true
            transcript = $transcript
            transcript_length = $transcript.Length
            target_text = $target3
            delivery = $delivery3
            appended_exactly_once = $true
            retained_audio_after = ''
            wav_count = @(Get-Wavs).Count
            speech_replayed = $false
            note = 'the repaired retry re-decoded the retained WAV; no playback was repeated'
        })
    Write-Output 'PASS repaired retry: Result without replay, appended exactly once, temporary audio removed.'

    # ---- 4. a later failure is cleaned up explicitly ---------------------
    $beforeCleanup = $target3
    $wavs4 = Start-CaptureWithFailure
    $null = Wait-Phase 'Failure'
    if ((Get-Field 'retry_available') -ne 'true') { throw 'The later failure did not offer a retry.' }
    Invoke-Control 'cleanup'
    $deadline = (Get-Date).AddSeconds(60)
    $cleanDetail = ''
    while ((Get-Date) -lt $deadline) {
        $cleanDetail = Get-Field 'detail'
        if ($cleanDetail -match 'deleted at your request') { break }
        Start-Sleep -Milliseconds 100
    }
    if ($cleanDetail -notmatch 'deleted at your request') { throw ('Cleanup did not report the deletion: ' + $cleanDetail) }
    if ((Get-Field 'retry_available') -ne 'false') { throw 'Cleanup did not disable the retry.' }
    if (@(Get-Wavs).Count -ne 0) { throw 'Cleanup did not remove the retained audio.' }
    Assert-Equal (Read-Target) $beforeCleanup 'Target after cleanup'
    $resultAfterCleanup = Read-Text $resultPath
    Assert-Equal $resultAfterCleanup $transcript 'Current result after cleanup'
    Invoke-Control 'retry'
    Start-Sleep -Milliseconds 1200
    if ((Get-Field 'retry_available') -ne 'false') { throw 'A retry after cleanup re-enabled itself.' }
    if (@(Get-Wavs).Count -ne 0) { throw 'A retry after cleanup created new audio.' }
    Assert-Equal (Read-Text $resultPath) $resultAfterCleanup 'Current result after a retry that follows cleanup'
    if ((Get-Field 'phase') -ne 'Failure') { throw ('Cleanup turned the failure into a result: ' + (Get-Field 'phase')) }
    Assert-Equal (Read-Target) $beforeCleanup 'Target after a retry that follows cleanup'
    Assert-NoSendOrEnter
    $cases.Add([ordered]@{
            case = 'cleanup-failed-audio'
            passed = $true
            cleanup_detail = $cleanDetail
            retry_available = (Get-Field 'retry_available')
            wav_count = 0
            current_result_unchanged = $true
            target_unchanged = $true
            retry_after_cleanup_created_audio = $false
        })
    Write-Output 'PASS cleanup: retained audio deleted, retry disabled, result and target unchanged.'

    Invoke-Control 'quit'
    if (-not $app.WaitForExit(15000)) { throw 'Listen did not exit after --control quit.' }
    if ($app.ExitCode -ne 0) { throw ('Listen did not exit cleanly: ' + $app.ExitCode) }
    $app.Dispose()
    $app = $null
    if (@(Get-Process -Name listen -ErrorAction SilentlyContinue)) { throw 'A Listen process is left running after quit.' }

    # ---- 5. a broken model is a preparation failure ----------------------
    # Nothing was captured, so there is no retained audio to retry or clean. Recorded
    # so the evidence states the difference instead of implying a retry from a
    # preparation failure.
    $brokenStatus = Join-Path $brokenRuntime 'status.txt'
    $brokenModel = Join-Path $brokenRuntime 'missing-model.onnx'
    Remove-Item -LiteralPath $brokenModel -ErrorAction SilentlyContinue
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.Arguments = '--engine sensevoice --sherpa-lib "' + $sherpaLib + '" --sensevoice-model "' + $brokenModel + '" --sensevoice-tokens "' + $tokens + '" --sensevoice-vad "' + $vad + '" --runtime "' + $brokenRuntime + '" --hidden'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $app = [Diagnostics.Process]::Start($info)
    $null = Wait-PhaseOn $brokenStatus 'Failure' 120
    $brokenPreparation = Get-Field 'preparation' $brokenStatus
    $brokenDetail = Get-Field 'detail' $brokenStatus
    $brokenRetry = Get-Field 'retry_available' $brokenStatus
    if ($brokenPreparation -ne 'Failed') { throw ('A broken model must fail preparation: ' + $brokenPreparation) }
    if ($brokenRetry -ne 'false') { throw 'A preparation failure claims a retry is available.' }
    if ($brokenDetail -notmatch 'SenseVoice') { throw ('The preparation failure does not name the engine: ' + $brokenDetail) }
        Invoke-Control 'retry'
    Start-Sleep -Milliseconds 800
    $retryDetail = Get-Field 'detail' $brokenStatus
    if ($retryDetail -notmatch 'No retained audio is available for a retry') {
        throw ('A retry after a preparation failure is not explained: ' + $retryDetail)
    }
    Invoke-Control 'cleanup'
    Start-Sleep -Milliseconds 800
    $cleanupDetail = Get-Field 'detail' $brokenStatus
    if ($cleanupDetail -notmatch 'There is no failed capture audio to clean') {
        throw ('Cleanup after a preparation failure is not explained: ' + $cleanupDetail)
    }
    if ((Get-Field 'retry_available' $brokenStatus) -ne 'false') { throw 'Cleanup after a preparation failure offered a retry.' }
    $brokenWavs = @(Get-ChildItem -LiteralPath $brokenRuntime -Recurse -Filter '*.wav' -ErrorAction SilentlyContinue)
    if ($brokenWavs.Count -ne 0) { throw 'A preparation failure captured audio.' }
    Assert-Equal (Read-Target) $beforeCleanup 'Target after the preparation-failure run'
    $cases.Add([ordered]@{
            case = 'broken-model-is-a-preparation-failure'
            passed = $true
            preparation = $brokenPreparation
            detail = $brokenDetail
            retry_available = $brokenRetry
            retry_detail = $retryDetail
            cleanup_detail = $cleanupDetail
            captured_audio = 0
            target_unchanged = $true
            note = 'nothing was captured, so retry and cleanup are explicit no-ops; this is not a recognition failure'
        })
    Write-Output 'PASS broken model: preparation=Failed, retry and cleanup explain there is nothing to recover.'

    Invoke-Control 'quit'
    if (-not $app.WaitForExit(15000)) { throw 'Listen did not exit after --control quit.' }
    if ($app.ExitCode -ne 0) { throw ('Listen did not exit cleanly: ' + $app.ExitCode) }
    $app.Dispose()
    $app = $null
    if (@(Get-Process -Name listen -ErrorAction SilentlyContinue)) { throw 'A Listen process is left running after the failure run.' }

    $evidence = [ordered]@{
        captured_utc = [DateTime]::UtcNow.ToString('o')
        package_root = $PackageRoot
        binary = $binary
        engine = 'sensevoice (--engine sensevoice, packaged sherpa-onnx + q8 model)'
        runtime = $runtime
        broken_model_runtime = $brokenRuntime
        audio_clip = $AudioClip
        fixture_root = $FixtureRoot
        oracle = 'control channel (--control) plus status.txt, current-result.txt and delivery.json; window handles are diagnostics only'
        fault_injection = 'deny-read ACE on the capture spool file (icacls /deny <user>:(RD)), removed before the successful retry'
        acl_log = @($aclLog.ToArray())
        observed_phases = @($phaseLog.ToArray())
        automatic_send_count = [PackageFixture]::Sends
        enter_key_count = [PackageFixture]::ReturnKeys
        runtime_files = @(Get-ChildItem -LiteralPath $runtime -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
        cases = @($cases.ToArray())
        result = 'PASS'
    }
    [IO.File]::WriteAllText((Join-Path $Output 'retry-evidence.json'), ($evidence | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    Write-Output ('Evidence: ' + (Join-Path $Output 'retry-evidence.json'))
}
catch {
    try {
        $evidence = [ordered]@{
            captured_utc = [DateTime]::UtcNow.ToString('o')
            package_root = $PackageRoot
            binary = $binary
            runtime = $runtime
            acl_log = @($aclLog.ToArray())
            observed_phases = @($phaseLog.ToArray())
            cases = @($cases.ToArray())
            result = 'FAIL'
            failure = $_.Exception.Message
        }
        [IO.File]::WriteAllText((Join-Path $Output 'retry-evidence.json'), ($evidence | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    }
    catch { }
    throw
}
finally {
    foreach ($path in @($deniedPaths.ToArray())) {
        try { & icacls.exe $path /remove:d $currentUser 2>&1 | Out-Null } catch { }
    }
    if ($null -ne $app -and -not $app.HasExited) {
        try { Invoke-Control 'quit' } catch { }
        if (-not $app.WaitForExit(15000)) {
            try { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue } catch { }
        }
    }
    if ($fixtureStarted) { try { [PackageFixture]::Close() } catch { } }
    if ($null -ne $app) { $app.Dispose() }
}
