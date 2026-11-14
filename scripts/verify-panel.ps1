# Verify the expanded result panel (issue #20) of a running Listen build.
#
# The panel is the only place Listen shows text: clicking the capsule opens it, a
# second click / Esc / the close button closes it, and a fallback result opens it by
# itself. This script drives the shipped listen.exe on the real desktop.
#
# It reports two kinds of checks, separately:
#
#   ACCEPTANCE (control channel only) - issue #20's acceptance criterion. Every
#   assertion here uses only the public automation contract:
#     --control show|panel|copy|start|stop|retry|cleanup|quit
#     runtime\status.txt   phase, panel_visible, panel_error, retry_available, ...
#     runtime\panel.txt    the full panel text, updated while the panel is hidden
#     runtime\current-result.txt
#   Nothing in this group reads a window handle, class name or control ID.
#
#   The ONE exception is the removal check: the old detail window's class
#   (ListenCaptureToCopyWindowV1) must no longer exist. It is labelled REMOVAL
#   CHECK because it is a one-time deletion assertion, not an acceptance oracle.
#
#   UI SELF-CHECKS (not acceptance) - a smaller set of window-level diagnostics:
#   the panel's topmost/toolwindow style, position on the capsule monitor, that
#   opening never takes focus, it stays inside the work area, its two read-only
#   EDIT controls exactly match panel.txt, its buttons are enabled/disabled
#   correctly, capsule click / Esc / tray menu behaviour, and screenshots.
#   These use the private window class name and control IDs on purpose; #24 must
#   not depend on them. See docs/issue-20-verification.md.
#
# ASCII only on purpose: this file must survive any code page.
#
# Example:
#   pwsh -File scripts\verify-panel.ps1
#   pwsh -File scripts\verify-panel.ps1 -Exe <listen.exe> -Runtime <dir> -Evidence <dir>

