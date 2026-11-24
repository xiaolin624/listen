# Desktop acceptance for issue #24, item 3: a real default-playback-device switch
# while a capture is running, on the packaged listen.exe using the local SenseVoice
# engine.
#
# What it checks:
#   1. the capture starts on the CURRENT default multimedia render endpoint and says so
#      in status.txt (endpoint_role / endpoint_id / endpoint_name);
#   2. switching that default to a DIFFERENT active render endpoint mid-capture
#      interrupts the capture: the run reports Failure with an explicit reason that
#      names both the original and the new endpoint, marks the outcome incomplete,
#      and does NOT silently keep recording or follow the new device;
#   3. the audio already received is preserved (retained_audio and/or the spooled WAV /
#      partial checkpoint), the previous current result and the target draft are
#      unchanged, no retry is offered (a device interruption is not a recognition
#      failure), and no automatic send/Enter happens;
#   4. the original default multimedia and communications endpoints are restored and
#      read back, and the evidence records the before/after endpoints.
#
# ORACLE: the control channel plus the runtime files:
#   --control start|stop|quit   runtime\status.txt   runtime\current-result.txt
#   runtime\delivery.json
# Window classes and control ids are never used for an assertion. The only native
# window calls here create and read the operator-owned TARGET fixture; Listen itself is
# driven through --control.
#
# The device switch uses the documented MMDeviceEnumerator + Windows policy COM server
# (AudioRoute.cs) and plays real PCM to the original endpoint so the loopback capture
# has samples. Nothing about the product is modified. The finally block always restores
# every original default and reads it back, even after an assertion failure.
#
# ASCII only on purpose: this file must survive any code page.
#
# Example:
#   pwsh -File scripts\verify-package-device.ps1 -PackageRoot <extracted package> `
#     -Output .artifacts\issue-24\device

param(
    [Parameter(Mandatory = $true)][string]$PackageRoot,
    [Parameter(Mandatory = $true)][string]$Output,
    [string]$FixtureRoot = '',
    [string]$PlaybackWav = '',
    [string]$SwitchEndpointName = '',
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
}
catch { }
$artifacts = Join-Path $sharedRoot '.artifacts'

$PackageRoot = [IO.Path]::GetFullPath($PackageRoot)
$Output = [IO.Path]::GetFullPath($Output)
if (-not $FixtureRoot) { $FixtureRoot = Join-Path $artifacts 'issue-11\accept' }
$FixtureRoot = [IO.Path]::GetFullPath($FixtureRoot)
if (-not $PlaybackWav) {
    foreach ($candidate in @((Join-Path $artifacts 'issue-2-integration\source-a.wav'), (Join-Path $artifacts 'issue-6-smoke\loop-96s.wav'))) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $PlaybackWav = $candidate; break }
    }
}
if (-not $PlaybackWav) { throw 'No playback WAV was found; pass -PlaybackWav.' }
$PlaybackWav = [IO.Path]::GetFullPath($PlaybackWav)

