param(
    [Parameter(Mandatory = $true)][ValidateSet('plain', 'rich')][string]$Target,
    [ValidateSet('normal', 'rebuild', 'readonly')][string]$Scenario = 'normal',
    [string]$ToolsRoot = '',
    [string]$Handshake = '',
    [string]$Profile = '',
    [string]$Chrome = 'C:\Program Files\Google\Chrome\Application\chrome.exe',
    [switch]$ExpectFallback,
    [switch]$KeepBrowser
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ListenWin32Focus {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint attach, uint attachTo, bool on);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
}
'@

$projectRoot = Split-Path $PSScriptRoot -Parent
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot locate the repository common directory' }
$repoRoot = Split-Path $commonGit -Parent
$artifactsRoot = Join-Path $repoRoot '.artifacts'
if (-not $Handshake) { $Handshake = Join-Path $artifactsRoot ('issue-4\run-' + $Target + '-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
$Handshake = [IO.Path]::GetFullPath($Handshake)
if (Test-Path $Handshake) { Remove-Item -Recurse -Force $Handshake }
New-Item -ItemType Directory -Force -Path $Handshake | Out-Null
if (-not $Profile) { $Profile = Join-Path $artifactsRoot 'issue-4-profile' }
$Profile = [IO.Path]::GetFullPath($Profile)
$fixture = Join-Path $projectRoot 'app\listen\tests\fixtures\generic-editors.html'
$url = ([Uri]$fixture).AbsoluteUri + '?target=' + $Target
$editLine = '采集中新增 Rust 123。'
$expectedTranscript = "第一行：Rust 和 WASAPI。`nSecond line, review before sending. 🦀"

function Get-FileText([string]$path) {
    return [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8)
}
function Write-JsonFile([string]$path, $value) {
    $json = $value | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText($path, $json, (New-Object Text.UTF8Encoding($false)))
}
function Normalize-Newlines([string]$value) {
    return ($value -replace "`r`n", "`n" -replace "`r", "`n")
}
function Get-RuntimeId($element) {
    return (($element.GetRuntimeId() | ForEach-Object { $_.ToString() }) -join '.')
}
function Get-FixtureWindows {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $classCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'Chrome_WidgetWin_1')
    $editCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
    $found = @()
    foreach ($candidate in $root.FindAll([System.Windows.Automation.TreeScope]::Children, $classCondition)) {
        if ($candidate.Current.Name -notlike 'Listen owned editor verification*') { continue }
        $pageEdits = @($candidate.FindAll([System.Windows.Automation.TreeScope]::Descendants, $editCondition) | Where-Object { $_.Current.ClassName -ne 'OmniboxViewViews' })
        if ($pageEdits.Count -ge 3) { $found += $candidate }
    }
    return $found
}
function Wait-FixtureWindow {
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        $windows = @(Get-FixtureWindows)
        if ($windows.Count -eq 1) { return $windows[0] }
        if ($windows.Count -gt 1) { throw "Expected one owned fixture window, found $($windows.Count)" }
        Start-Sleep -Milliseconds 500
    }
    throw 'The owned fixture window did not appear'
}
function Find-Edit($window, [string]$name) {
    $nameCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
    $typeCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
    $condition = New-Object System.Windows.Automation.AndCondition($nameCondition, $typeCondition)
    foreach ($element in $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
        if ($element.Current.ClassName -ne 'OmniboxViewViews') { return $element }
    }
    throw "Fixture edit element not found: $name"
}
function Find-Button($window, [string]$name) {
    $nameCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
    foreach ($element in $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $nameCondition)) {
        if ($element.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button) { return $element }
    }
    throw "Fixture button not found: $name"
}
function Invoke-Button($element) {
    $pattern = $null
    if (-not $element.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
        throw "Element [$($element.Current.Name)] has no InvokePattern"
    }
    $pattern.Invoke()
}
function Get-ValuePattern($element) {
    $pattern = $null
    if (-not $element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) {
        throw "Element [$($element.Current.Name)] has no ValuePattern"
    }
    return $pattern
}
function Get-OtherValue($element) {
    return (Get-ValuePattern $element).Current.Value
}
function Focus-OwnedElement($window, $element, $chromeProcess) {
    $hwnd = [IntPtr]$window.Current.NativeWindowHandle
    if ($hwnd -eq [IntPtr]::Zero) { throw 'The fixture window has no native handle' }
    $shell = New-Object -ComObject WScript.Shell
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        [ListenWin32Focus]::ShowWindow($hwnd, 9) | Out-Null
        [ListenWin32Focus]::SetForegroundWindow($hwnd) | Out-Null
        if ([ListenWin32Focus]::GetForegroundWindow() -ne $hwnd) {
            try { $shell.AppActivate($chromeProcess.Id) | Out-Null } catch { }
            [ListenWin32Focus]::SetForegroundWindow($hwnd) | Out-Null
        }
        Start-Sleep -Milliseconds 200
        $element.SetFocus()
        Start-Sleep -Milliseconds 350
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        if ($null -ne $focused -and (Get-RuntimeId $focused) -eq (Get-RuntimeId $element)) {
            return
        }
    }
    throw 'Could not move keyboard focus to the owned fixture target'
}
function Read-FixtureEvidence($window) {
    # The fixture's <output role="status"> element is exposed as a StatusBar without a
    # TextPattern, so read the document's text and extract the JSON object it renders.
    $documentCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Document)
    $document = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $documentCondition)
    if ($null -eq $document) { throw 'The fixture document element is missing' }
    $textPattern = $null
    if (-not $document.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$textPattern)) {
        throw 'The fixture document has no TextPattern'
    }
    $text = $textPattern.DocumentRange.GetText(-1)
    $match = [regex]::Match($text, '(?s)\{\s*"enter"\s*:.*\}')
    if (-not $match.Success) { throw "The fixture evidence is not JSON: $text" }
    return ($match.Value | ConvertFrom-Json)
}
function Wait-Handshake([string]$name, $testProcess, [int]$timeoutSeconds) {
    $path = Join-Path $Handshake $name
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while (-not (Test-Path $path)) {
        if ($testProcess.HasExited) {
            throw "The complete-flow test exited before writing '$name' (exit $($testProcess.ExitCode))"
        }
        if ((Get-Date) -gt $deadline) { throw "Timed out waiting for operator marker: $name" }
        Start-Sleep -Milliseconds 100
    }
}
function Start-OwnedProcess([string]$fileName, [string]$arguments) {
    # Start-Process rebuilds the environment as a case-insensitive dictionary and this
    # host currently exposes both HTTP_PROXY and http_proxy. ProcessStartInfo passes the
    # block through without that conversion.
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $fileName
    $info.Arguments = $arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $false
    return [System.Diagnostics.Process]::Start($info)
}
function Get-ProfileProcesses {
    return @(Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" -ErrorAction SilentlyContinue | Where-Object {
            $_.CommandLine -and $_.CommandLine.IndexOf($Profile, [StringComparison]::OrdinalIgnoreCase) -ge 0
        })
}
function Close-OwnedBrowser {
    foreach ($window in @(Get-FixtureWindows)) {
        $windowPattern = $null
        try {
            if ($window.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$windowPattern)) {
                $windowPattern.Close()
            }
        }
        catch { }
    }
    Start-Sleep -Seconds 2
    foreach ($chromeProcess in (Get-ProfileProcesses)) {
        try { Stop-Process -Id $chromeProcess.ProcessId -Force -ErrorAction SilentlyContinue } catch { }
    }
    Start-Sleep -Seconds 1
    for ($attempt = 0; $attempt -lt 20 -and (Test-Path $Profile); $attempt++) {
        try { Remove-Item -Recurse -Force -Path $Profile -ErrorAction Stop } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path $Profile) { throw "Could not delete the test profile: $Profile" }
}