param(
    [string]$Exe = '',
    [string]$Runtime = '',
    [string]$Evidence = '',
    [string]$AudioClip = '',
    [int]$TimeoutSeconds = 120
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
if (-not $Exe) { $Exe = Join-Path $artifacts 'cargo-target\release\listen.exe' }
if (-not $Runtime) { $Runtime = Join-Path $artifacts 'issue-20\runtime' }
if (-not $Evidence) { $Evidence = Join-Path $artifacts 'issue-20' }
if (-not $AudioClip) {
    $candidate = Join-Path $artifacts 'asr-quality-probe\zh-term.wav'
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $AudioClip = $candidate }
}
$Exe = [IO.Path]::GetFullPath($Exe)
$Runtime = [IO.Path]::GetFullPath($Runtime)
$Evidence = [IO.Path]::GetFullPath($Evidence)
$FailureRuntime = [IO.Path]::GetFullPath((Join-Path $Evidence 'runtime-failure'))
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "listen.exe is missing: $Exe" }
New-Item -ItemType Directory -Force -Path $Runtime, $Evidence, $FailureRuntime | Out-Null
$statusPath = Join-Path $Runtime 'status.txt'
$panelPath = Join-Path $Runtime 'panel.txt'
$resultPath = Join-Path $Runtime 'current-result.txt'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class PanelProbe
{
    public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lparam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lparam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint attach, uint attachTo, bool value);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hwnd, StringBuilder name, int length);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextLength(IntPtr hwnd);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr parent, int id);    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] public static extern bool GetMonitorInfoW(IntPtr monitor, ref MONITORINFO info);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageTimeoutW")] public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, UIntPtr wparam, StringBuilder lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageTimeoutW")] public static extern IntPtr SendMessageTimeoutPtr(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr menu);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetMenuStringW(IntPtr menu, uint item, StringBuilder text, int length, uint flags);
    [DllImport("user32.dll")] public static extern uint GetMenuState(IntPtr menu, uint item, uint flags);
    [DllImport("winmm.dll", CharSet = CharSet.Unicode)] public static extern bool PlaySound(string name, IntPtr module, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowExW(uint exStyle, string className, string title, uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct MONITORINFO { public uint cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; }

    public const long WS_EX_TOPMOST = 0x00000008;
    public const long WS_EX_TOOLWINDOW = 0x00000080;
    public const uint WM_APP = 0x8000;
    public const uint TRAY_MESSAGE = WM_APP + 11;
    public const uint WM_COMMAND = 0x0111;
    public const uint WM_KEYDOWN = 0x0100;
    public const uint WM_LBUTTONDOWN = 0x0201;
    public const uint WM_LBUTTONUP = 0x0202;
    public const uint WM_RBUTTONUP = 0x0205;
    public const uint WM_CANCELMODE = 0x001F;
    public const uint BM_CLICK = 0x00F5;
    public const uint WM_GETTEXT = 0x000D;
    public const uint MN_GETHMENU = 0x01E1;
    public const uint SMTO_ABORTIFHUNG = 0x0002;
    public const uint MONITOR_DEFAULTTONEAREST = 2;
    public const uint MF_BYPOSITION = 0x0400;
    public const uint MF_GRAYED = 0x0001;
    public const uint MF_SEPARATOR = 0x0800;
    public const int VK_ESCAPE = 0x1B;

    public static void UsePerMonitorDpi()
    {
        SetThreadDpiAwarenessContext((IntPtr)(-4));
    }

    public static IntPtr[] FindAll(uint pid, string className)
    {
        var found = new List<IntPtr>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr lparam)
        {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == pid && ClassName(hwnd) == className) found.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }

    public static IntPtr FindVisible(uint pid, string className)
    {
        foreach (IntPtr hwnd in FindAll(pid, className))
        {
            if (IsWindowVisible(hwnd)) return hwnd;
        }
        return IntPtr.Zero;
    }

    public static string ClassName(IntPtr hwnd)
    {
        var name = new StringBuilder(256);
        GetClassName(hwnd, name, name.Capacity);
        return name.ToString();
    }

    public static RECT Rect(IntPtr hwnd)
    {
        RECT rect;
        GetWindowRect(hwnd, out rect);
        return rect;
    }

    public static long ExStyle(IntPtr hwnd)
    {
        return GetWindowLongPtr(hwnd, -20).ToInt64();
    }

    public static uint Dpi(IntPtr hwnd)
    {
        return GetDpiForWindow(hwnd);
    }

    public static IntPtr Monitor(IntPtr hwnd)
    {
        return MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    }

    public static RECT WorkArea(IntPtr hwnd)
    {
        MONITORINFO info = new MONITORINFO();
        info.cbSize = (uint)Marshal.SizeOf(typeof(MONITORINFO));
        if (!GetMonitorInfoW(Monitor(hwnd), ref info)) throw new InvalidOperationException("Cannot read the monitor work area");
        return info.rcWork;
    }

    public static bool Post(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam)
    {
        return PostMessage(hwnd, message, wparam, lparam);
    }

    // GetWindowText cannot read another process's edit control; WM_GETTEXT can.
    public static string EditText(IntPtr hwnd)
    {
        var buffer = new StringBuilder(1 << 20);
        UIntPtr ignored;
        SendMessageTimeout(hwnd, WM_GETTEXT, (UIntPtr)buffer.Capacity, buffer, SMTO_ABORTIFHUNG, 4000, out ignored);
        return buffer.ToString();
    }

    public static ulong Click(IntPtr hwnd)
    {
        RECT rect = Rect(hwnd);
        int x = (rect.right - rect.left) / 2;
        int y = (rect.bottom - rect.top) / 2;
        IntPtr packed = (IntPtr)(((y & 0xffff) << 16) | (x & 0xffff));
        UIntPtr ignored;
        SendMessageTimeoutPtr(hwnd, WM_LBUTTONDOWN, (UIntPtr)1, packed, SMTO_ABORTIFHUNG, 2000, out ignored);
        SendMessageTimeoutPtr(hwnd, WM_LBUTTONUP, UIntPtr.Zero, packed, SMTO_ABORTIFHUNG, 2000, out ignored);
        return 0;
    }

    public static bool Focus(IntPtr hwnd)
    {
        if (GetForegroundWindow() == hwnd) return true;
        uint foregroundPid;
        uint foregroundThread = GetWindowThreadProcessId(GetForegroundWindow(), out foregroundPid);
        uint currentThread = GetCurrentThreadId();
        bool attached = AttachThreadInput(currentThread, foregroundThread, true);
        bool raised = SetForegroundWindow(hwnd);
        if (attached) AttachThreadInput(currentThread, foregroundThread, false);
        if (!raised) return false;
        System.Threading.Thread.Sleep(120);
        return GetForegroundWindow() == hwnd;
    }

    // A plain STATIC window with no editable control. Focusing it before a capture
    // guarantees the capture binds no target, so the fallback path is exercised
    // without appending the transcript to an unrelated window on a shared desktop.
    public static IntPtr CreateProbeWindow()
    {
        return CreateWindowExW(0x00000080u, "STATIC", "listen panel probe", 0x80000000u | 0x10000000u, 40, 40, 320, 160, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
    }

    public static void Play(string path)
    {
        PlaySound(path, IntPtr.Zero, 0x00020000 | 0x00000001 | 0x00000008 | 0x00000002);
    }

    public static void StopPlayback()
    {
        PlaySound(null, IntPtr.Zero, 0);
    }

    public static IntPtr MenuHandle(IntPtr menuWindow)
    {
        UIntPtr result;
        SendMessageTimeoutPtr(menuWindow, MN_GETHMENU, UIntPtr.Zero, IntPtr.Zero, SMTO_ABORTIFHUNG, 3000, out result);
        return (IntPtr)(long)result.ToUInt64();
    }

    public static string MenuText(IntPtr menu, int index)
    {
        var text = new StringBuilder(512);
        GetMenuStringW(menu, (uint)index, text, text.Capacity, MF_BYPOSITION);
        return text.ToString();
    }
}
'@

# Match the panel's DPI awareness so window messages keep their coordinates.
[PanelProbe]::UsePerMonitorDpi()

$failures = [Collections.Generic.List[string]]::new()
$checks = [Collections.Generic.List[object]]::new()
# `acceptance` checks use the control channel only; `ui` checks inspect the window
# and are reported separately so the acceptance claim never covers them.
$acceptanceCount = 0
$acceptancePassed = 0
$uiCount = 0
$uiPassed = 0
function Check([bool]$Condition, [string]$Message, [string]$Kind = 'acceptance') {
    $state = if ($Condition) { 'PASS' } else { 'FAIL' }
    $checks.Add([pscustomobject]@{ result = $state; kind = $Kind; check = $Message })
    Write-Output ("{0}: {1}" -f $state, $Message)
    if (-not $Condition) { $failures.Add($Message) }
    if ($Kind -eq 'ui') {
        $script:uiCount++
        if ($Condition) { $script:uiPassed++ }
    } else {
        $script:acceptanceCount++
        if ($Condition) { $script:acceptancePassed++ }
    }
}
function Removal-Check([bool]$Condition, [string]$Message) {
    Check $Condition ("REMOVAL CHECK: " + $Message) 'removal'
}

function ConvertTo-QuotedWindowsPath([string]$Path) {
    return '"' + ([IO.Path]::GetFullPath($Path) -replace '(\\+)$', '$1$1') + '"'
}

function Read-Status {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try { return [IO.File]::ReadAllText($statusPath) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw 'The status file stayed locked by Listen.'
}

function Read-Text([string]$Path) {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try { return [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) }
        catch [IO.IOException] { Start-Sleep -Milliseconds 25 }
    }
    throw "The text file stayed locked: $Path"
}

function Get-StatusValue([string]$Text, [string]$Key) {
    $match = [regex]::Match($Text, "(?m)^$([regex]::Escape($Key))=(.*)$")
    if ($match.Success) { return $match.Groups[1].Value.TrimEnd("`r") }
    return ''
}

function Wait-Status([string]$Pattern, [int]$Seconds = $TimeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    $last = ''
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $last = Read-Status
            if ($last -match $Pattern) { return $last }
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Timed out waiting for status /$Pattern/. Last status: $last"
}

function Wait-PanelVisibility([bool]$Visible, [int]$Seconds = 10) {
    $pattern = if ($Visible) { '(?m)^panel_visible=1\r?$' } else { '(?m)^panel_visible=0\r?$' }
    return Wait-Status $pattern $Seconds
}

# Same wait as Wait-Status, for the second (failure) runtime directory.
function Wait-StatusFile([string]$Path, [string]$Pattern, [int]$Seconds = $TimeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    $last = ''
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            $last = Read-Text $Path
            if ($last -match $Pattern) { return $last }
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Timed out waiting for $Path /$Pattern/. Last status: $last"
}

function Wait-Phase([Diagnostics.Process]$Process, [string]$Phase, [int]$Seconds = $TimeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    $last = ''
    while ((Get-Date) -lt $deadline) {
        if ($Process.HasExited) { throw "Listen exited while waiting for phase $Phase (exit $($Process.ExitCode))." }
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $last = Read-Status
            if ($last -match "(?m)^phase=$Phase`r?$") { return $last }
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Timed out waiting for phase $Phase. Last status: $last"
}

function Wait-Idle([Diagnostics.Process]$Process, [int]$Seconds = $TimeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    $last = ''
    while ((Get-Date) -lt $deadline) {
        if ($Process.HasExited) { throw "Listen exited while waiting for an idle phase (exit $($Process.ExitCode))." }
        if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
            $last = Read-Status
            if ($last -match "(?m)^phase=(Ready|Result|NoSpeech|Failure)`r?$") { return $last }
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Timed out waiting for an idle phase. Last status: $last"
}

function Start-Control([string]$Command) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Exe
    $info.Arguments = '--control ' + $Command
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardError = $true
    return [Diagnostics.Process]::Start($info)
}

function Complete-Control([Diagnostics.Process]$Process, [string]$Command) {
    try {
        if (-not $Process.WaitForExit(15000)) {
            try { $Process.Kill() } catch { }
            throw "Listen control '$Command' did not exit within 15 seconds."
        }
        $stderr = $Process.StandardError.ReadToEnd().Trim()
        if ($Process.ExitCode -ne 0) { throw "Listen control '$Command' failed ($($Process.ExitCode)): $stderr" }
        if ($stderr) { Write-Output $stderr }
    }
    finally { $Process.Dispose() }
}

function Invoke-Control([string]$Command) {
    Complete-Control (Start-Control $Command) $Command
}

function Start-Listen([string]$RunDirectory, [string[]]$Extra = @()) {
    $arguments = @('--runtime', (ConvertTo-QuotedWindowsPath $RunDirectory), '--hidden')
    $arguments += $Extra
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Exe
    $info.Arguments = ($arguments -join ' ')
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    return [Diagnostics.Process]::Start($info)
}

function Stop-Listen([Diagnostics.Process]$Process) {
    if ($null -eq $Process) { return }
    try {
        if (-not $Process.HasExited) {
            try { Invoke-Control 'quit' } catch { }
            if (-not $Process.WaitForExit(15000)) {
                try { $Process.Kill() } catch { }
                try { $Process.WaitForExit(5000) } catch { }
            }
        }
    }
    finally { $Process.Dispose() }
}

function Get-OurListenProcesses {
    $ours = @()
    foreach ($process in @(Get-Process -Name listen -ErrorAction SilentlyContinue)) {
        $path = ''
        try { $path = $process.Path } catch { }
        if ($path -eq $Exe) { $ours += $process }
    }
    return $ours
}

function Get-Panel([uint32]$ProcessId) {
    return [PanelProbe]::FindVisible($ProcessId, 'ListenResultPanelV1')
}

function Get-Capsule([uint32]$ProcessId) {
    return [PanelProbe]::FindVisible($ProcessId, 'ListenStatusCapsuleV1')
}

function Get-HostWindow([uint32]$ProcessId) {
    return [PanelProbe]::FindAll($ProcessId, 'ListenHostWindowV2') | Select-Object -First 1
}

function Wait-PanelVisible([uint32]$ProcessId, [bool]$Visible, [int]$Seconds = 10) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $panel = Get-Panel $ProcessId
        $isVisible = ($panel -ne [IntPtr]::Zero)
        if ($isVisible -eq $Visible) { return $panel }
        Start-Sleep -Milliseconds 25
    }
    return [IntPtr]::Zero
}

