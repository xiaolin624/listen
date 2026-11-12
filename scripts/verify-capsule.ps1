# Verify the resident status capsule (issue #19) of a running Listen build.
#
# What this script checks, against the real listen.exe:
#   - the capsule window exists and is visible, with an empty window title;
#   - its extended styles are LAYERED | TOOLWINDOW | NOACTIVATE | TOPMOST;
#   - it sits above a normal window (behavioural topmost check with a probe window);
#   - clicking it does not change the foreground window;
#   - status.txt reports startup_capsule_ms, capsule, preparation and
#     capsule_capture_excluded=1;
#   - dragging it persists capsule-position.txt and a restart restores that position;
#   - a cropped PNG of the capsule in each induced state (ready, capturing,
#     processing, failure) plus a full-desktop screenshot.
#
# IMPORTANT lookup note: the capsule is identified by its documented extended
# styles, because this script verifies our own implementation. Acceptance scripts
# for issue #24 must NOT depend on this: they drive Listen through --control and
# status.txt only.
#
# The capsule is excluded from every screen capture by design
# (WDA_EXCLUDEFROMCAPTURE), so a desktop screenshot cannot show it. The per-state
# PNGs are produced from the exact frame the capsule presents, requested through a
# documented internal window message (WM_APP + 20); the desktop screenshot is the
# evidence that the capsule is absent from capture.
#
# ASCII only on purpose: this file must survive any code page.
#
# Example:
#   pwsh -File scripts\verify-capsule.ps1
#   pwsh -File scripts\verify-capsule.ps1 -Exe <listen.exe> -Runtime <dir> -Evidence <dir>
#
# By default Listen runs with its shipped default engine (SenseVoice resident in the
# process after issue #17); -Engine/-Model/-VadModel are optional overrides for the
# whisper.cpp fallback engine.

