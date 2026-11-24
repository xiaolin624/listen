using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Forms;

// Task-owned native EDIT fixture for the packaged Listen acceptance. Listen receives
// no handles or control identities from this host. Modes simulate real platform edges:
//   0 normal; 1 host-enforced capacity; 2 not used here (read-only is set by message);
//   3 silently drops programmatic WM_SETTEXT but still accepts real paste/keyboard.
public static class PackageFixture
{
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "CreateWindowExW", ExactSpelling = true, SetLastError = true)]
    private static extern IntPtr CreateWindowExW(uint ex, string cls, string text, uint style,
        int x, int y, int width, int height, IntPtr parent, IntPtr id, IntPtr instance, IntPtr data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageW", ExactSpelling = true)]
    private static extern IntPtr SendMessageW(IntPtr window, uint message, IntPtr first, IntPtr second);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SetWindowLongPtrW", ExactSpelling = true, SetLastError = true)]
    private static extern IntPtr SetWindowLongPtrW(IntPtr window, int index, IntPtr value);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "CallWindowProcW", ExactSpelling = true)]
    private static extern IntPtr CallWindowProcW(IntPtr previous, IntPtr window, uint message, IntPtr first, IntPtr second);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetWindowTextLengthW", ExactSpelling = true)]
    private static extern int GetWindowTextLengthW(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetWindowTextW", ExactSpelling = true)]
    private static extern int GetWindowTextW(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassNameW(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetWindowTextW")] private static extern int GetWindowTextTitleW(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);

    private const uint WM_SETTEXT = 0x000C;
    private const uint WM_KEYDOWN = 0x0100;
    private const uint WM_KEYUP = 0x0101;
    private const uint WM_SETFOCUS = 0x0007;
    private const uint WM_KILLFOCUS = 0x0008;
    private const uint WM_PASTE = 0x0302;
    private const uint EM_SETREADONLY = 0x00CF;
    private const int GWLP_WNDPROC = -4;

    private delegate IntPtr WindowProc(IntPtr window, uint message, IntPtr first, IntPtr second);

    private static Thread thread;
    private static Form primary, secondary;
    private static readonly List<NativeWindow> hooks = new List<NativeWindow>();
    private static readonly List<WindowProc> delegates = new List<WindowProc>();
    private static IntPtr originalProc;
    public static IntPtr Primary, Secondary, Target, Neighbor, Other, SendButton, NoTargetButton;
    public static int Sends, ReturnKeys, SetTextCalls, PasteCalls, AKeys, CKeys, VKeys;
    private static readonly List<string> eventLog = new List<string>();
    public static int Capacity;
    public static int Mode;
    public static string TargetDraft = "original draft: Rust 123.\r\nsecond line draft.";
    public static string NeighborDraft = "neighbor: NEIGHBOR_UNCHANGED 789";
    public static string OtherDraft = "other: OTHER_UNCHANGED 456";

    private sealed class EditObserver : NativeWindow
    {
        public EditObserver(IntPtr handle) { AssignHandle(handle); }
        protected override void WndProc(ref Message message)
        {
            if (message.Msg == WM_KEYDOWN && message.WParam.ToInt64() == 13) Interlocked.Increment(ref ReturnKeys);
            base.WndProc(ref message);
        }
    }

    private static IntPtr FixtureProc(IntPtr window, uint message, IntPtr first, IntPtr second)
    {
        if (message == WM_KEYDOWN)
        {
            long key = first.ToInt64();
            lock (eventLog) { eventLog.Add("key:" + key.ToString("X")); }
            if (key == 0x41) Interlocked.Increment(ref AKeys);
            if (key == 0x43) Interlocked.Increment(ref CKeys);
            if (key == 0x56) Interlocked.Increment(ref VKeys);
        }
        if (message == WM_KEYUP) { lock (eventLog) { eventLog.Add("up:" + first.ToInt64().ToString("X")); } }
        if (message == WM_SETFOCUS) { lock (eventLog) { eventLog.Add("focus"); } }
        if (message == WM_KILLFOCUS) {
            IntPtr foreground = GetForegroundWindow(); uint owner;
            GetWindowThreadProcessId(foreground, out owner);
            var cls = new StringBuilder(128); GetClassNameW(foreground, cls, cls.Capacity);
            var title = new StringBuilder(128); GetWindowTextTitleW(foreground, title, title.Capacity);
            lock (eventLog) { eventLog.Add("killfocus->" + owner + ":" + cls + ":" + title); }
        }
        if (message == WM_PASTE) { lock (eventLog) { eventLog.Add("paste"); } }
        if (message == WM_SETTEXT) { lock (eventLog) { eventLog.Add("settext"); } }
        if (message == WM_SETTEXT)
        {
            Interlocked.Increment(ref SetTextCalls);
            if (Mode == 3)
            {
                // Pretend success but keep the old text: a provider that silently drops
                // the write. The product must not trust SetValue and must confirm by read.
                return (IntPtr)1;
            }
            if (Mode == 1 && second != IntPtr.Zero)
            {
                string incoming = Marshal.PtrToStringUni(second);
                int capacity = Capacity;
                if (incoming != null && capacity > 0 && incoming.Length > capacity)
                {
                    IntPtr buffer = Marshal.StringToHGlobalUni(incoming.Substring(0, capacity));
                    try { return CallWindowProcW(originalProc, window, message, first, buffer); }
                    finally { Marshal.FreeHGlobal(buffer); }
                }
            }
        }
        if (message == WM_PASTE) Interlocked.Increment(ref PasteCalls);
        return CallWindowProcW(originalProc, window, message, first, second);
    }

    private static IntPtr Edit(Form owner, int id, int x, int y, int width, int height, string value)
    {
        IntPtr result = CreateWindowExW(0, "EDIT", value, 0x50B11044, x, y, width, height,
            owner.Handle, (IntPtr)id, IntPtr.Zero, IntPtr.Zero);
        if (result == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        hooks.Add(new EditObserver(result));
        return result;
    }

    public static void Start()
    {
        var ready = new ManualResetEvent(false); Exception failure = null;
        thread = new Thread(delegate() {
            try {
                primary = new Form { Text = "Listen #11 QA - native target", Width = 900, Height = 480,
                    StartPosition = FormStartPosition.Manual, Location = new Point(60, 100), TopMost = true };
                secondary = new Form { Text = "Listen #11 QA - other window", Width = 900, Height = 300,
                    StartPosition = FormStartPosition.Manual, Location = new Point(220, 180), TopMost = true };
                var send = new Button { Text = "manual send (never clicked)", Left = 20, Top = 355, Width = 300, Height = 30 };
                var neutral = new Button { Text = "neutral button: no editable target", Left = 340, Top = 355, Width = 320, Height = 30 };
                send.Click += delegate { Interlocked.Increment(ref Sends); };
                primary.Controls.Add(send);
                primary.Controls.Add(neutral);
                primary.Show(); secondary.Show();
                Primary = primary.Handle; Secondary = secondary.Handle;
                SendButton = send.Handle; NoTargetButton = neutral.Handle;
                Target = Edit(primary, 401, 20, 25, 840, 150, TargetDraft);
                Neighbor = Edit(primary, 402, 20, 190, 840, 110, NeighborDraft);
                Other = Edit(secondary, 403, 20, 25, 840, 150, OtherDraft);
                var proc = new WindowProc(FixtureProc);
                delegates.Add(proc);
                originalProc = SetWindowLongPtrW(Target, GWLP_WNDPROC, Marshal.GetFunctionPointerForDelegate(proc));
                if (originalProc == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
                ready.Set();
                Application.Run(primary);
            } catch (Exception error) { failure = error; ready.Set(); }
            finally { if (secondary != null && !secondary.IsDisposed) secondary.Dispose(); }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.IsBackground = true; thread.Start();
        if (!ready.WaitOne(5000)) throw new TimeoutException("Package fixture did not start.");
        if (failure != null) throw failure;
    }

    public static void SetMode(int mode) { Mode = mode; }
    public static void SetCapacity(int characters) { Capacity = characters; }
    public static void SetPasteCalls(int count) { PasteCalls = count; }
    public static void ResetKeyCounts() { AKeys = 0; CKeys = 0; VKeys = 0; PasteCalls = 0; lock (eventLog) { eventLog.Clear(); } }
    public static string EventLog() { lock (eventLog) { return string.Join(",", eventLog.ToArray()); } }

    public static void ResetTarget(string value)
    {
        int savedMode = Mode; int savedCapacity = Capacity;
        Mode = 0; Capacity = 0;
        IntPtr buffer = Marshal.StringToHGlobalUni(value);
        try { SendMessageW(Target, WM_SETTEXT, IntPtr.Zero, buffer); }
        finally { Marshal.FreeHGlobal(buffer); Mode = savedMode; Capacity = savedCapacity; }
    }

    public static void SetReadOnly(bool readOnly)
    {
        SendMessageW(Target, EM_SETREADONLY, (IntPtr)(readOnly ? 1 : 0), IntPtr.Zero);
    }

    public static void DestroyTarget()
    {
        if (primary == null || primary.IsDisposed) throw new InvalidOperationException("The fixture window is not available.");
        primary.Invoke((MethodInvoker)delegate {
            if (Target != IntPtr.Zero) { DestroyWindow(Target); Target = IntPtr.Zero; }
        });
    }

    public static string Text(IntPtr window)
    {
        int length = GetWindowTextLengthW(window);
        var text = new StringBuilder(length + 1);
        if (GetWindowTextW(window, text, text.Capacity) != length) throw new InvalidOperationException("Could not read the complete native text.");
        return text.ToString();
    }

    public static void Hide(IntPtr window) { ShowWindow(window, 0); }

    public static void Close()
    {
        if (primary != null && !primary.IsDisposed) primary.Invoke((MethodInvoker)delegate { primary.Close(); });
        if (thread != null && !thread.Join(5000)) throw new TimeoutException("Package fixture did not close.");
    }
}
