using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Drawing;
using System.Drawing.Imaging;

// Task-owned UI acceptance driver, authorized by the user. Targets are bound to
// the newly launched Listen PID; no other application's controls are operated.
public static class ListenNativeQa
{
    private delegate bool EnumProc(IntPtr window, IntPtr state);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc callback, IntPtr state);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder name, int length);
    [DllImport("user32.dll")] private static extern IntPtr GetDlgItem(IntPtr window, int id);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll", SetLastError = true)] private static extern uint SendInput(uint count, INPUT[] events, int size);
    [DllImport("user32.dll", SetLastError = true)] private static extern bool PostMessage(IntPtr window, uint message, IntPtr first, IntPtr second);
    [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr first, IntPtr second, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr SendTextTimeout(IntPtr window, uint message, UIntPtr first, StringBuilder second, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] private static extern bool GetGUIThreadInfo(uint thread, ref GUIINFO info);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr window, out RECT rect);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out POINT point);
    [DllImport("user32.dll")] private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(POINT point);
    [DllImport("user32.dll", SetLastError = true)] private static extern bool PrintWindow(IntPtr window, IntPtr context, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();

    [StructLayout(LayoutKind.Sequential)] private struct KEYBOARD { public ushort key, scan; public uint flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] private struct MOUSE { public int x, y; public uint data, flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] private struct UNION { [FieldOffset(0)] public KEYBOARD keyboard; [FieldOffset(0)] public MOUSE mouse; }
    [StructLayout(LayoutKind.Sequential)] private struct INPUT { public uint type; public UNION data; }
    [StructLayout(LayoutKind.Sequential)] private struct RECT { public int left, top, right, bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct POINT { public int x, y; }
    [StructLayout(LayoutKind.Sequential)] private struct GUIINFO { public uint size, flags; public IntPtr active, focus, capture, menuOwner, moveSize, caret; public RECT caretRect; }

    private static string ClassName(IntPtr window) { var name = new StringBuilder(256); GetClassName(window, name, name.Capacity); return name.ToString(); }
    public static void RequireOwner(IntPtr window, int process)
    {
        uint owner; GetWindowThreadProcessId(window, out owner);
        if (window == IntPtr.Zero || owner != (uint)process) throw new InvalidOperationException("UI target is not owned by the task's Listen process.");
    }
    public static IntPtr FindWindow(int process)
    {
        IntPtr found = IntPtr.Zero; int matches = 0;
        EnumWindows(delegate(IntPtr window, IntPtr ignored) {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == (uint)process && ClassName(window) == "ListenCaptureToCopyWindowV1") { found = window; matches++; }
            return true;
        }, IntPtr.Zero);
        if (matches != 1) throw new InvalidOperationException("Expected exactly one task-owned Listen window; found " + matches);
        return found;
    }
    public static IntPtr Control(IntPtr window, int process, int id, string expectedClass)
    {
        RequireOwner(window, process);
        IntPtr control = GetDlgItem(window, id); RequireOwner(control, process);
        if (!String.Equals(ClassName(control), expectedClass, StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("Unexpected Listen control class.");
        return control;
    }
    public static void Focus(IntPtr window, int process)
    {
        RequireOwner(window, process); ShowWindow(window, 9); SetForegroundWindow(window); Thread.Sleep(150);
        uint foregroundOwner; GetWindowThreadProcessId(GetForegroundWindow(), out foregroundOwner);
        if (foregroundOwner != (uint)process) {
            // Activate only this task-owned window by clicking its verified caption.
            // Do not change the system foreground-lock setting or other apps.
            POINT original; if (!GetCursorPos(out original)) throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                if (!SetWindowPos(window, (IntPtr)(-1), 0, 0, 0, 0, 0x53)) throw new Win32Exception(Marshal.GetLastWin32Error());
                RECT bounds; if (!GetWindowRect(window, out bounds)) throw new Win32Exception(Marshal.GetLastWin32Error());
                var target = new POINT(); target.x = (bounds.left + bounds.right) / 2; target.y = bounds.top + 15;
                RequireOwner(WindowFromPoint(target), process);
                if (!SetCursorPos(target.x, target.y)) throw new Win32Exception(Marshal.GetLastWin32Error());
                RequireOwner(WindowFromPoint(target), process);
                var down = new INPUT(); down.data.mouse.flags = 2;
                var up = new INPUT(); up.data.mouse.flags = 4;
                uint accepted = SendInput(2, new INPUT[] { down, up }, Marshal.SizeOf(typeof(INPUT)));
                if (accepted != 2) { SendInput(1, new INPUT[] {up}, Marshal.SizeOf(typeof(INPUT))); throw new Win32Exception(Marshal.GetLastWin32Error(), "Activation click was not accepted."); }
                Thread.Sleep(150);
            } finally {
                SetWindowPos(window, (IntPtr)(-2), 0, 0, 0, 0, 0x53);
                SetCursorPos(original.x, original.y);
            }
        }
        RequireOwner(GetForegroundWindow(), process);
        if (!IsWindowVisible(window)) throw new InvalidOperationException("Listen did not become visible.");
    }
    private static INPUT Key(ushort key, bool up)
    {
        var item = new INPUT(); item.type = 1; item.data.keyboard.key = key; item.data.keyboard.flags = up ? 2u : 0u; return item;
    }
    public static uint Hotkey(IntPtr window, int process)
    {
        RequireOwner(window, process); RequireOwner(GetForegroundWindow(), process);
        foreach (int key in new int[] {0x10, 0x11, 0x12, 0x52}) if ((GetAsyncKeyState(key) & 0x8000) != 0) throw new InvalidOperationException("A keyboard modifier or R is held; no key input sent.");
        INPUT[] keys = new INPUT[] { Key(0x11,false), Key(0x12,false), Key(0x52,false), Key(0x52,true), Key(0x12,true), Key(0x11,true) };
        uint accepted = SendInput((uint)keys.Length, keys, Marshal.SizeOf(typeof(INPUT)));
        if (accepted != keys.Length) {
            INPUT[] releases = new INPUT[] { Key(0x52,true), Key(0x12,true), Key(0x11,true) };
            SendInput((uint)releases.Length, releases, Marshal.SizeOf(typeof(INPUT)));
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Windows did not accept the complete hotkey.");
        }
        return accepted;
    }
    private static void RequireFocus(IntPtr control, int process)
    {
        RequireOwner(control, process); RequireOwner(GetForegroundWindow(), process);
        uint owner; uint thread = GetWindowThreadProcessId(control, out owner);
        var info = new GUIINFO(); info.size = (uint)Marshal.SizeOf(typeof(GUIINFO));
        if (!GetGUIThreadInfo(thread, ref info)) throw new Win32Exception(Marshal.GetLastWin32Error());
        if (info.focus != control) throw new InvalidOperationException("The task-owned control is not the focused control.");
    }
    public static void ClickControl(IntPtr root, IntPtr control, int process)
    {
        Focus(root, process); RequireOwner(control, process);
        RECT bounds; if (!GetWindowRect(control, out bounds)) throw new Win32Exception(Marshal.GetLastWin32Error());
        POINT original; if (!GetCursorPos(out original)) throw new Win32Exception(Marshal.GetLastWin32Error());
        var point = new POINT(); point.x = bounds.left + Math.Min(120, (bounds.right - bounds.left) / 2); point.y = bounds.top + 12;
        try {
            if (WindowFromPoint(point) != control) throw new InvalidOperationException("The observed test control is occluded; no input sent.");
            if (!SetCursorPos(point.x, point.y)) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (WindowFromPoint(point) != control) throw new InvalidOperationException("The test control moved before the click.");
            var down = new INPUT(); down.data.mouse.flags = 2;
            var up = new INPUT(); up.data.mouse.flags = 4;
            if (SendInput(2, new INPUT[] {down, up}, Marshal.SizeOf(typeof(INPUT))) != 2) throw new Win32Exception(Marshal.GetLastWin32Error());
            Thread.Sleep(150); RequireFocus(control, process);
        } finally { SetCursorPos(original.x, original.y); }
    }
    public static void AppendUserTyping(IntPtr control, int process, string literal)
    {
        RequireFocus(control, process);
        foreach (int key in new int[] {0x10, 0x11, 0x12}) if ((GetAsyncKeyState(key) & 0x8000) != 0) throw new InvalidOperationException("A modifier is held; no typing sent.");
        INPUT[] end = new INPUT[] {Key(0x11,false), Key(0x23,false), Key(0x23,true), Key(0x11,true)};
        if (SendInput((uint)end.Length, end, Marshal.SizeOf(typeof(INPUT))) != end.Length) throw new Win32Exception(Marshal.GetLastWin32Error());
        Thread.Sleep(50); RequireFocus(control, process);
        foreach (char code in literal) {
            if (code == '\r' || code == '\n') throw new InvalidOperationException("QA user edits must not contain Enter.");
            var down = new INPUT(); down.type = 1; down.data.keyboard.scan = code; down.data.keyboard.flags = 4;
            var up = down; up.data.keyboard.flags = 6;
            if (SendInput(2, new INPUT[] {down, up}, Marshal.SizeOf(typeof(INPUT))) != 2) throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        Thread.Sleep(150); RequireFocus(control, process);
    }
    public static string ReadText(IntPtr window, int process)
    {
        RequireOwner(window, process); UIntPtr count;
        if (SendMessageTimeout(window, 14, UIntPtr.Zero, IntPtr.Zero, 2, 1500, out count) == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not read native text length.");
        if (count.ToUInt64() > 1000000) throw new InvalidOperationException("Unexpectedly large text for this short-speech test; no truncation accepted.");
        var text = new StringBuilder((int)count.ToUInt64() + 1);
        UIntPtr copied;
        if (SendTextTimeout(window, 13, (UIntPtr)(uint)text.Capacity, text, 2, 1500, out copied) == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not read native text.");
        if (copied.ToUInt64() != count.ToUInt64()) throw new InvalidOperationException("Native result text was not read completely (length=" + count.ToUInt64() + ", copied=" + copied.ToUInt64() + ").");
        return text.ToString();
    }
    public static void OpenTrayMenu(IntPtr window, int process)
    {
        RequireOwner(window, process); RequireOwner(GetForegroundWindow(), process);
        // Simulate the Shell's registered right-click callback. This opens the
        // real TrackPopupMenu loop, without clicking another app's taskbar UI.
        if (!PostMessage(window, 0x800B, (IntPtr)1, (IntPtr)0x205)) throw new Win32Exception(Marshal.GetLastWin32Error());
    }
    public static bool MenuActive(IntPtr window, int process)
    {
        RequireOwner(window, process); uint owner; uint thread = GetWindowThreadProcessId(window, out owner);
        var info = new GUIINFO(); info.size = (uint)Marshal.SizeOf(typeof(GUIINFO));
        if (!GetGUIThreadInfo(thread, ref info)) throw new Win32Exception(Marshal.GetLastWin32Error());
        return (info.flags & (4u | 16u)) != 0;
    }
    public static void CloseMenu(IntPtr window, int process)
    {
        RequireOwner(window, process); UIntPtr ignored;
        if (SendMessageTimeout(window, 0x1F, UIntPtr.Zero, IntPtr.Zero, 2, 1500, out ignored) == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Menu cancel did not return.");
    }
    public static void ClickCopy(IntPtr button, int process)
    {
        RequireOwner(button, process);
        if (!IsWindowEnabled(button) || !IsWindowVisible(button)) throw new InvalidOperationException("Copy button is not enabled and visible.");
        UIntPtr ignored;
        if (SendMessageTimeout(button, 0xF5, UIntPtr.Zero, IntPtr.Zero, 2, 1500, out ignored) == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Copy button did not return.");
    }
    // A button that changes the application state can disable itself while handling
    // BM_CLICK, so the clicked control need not still own keyboard focus afterwards.
    // This still requires that the task-owned button is enabled and visible.
    public static void PressButton(IntPtr button, int process)
    {
        RequireOwner(button, process);
        if (!IsWindowEnabled(button) || !IsWindowVisible(button)) throw new InvalidOperationException("Button is not enabled and visible.");
        UIntPtr ignored;
        if (SendMessageTimeout(button, 0xF5, UIntPtr.Zero, IntPtr.Zero, 2, 1500, out ignored) == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Button did not return.");
    }
    public static void Show(IntPtr window, int process)
    {
        RequireOwner(window, process); ShowWindow(window, 9);
    }
    public static void Hide(IntPtr window, int process)
    {
        RequireOwner(window, process); ShowWindow(window, 0);
    }
    public static void Screenshot(IntPtr window, int process, string path)
    {
        RequireOwner(window, process); RECT bounds;
        if (!IsWindowVisible(window) || !GetWindowRect(window, out bounds)) throw new InvalidOperationException("Listen window is not visible for screenshot.");
        using (var bitmap = new Bitmap(bounds.right - bounds.left, bounds.bottom - bounds.top))
        using (var graphics = Graphics.FromImage(bitmap)) {
            IntPtr context = graphics.GetHdc();
            try { if (!PrintWindow(window, context, 0)) throw new Win32Exception(Marshal.GetLastWin32Error(), "Listen screenshot failed."); }
            finally { graphics.ReleaseHdc(context); }
            bitmap.Save(path, ImageFormat.Png);
        }
    }
}