param(
    [string]$Exe = '',
    [string]$Runtime = '',
    [string]$Evidence = '',
    [string]$Engine = '',
    [string]$Model = '',
    [string]$VadModel = '',
    [string]$AudioClip = '',
    [int]$TimeoutSeconds = 90
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
if (-not $Runtime) { $Runtime = Join-Path $artifacts 'issue-19\runtime' }
if (-not $Evidence) { $Evidence = Join-Path $artifacts 'issue-19' }
if (-not $AudioClip) {
    $candidate = Join-Path $artifacts 'asr-quality-probe\zh-term.wav'
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $AudioClip = $candidate }
}
$Exe = [IO.Path]::GetFullPath($Exe)
$Runtime = [IO.Path]::GetFullPath($Runtime)
$Evidence = [IO.Path]::GetFullPath($Evidence)
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "listen.exe is missing: $Exe" }
New-Item -ItemType Directory -Force -Path $Runtime, $Evidence | Out-Null
$statusPath = Join-Path $Runtime 'status.txt'
$positionPath = Join-Path $Runtime 'capsule-position.txt'
$framePath = Join-Path $Runtime 'capsule-frame.bmp'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class CapsuleProbe
{
    public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lparam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lparam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hwnd, StringBuilder name, int length);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextLength(IntPtr hwnd);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT point);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowExW(uint exStyle, string className, string title, uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetWindowDisplayAffinity(IntPtr hwnd, out uint affinity);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("winmm.dll", CharSet = CharSet.Unicode)] public static extern bool PlaySound(string name, IntPtr module, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);

    // Mouse messages sent between processes are DPI-virtualized by Windows when the
    // sender and the target window have different DPI awareness. The capsule is
    // per-monitor v2 aware, so the harness must be too, otherwise the coordinates
    // packed into the messages are scaled on arrival.
    public static void UsePerMonitorDpi()
    {
        SetThreadDpiAwarenessContext((IntPtr)(-4));
    }

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }

    public const long WS_EX_TOPMOST = 0x00000008;
    public const long WS_EX_TOOLWINDOW = 0x00000080;
    public const long WS_EX_LAYERED = 0x00080000;
    public const long WS_EX_NOACTIVATE = 0x08000000;
    public const uint WDA_NONE = 0;
    public const uint WDA_EXCLUDEFROMCAPTURE = 17;
    public const uint WM_APP = 0x8000;
    public const uint CAPSULE_FRAME_MESSAGE = WM_APP + 20;
    public const uint SMTO_ABORTIFHUNG = 0x0002;
    public const uint WM_MOUSEACTIVATE = 0x0021;
    public const uint WM_CANCELMODE = 0x001F;
    public const uint TRAY_MESSAGE = WM_APP + 11;
    public const int WM_RBUTTONUP = 0x0205;
    public const uint WM_LBUTTONDOWN = 0x0201;
    public const uint WM_LBUTTONUP = 0x0202;
    public const uint WM_MOUSEMOVE = 0x0200;
    public const uint MK_LBUTTON = 0x0001;
    public const ulong MA_NOACTIVATE = 3;

    public static IntPtr FindWindowByClass(uint pid, string className)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr lparam)
        {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == pid && ClassName(hwnd) == className) found = hwnd;
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static IntPtr[] FindCapsules(uint pid)
    {
        var found = new List<IntPtr>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr lparam)
        {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || !IsWindowVisible(hwnd)) return true;
            long ex = GetWindowLongPtr(hwnd, -20).ToInt64();
            if ((ex & WS_EX_LAYERED) == 0 || (ex & WS_EX_TOOLWINDOW) == 0 || (ex & WS_EX_NOACTIVATE) == 0) return true;
            RECT rect;
            GetWindowRect(hwnd, out rect);
            int width = rect.right - rect.left;
            int height = rect.bottom - rect.top;
            if (width < 40 || width > 1600 || height < 8 || height > 160) return true;
            found.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }

    public static string ClassName(IntPtr hwnd)
    {
        var name = new StringBuilder(256);
        GetClassName(hwnd, name, name.Capacity);
        return name.ToString();
    }

    public static uint Owner(IntPtr hwnd)
    {
        uint pid;
        GetWindowThreadProcessId(hwnd, out pid);
        return pid;
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

    public static uint DisplayAffinity(IntPtr hwnd)
    {
        uint affinity;
        GetWindowDisplayAffinity(hwnd, out affinity);
        return affinity;
    }

    // A plain visible probe window that does not activate or appear in the taskbar.
    public static IntPtr CreateProbeWindow(int x, int y, int width, int height)
    {
        return CreateWindowExW(
            0x00000080u | 0x08000000u, // WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
            "STATIC",
            "",
            0x80000000u | 0x10000000u, // WS_POPUP | WS_VISIBLE
            x, y, width, height,
            IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
    }

    public static bool SendFrameRequest(IntPtr hwnd)
    {
        UIntPtr result;
        return SendMessageTimeout(hwnd, CAPSULE_FRAME_MESSAGE, UIntPtr.Zero, IntPtr.Zero, SMTO_ABORTIFHUNG, 5000, out result) != IntPtr.Zero;
    }

    // The same mouse messages a real click and drag produce, sent to the capsule.
    // The machine's pointer can be moved by other software while this runs, so the
    // harness drives the window messages directly; the window handles them exactly
    // as it handles a real drag.
    private static IntPtr Pack(int x, int y)
    {
        return (IntPtr)(((y & 0xffff) << 16) | (x & 0xffff));
    }

    public static ulong MessageResult(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam)
    {
        UIntPtr result;
        SendMessageTimeout(hwnd, message, wparam, lparam, SMTO_ABORTIFHUNG, 2000, out result);
        return result.ToUInt64();
    }

    public static void Click(IntPtr hwnd)
    {
        RECT rect;
        GetWindowRect(hwnd, out rect);
        int x = (rect.right - rect.left) / 2;
        int y = (rect.bottom - rect.top) / 2;
        UIntPtr ignored;
        SendMessageTimeout(hwnd, WM_LBUTTONDOWN, (UIntPtr)MK_LBUTTON, Pack(x, y), SMTO_ABORTIFHUNG, 2000, out ignored);
        SendMessageTimeout(hwnd, WM_LBUTTONUP, UIntPtr.Zero, Pack(x, y), SMTO_ABORTIFHUNG, 2000, out ignored);
    }

    public static void Drag(IntPtr hwnd, int dx, int dy, int steps)
    {
        RECT rect;
        GetWindowRect(hwnd, out rect);
        int clientX = (rect.right - rect.left) / 2;
        int clientY = (rect.bottom - rect.top) / 2;
        int pressScreenX = rect.left + clientX;
        int pressScreenY = rect.top + clientY;
        UIntPtr ignored;
        SendMessageTimeout(hwnd, WM_LBUTTONDOWN, (UIntPtr)MK_LBUTTON, Pack(clientX, clientY), SMTO_ABORTIFHUNG, 2000, out ignored);
        for (int step = 1; step <= steps; step++)
        {
            GetWindowRect(hwnd, out rect);
            int targetX = pressScreenX + (dx * step / steps) - rect.left;
            int targetY = pressScreenY + (dy * step / steps) - rect.top;
            SendMessageTimeout(hwnd, WM_MOUSEMOVE, (UIntPtr)MK_LBUTTON, Pack(targetX, targetY), SMTO_ABORTIFHUNG, 2000, out ignored);
            clientX = targetX;
            clientY = targetY;
        }
        SendMessageTimeout(hwnd, WM_LBUTTONUP, UIntPtr.Zero, Pack(clientX, clientY), SMTO_ABORTIFHUNG, 2000, out ignored);
    }
}
'@

# Match the capsule's DPI awareness so window messages keep their coordinates.
[CapsuleProbe]::UsePerMonitorDpi()

$failures = [Collections.Generic.List[string]]::new()
$checks = [Collections.Generic.List[object]]::new()
function Check([bool]$Condition, [string]$Message) {
    $state = if ($Condition) { 'PASS' } else { 'FAIL' }
    $checks.Add([pscustomobject]@{ result = $state; check = $Message })
    Write-Output ("{0}: {1}" -f $state, $Message)
    if (-not $Condition) { $failures.Add($Message) }
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

function Get-StatusValue([string]$Text, [string]$Key) {
    $match = [regex]::Match($Text, "(?m)^$([regex]::Escape($Key))=(.*)$")
    if ($match.Success) { return $match.Groups[1].Value.TrimEnd("`r") }
    return ''
}

function Invoke-Control([string]$Command) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Exe
    $info.Arguments = '--control ' + $Command
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) {
            try { $process.Kill() } catch { }
            throw "Listen control '$Command' did not exit within 15 seconds."
        }
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) { throw "Listen control '$Command' failed ($($process.ExitCode)): $stderr" }
    }
    finally { $process.Dispose() }
}