function Save-Shot([string]$Path, [int]$X, [int]$Y, [int]$Width, [int]$Height) {
    # A missing or too-small window must not crash the run; the matching check
    # below reports the missing screenshot instead.
    if ($Width -le 0 -or $Height -le 0) { return }
    $bitmap = [System.Drawing.Bitmap]::new($Width, $Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($X, $Y, 0, 0, [System.Drawing.Size]::new($Width, $Height)) }
    finally { $graphics.Dispose() }
    try { $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $bitmap.Dispose() }
}

function Save-PanelShot([IntPtr]$Panel, [string]$Path) {
    if ($Panel -eq [IntPtr]::Zero) { return }
    $rect = [PanelProbe]::Rect($Panel)
    Save-Shot $Path $rect.left $rect.top ($rect.right - $rect.left) ($rect.bottom - $rect.top)
}

# The panel's full text is the status control, a blank line and the result control.
# Returns $null when the panel window does not exist.
function Read-VisiblePanelText([IntPtr]$Panel) {
    if ($Panel -eq [IntPtr]::Zero) { return $null }
    $statusEdit = [PanelProbe]::GetDlgItem($Panel, 211)
    $resultEdit = [PanelProbe]::GetDlgItem($Panel, 212)
    if ($statusEdit -eq [IntPtr]::Zero -or $resultEdit -eq [IntPtr]::Zero) { return $null }
    $status = ([PanelProbe]::EditText($statusEdit)).Replace("`r`n", "`n")
    $result = ([PanelProbe]::EditText($resultEdit)).Replace("`r`n", "`n")
    return $status + "`n`n" + $result
}

