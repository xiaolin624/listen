# Desktop acceptance for issue #24: the target append regression, plus a target that
# became read-only, a target that enforces a host character capacity, and a target
# window that was destroyed.
#
# What it checks, on a real packaged listen.exe with the local SenseVoice engine:
#   - a normal capture appends the transcript to the focused target exactly once, even
#     though stop is sent twice (a repeated stop must not append or deliver again), the
#     neighbour and the other window stay byte-identical, and no Enter or Send happens;
#   - a read-only target during capture is never written, the complete result goes to
#     the expanded result panel, and the delivery is reported as a fallback;
#   - a target with a 40-character host capacity reports a truncated delivery (a stable
#     prefix, never "success"), keeps the complete result in the panel, and leaves the
#     target shorter than the intended text;
#   - a target destroyed during capture is not written at all and the complete result
#     still reaches the panel;
#   - in every case the neighbouring control and the other window are untouched, no
#     Enter or Send is ever produced, and no temporary audio is left behind.
#
# ORACLE: the acceptance oracle is the control channel plus the runtime files:
#   --control start|stop|show|copy|cleanup|quit
#   runtime\status.txt          phase, delivery state, detail, panel_visible, ...
#   runtime\panel.txt           the complete panel text
#   runtime\current-result.txt  the complete result
# Window classes, control ids and screenshots are diagnostics only. The only native
# window calls here are the ones that create the operator-owned TARGET fixture and read
# its text back; Listen itself is always driven through --control.
#
# The target fixture is a task-owned WinForms EDIT control (PackageFixture.cs): Listen
# receives no handles, ids or coordinates from it.
#
# ASCII only on purpose: this file must survive any code page.
#
# Example:
#   pwsh -File scripts\verify-native-target.ps1 -PackageRoot <extracted package> `
#     -Output .artifacts\issue-24\native-target

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
$statusPath = Join-Path $runtime 'status.txt'
$panelPath = Join-Path $runtime 'panel.txt'
$resultPath = Join-Path $runtime 'current-result.txt'

Add-Type -Path $nativeQa -ReferencedAssemblies System.Drawing
Add-Type -Path $packageFixture -ReferencedAssemblies System.Drawing, System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
/// Activation helper for the operator-owned fixture window. A shared desktop may put
/// another window in front between captures; one ALT tap clears the foreground lock so
/// this process may raise its own fixture again.
public static class OperatorWindow
{
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(int process);
    [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
    public static bool Activate(IntPtr hwnd)
    {
        AllowSetForegroundWindow(-1);
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 2, UIntPtr.Zero);
        ShowWindow(hwnd, 9);
        BringWindowToTop(hwnd);
        return SetForegroundWindow(hwnd);
    }
}
'@
$null = [ListenNativeQa]::SetProcessDPIAware()

$app = $null
$fixtureStarted = $false
$phaseLog = [Collections.Generic.List[string]]::new()
$cases = [Collections.Generic.List[object]]::new()