$binary = Join-Path $PackageRoot 'listen.exe'
$sherpaLib = Join-Path $PackageRoot 'engine\sherpa'
$model = Join-Path $PackageRoot 'models\sense-voice\model.int8.onnx'
$tokens = Join-Path $PackageRoot 'models\sense-voice\tokens.txt'
$vad = Join-Path $PackageRoot 'models\sense-voice\silero_vad.onnx'
$nativeQa = Join-Path $FixtureRoot 'ListenNativeQa.cs'
$packageFixture = Join-Path $FixtureRoot 'PackageFixture.cs'
$audioRoute = Join-Path $FixtureRoot 'AudioRoute.cs'
foreach ($required in @($binary, $nativeQa, $packageFixture, $audioRoute)) {
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
$statusPath = Join-Path $runtime 'status.txt'
$resultPath = Join-Path $runtime 'current-result.txt'
$deliveryPath = Join-Path $runtime 'delivery.json'

Add-Type -Path $nativeQa -ReferencedAssemblies System.Drawing
Add-Type -Path $packageFixture -ReferencedAssemblies System.Drawing, System.Windows.Forms
Add-Type -Path $audioRoute
$null = [ListenNativeQa]::SetProcessDPIAware()

$app = $null
$fixtureStarted = $false
$originalMm = $null
$originalComm = $null
$originalConsole = $null
$restoreVerified = $false

function Read-StatusText([string]$Path = '') {
    if (-not $Path) { $Path = $statusPath }
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        try { return [IO.File]::ReadAllText($Path) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw "The status file stayed locked by Listen: $Path"
}
function Get-Field([string]$Name) {
    $text = Read-StatusText
    $match = [regex]::Match($text, ('(?m)^' + [regex]::Escape($Name) + '=(.*)$'))
    if ($match.Success) { return $match.Groups[1].Value.TrimEnd("`r") }
    return ''
}
function Get-PhaseText([string]$Text) {
    if ($Text -match '(?m)^phase=(\w+)') { return $Matches[1] }
    return 'unknown'
}
function Wait-Phase([string]$Wanted, [int]$Seconds = $TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $text = Read-StatusText
            if ((Get-PhaseText $text) -eq $Wanted) { return $text }
        }
        if ($app.HasExited) { throw 'Listen exited during the device acceptance.' }
        Start-Sleep -Milliseconds 50
    } until ([DateTime]::UtcNow -gt $deadline)
    throw ("Wait expired for phase ${Wanted}:`n" + (Read-StatusText))
}
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
function Endpoint-Json($endpoint) {
    return [ordered]@{ role = $endpoint.Role; id = $endpoint.Id; name = $endpoint.Name }
}
function Set-Default([string]$DeviceId, [int]$Role, [string]$Label) {
    [AudioRoute]::SetDefault($DeviceId, $Role)
    Start-Sleep -Milliseconds 400
    $now = if ($Role -eq 1) { [AudioRoute]::Multimedia() } else { [AudioRoute]::Communications() }
    if ($now.Id -ne $DeviceId) { throw ($Label + ' default did not change to ' + $DeviceId) }
    return $now
}
function Get-Wavs {
    return @(Get-ChildItem -LiteralPath $runtime -Recurse -Filter '*.wav' -ErrorAction SilentlyContinue)
}
function Get-PartialResults {
    return @(Get-ChildItem -LiteralPath $runtime -Recurse -Filter 'partial-result.txt' -ErrorAction SilentlyContinue)
}
function Save-Screen([string]$Path) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap([int]$bounds.Width, [int]$bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen([int]$bounds.X, [int]$bounds.Y, 0, 0, $bitmap.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

try {
    if (Test-Path -LiteralPath $runtime) { Remove-Item -LiteralPath $runtime -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $runtime | Out-Null
    $sentinel = 'previous complete result that a device interruption must not replace'
    [IO.File]::WriteAllText($resultPath, $sentinel, [Text.UTF8Encoding]::new($false))

    # ---- capture the original routing ----------------------------------
    $originalMm = [AudioRoute]::Multimedia()
    $originalComm = [AudioRoute]::Communications()
    $originalConsole = [AudioRoute]::Console()
    $endpoints = @([AudioRoute]::ActiveRenderEndpoints())
    $switch = $null
    if ($SwitchEndpointName) {
        $switch = $endpoints | Where-Object { $_.Name -like $SwitchEndpointName -and $_.Id -ne $originalMm.Id } | Select-Object -First 1
    }
    if (-not $switch) {
        $switch = $endpoints | Where-Object { $_.Id -ne $originalMm.Id -and $_.Name -like '*Realtek*' } | Select-Object -First 1
    }
    if (-not $switch) {
        $switch = $endpoints | Where-Object { $_.Id -ne $originalMm.Id } | Select-Object -First 1
    }
    if (-not $switch) { throw 'No second active render endpoint is available for the route-change scenario.' }
    $originalRecord = [ordered]@{
        captured_utc = [DateTime]::UtcNow.ToString('o')
        multimedia = Endpoint-Json $originalMm
        communications = Endpoint-Json $originalComm
        console = Endpoint-Json $originalConsole
        active_render_endpoints = @($endpoints | ForEach-Object { Endpoint-Json $_ })
    }
    [IO.File]::WriteAllText((Join-Path $Output 'original-endpoints.json'), ($originalRecord | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    Write-Output ('ORIGINAL multimedia=' + $originalMm.ToString())
    Write-Output ('SWITCH TARGET=' + $switch.ToString())

    # ---- start the packaged local engine -------------------------------
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $binary
    $info.Arguments = '--engine sensevoice --sherpa-lib "' + $sherpaLib + '" --sensevoice-model "' + $model +
    '" --sensevoice-tokens "' + $tokens + '" --sensevoice-vad "' + $vad + '" --runtime "' + $runtime + '" --hidden'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $app = [Diagnostics.Process]::Start($info)

    $null = Wait-Phase 'Ready'
    if ((Get-Field 'preparation') -ne 'Ready') { throw ('The local engine is not ready: ' + (Get-Field 'detail')) }
    if ((Get-Field 'recognition_mode') -ne 'local') { throw ('The run is not on the local engine: ' + (Get-Field 'recognition_mode')) }
    if ((Get-Field 'backend') -notmatch 'SenseVoice q8') { throw ('The SenseVoice backend is not reported: ' + (Get-Field 'backend')) }
    Write-Output ('Ready: ' + (Get-Field 'backend'))

    [PackageFixture]::Start()
    $fixtureStarted = $true
    Start-Sleep -Milliseconds 400
    [PackageFixture]::ResetTarget('device draft: keep this draft.')
    $initialTarget = Read-Target
    $initialNeighbor = Read-Neighbor
    $initialOther = Read-Other

    # ---- switch the default render endpoint during the capture ---------
    [ListenNativeQa]::ClickControl([PackageFixture]::Primary, [PackageFixture]::Target, $PID)
    Invoke-Control 'start'
    $capturing = Wait-Phase 'Capturing' 30
    if ($capturing -notmatch ('endpoint_id=' + [regex]::Escape($originalMm.Id))) {
        throw ('The capture did not name the original multimedia endpoint: ' + $capturing)
    }
    if ((Get-Field 'endpoint_role') -notmatch 'multimedia') {
        throw ('The capture does not report a multimedia endpoint role: ' + (Get-Field 'endpoint_role'))
    }
    [AudioRoute]::StartPlayback($originalMm.Id, $PlaybackWav)
    Start-Sleep -Seconds 3
    $switched = Set-Default $switch.Id 1 'multimedia'
    Write-Output ('SWITCHED multimedia=' + $switched.ToString())

    $failureText = Wait-Phase 'Failure'
    $playbackDone = [AudioRoute]::WaitPlayback(30000)
    if (-not $playbackDone) { throw 'Timed-out acceptance playback.' }
    [IO.File]::WriteAllText((Join-Path $Output 'status-failure.txt'), $failureText, [Text.UTF8Encoding]::new($false))
    Save-Screen (Join-Path $Output 'device-failure.png')
    $detail = Get-Field 'detail'

    # The reason must be explicit and name both devices, and the outcome must be marked
    # incomplete instead of quietly continuing or following the new default.
    foreach ($need in @('incomplete', $originalMm.Name, $originalMm.Id, $switch.Name, $switch.Id)) {
        if (-not $detail.Contains($need)) {
            throw ('The interruption detail does not state {' + $need + '}:' + [Environment]::NewLine + $detail)
        }
    }
    if ($detail -notmatch 'changed during capture|AUDCLNT_E_DEVICE_INVALIDATED') {
        throw ('The interruption reason is not explicit: ' + $detail)
    }
    # It must not have adopted the new default endpoint as the capture source.
    if ((Get-Field 'endpoint_id') -ne $originalMm.Id) {
        throw ('status.txt no longer names the original endpoint after the switch: ' + (Get-Field 'endpoint_id'))
    }
    # It must not keep capturing and must not start a replacement capture on its own.
    $settle = [DateTime]::UtcNow.AddSeconds(3)
    do {
        Start-Sleep -Milliseconds 200
        $now = Read-StatusText
        if ((Get-PhaseText $now) -ne 'Failure') {
            throw ('The interrupted run left the Failure phase on its own: ' + (Get-PhaseText $now))
        }
        if ($now -match '(?m)^endpoint_id=(.+)$' -and $Matches[1].Trim() -ne $originalMm.Id) {
            throw ('The interrupted run followed the new default endpoint: ' + $Matches[1])
        }
    } until ([DateTime]::UtcNow -gt $settle)

    # The already received audio has to survive the interruption.
    $retained = Get-Field 'retained_audio'
    $partial = @(Get-PartialResults)
    $wavs = @(Get-Wavs)
    if (-not $retained -and $partial.Count -eq 0 -and $wavs.Count -eq 0) {
        throw 'Neither retained audio nor a checkpoint survived the device interruption.'
    }
    # A device interruption is not a recognition failure: no local retry offer.
    if ((Get-Field 'retry_available') -ne 'false') {
        throw 'A device interruption incorrectly offered a recognition retry.'
    }
    if ((Read-Text $resultPath) -cne $sentinel) { throw 'The interrupted capture overwrote the previous current result.' }
    Assert-Equal (Read-Target) $initialTarget 'Target after the device interruption'
    Assert-Equal (Read-Neighbor) $initialNeighbor 'Neighbour after the device interruption'
    Assert-Equal (Read-Other) $initialOther 'Other window after the device interruption'
    if ([PackageFixture]::Sends -ne 0) { throw 'An automatic send action occurred.' }
    if ([PackageFixture]::ReturnKeys -ne 0) { throw 'An automatic Enter key action occurred.' }

    # Visual evidence only: expand the panel so the same reason is visible, and record
    # whether the panel text repeats it. No assertion depends on the panel layout.
    $null = Invoke-Control 'show'
    Start-Sleep -Milliseconds 900
    Save-Screen (Join-Path $Output 'device-panel.png')
    $panel = Read-Text (Join-Path $runtime 'panel.txt')

    $evidence = [ordered]@{
        captured_utc = [DateTime]::UtcNow.ToString('o')
        package_root = $PackageRoot
        binary = $binary
        runtime = $runtime
        playback_wav = $PlaybackWav
        engine = 'sensevoice'
        oracle = '--control start/quit plus runtime\status.txt, current-result.txt and delivery.json; no window class or control id is used as evidence'
        original_endpoints = $originalRecord
        switched_endpoint = Endpoint-Json $switch
        capturing_status = $capturing
        failure_status = $failureText
        failure_detail = $detail
        phase_sequence = @('Ready', 'Capturing', 'Failure')
        incomplete_marked = $true
        followed_new_default = $false
        retained_audio = $retained
        partial_result_files = @($partial | ForEach-Object { $_.FullName })
        spooled_wav_files = @($wavs | ForEach-Object { $_.FullName })
        previous_current_result_unchanged = $true
        target_unchanged = $true
        neighbour_unchanged = $true
        other_window_unchanged = $true
        retry_available = (Get-Field 'retry_available')
        panel_visible = (Get-Field 'panel_visible')
        panel_mentions_incomplete = ($panel -match 'incomplete')
        panel_excerpt = @(($panel -split "`n") | Where-Object { $_ -match 'incomplete|changed during capture' } | Select-Object -First 2)
        screenshots = @((Join-Path $Output 'device-failure.png'), (Join-Path $Output 'device-panel.png'))
        automatic_send_count = [PackageFixture]::Sends
        enter_key_count = [PackageFixture]::ReturnKeys
    }
    [IO.File]::WriteAllText((Join-Path $Output 'device-evidence.json'), ($evidence | ConvertTo-Json -Depth 10), [Text.UTF8Encoding]::new($false))
    Write-Output 'PASS real default-endpoint switch: interrupted with both endpoint identities, marked incomplete, received audio preserved, previous result and target unchanged.'
}
finally {
    if ($null -ne $app -and -not $app.HasExited) {
        try { Invoke-Control 'quit' } catch { }
        if (-not $app.WaitForExit(10000)) { try { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue } catch { } }
    }
    if ($fixtureStarted) { try { [PackageFixture]::Close() } catch { } }
    if ($null -ne $app) { $app.Dispose() }
    # Always restore and read back every role, including after an assertion failure.
    try {
        if ($null -ne $originalMm) { $null = Set-Default $originalMm.Id 1 'multimedia' }
        if ($null -ne $originalComm) { $null = Set-Default $originalComm.Id 2 'communications' }
        $restoreVerified = ($null -ne $originalMm -and $null -ne $originalComm -and
            [AudioRoute]::Multimedia().Id -eq $originalMm.Id -and
            [AudioRoute]::Communications().Id -eq $originalComm.Id)
    }
    catch { }
    $restore = [ordered]@{
        restored_utc = [DateTime]::UtcNow.ToString('o')
        restore_verified = $restoreVerified
        multimedia = if ($null -ne $originalMm) { Endpoint-Json ([AudioRoute]::Multimedia()) } else { $null }
        communications = if ($null -ne $originalComm) { Endpoint-Json ([AudioRoute]::Communications()) } else { $null }
        console = if ($null -ne $originalConsole) { Endpoint-Json ([AudioRoute]::Console()) } else { $null }
    }
    [IO.File]::WriteAllText((Join-Path $Output 'restore.json'), ($restore | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
    if (-not $restoreVerified) { Write-Warning 'The original multimedia endpoint was not verified after restore.' }
}