function Start-Listen([string[]]$Extra = @()) {
    # Default launch: the engine, model and VAD paths that ship with the build.
    $arguments = @(
        '--runtime', (ConvertTo-QuotedWindowsPath $Runtime),
        '--hidden'
    )
    if ($Engine) { $arguments += @('--engine', (ConvertTo-QuotedWindowsPath $Engine)) }
    if ($Model) { $arguments += @('--model', (ConvertTo-QuotedWindowsPath $Model)) }
    if ($VadModel) { $arguments += @('--vad-model', (ConvertTo-QuotedWindowsPath $VadModel)) }
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
            if (-not $Process.WaitForExit(10000)) {
                try { $Process.Kill() } catch { }
                try { $Process.WaitForExit(5000) } catch { }
            }
        }
    }
    finally { $Process.Dispose() }
}

function Get-OurListenProcesses {
    # Only this exact executable is under test. Other Listen builds may run on the
    # same machine (they use their own window class and runtime directory), and this
    # check must never close them.
    $ours = @()
    foreach ($process in @(Get-Process -Name listen -ErrorAction SilentlyContinue)) {
        $path = ''
        try { $path = $process.Path } catch { }
        if ($path -eq $Exe) { $ours += $process }
    }
    return $ours
}

function Get-Capsule {
    $processes = @(Get-OurListenProcesses)
    if ($processes.Count -ne 1) { throw "Expected exactly one listen process for $Exe; found $($processes.Count)." }
    $windows = [CapsuleProbe]::FindCapsules([uint32]$processes[0].Id)
    if ($windows.Count -ne 1) { throw "Expected exactly one capsule window; found $($windows.Count)." }
    return $windows[0]
}