function Get-ForegroundIsListen([uint32]$ProcessId) {
    $foreground = [PanelProbe]::GetForegroundWindow()
    if ($foreground -eq [IntPtr]::Zero) { return $false }
    $owner = [uint32]0
    [void][PanelProbe]::GetWindowThreadProcessId($foreground, [ref]$owner)
    return ($owner -eq $ProcessId)
}

$listen = $null
$probe = [IntPtr]::Zero
$summary = [ordered]@{
    checked_utc = [DateTime]::UtcNow.ToString('o')
    exe = $Exe
    runtime = $Runtime
    failure_runtime = $FailureRuntime
    audio_clip = $AudioClip
    mode = 'acceptance = control channel only; ui_self_checks = window diagnostics (not acceptance)'
    acceptance = @{}
    ui_self_checks = @{}
    removal = @{}
    screenshots = [Collections.Generic.List[string]]::new()
    checks = $checks
    result = 'FAIL'
}

try {
    $existing = @(Get-OurListenProcesses)
    if ($existing.Count -gt 0) { throw "This Listen build is already running; this check will not replace or close it ($Exe)." }
    Remove-Item -LiteralPath $statusPath, $panelPath, $resultPath -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path $FailureRuntime 'status.txt'), (Join-Path $FailureRuntime 'panel.txt'), (Join-Path $FailureRuntime 'current-result.txt') -ErrorAction SilentlyContinue

    # ===================== ACCEPTANCE: control channel only =================
    # --- Ready state: contract files and keys ------------------------------
    $listen = Start-Listen $Runtime
    $status = Wait-Phase $listen 'Ready'
    $pidListen = [uint32]$listen.Id
    Check (Test-Path -LiteralPath $panelPath -PathType Leaf) 'runtime\panel.txt exists before the panel is ever opened'
    Check ($null -ne (Read-Text $panelPath)) 'runtime\panel.txt is readable as UTF-8'
    Check ((Get-StatusValue $status 'panel_visible') -eq '0') "status.txt reports panel_visible=0 initially ($(Get-StatusValue $status 'panel_visible'))"
    Check ((Get-StatusValue $status 'panel_error') -eq '') 'status.txt reports no panel_error'
    $requiredKeys = @('phase', 'detail', 'endpoint_role', 'endpoint_id', 'endpoint_name', 'current_result', 'retained_audio', 'retry_available', 'backend', 'preparation', 'refine', 'refine_reason', 'notice', 'capsule', 'panel_visible', 'panel_error', 'startup_capsule_ms', 'capsule_capture_excluded', 'timing_capture_start_ms', 'timing_preparation_ms', 'timing_stop_to_result_ms', 'timing_finalize_ms', 'timing_recognition_ms', 'timing_last_window_wait_ms', 'timing_last_window_inference_ms', 'timing_save_ms', 'timing_delivery_ms', 'timing_recognition_windows', 'process_id')
    $missingKeys = @($requiredKeys | Where-Object { $status -notmatch ("(?m)^" + [regex]::Escape($_) + "=") })
    Check ($missingKeys.Count -eq 0) ("status.txt keeps every previous key and adds panel_visible (missing: " + ($missingKeys -join ', ') + ')')
    $panelText = Read-Text $panelPath
    Check ($panelText.Contains((Get-StatusValue $status 'detail'))) 'panel.txt contains the human-readable detail from status.txt'
    Check ($panelText.Length -gt (Get-StatusValue $status 'detail').Length) 'panel.txt has a result section after the status detail'

    # REMOVAL CHECK - the only legacy-window assertion in this script. It proves
    # issue #20 deleted the old detail window; it is not an acceptance oracle and
    # nothing in the acceptance group depends on window handles.
    $legacy = @([PanelProbe]::FindAll($pidListen, 'ListenCaptureToCopyWindowV1'))
    Removal-Check ($legacy.Count -eq 0) 'the old ListenCaptureToCopyWindowV1 detail window no longer exists'

    # --- --control show reports the panel as visible ----------------------
    Invoke-Control 'show'
    $status = Wait-PanelVisibility $true
    Check ((Get-StatusValue $status 'panel_visible') -eq '1') '--control show reports panel_visible=1 in status.txt'

    # ============== UI SELF-CHECKS (not acceptance; see header) =============
    # Window-level diagnostics. They read the private class/control IDs on purpose
    # and are excluded from the acceptance claim above.
    $capsule = Get-Capsule $pidListen
    Check ($capsule -ne [IntPtr]::Zero) 'the status capsule exists' 'ui'
    $capsuleBefore = [PanelProbe]::Rect($capsule)
    $panel = Wait-PanelVisible $pidListen $true
    Check ($panel -ne [IntPtr]::Zero) '--control show makes the panel window visible' 'ui'
    $panelRect = [PanelProbe]::Rect($panel)
    $panelStyle = [PanelProbe]::ExStyle($panel)
    $capsuleAfter = [PanelProbe]::Rect($capsule)
    $work = [PanelProbe]::WorkArea($panel)
    $sameMonitor = ([PanelProbe]::Monitor($panel) -eq [PanelProbe]::Monitor($capsule))
    Check ((($panelStyle -band [PanelProbe]::WS_EX_TOOLWINDOW) -ne 0)) 'the panel has WS_EX_TOOLWINDOW (no taskbar or Alt+Tab entry)' 'ui'
    Check ((($panelStyle -band [PanelProbe]::WS_EX_TOPMOST) -ne 0)) 'the panel has WS_EX_TOPMOST' 'ui'
    Check $sameMonitor 'the panel opens on the capsule monitor' 'ui'
    Check (($panelRect.left -ge $work.left) -and ($panelRect.top -ge $work.top) -and ($panelRect.right -le $work.right) -and ($panelRect.bottom -le $work.bottom)) "the panel is fully inside the monitor work area ($($panelRect.left),$($panelRect.top) $($panelRect.right)x$($panelRect.bottom), work $($work.left),$($work.top) $($work.right)x$($work.bottom))" 'ui'
    Check (([PanelProbe]::Dpi($panel)) -eq ([PanelProbe]::Dpi($capsule))) "the panel uses the capsule DPI ($([PanelProbe]::Dpi($panel)) vs $([PanelProbe]::Dpi($capsule)))" 'ui'
    Check (($capsuleBefore.left -eq $capsuleAfter.left) -and ($capsuleBefore.top -eq $capsuleAfter.top)) 'opening the panel does not move the capsule' 'ui'
    # A shared desktop can change the foreground window for unrelated reasons; each
    # attempt is short and the checks are retried. Taking focus is never retried
    # because it would always be a real failure.
    $foregroundStable = $false
    $tookFocus = $false
    $foregroundAttempts = [Collections.Generic.List[string]]::new()
    for ($attempt = 0; $attempt -lt 6 -and -not $foregroundStable -and -not $tookFocus; $attempt++) {
        $foregroundBefore = [PanelProbe]::GetForegroundWindow()
        $beforeIsListen = Get-ForegroundIsListen $pidListen
        Invoke-Control 'show'
        Start-Sleep -Milliseconds 150
        $foregroundAfter = [PanelProbe]::GetForegroundWindow()
        $afterIsListen = Get-ForegroundIsListen $pidListen
        if (-not $beforeIsListen -and $afterIsListen) {
            # Only a transition to Listen counts: the shared desktop may already
            # have had a Listen window in front for unrelated reasons.
            $tookFocus = $true
            $foregroundAttempts.Add(("attempt {0}: Listen became foreground ({1})" -f $attempt, $foregroundAfter))
        }
        elseif ($foregroundBefore -eq $foregroundAfter) {
            $foregroundStable = $true
            $foregroundAttempts.Add(("attempt {0}: unchanged ({1})" -f $attempt, $foregroundAfter))
        }
        else {
            $foregroundAttempts.Add(("attempt {0}: {1} -> {2}" -f $attempt, $foregroundBefore, $foregroundAfter))
        }
    }
    $summary.ui_self_checks.foreground = [ordered]@{
        stable = $foregroundStable
        took_focus = $tookFocus
        attempts = @($foregroundAttempts.ToArray())
    }
    Check (-not $tookFocus) 'opening the panel never makes Listen the foreground window' 'ui'
    Check $foregroundStable 'opening the panel leaves the foreground window unchanged' 'ui'
    $visibleText = Read-VisiblePanelText $panel
    Check ($visibleText -ceq (Read-Text $panelPath)) 'panel.txt is exactly the text shown by the two read-only controls' 'ui'
    $readyShot = Join-Path $Evidence 'panel-ready.png'
    Save-PanelShot $panel $readyShot
    $summary.screenshots.Add($readyShot)
    Check (Test-Path -LiteralPath $readyShot) "saved the ready-state panel screenshot: $readyShot" 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($panel, 201))) -eq $false) 'the copy button is disabled while there is no result' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($panel, 204))) -eq $false) 'the refine placeholder is disabled' 'ui'
    $refineLabel = [PanelProbe]::EditText([PanelProbe]::GetDlgItem($panel, 204))
    Check ($refineLabel.Length -gt 0) 'the refine placeholder is visible and labelled' 'ui'

    # ===================== ACCEPTANCE: control channel only =================
    # --- --control copy with no result must not touch the clipboard -------
    Set-Clipboard -Value 'listen-panel-no-result-sentinel'
    Invoke-Control 'copy'
    Start-Sleep -Milliseconds 250
    Check ((Get-Clipboard -Raw) -eq 'listen-panel-no-result-sentinel') '--control copy with no result leaves the clipboard untouched'

    # --- toggle through --control panel -----------------------------------
    Invoke-Control 'panel'
    $status = Wait-PanelVisibility $false
    Check ((Get-StatusValue $status 'panel_visible') -eq '0') '--control panel reports panel_visible=0'
    Invoke-Control 'panel'
    $status = Wait-PanelVisibility $true
    Check ((Get-StatusValue $status 'panel_visible') -eq '1') 'a second --control panel reports panel_visible=1 again'

    # ============== UI SELF-CHECKS (not acceptance; see header) =============
    # --- capsule click, Esc and the tray: window-level behaviour ----------
    [void][PanelProbe]::Click($capsule)
    Check ((Wait-PanelVisible $pidListen $false) -eq [IntPtr]::Zero) 'clicking the capsule closes the panel' 'ui'
    [void][PanelProbe]::Click($capsule)
    Check ((Wait-PanelVisible $pidListen $true) -ne [IntPtr]::Zero) 'clicking the capsule again opens the panel' 'ui'

    # --- Esc closes -------------------------------------------------------
    [void][PanelProbe]::Post($panel, [PanelProbe]::WM_KEYDOWN, [UIntPtr]::new([PanelProbe]::VK_ESCAPE), [IntPtr]::Zero)
    Check ((Wait-PanelVisible $pidListen $false) -eq [IntPtr]::Zero) 'Esc closes the panel' 'ui'

    # --- tray left click opens -------------------------------------------
    $hostWindow = Get-HostWindow $pidListen
    Check ($hostWindow -ne [IntPtr]::Zero) 'the hidden window that owns the tray icon exists' 'ui'
    [void][PanelProbe]::Post($hostWindow, [PanelProbe]::TRAY_MESSAGE, [UIntPtr]::new(1), [IntPtr][PanelProbe]::WM_LBUTTONUP)
    Check ((Wait-PanelVisible $pidListen $true) -ne [IntPtr]::Zero) 'a tray left click opens the panel' 'ui'

    # --- tray menu contains the open-panel item and opens the panel -------
    [void][PanelProbe]::Post($hostWindow, [PanelProbe]::TRAY_MESSAGE, [UIntPtr]::new(1), [IntPtr][PanelProbe]::WM_RBUTTONUP)
    $menuWindow = [IntPtr]::Zero
    for ($attempt = 0; $attempt -lt 100 -and $menuWindow -eq [IntPtr]::Zero; $attempt++) {
        Start-Sleep -Milliseconds 30
        $menuWindow = [PanelProbe]::FindVisible($pidListen, '#32768')
    }
    Check ($menuWindow -ne [IntPtr]::Zero) 'the tray context menu opens' 'ui'
    if ($menuWindow -ne [IntPtr]::Zero) {
        $menuShot = Join-Path $Evidence 'tray-menu.png'
        Save-PanelShot $menuWindow $menuShot
        $summary.screenshots.Add($menuShot)
        $menu = [PanelProbe]::MenuHandle($menuWindow)
        $openPanelLabel = -join ([char[]]@(0x6253, 0x5F00, 0x7ED3, 0x679C, 0x9762, 0x677F))
        Check ($menu -ne [IntPtr]::Zero) 'the tray menu handle can be inspected' 'ui'
        Check (([PanelProbe]::GetMenuItemCount($menu)) -eq 6) "the tray menu keeps start/stop, retry, cleanup, exit and adds the open-panel item ($([PanelProbe]::GetMenuItemCount($menu)) items)" 'ui'
        Check (([PanelProbe]::MenuText($menu, 0)) -eq $openPanelLabel) "the first tray item is the open-panel label" 'ui'
        Check ((([PanelProbe]::GetMenuState($menu, 4, [PanelProbe]::MF_BYPOSITION)) -band [PanelProbe]::MF_SEPARATOR) -ne 0) 'the tray menu keeps its separator before exit' 'ui'
        $retryGrayed = ((([PanelProbe]::GetMenuState($menu, 2, [PanelProbe]::MF_BYPOSITION)) -band [PanelProbe]::MF_GRAYED) -ne 0)
        $cleanupGrayed = ((([PanelProbe]::GetMenuState($menu, 3, [PanelProbe]::MF_BYPOSITION)) -band [PanelProbe]::MF_GRAYED) -ne 0)
        $retryAvailable = (Get-StatusValue $status 'retry_available') -eq 'True'
        Check (($retryGrayed -eq (-not $retryAvailable)) -and ($cleanupGrayed -eq (-not $retryAvailable))) 'retry and cleanup are grayed out exactly matching status.txt retry_available' 'ui'
        [void][PanelProbe]::Post($hostWindow, [PanelProbe]::WM_CANCELMODE, [UIntPtr]::Zero, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        Check (([PanelProbe]::FindVisible($pidListen, '#32768')) -eq [IntPtr]::Zero) 'the tray menu closes without selecting an item' 'ui'
    }
    # Selecting the open-panel item posts WM_COMMAND with its menu id (ID_SHOW = 104).
    [void][PanelProbe]::Post($hostWindow, [PanelProbe]::WM_COMMAND, [UIntPtr]::new(104), [IntPtr]::Zero)
    Check ((Wait-PanelVisible $pidListen $true) -ne [IntPtr]::Zero) 'choosing the tray open-panel item opens the panel' 'ui'

    # ===================== ACCEPTANCE: control channel only =================
    # --- capturing: live progress in panel.txt ----------------------------
    Invoke-Control 'panel'
    $status = Wait-PanelVisibility $false
    Check ((Get-StatusValue $status 'panel_visible') -eq '0') '--control panel reports panel_visible=0 before the capture checks'
    # Focus a plain non-editable probe window. This is setup for the no-target
    # fallback, never an assertion of the acceptance group: capture binds no
    # writable target, so the transcript cannot land in an unrelated window.
    $probe = [PanelProbe]::CreateProbeWindow()
    Check (([PanelProbe]::Focus($probe)) -eq $true) 'the acceptance probe window takes the foreground before capture' 'ui'
    $hasClip = ($AudioClip -and (Test-Path -LiteralPath $AudioClip -PathType Leaf))
    # The shared desktop's audio route can change between runs, so a capture that
    # receives the clip but reports NoSpeech is retried (up to 3 attempts). Every
    # attempt asserts the same public contract; only the first one also checks live
    # progress and takes the capture screenshot.
    for ($captureAttempt = 1; $captureAttempt -le 3; $captureAttempt++) {
        [void][PanelProbe]::Focus($probe)
        Invoke-Control 'start'
        $status = Wait-Phase $listen 'Capturing'
        if ($captureAttempt -eq 1) {
            # #15's "按下快捷键 <1 s 可开始采集": the application's
            # dispatch-to-Capturing measurement, published in status.txt.
            $captureStartMs = Get-StatusValue $status 'timing_capture_start_ms'
            Check (($captureStartMs -ne '') -and ([int]$captureStartMs -ge 0) -and ([int]$captureStartMs -lt 1000)) "capture start latency is measured and under the 1 s target (timing_capture_start_ms=$captureStartMs)"
            # Play the probe clip while the panel is open: the live received duration
            # then proves the capture is moving, not just that a line exists.
            if ($hasClip) { [PanelProbe]::Play($AudioClip) }
            Invoke-Control 'show'
            $status = Wait-PanelVisibility $true
            $panel = Wait-PanelVisible $pidListen $true
            Start-Sleep -Milliseconds 1500
            $panelText = Read-Text $panelPath
            Check ($panelText.Contains('Capturing the')) 'while capturing, panel.txt carries the capture detail'
            Check ($panelText.Contains('s received;')) 'while capturing, panel.txt carries live progress'
            $received = [regex]::Match($panelText, '([0-9]+\.[0-9]) s received').Groups[1].Value
            Check (($received -ne '') -and ([double]$received -gt 0.5)) "while capturing, panel.txt reports a live received duration ($received s)"
            $status = Read-Status
            Check ((Get-StatusValue $status 'capsule') -eq 'Capturing') 'status.txt reports capsule=Capturing while capturing'
            $capturingShot = Join-Path $Evidence 'panel-capturing.png'
            Save-PanelShot $panel $capturingShot
            $summary.screenshots.Add($capturingShot)
            # Closing the panel before the result proves the fallback opens it by itself.
            Invoke-Control 'panel'
            $status = Wait-PanelVisibility $false
            Check ((Get-StatusValue $status 'panel_visible') -eq '0') 'the panel is closed again before the result arrives'
            if ($hasClip) { Start-Sleep -Seconds 5 }
        }
        elseif ($hasClip) {
            [PanelProbe]::Play($AudioClip)
            Start-Sleep -Seconds 5
        }
        if ($hasClip) { [PanelProbe]::StopPlayback() }
        # Start the stop control without waiting for that short-lived process to
        # exit: local recognition is now fast enough that waiting first can miss the
        # Processing state entirely. The assertions still read only status.txt and
        # panel.txt.
        $stopControl = Start-Control 'stop'
        $sawProcessing = $false
        $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
        while ((Get-Date) -lt $deadline) {
            $status = Read-Status
            if ($status -match '(?m)^phase=Processing') {
                $sawProcessing = $true
                if ($captureAttempt -eq 1) {
                    $processingText = Read-Text $panelPath
                    Check (
                        $processingText.Contains('Processing locally') -or
                        $processingText.StartsWith((-join [char[]](0x6b63, 0x5728, 0x672c, 0x673a, 0x8bc6, 0x522b)))
                    ) 'while processing, panel.txt says recognition is local'
                }
                break
            }
            if ($status -match '(?m)^phase=(Result|NoSpeech|Failure)') { break }
            Start-Sleep -Milliseconds 2
        }
        Complete-Control $stopControl 'stop'
        if ($captureAttempt -eq 1) {
            Check $sawProcessing 'the processing state was observable in panel.txt'
        }
        $status = Wait-Idle $listen
        $phase = Get-StatusValue $status 'phase'
        if ($phase -eq 'Result') { break }
        if ($phase -ne 'NoSpeech') { break }
        if ($captureAttempt -lt 3) {
            Write-Output "NOTE: the audio clip produced NoSpeech (shared-desktop audio route); retrying the fallback capture (attempt $captureAttempt of 3 completed)"
        }
    }
    $summary.acceptance.fallback = [ordered]@{
        phase = Get-StatusValue $status 'phase'
        detail = Get-StatusValue $status 'detail'
        panel_visible = Get-StatusValue $status 'panel_visible'
        audio_clip = $AudioClip
    }
    Check ($summary.acceptance.fallback.phase -eq 'Result') "audio playback produced a result ($($summary.acceptance.fallback.phase))"
    Check ($summary.acceptance.fallback.detail -like '*expanded result panel*') 'the result was not appended and names the result panel as the copy source'
    Check ($summary.acceptance.fallback.panel_visible -eq '1') 'status.txt reports panel_visible=1 for a fallback result (auto-open)'
    Check (-not (Get-ForegroundIsListen $pidListen)) 'auto-opening the panel does not take focus' 'ui'
    $result = if (Test-Path -LiteralPath $resultPath -PathType Leaf) { Read-Text $resultPath } else { '' }
    $summary.acceptance.fallback.result = $result
    Check ($result.Length -gt 0) "the complete result was saved (current-result.txt, $($result.Length) chars)"
    $panelText = Read-Text $panelPath
    Check (($result.Length -gt 0) -and $panelText.Contains($result)) 'panel.txt contains the complete result'
    Check ($panelText.Contains((Get-StatusValue $status 'detail'))) 'panel.txt contains the fallback explanation from status.txt'
    $panel = Get-Panel $pidListen
    Check ((Read-VisiblePanelText $panel) -ceq $panelText) 'panel.txt is exactly the visible text after a fallback result' 'ui'
    $fallbackShot = Join-Path $Evidence 'panel-fallback.png'
    Save-PanelShot $panel $fallbackShot
    $summary.screenshots.Add($fallbackShot)
    $resultEdit = [PanelProbe]::GetDlgItem($panel, 212)
    Check (([PanelProbe]::EditText($resultEdit)).Replace("`r`n", "`n") -ceq $result) 'the panel shows the complete result in its read-only control' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($panel, 201))) -eq $true) 'the copy button is enabled once a result exists' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($panel, 202))) -eq $false) 'the retry button stays disabled when no failed audio is retained' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($panel, 203))) -eq $false) 'the cleanup button stays disabled when no failed audio is retained' 'ui'

    # --- copy through --control and through the panel button --------------
    Set-Clipboard -Value 'listen-panel-copy-sentinel'
    Invoke-Control 'copy'
    Start-Sleep -Milliseconds 300
    Check ((Get-Clipboard -Raw) -ceq $result) '--control copy puts the complete result on the clipboard'
    Set-Clipboard -Value 'listen-panel-button-sentinel'
    [void][PanelProbe]::SendMessage([PanelProbe]::GetDlgItem($panel, 201), [PanelProbe]::BM_CLICK, [UIntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 300
    Check ((Get-Clipboard -Raw) -ceq $result) 'the panel copy button copies the complete result' 'ui'

    # --- no speech: nothing delivered, previous result unchanged ----------
    Invoke-Control 'panel'
    $status = Wait-PanelVisibility $false
    Check ((Get-StatusValue $status 'panel_visible') -eq '0') '--control panel reports panel_visible=0 before the no-speech capture'
    [void][PanelProbe]::Focus($probe)
    $beforeNoSpeech = if (Test-Path -LiteralPath $resultPath -PathType Leaf) { Read-Text $resultPath } else { '' }
    Invoke-Control 'start'
    [void](Wait-Phase $listen 'Capturing')
    Start-Sleep -Seconds 2
    Invoke-Control 'stop'
    $status = Wait-Idle $listen
    $afterNoSpeech = if (Test-Path -LiteralPath $resultPath -PathType Leaf) { Read-Text $resultPath } else { '' }
    $summary.acceptance.no_speech = [ordered]@{
        phase = Get-StatusValue $status 'phase'
        panel_visible = Get-StatusValue $status 'panel_visible'
        result_unchanged = ($afterNoSpeech -ceq $beforeNoSpeech)
    }
    Check ($summary.acceptance.no_speech.phase -eq 'NoSpeech') "silence produces NoSpeech ($($summary.acceptance.no_speech.phase))"
    Check $summary.acceptance.no_speech.result_unchanged 'NoSpeech leaves current-result.txt unchanged'
    Check ($summary.acceptance.no_speech.panel_visible -eq '0') 'NoSpeech does not auto-open the panel'
    $panelText = Read-Text $panelPath
    Check ($panelText.Contains('No valid speech detected')) 'panel.txt explains that nothing was delivered for NoSpeech'
    Check (($beforeNoSpeech.Length -eq 0) -or $panelText.Contains($beforeNoSpeech)) 'the previous complete result stays visible in panel.txt after NoSpeech'

    # --- quit cleanly ------------------------------------------------------
    Invoke-Control 'quit'
    if (-not $listen.WaitForExit(15000)) { throw 'Listen did not exit after --control quit.' }
    Check ($listen.ExitCode -eq 0) 'the application exits cleanly through --control quit'
    $listen.Dispose()
    $listen = $null
    Check (-not @(Get-OurListenProcesses)) 'no Listen process is left running'

    # --- a failure is readable after the user opens the panel -------------
    # Issue #20 story 10: a failure turns the capsule red and the user clicks to
    # read the reason. The panel must NOT auto-open on Failure.
    $brokenModel = Join-Path $FailureRuntime 'missing-model.onnx'
    Remove-Item -LiteralPath $brokenModel -ErrorAction SilentlyContinue
    $listen = Start-Listen $FailureRuntime @('--sensevoice-model', (ConvertTo-QuotedWindowsPath $brokenModel))
    $failureStatusPath = Join-Path $FailureRuntime 'status.txt'
    $failurePanelPath = Join-Path $FailureRuntime 'panel.txt'
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $status = ''
    while ((Get-Date) -lt $deadline) {
        if ($listen.HasExited) { throw "Listen exited while waiting for failure (exit $($listen.ExitCode))." }
        if (Test-Path -LiteralPath $failureStatusPath -PathType Leaf) {
            $status = [IO.File]::ReadAllText($failureStatusPath)
            if ($status -match '(?m)^phase=Failure\r?$') { break }
        }
        Start-Sleep -Milliseconds 25
    }
    $pidFailure = [uint32]$listen.Id
    $failureDetail = Get-StatusValue $status 'detail'
    $summary.acceptance.failure = [ordered]@{
        phase = Get-StatusValue $status 'phase'
        detail = $failureDetail
        panel_visible = Get-StatusValue $status 'panel_visible'
        preparation = Get-StatusValue $status 'preparation'
    }
    Check ($summary.acceptance.failure.phase -eq 'Failure') "a broken model produces Failure ($($summary.acceptance.failure.phase))"
    Check ($summary.acceptance.failure.preparation -eq 'Failed') 'status.txt reports preparation=Failed'
    Check ($summary.acceptance.failure.panel_visible -eq '0') 'a failure does not auto-open the panel (the user clicks the capsule)'
    Invoke-Control 'show'
    $status = Wait-StatusFile $failureStatusPath '(?m)^panel_visible=1\r?$'
    Check ((Get-StatusValue $status 'panel_visible') -eq '1') '--control show opens the panel on a failure'
    $failureText = [IO.File]::ReadAllText($failurePanelPath, [Text.Encoding]::UTF8)
    Check ($failureText.Contains($failureDetail)) 'panel.txt shows the complete failure reason from status.txt after the user opens the panel'

    # --- UI SELF-CHECKS: the opened failure panel --------------------------
    $failurePanel = Get-Panel $pidFailure
    Check ($failurePanel -ne [IntPtr]::Zero) 'the failure panel window is visible after --control show' 'ui'
    Check ((Read-VisiblePanelText $failurePanel) -ceq $failureText) 'panel.txt is exactly the visible failure text' 'ui'
    $failureShot = Join-Path $Evidence 'panel-failure.png'
    Save-PanelShot $failurePanel $failureShot
    $summary.screenshots.Add($failureShot)
    Check (Test-Path -LiteralPath $failureShot) 'saved the failure-state panel screenshot' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($failurePanel, 201))) -eq $false) 'the copy button is disabled when a fresh failure has no result' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($failurePanel, 202))) -eq $false) 'the retry button is disabled when no audio was retained' 'ui'
    Check (([PanelProbe]::IsWindowEnabled([PanelProbe]::GetDlgItem($failurePanel, 203))) -eq $false) 'the cleanup button is disabled when no audio was retained' 'ui'

    Invoke-Control 'quit'
    if (-not $listen.WaitForExit(15000)) { throw 'Listen did not exit after --control quit.' }
    Check ($listen.ExitCode -eq 0) 'the failure run exits cleanly through --control quit'
    $listen.Dispose()
    $listen = $null
    Check (-not @(Get-OurListenProcesses)) 'no Listen process is left running after the failure run'
}
catch {
    $failures.Add($_.Exception.Message)
    throw
}
finally {
    try { if ($probe -ne [IntPtr]::Zero) { [void][PanelProbe]::DestroyWindow($probe) } } catch { }
    if ($null -ne $listen) { Stop-Listen $listen }
    $summary.result = if ($failures.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $summary.acceptance_checks = "$acceptancePassed/$acceptanceCount passed"
    $summary.ui_self_check_count = "$uiPassed/$uiCount passed"
    $summary.failures = @($failures.ToArray())
    $summaryPath = Join-Path $Evidence 'panel-verification.json'
    [IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    Write-Output ''
    Write-Output ("Result: {0}" -f $summary.result)
    Write-Output ("  acceptance (control channel only): {0} of {1} passed" -f $acceptancePassed, $acceptanceCount)
    Write-Output ("  UI self-checks (not acceptance):   {0} of {1} passed" -f $uiPassed, $uiCount)
    Write-Output ("  failures: {0}" -f $failures.Count)
    Write-Output ("Evidence: {0}" -f $summaryPath)
}
if ($failures.Count -gt 0) { exit 1 }