function Read-StatusText {
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        try { return [IO.File]::ReadAllText($statusPath) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw 'The status file stayed locked by Listen.'
}
function Get-Field([string]$Name) {
    $text = Read-StatusText
    $match = [regex]::Match($text, ('(?m)^' + [regex]::Escape($Name) + '=(.*)$'))
    if ($match.Success) { return $match.Groups[1].Value.TrimEnd("`r") }
    return ''
}
function Get-Phase([string]$Text) {
    if ($Text -match '(?m)^phase=(\w+)') { return $Matches[1] }
    return 'unknown'
}
function Record-Phase([string]$Phase) {
    if ($phaseLog.Count -eq 0 -or $phaseLog[$phaseLog.Count - 1] -ne $Phase) { $phaseLog.Add($Phase) }
}
function Wait-Phase([string]$Wanted, [int]$Seconds = $TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $text = Read-StatusText
            $phase = Get-Phase $text
            Record-Phase $phase
            if ($phase -eq $Wanted) { return $text }
            if ($phase -eq 'Failure' -and $Wanted -ne 'Failure') {
                throw ('Listen reported Failure while waiting for ' + $Wanted + ':' + [Environment]::NewLine + $text)
            }
        }
        if ($app.HasExited) { throw 'Listen exited during acceptance.' }
        Start-Sleep -Milliseconds 50
    } until ([DateTime]::UtcNow -gt $deadline)
    throw ('Wait expired for phase ' + $Wanted + ':' + [Environment]::NewLine + (Read-StatusText))
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
function Read-Target {
    if ([PackageFixture]::Target -eq [IntPtr]::Zero) { throw 'The fixture target window is gone.' }
    return [ListenNativeQa]::ReadText([PackageFixture]::Target, $PID)
}
function Read-Neighbor { return [ListenNativeQa]::ReadText([PackageFixture]::Neighbor, $PID) }
function Read-Other { return [ListenNativeQa]::ReadText([PackageFixture]::Other, $PID) }
function Play-Speech {
    $player = [System.Media.SoundPlayer]::new($AudioClip)
    try { $player.PlaySync() } finally { $player.Dispose() }
}
function Get-Wavs {
    return @(Get-ChildItem -LiteralPath $runtime -Recurse -Filter '*.wav' -ErrorAction SilentlyContinue)
}
function Get-Transcript {
    if (-not (Test-Path -LiteralPath $resultPath)) { throw 'The complete result file is missing.' }
    $text = Read-Text $resultPath
    if ([string]::IsNullOrWhiteSpace($text)) { throw 'The complete result is empty.' }
    return $text
}
function Assert-NoSendOrEnter {
    if ([PackageFixture]::Sends -ne 0) { throw 'An automatic send action occurred.' }
    if ([PackageFixture]::ReturnKeys -ne 0) { throw 'An automatic Enter key action occurred.' }
}
function Assert-NoTemporaryAudio {
    if ((Get-Wavs).Count -ne 0) { throw 'A completed recognition left temporary audio in the runtime directory.' }
}
function Save-DesktopShot([string]$Path) {
    $screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bitmap = [System.Drawing.Bitmap]::new($screen.Width, $screen.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($screen.X, $screen.Y, 0, 0, $bitmap.Size) }
    finally { $graphics.Dispose() }
    try { $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $bitmap.Dispose() }
}
function Focus-Target {
    # Raise the operator-owned fixture and give it keyboard focus on the target control.
    # Retried because a shared desktop can take the foreground between captures.
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        [void][OperatorWindow]::Activate([PackageFixture]::Primary)
        Start-Sleep -Milliseconds 250
        try {
            [ListenNativeQa]::ClickControl([PackageFixture]::Primary, [PackageFixture]::Target, $PID)
            return
        }
        catch {
            if ($attempt -eq 7) { throw }
            Start-Sleep -Milliseconds 400
        }
    }
}
function Focus-OtherWindow {
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        [void][OperatorWindow]::Activate([PackageFixture]::Secondary)
        Start-Sleep -Milliseconds 250
        try {
            [ListenNativeQa]::ClickControl([PackageFixture]::Secondary, [PackageFixture]::Other, $PID)
            return
        }
        catch {
            if ($attempt -eq 7) { throw }
            Start-Sleep -Milliseconds 400
        }
    }
}
function Start-Capture {
    # Focus the operator-owned target, start through the control channel, play real
    # speech into the default render endpoint, then stop. The target keeps the focus so
    # the application reads its draft exactly as a user would leave it.
    Focus-Target
    Invoke-Control 'start'
    $null = Wait-Phase 'Capturing' 30
    Play-Speech
    Invoke-Control 'stop'
}
function Complete-Capture([string]$CaseName) {
    $status = Wait-Phase 'Result'
    $detail = Get-Field 'detail'
    $delivery = 'unknown'
    try {
        # delivery.json is the durable delivery ledger with one record per capture
        # order; the newest capture is the last record, so take the last state.
        $deliveryText = Read-Text (Join-Path $runtime 'delivery.json')
        if ($deliveryText) {
            $states = [regex]::Matches($deliveryText, '"state":"(\w+)"')
            if ($states.Count -gt 0) { $delivery = $states[$states.Count - 1].Groups[1].Value }
        }
    } catch { }
    $shot = Join-Path $Output ('target-' + $CaseName + '.png')
    Save-DesktopShot $shot
    return [ordered]@{
        status = $status
        detail = $detail
        delivery = $delivery
        transcript = (Get-Transcript)
        panel_visible = (Get-Field 'panel_visible')
        panel_text = (Read-Text $panelPath)
        shot = $shot
    }
}