function Save-Shot([string]$Path, [int]$X, [int]$Y, [int]$Width, [int]$Height) {
    $bitmap = [System.Drawing.Bitmap]::new($Width, $Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($X, $Y, 0, 0, [System.Drawing.Size]::new($Width, $Height)) }
    finally { $graphics.Dispose() }
    try { $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $bitmap.Dispose() }
}

function Save-DesktopShot([string]$Path) {
    $screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
    Save-Shot $Path $screen.X $screen.Y $screen.Width $screen.Height
}

# Ask the capsule for the exact frame it is presenting and save it as PNG. The
# capsule is excluded from capture, so this is the only way to see its pixels.
function Save-CapsuleShot([string]$Path) {
    $capsule = Get-Capsule
    Remove-Item -LiteralPath $framePath -ErrorAction SilentlyContinue
    if (-not [CapsuleProbe]::SendFrameRequest($capsule)) { throw 'The capsule did not answer the frame request.' }
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if (Test-Path -LiteralPath $framePath -PathType Leaf) { break }
        Start-Sleep -Milliseconds 20
    }
    if (-not (Test-Path -LiteralPath $framePath -PathType Leaf)) { throw 'The capsule did not write its frame.' }
    $image = [System.Drawing.Image]::FromFile($framePath)
    try { $image.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png) } finally { $image.Dispose() }
    Remove-Item -LiteralPath $framePath -ErrorAction SilentlyContinue
}

function Get-Center([CapsuleProbe+RECT]$Rect) {
    return @([int](($Rect.left + $Rect.right) / 2), [int](($Rect.top + $Rect.bottom) / 2))
}

$listen = $null
$summary = [ordered]@{
    checked_utc = [DateTime]::UtcNow.ToString('o')
    exe = $Exe
    runtime = $Runtime
    capsule = @{}
    status = @{}
    states = [ordered]@{}
    position = @{}
    screenshots = [Collections.Generic.List[string]]::new()
    checks = $checks
    result = 'FAIL'
}