$chromeProcess = $null
$testProcess = $null
$summary = $null
try {
    foreach ($stale in @(Get-ProfileProcesses)) {
        try { Stop-Process -Id $stale.ProcessId -Force -ErrorAction SilentlyContinue } catch { }
    }
    if (Test-Path $Profile) { Remove-Item -Recurse -Force -Path $Profile }
    $chromeProcess = Start-OwnedProcess $Chrome ('"--user-data-dir={0}" --no-first-run --no-default-browser-check --disable-session-crashed-bubble --new-window "{1}"' -f $Profile, $url)
    $window = Wait-FixtureWindow
    $targetName = if ($Target -eq 'plain') { 'Listen plain target' } else { '网页富文本编辑区' }
    $plain = Find-Edit $window 'Listen plain target'
    $rich = Find-Edit $window '网页富文本编辑区'
    $other = Find-Edit $window 'Listen non-target'
    $targetElement = if ($Target -eq 'plain') { $plain } else { $rich }
    $originalPlain = Get-OtherValue $plain
    $originalRich = Get-OtherValue $rich
    $originalOther = Get-OtherValue $other
    $originalTarget = Get-OtherValue $targetElement

    $expectFallback = $ExpectFallback -or ($Scenario -ne 'normal')
    $testScript = Join-Path $PSScriptRoot 'test-focused-editor.ps1'
    $testArguments = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -ToolsRoot "{1}" -Handshake "{2}"' -f $testScript, $ToolsRoot, $Handshake
    if ($expectFallback) { $testArguments += ' -ExpectFallback' }
    $testProcess = Start-OwnedProcess 'powershell.exe' $testArguments

    Wait-Handshake 'ready' $testProcess 300
    Focus-OwnedElement $window $targetElement $chromeProcess
    [IO.File]::WriteAllText((Join-Path $Handshake 'focused'), 'The operator moved keyboard focus to the selected fixture editor.', (New-Object Text.UTF8Encoding($false)))

    Wait-Handshake 'capturing' $testProcess 120
    if ($Scenario -eq 'normal') {
        $targetValue = Get-ValuePattern $targetElement
        $editedTarget = $targetValue.Current.Value + "`r`n" + $editLine
        $targetValue.SetValue($editedTarget)
        Start-Sleep -Milliseconds 500
        $editedReadback = $targetValue.Current.Value
        if ((Normalize-Newlines $editedReadback) -ne (Normalize-Newlines $editedTarget)) {
            throw "The capture-time edit was not accepted: [$editedReadback]"
        }
        $captureAction = 'The operator edited the draft during capture.'
    }
    else {
        if ($Scenario -eq 'rebuild') {
            Invoke-Button (Find-Button $window 'Rebuild selected target')
            Start-Sleep -Milliseconds 600
            $editedTarget = Get-OtherValue (Find-Edit $window $targetName)
            if ((Normalize-Newlines $editedTarget) -ne '重建的相似输入框') {
                throw "The fixture rebuild did not replace the target: [$editedTarget]"
            }
            $captureAction = 'The operator replaced the target with a same-named rebuilt control during capture.'
        }
        else {
            Invoke-Button (Find-Button $window 'Make selected target read-only')
            Start-Sleep -Milliseconds 500
            $readOnlyTarget = Find-Edit $window $targetName
            $readOnlyPattern = Get-ValuePattern $readOnlyTarget
            if (-not $readOnlyPattern.Current.IsReadOnly) {
                throw 'The fixture did not make the selected target read-only'
            }
            $editedTarget = $readOnlyPattern.Current.Value
            $captureAction = 'The operator made the selected target read-only during capture.'
        }
    }
    Focus-OwnedElement $window $other $chromeProcess
    $focusedOther = [System.Windows.Automation.AutomationElement]::FocusedElement
    if ((Get-RuntimeId $focusedOther) -ne (Get-RuntimeId $other)) { throw 'The adjacent input did not receive focus' }
    [IO.File]::WriteAllText((Join-Path $Handshake 'stop'), $captureAction + ' Focus is on the non-target input.', (New-Object Text.UTF8Encoding($false)))

    Wait-Handshake 'finished' $testProcess 120
    if (-not $testProcess.WaitForExit(30000)) { throw 'The complete-flow test did not exit after finishing' }
    if ($testProcess.ExitCode -ne 0) { throw "The complete-flow test failed with exit code $($testProcess.ExitCode)" }

    Start-Sleep -Milliseconds 500
    $delivery = Get-FileText (Join-Path $Handshake 'delivery.txt')
    $detail = Get-FileText (Join-Path $Handshake 'detail.txt')
    $transcript = Get-FileText (Join-Path $Handshake 'transcript.txt')
    $evidence = Read-FixtureEvidence $window
    # The fixture updates its rendered JSON only from input/focus events. Chromium's
    # contenteditable SetValue does not fire those events, so read live values instead.
    $livePlain = Get-OtherValue (Find-Edit $window 'Listen plain target')
    $liveRich = Get-OtherValue (Find-Edit $window '网页富文本编辑区')
    $finalOther = Get-OtherValue (Find-Edit $window 'Listen non-target')
    $focusedNow = [System.Windows.Automation.AutomationElement]::FocusedElement
    $finalPlain = $livePlain
    $finalRich = $liveRich
    $finalTarget = if ($Target -eq 'plain') { $finalPlain } else { $finalRich }
    $finalOtherTarget = if ($Target -eq 'plain') { $finalRich } else { $finalPlain }

    if ($transcript -ne $expectedTranscript) { throw "Transcript mismatch: [$transcript]" }
    if ([int]$evidence.enter -ne 0) { throw "The fixture observed Enter events: $($evidence.enter)" }
    if ([int]$evidence.submit -ne 0) { throw "The fixture observed Submit events: $($evidence.submit)" }
    if ($finalOther -ne $originalOther) { throw "The non-target input changed: [$finalOther]" }
    if ($null -eq $focusedNow -or (Get-RuntimeId $focusedNow) -ne (Get-RuntimeId $other)) {
        $activeName = if ($null -eq $focusedNow) { '<none>' } else { $focusedNow.Current.Name }
        throw "Focus was not left on the non-target input: [$activeName]"
    }

    $expectedDelivery = if ($expectFallback) { 'Some(Fallback)' } else { 'Some(Appended)' }
    if ($delivery -ne $expectedDelivery) { throw "Delivery mismatch: expected $expectedDelivery, observed $delivery. Detail: $detail" }
    if ($expectFallback) {
        if ($detail -notlike '*fallback text box*') { throw "Fallback detail did not explain the fallback result: $detail" }
        if ($Scenario -eq 'rebuild' -and $detail -notlike '*no longer available*' -and $detail -notlike '*rebuilt*') {
            throw "Fallback detail did not explain the rebuilt target: $detail"
        }
        if ($Scenario -eq 'readonly' -and $detail -notlike '*not an enabled writable editor*') {
            throw "Fallback detail did not explain the read-only target: $detail"
        }
        if ($Scenario -eq 'normal' -and $detail -notlike '*No supported editable target*') {
            throw "Fallback detail did not explain the unsupported target: $detail"
        }
        if ((Normalize-Newlines $finalTarget) -ne (Normalize-Newlines $editedTarget)) {
            throw "The unsupported target draft changed beyond the operator action: [$finalTarget]"
        }
    }
    else {
        if ($detail -notlike '*appended to the original target*') { throw "Appended detail is wrong: $detail" }
        $expectedTarget = (Normalize-Newlines ($editedTarget + "`r`n" + $transcript))
        if ((Normalize-Newlines $finalTarget) -ne $expectedTarget) {
            throw "The final target text is wrong. Expected [$expectedTarget], observed [$finalTarget]"
        }
        if ($Target -eq 'plain' -and (Normalize-Newlines $finalOtherTarget) -ne (Normalize-Newlines $originalRich)) {
            throw "The non-target rich editor changed"
        }
        if ($Target -eq 'rich' -and (Normalize-Newlines $finalOtherTarget) -ne (Normalize-Newlines $originalPlain)) {
            throw "The non-target plain editor changed"
        }
    }

    $bounds = $window.Current.BoundingRectangle
    if ($bounds.Width -gt 0 -and $bounds.Height -gt 0) {
        $bitmap = New-Object System.Drawing.Bitmap([int]$bounds.Width, [int]$bounds.Height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen([int]$bounds.X, [int]$bounds.Y, 0, 0, $bitmap.Size)
            $bitmap.Save((Join-Path $Handshake 'page.png'), [System.Drawing.Imaging.ImageFormat]::Png)
        }
        finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }

    $summary = [ordered]@{
        target = $Target
        scenario = $Scenario
        handshake = $Handshake
        profile = $Profile
        expected = if ($expectFallback) { 'Fallback' } else { 'Appended' }
        capture_action = $captureAction
        delivery = $delivery
        detail = $detail
        transcript = $transcript
        original_target = $originalTarget
        capture_time_edit = $editLine
        edited_target = $editedTarget
        final_target = $finalTarget
        final_plain = $finalPlain
        final_rich = $finalRich
        other = $finalOther
        other_unchanged = ($finalOther -eq $originalOther)
        active = $focusedNow.Current.Name
        focused_is_other = ((Get-RuntimeId $focusedNow) -eq (Get-RuntimeId $other))
        rendered_evidence_target = if ($Target -eq 'plain') { [string]$evidence.plain } else { [string]$evidence.rich }
        rendered_evidence_active = $evidence.active
        enter_count = [int]$evidence.enter
        submit_count = [int]$evidence.submit
        input_event_count = [int]$evidence.input
        retained = $null
    }
    Write-JsonFile (Join-Path $Handshake 'driver-evidence.json') $summary
    Write-Output ('RESULT target={0} delivery={1} enter={2} submit={3} input={4} active={5}' -f $Target, $delivery, $evidence.enter, $evidence.submit, $evidence.input, $focusedNow.Current.Name)
    Write-Output ('DETAIL ' + $detail)
    Write-Output ('FINAL_TARGET ' + ($finalTarget -replace "`r", '\r' -replace "`n", '\n'))
    Write-Output ('EVIDENCE ' + (Join-Path $Handshake 'driver-evidence.json'))
}
finally {
    if (-not $KeepBrowser) { Close-OwnedBrowser }
    if ($null -ne $testProcess -and -not $testProcess.HasExited) {
        try { Stop-Process -Id $testProcess.Id -Force -ErrorAction SilentlyContinue } catch { }
    }
}