try {
    if (Test-Path -LiteralPath $runtime) { Remove-Item -LiteralPath $runtime -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $runtime | Out-Null

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
    if ((Get-Field 'panel_visible') -ne '0') { throw 'The panel is visible before any result.' }
    Write-Output ('Ready: ' + (Get-Field 'backend'))

    [PackageFixture]::Start()
    $fixtureStarted = $true
    Start-Sleep -Milliseconds 400

    # ---- 1. appended delivery, with a repeated stop ----------------------
    # The whole append regression on the packaged binary: exactly one append, the
    # neighbours untouched, no Enter/Send, and a second stop that must change nothing.
    [PackageFixture]::SetMode(0)
    [PackageFixture]::SetCapacity(0)
    [PackageFixture]::SetReadOnly($false)
    [PackageFixture]::ResetTarget('append draft: keep this exactly.')
    $before0 = Read-Target
    $neighbor0 = Read-Neighbor
    $other0 = Read-Other

    Focus-Target
    Invoke-Control 'start'
    $null = Wait-Phase 'Capturing' 30
    Play-Speech
    Invoke-Control 'stop'
    # A repeated stop must be a no-op: it may arrive while the capture is still being
    # finalized or after the result, and must never deliver a second time.
    Invoke-Control 'stop'
    $status0 = Wait-Phase 'Result'
    $case0 = Complete-Capture 'append'
    $after0 = Read-Target
    $transcriptLength0 = (Get-Transcript).Length

    if ($case0.delivery -ne 'appended') { throw ('A focused writable target did not receive an append: ' + $case0.delivery + ' / ' + $case0.detail) }
    Assert-Equal $after0 ($before0 + "`n" + $case0.transcript) 'The transcript was not appended exactly once'
    if (($after0 -match [regex]::Escape($case0.transcript)) -and (([regex]::Matches($after0, [regex]::Escape($case0.transcript))).Count -ne 1)) {
        throw 'The transcript was appended more than once.'
    }
    if ($case0.panel_visible -ne '0') { throw ('An appended result opened the panel instead of writing the target: panel_visible=' + $case0.panel_visible) }
    Assert-Equal (Read-Neighbor) $neighbor0 'Neighbour after the appended capture'
    Assert-Equal (Read-Other) $other0 'Other window after the appended capture'
    Assert-NoSendOrEnter
    Assert-NoTemporaryAudio

    # A stop after the result must leave the target exactly as it is.
    Invoke-Control 'stop'
    Start-Sleep -Milliseconds 800
    Assert-Equal (Read-Target) $after0 'Target after a stop following the result'
    if ((Get-Field 'phase') -ne 'Result') { throw ('A stop after the result changed the phase: ' + (Get-Field 'phase')) }
    Assert-Equal (Get-Transcript) $case0.transcript 'Current result after a stop following the result'
    Assert-NoSendOrEnter
    $cases.Add([ordered]@{
            case = 'appended-delivery-with-repeated-stop'
            passed = $true
            delivery = $case0.delivery
            detail = $case0.detail
            transcript_length = $transcriptLength0
            target_before = $before0
            target_after = $after0
            appended_exactly_once = $true
            repeated_stop_changed_target = $false
            neighbor_unchanged = $true
            other_unchanged = $true
            panel_visible = $case0.panel_visible
            enter_count = [PackageFixture]::ReturnKeys
            send_count = [PackageFixture]::Sends
            screenshot = $case0.shot
        })
    Write-Output 'PASS appended delivery: one append, repeated stop a no-op, neighbours untouched, 0 Enter/0 Send.'
    Invoke-Control 'cleanup'

    # ---- 2. read-only target during capture ------------------------------
    [PackageFixture]::SetMode(0)
    [PackageFixture]::SetReadOnly($false)
    [PackageFixture]::ResetTarget('readonly draft: keep this exactly.')
    $before1 = Read-Target
    $neighbor1 = Read-Neighbor
    $other1 = Read-Other

    Focus-Target
    Invoke-Control 'start'
    $null = Wait-Phase 'Capturing' 30
    [PackageFixture]::SetReadOnly($true)
    Start-Sleep -Milliseconds 300
    Play-Speech
    Invoke-Control 'stop'
    $case1 = Complete-Capture 'readonly'
    [PackageFixture]::SetReadOnly($false)

    if ($case1.delivery -ne 'fallback') { throw ('The read-only target was not reported as a fallback: ' + $case1.delivery + ' / ' + $case1.detail) }
    if ($case1.detail -notmatch 'read-only|read only') { throw ('The read-only detail is not explicit: ' + $case1.detail) }
    if ($case1.detail -notmatch 'nothing was written') { throw ('The read-only detail does not say nothing was written: ' + $case1.detail) }
    if ($case1.panel_visible -ne '1') { throw ('A fallback result did not open the panel: panel_visible=' + $case1.panel_visible) }
    if (-not $case1.panel_text.Contains($case1.transcript)) { throw 'panel.txt does not contain the complete result.' }
    Assert-Equal (Read-Target) $before1 'Read-only target content'
    Assert-Equal (Read-Neighbor) $neighbor1 'Neighbour after the read-only capture'
    Assert-Equal (Read-Other) $other1 'Other window after the read-only capture'
    Assert-NoSendOrEnter
    Assert-NoTemporaryAudio
    $cases.Add([ordered]@{
            case = 'read-only-target'
            passed = $true
            delivery = $case1.delivery
            detail = $case1.detail
            transcript_length = $case1.transcript.Length
            target_before = $before1
            target_after = (Read-Target)
            neighbor_unchanged = $true
            other_unchanged = $true
            panel_visible = $case1.panel_visible
            screenshot = $case1.shot
        })
    Write-Output 'PASS read-only target: nothing written, complete result in the panel.'
    Invoke-Control 'cleanup'

    # ---- 2. host capacity of 40 characters -------------------------------
    [PackageFixture]::SetMode(0)
    [PackageFixture]::SetCapacity(0)
    [PackageFixture]::ResetTarget('capacity draft: Rust 123.')
    [PackageFixture]::SetMode(1)
    [PackageFixture]::SetCapacity(40)
    $before2 = Read-Target
    $neighbor2 = Read-Neighbor
    $other2 = Read-Other

    Start-Capture
    $case2 = Complete-Capture 'capacity'
    [PackageFixture]::SetMode(0)
    [PackageFixture]::SetCapacity(0)
    $after2 = Read-Target

    if ($case2.delivery -eq 'appended') { throw ('The truncated write was reported as a complete append: ' + $case2.detail) }
    # A truncated write is recorded as uncertain: the product cannot prove the
    # target holds the complete result, so it must never be recorded as an append. This
    # matches the Rust contract (Delivery::Uncertain for a capacity-limited target).
    if ($case2.delivery -ne 'uncertain') { throw ('The truncated write is not reported as uncertain: ' + $case2.delivery + ' / ' + $case2.detail) }
    if ($case2.detail -notmatch 'truncated') { throw ('The truncation detail is not explicit: ' + $case2.detail) }
    if ($case2.detail -notmatch 'not a complete delivery') { throw ('The truncation detail does not say it is incomplete: ' + $case2.detail) }
    if ($case2.detail -match 'The complete result was appended') { throw ('A truncated write claimed complete success: ' + $case2.detail) }
    $counted = [regex]::Match($case2.detail, 'only (\d+) of (\d+) characters')
    if (-not $counted.Success) { throw ('The truncation detail does not state how much was written: ' + $case2.detail) }
    if ($case2.panel_visible -ne '1') { throw ('A truncated delivery did not open the panel: panel_visible=' + $case2.panel_visible) }
    if (-not $case2.panel_text.Contains($case2.transcript)) { throw 'panel.txt does not contain the complete result after truncation.' }
    $intended = $before2 + "`n" + $case2.transcript
    if ((Normalize $after2).Length -ge (Normalize $intended).Length) {
        throw ('The capacity target unexpectedly holds the complete intended text: ' + $after2.Length + ' >= ' + $intended.Length)
    }
    if ((Normalize $after2).Length -gt 40) { throw ('The capacity target holds more than its 40-character limit: ' + $after2.Length) }
    if ([int]$counted.Groups[1].Value -gt 40) { throw ('status.txt claims more than the 40-character capacity was written: ' + $counted.Groups[1].Value) }
    if ([int]$counted.Groups[1].Value -ne (Normalize $after2).Length) {
        throw ('status.txt reports ' + $counted.Groups[1].Value + ' characters written but the target holds ' + $after2.Length + '.')
    }
    if ((Normalize $before2) -ne '' -and -not (Normalize $intended).StartsWith((Normalize $after2))) {
        throw ('The truncated text is not a prefix of the intended text: ' + $after2)
    }
    Assert-Equal (Read-Neighbor) $neighbor2 'Neighbour after the capacity capture'
    Assert-Equal (Read-Other) $other2 'Other window after the capacity capture'
    Assert-NoSendOrEnter
    Assert-NoTemporaryAudio
    $cases.Add([ordered]@{
            case = 'host-capacity-truncation'
            passed = $true
            capacity = 40
            delivery = $case2.delivery
            detail = $case2.detail
            target_before = $before2
            target_after = $after2
            target_length = $after2.Length
            intended_length = $intended.Length
            transcript_length = $case2.transcript.Length
            neighbour_unchanged = $true
            other_unchanged = $true
            panel_visible = $case2.panel_visible
            screenshot = $case2.shot
        })
    Write-Output ('PASS capacity truncation: target holds ' + $after2.Length + ' of ' + $intended.Length + ' characters, complete result in the panel.')
    Invoke-Control 'cleanup'

    # ---- 3. target destroyed during capture ------------------------------
    [PackageFixture]::SetMode(0)
    [PackageFixture]::SetCapacity(0)
    [PackageFixture]::ResetTarget('destroyed draft: keep this exactly.')
    $before3 = Read-Target
    $neighbor3 = Read-Neighbor
    $other3 = Read-Other

    Focus-Target
    Invoke-Control 'start'
    $null = Wait-Phase 'Capturing' 30
    [PackageFixture]::DestroyTarget()
    Start-Sleep -Milliseconds 300
    Focus-OtherWindow
    Play-Speech
    Invoke-Control 'stop'
    $case3 = Complete-Capture 'destroyed-target'

    if ($case3.delivery -ne 'fallback') { throw ('The destroyed target was not reported as a fallback: ' + $case3.delivery + ' / ' + $case3.detail) }
    if ($case3.detail -notmatch 'no longer available|not available|no supported|destroyed') {
        throw ('The destroyed-target detail is not explicit: ' + $case3.detail)
    }
    if ($case3.panel_visible -ne '1') { throw ('A lost target did not open the panel: panel_visible=' + $case3.panel_visible) }
    if (-not $case3.panel_text.Contains($case3.transcript)) { throw 'panel.txt does not contain the complete result after the target was lost.' }
    Assert-Equal (Read-Neighbor) $neighbor3 'Neighbour after the destroyed-target capture'
    Assert-Equal (Read-Other) $other3 'Other window after the destroyed-target capture'
    Assert-NoSendOrEnter
    Assert-NoTemporaryAudio
    $cases.Add([ordered]@{
            case = 'destroyed-target'
            passed = $true
            delivery = $case3.delivery
            detail = $case3.detail
            target_before = $before3
            target_window_destroyed = ([PackageFixture]::Target -eq [IntPtr]::Zero)
            neighbour_unchanged = $true
            other_unchanged = $true
            panel_visible = $case3.panel_visible
            transcript_length = $case3.transcript.Length
            screenshot = $case3.shot
        })
    Write-Output 'PASS destroyed target: nothing written, complete result in the panel.'

    $evidence = [ordered]@{
        captured_utc = [DateTime]::UtcNow.ToString('o')
        package_root = $PackageRoot
        binary = $binary
        engine = 'sensevoice (--engine sensevoice, packaged sherpa-onnx + q8 model)'
        runtime = $runtime
        audio_clip = $AudioClip
        fixture_root = $FixtureRoot
        oracle = 'control channel (--control) plus status.txt, panel.txt and current-result.txt; window handles are diagnostics only'
        observed_phases = @($phaseLog.ToArray())
        automatic_send_count = [PackageFixture]::Sends
        enter_key_count = [PackageFixture]::ReturnKeys
        runtime_files = @(Get-ChildItem -LiteralPath $runtime -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
        cases = @($cases.ToArray())
        result = 'PASS'
    }
    [IO.File]::WriteAllText((Join-Path $Output 'target-evidence.json'), ($evidence | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    Write-Output ('Evidence: ' + (Join-Path $Output 'target-evidence.json'))
}
catch {
    try {
        $evidence = [ordered]@{
            captured_utc = [DateTime]::UtcNow.ToString('o')
            package_root = $PackageRoot
            binary = $binary
            runtime = $runtime
            observed_phases = @($phaseLog.ToArray())
            cases = @($cases.ToArray())
            result = 'FAIL'
            failure = $_.Exception.Message
        }
        [IO.File]::WriteAllText((Join-Path $Output 'target-evidence.json'), ($evidence | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    }
    catch { }
    throw
}
finally {
    if ($null -ne $app -and -not $app.HasExited) {
        try { Invoke-Control 'quit' } catch { }
        if (-not $app.WaitForExit(15000)) {
            try { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue } catch { }
        }
    }
    if ($fixtureStarted) { try { [PackageFixture]::Close() } catch { } }
    if ($null -ne $app) { $app.Dispose() }
}