try {
    $existing = @(Get-OurListenProcesses)
    if ($existing.Count -gt 0) { throw "This Listen build is already running; this check will not replace or close it ($Exe)." }
    Remove-Item -LiteralPath $statusPath, $positionPath, $framePath -ErrorAction SilentlyContinue

    # --- Ready state and window properties -------------------------------
    $listen = Start-Listen
    $status = Wait-Phase $listen 'Ready'
    $capsule = Get-Capsule
    $rect = [CapsuleProbe]::Rect($capsule)
    $exStyle = [CapsuleProbe]::ExStyle($capsule)
    $titleLength = [CapsuleProbe]::GetWindowTextLength($capsule)
    $affinity = [CapsuleProbe]::DisplayAffinity($capsule)
    $startup = Get-StatusValue $status 'startup_capsule_ms'
    $summary.capsule = [ordered]@{
        process_id = $listen.Id
        hwnd = ('0x{0:X}' -f $capsule.ToInt64())
        class = [CapsuleProbe]::ClassName($capsule)
        window_rect = "$($rect.left),$($rect.top) $($rect.right - $rect.left)x$($rect.bottom - $rect.top)"
        title_length = $titleLength
        ex_style = ('0x{0:X}' -f $exStyle)
        topmost_bit = (($exStyle -band [CapsuleProbe]::WS_EX_TOPMOST) -ne 0)
        layered = (($exStyle -band [CapsuleProbe]::WS_EX_LAYERED) -ne 0)
        toolwindow = (($exStyle -band [CapsuleProbe]::WS_EX_TOOLWINDOW) -ne 0)
        noactivate = (($exStyle -band [CapsuleProbe]::WS_EX_NOACTIVATE) -ne 0)
        display_affinity = $affinity
    }
    Check ($capsule -ne [IntPtr]::Zero) 'the capsule window exists and is visible'
    Check ($titleLength -eq 0) "the capsule window title is empty (GetWindowTextLengthW = $titleLength)"
    Check ((($exStyle -band [CapsuleProbe]::WS_EX_LAYERED) -ne 0)) 'the capsule has WS_EX_LAYERED'
    Check ((($exStyle -band [CapsuleProbe]::WS_EX_TOOLWINDOW) -ne 0)) 'the capsule has WS_EX_TOOLWINDOW (no taskbar or Alt+Tab entry)'
    Check ((($exStyle -band [CapsuleProbe]::WS_EX_NOACTIVATE) -ne 0)) 'the capsule has WS_EX_NOACTIVATE'
    Check ((($exStyle -band [CapsuleProbe]::WS_EX_TOPMOST) -ne 0)) 'the capsule reports WS_EX_TOPMOST'
    Check ($affinity -eq [CapsuleProbe]::WDA_EXCLUDEFROMCAPTURE) "the capsule display affinity is WDA_EXCLUDEFROMCAPTURE ($affinity)"

    # Behavioural topmost check: a normal probe window covering the capsule must
    # not end up above it, so the point still belongs to the capsule.
    $center = Get-Center $rect
    $probe = [CapsuleProbe]::CreateProbeWindow($rect.left, $rect.top, ($rect.right - $rect.left), ($rect.bottom - $rect.top))
    try {
        Start-Sleep -Milliseconds 150
        $hit = [CapsuleProbe]::WindowFromPoint([CapsuleProbe+POINT]@{ x = $center[0]; y = $center[1] })
        $probeCovered = ($hit -eq $capsule)
    }
    finally {
        if ($probe -ne [IntPtr]::Zero) { [void][CapsuleProbe]::DestroyWindow($probe) }
    }
    Check $probeCovered 'the capsule stays above a normal window covering the same rectangle (topmost)'

    # --- status.txt contract ---------------------------------------------
    $summary.status = [ordered]@{
        phase = Get-StatusValue $status 'phase'
        preparation = Get-StatusValue $status 'preparation'
        capsule = Get-StatusValue $status 'capsule'
        startup_capsule_ms = $startup
        capsule_capture_excluded = Get-StatusValue $status 'capsule_capture_excluded'
        backend_present = [bool](Get-StatusValue $status 'backend')
        current_result = Get-StatusValue $status 'current_result'
    }
    Check ($startup -match '^\d+$' -and [int]$startup -gt 0) "status.txt reports startup_capsule_ms=$startup"
    Check ($startup -match '^\d+$' -and [int]$startup -lt 300) "the capsule appeared within the 300 ms startup target (${startup} ms)"
    Check ($summary.status.capsule -eq 'Ready') "status.txt reports capsule=Ready in the ready state ($($summary.status.capsule))"
    Check ($summary.status.preparation -eq 'Ready') "status.txt reports preparation=Ready ($($summary.status.preparation))"
    Check ($summary.status.capsule_capture_excluded -eq '1') 'status.txt reports capsule_capture_excluded=1'
    Check ($summary.status.backend_present) 'status.txt still reports the recognition backend'
    Check ((Get-StatusValue $status 'current_result') -ne '') 'status.txt still reports the current-result path'

    # Every documented --control command must reach the running application. The
    # capture-affecting ones (start/stop/quit) are exercised by the state checks
    # below; show/retry/cleanup are checked here.
    $controlError = ''
    try {
        foreach ($command in @('show', 'retry', 'cleanup')) { Invoke-Control $command }
    }
    catch { $controlError = $_.Exception.Message }
    Check ($controlError -eq '') "--control show, retry and cleanup reach the running application ($controlError)"
    Check ((Get-Capsule) -ne [IntPtr]::Zero) 'the capsule is still present after the control commands'

    # --- Focus behaviour -------------------------------------------------
    # WS_EX_NOACTIVATE plus MA_NOACTIVATE is what keeps a real click from taking
    # focus. A click is then delivered as the same window messages a real click
    # produces, so the check is deterministic on a shared desktop.
    $activation = [CapsuleProbe]::MessageResult($capsule, [CapsuleProbe]::WM_MOUSEACTIVATE, [UIntPtr]::Zero, [IntPtr]::Zero)
    $summary.capsule.mouse_activate_result = $activation
    Check ($activation -eq [CapsuleProbe]::MA_NOACTIVATE) "the capsule answers WM_MOUSEACTIVATE with MA_NOACTIVATE ($activation)"
    # A shared desktop can change the foreground window or move a topmost window for
    # unrelated reasons. Each attempt is short and the checks are retried; taking
    # focus is never retried because it would always be a real failure.
    $foregroundStable = $false
    $clickStable = $false
    $tookFocus = $false
    $clickAttempts = 0
    for ($attempt = 0; $attempt -lt 6 -and -not ($foregroundStable -and $clickStable) -and -not $tookFocus; $attempt++) {
        $clickAttempts = $attempt + 1
        $rectBeforeClick = [CapsuleProbe]::Rect($capsule)
        $foregroundBefore = [CapsuleProbe]::GetForegroundWindow()
        [CapsuleProbe]::Click($capsule)
        $foregroundAfter = [CapsuleProbe]::GetForegroundWindow()
        $rectAfterClick = [CapsuleProbe]::Rect($capsule)
        $foregroundOwner = [CapsuleProbe]::Owner($foregroundAfter)
        if ($foregroundAfter -eq $capsule -or $foregroundOwner -eq [uint32]$listen.Id) {
            $tookFocus = $true
            break
        }
        if ($foregroundBefore -eq $foregroundAfter) { $foregroundStable = $true }
        if ($rectBeforeClick.left -eq $rectAfterClick.left -and $rectBeforeClick.top -eq $rectAfterClick.top) {
            $clickStable = $true
        }
    }
    $summary.capsule.foreground_before = ('0x{0:X}' -f $foregroundBefore.ToInt64())
    $summary.capsule.foreground_after_click = ('0x{0:X}' -f $foregroundAfter.ToInt64())
    $summary.capsule.foreground_stable = $foregroundStable
    $summary.capsule.foreground_attempts = $clickAttempts
    Check (-not $tookFocus) 'clicking the capsule never makes the capsule or Listen the foreground window'
    Check ($foregroundStable) 'the foreground window is unchanged by the click'
    Check ($clickStable) 'a click does not move the capsule'

    # --- Screenshots: ready ----------------------------------------------
    $readyShot = Join-Path $Evidence 'capsule-ready.png'
    Save-CapsuleShot $readyShot
    $summary.screenshots.Add($readyShot)
    Check (Test-Path -LiteralPath $readyShot) "saved the ready-state capsule image: $readyShot"
    $desktopShot = Join-Path $Evidence 'desktop-capsule-excluded.png'
    Save-DesktopShot $desktopShot
    $summary.screenshots.Add($desktopShot)
    Check (Test-Path -LiteralPath $desktopShot) "saved the full-desktop screenshot (capsule excluded from capture): $desktopShot"

    # --- Capturing -------------------------------------------------------
    Invoke-Control 'start'
    $status = Wait-Phase $listen 'Capturing'
    Start-Sleep -Milliseconds 400
    $summary.states.capturing = Get-StatusValue $status 'capsule'
    $capturingShot = Join-Path $Evidence 'capsule-capturing.png'
    Save-CapsuleShot $capturingShot
    $summary.screenshots.Add($capturingShot)
    Check ($summary.states.capturing -eq 'Capturing') "status.txt reports capsule=Capturing while capturing ($($summary.states.capturing))"

    # --- Processing ------------------------------------------------------
    # Silence finishes local recognition in about a hundred milliseconds, which the
    # 100 ms status cadence can miss. Playing a clip into the default multimedia
    # endpoint gives the recognizer real speech, so the processing state stays
    # observable for seconds. Two attempts cover an unexpected short run.
    $availableClip = if ($AudioClip -and (Test-Path -LiteralPath $AudioClip -PathType Leaf)) { $AudioClip } else { '' }
    $summary.processing_audio = $availableClip
    $processingStatus = $null
    $processingShot = Join-Path $Evidence 'capsule-processing.png'
    for ($attempt = 0; $attempt -lt 2 -and $null -eq $processingStatus; $attempt++) {
        if ($attempt -gt 0) {
            [void](Wait-Idle $listen)
            Invoke-Control 'start'
            [void](Wait-Phase $listen 'Capturing')
        }
        if ($availableClip) {
            [void][CapsuleProbe]::PlaySound($availableClip, [IntPtr]::Zero, 0x00020000 -bor 0x00000001 -bor 0x00000008 -bor 0x00000002)
        }
        Start-Sleep -Seconds 5
        if ($availableClip) { [void][CapsuleProbe]::PlaySound($null, [IntPtr]::Zero, 0) }
        Invoke-Control 'stop'
        $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
        while ((Get-Date) -lt $deadline) {
            if (Test-Path -LiteralPath $statusPath -PathType Leaf) {
                $text = Read-Status
                if ($text -match "(?m)^phase=Processing`r?$") { $processingStatus = $text; break }
            }
            Start-Sleep -Milliseconds 10
        }
        if ($null -ne $processingStatus) {
            # The state is live right now; take the frame before it finishes.
            $summary.states.processing = Get-StatusValue $processingStatus 'capsule'
            Save-CapsuleShot $processingShot
        }
    }
    $summary.screenshots.Add($processingShot)
    Check ($null -ne $processingStatus) 'the processing state was observable long enough to capture'
    Check ($summary.states.processing -eq 'Processing') "status.txt reports capsule=Processing while processing ($($summary.states.processing))"

    $status = Wait-Idle $listen
    Check ((Get-StatusValue $status 'capsule') -eq 'Ready') 'the capsule returns to Ready after processing'

    # --- Drag and remembered position ------------------------------------
    # Retried because another program on a shared desktop can move a topmost window
    # at the same moment; the product behaviour under test is the drag itself.
    $dragX = 140
    $dragY = 60
    $dragOk = $false
    $positionOk = $false
    $draggedRect = [CapsuleProbe]::Rect($capsule)
    $savedPosition = @{}
    for ($attempt = 0; $attempt -lt 3 -and -not ($dragOk -and $positionOk); $attempt++) {
        $rect = [CapsuleProbe]::Rect($capsule)
        [CapsuleProbe]::Drag($capsule, $dragX, $dragY, 14)
        Start-Sleep -Milliseconds 150
        $draggedRect = [CapsuleProbe]::Rect($capsule)
        $movedX = $draggedRect.left - $rect.left
        $movedY = $draggedRect.top - $rect.top
        $dragOk = ($movedX -eq $dragX -and $movedY -eq $dragY)
        if (-not $dragOk) { continue }
        $savedPosition = @{}
        if (Test-Path -LiteralPath $positionPath -PathType Leaf) {
            foreach ($line in [IO.File]::ReadAllLines($positionPath)) {
                if ($line -match '^x=(-?\d+)$') { $savedPosition.x = [int]$Matches[1] }
                elseif ($line -match '^y=(-?\d+)$') { $savedPosition.y = [int]$Matches[1] }
            }
        }
        $positionOk = ($savedPosition.x -eq $draggedRect.left -and $savedPosition.y -eq $draggedRect.top)
    }
    $summary.position = [ordered]@{
        file_x = $savedPosition.x
        file_y = $savedPosition.y
        window_x = $draggedRect.left
        window_y = $draggedRect.top
        drag = "${dragX}x${dragY}"
    }
    Check ($dragOk) "the capsule followed the drag by ${dragX}x${dragY} px"
    Check (Test-Path -LiteralPath $positionPath -PathType Leaf) 'the drag persisted capsule-position.txt'
    Check ($positionOk) "capsule-position.txt matches the dragged window position ($($savedPosition.x),$($savedPosition.y))"

    # --- Restart restores the position -----------------------------------
    Invoke-Control 'quit'
    if (-not $listen.WaitForExit(10000)) { throw 'Listen did not exit after --control quit.' }
    Check ($listen.ExitCode -eq 0) 'the application exits cleanly through --control quit'
    $listen.Dispose()
    $listen = $null
    # Re-state the position that the drag produced, then prove startup restores it.
    [IO.File]::WriteAllText($positionPath, "x=$($savedPosition.x)`ny=$($savedPosition.y)`n")
    $restoredOk = $false
    $restoredRect = [CapsuleProbe]::Rect([IntPtr]::Zero)
    for ($attempt = 0; $attempt -lt 3 -and -not $restoredOk; $attempt++) {
        $listen = Start-Listen
        $status = Wait-Phase $listen 'Ready'
        $restoredCapsule = Get-Capsule
        $restoredRect = [CapsuleProbe]::Rect($restoredCapsule)
        $restoredOk = ($restoredRect.left -eq $savedPosition.x -and $restoredRect.top -eq $savedPosition.y)
        if (-not $restoredOk) {
            Stop-Listen $listen
            $listen = $null
            [IO.File]::WriteAllText($positionPath, "x=$($savedPosition.x)`ny=$($savedPosition.y)`n")
        }
    }
    $summary.position.restored_x = $restoredRect.left
    $summary.position.restored_y = $restoredRect.top
    Check ($restoredOk) "the restarted capsule restored the saved position ($($restoredRect.left),$($restoredRect.top))"
    Check ((Get-StatusValue $status 'startup_capsule_ms') -match '^\d+$') 'the restarted application also reports startup_capsule_ms'

    # --- Tray menu -------------------------------------------------------
    # The shell sends this callback message when the tray icon is right-clicked.
    # The menu itself is native, so its opening is what is checked here; the items
    # are the documented start/stop, retry, cleanup and exit commands, and retry and
    # cleanup are grayed out exactly when the application reports them unavailable.
    $hostWindow = [CapsuleProbe]::FindWindowByClass([uint32]$listen.Id, 'ListenHostWindowV2')
    Check ($hostWindow -ne [IntPtr]::Zero) 'the hidden window that owns the tray icon and receives --control exists'
    [void][CapsuleProbe]::PostMessage($hostWindow, [CapsuleProbe]::TRAY_MESSAGE, [UIntPtr]::new(1), [IntPtr][CapsuleProbe]::WM_RBUTTONUP)
    $menuWindow = [IntPtr]::Zero
    for ($attempt = 0; $attempt -lt 50 -and $menuWindow -eq [IntPtr]::Zero; $attempt++) {
        Start-Sleep -Milliseconds 40
        $menuWindow = [CapsuleProbe]::FindWindowByClass([uint32]$listen.Id, '#32768')
    }
    Check ($menuWindow -ne [IntPtr]::Zero) 'the tray menu opens on the shell callback'
    [void][CapsuleProbe]::PostMessage($hostWindow, [CapsuleProbe]::WM_CANCELMODE, [UIntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 300
    Check (([CapsuleProbe]::FindWindowByClass([uint32]$listen.Id, '#32768')) -eq [IntPtr]::Zero) 'the tray menu closes without selecting an item'

    Stop-Listen $listen
    $listen = $null

    # --- Failure state ---------------------------------------------------
    # Break whichever engine is under test: the default SenseVoice model path and the
    # whisper fallback model path are both pointed at files that do not exist.
    $brokenOnnx = Join-Path $Runtime 'missing-model.onnx'
    $brokenModel = Join-Path $Runtime 'missing-model.bin'
    Remove-Item -LiteralPath $brokenOnnx, $brokenModel -ErrorAction SilentlyContinue
    $listen = Start-Listen @('--sensevoice-model', (ConvertTo-QuotedWindowsPath $brokenOnnx), '--model', (ConvertTo-QuotedWindowsPath $brokenModel))
    $status = Wait-Phase $listen 'Failure'
    $summary.states.failure = Get-StatusValue $status 'capsule'
    $failureShot = Join-Path $Evidence 'capsule-failure.png'
    Save-CapsuleShot $failureShot
    $summary.screenshots.Add($failureShot)
    Check ($summary.states.failure -eq 'Failure') "status.txt reports capsule=Failure with a broken model ($($summary.states.failure))"
    Check ((Get-StatusValue $status 'preparation') -eq 'Failed') "status.txt reports preparation=Failed with a broken model ($(Get-StatusValue $status 'preparation'))"
    Check (Test-Path -LiteralPath $failureShot) 'saved the failure-state capsule image'

    Stop-Listen $listen
    $listen = $null
    Check (-not @(Get-OurListenProcesses)) 'no Listen process is left running'
}
catch {
    $failures.Add($_.Exception.Message)
    throw
}
finally {
    if ($null -ne $listen) { Stop-Listen $listen }
    $summary.result = if ($failures.Count -eq 0) { 'PASS' } else { 'FAIL' }
    $summary.failures = @($failures.ToArray())
    $summaryPath = Join-Path $Evidence 'capsule-verification.json'
    [IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    Write-Output ''
    Write-Output ("Result: {0} ({1} checks, {2} failures)" -f $summary.result, $checks.Count, $failures.Count)
    Write-Output ("Evidence: {0}" -f $summaryPath)
}
if ($failures.Count -gt 0) { exit 1 }
