using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

// Task-owned audio-route driver for the issue-8 acceptance. It reads and changes the
// real Windows default render endpoints through the documented MMDeviceEnumerator and
// the Windows policy COM server, and renders a known WAV directly to a named real
// endpoint (the controlled substitute for an application routed away from the default).
public static class AudioRoute
{
    private const int CLSCTX_ALL = 23;
    private const int STGM_READ = 0;
    private const int eRender = 0;
    private const int eMultimedia = 1;
    private const int eCommunications = 2;
    private const int eConsole = 0;
    private const int DEVICE_STATE_ACTIVE = 1;
    private const int AUDCLNT_SHAREMODE_SHARED = 0;
    private const ushort WAVE_FORMAT_PCM = 1;
    private const ushort WAVE_FORMAT_IEEE_FLOAT = 3;
    private const ushort WAVE_FORMAT_EXTENSIBLE = 0xfffe;

    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    private class MMDeviceEnumeratorComObject { }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(int dataFlow, int stateMask, out IMMDeviceCollection devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice device);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out IMMDevice device);
        [PreserveSig] int RegisterEndpointNotificationCallback(IntPtr client);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IntPtr client);
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceCollection
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int Item(uint index, out IMMDevice device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object instance);
        [PreserveSig] int OpenPropertyStore(int stgmAccess, out IPropertyStore properties);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out int state);
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        [PreserveSig] int GetCount(out uint count);
        [PreserveSig] int GetAt(uint index, out PROPERTYKEY key);
        [PreserveSig] int GetValue(ref PROPERTYKEY key, out PROPVARIANT value);
        [PreserveSig] int SetValue(ref PROPERTYKEY key, ref PROPVARIANT value);
        [PreserveSig] int Commit();
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROPERTYKEY
    {
        public Guid fmtid;
        public uint pid;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct PROPVARIANT
    {
        [FieldOffset(0)] public ushort vt;
        [FieldOffset(2)] public ushort reserved1;
        [FieldOffset(4)] public ushort reserved2;
        [FieldOffset(6)] public ushort reserved3;
        [FieldOffset(8)] public IntPtr pointer;
        [FieldOffset(8)] public long longValue;
    }

    [DllImport("ole32.dll")]
    private static extern int PropVariantClear(ref PROPVARIANT value);

    [ComImport, Guid("870AF99C-171D-4F9E-AF0D-E63DF40C2BC9")]
    private class CPolicyConfigClient { }

    [ComImport, Guid("F8679F50-850A-41CF-9C72-430F290290C8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPolicyConfig
    {
        [PreserveSig] int GetMixFormat([MarshalAs(UnmanagedType.LPWStr)] string deviceName, out IntPtr format);
        [PreserveSig] int GetDeviceFormat([MarshalAs(UnmanagedType.LPWStr)] string deviceName, bool defaultFormat, out IntPtr format);
        [PreserveSig] int ResetDeviceFormat([MarshalAs(UnmanagedType.LPWStr)] string deviceName);
        [PreserveSig] int SetDeviceFormat([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr endpointFormat, IntPtr mixFormat);
        [PreserveSig] int GetProcessingPeriod([MarshalAs(UnmanagedType.LPWStr)] string deviceName, bool defaultPeriod, IntPtr defaultPeriodPtr, IntPtr minimumPeriodPtr);
        [PreserveSig] int SetProcessingPeriod([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr period);
        [PreserveSig] int GetShareMode([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr mode);
        [PreserveSig] int SetShareMode([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr mode);
        [PreserveSig] int GetPropertyValue([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr key, IntPtr value);
        [PreserveSig] int SetPropertyValue([MarshalAs(UnmanagedType.LPWStr)] string deviceName, IntPtr key, IntPtr value);
        [PreserveSig] int SetDefaultEndpoint([MarshalAs(UnmanagedType.LPWStr)] string deviceName, int role);
        [PreserveSig] int SetEndpointVisibility([MarshalAs(UnmanagedType.LPWStr)] string deviceName, bool visible);
    }

    [ComImport, Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioClient
    {
        [PreserveSig] int Initialize(int shareMode, int streamFlags, long bufferDuration, long periodicity, IntPtr format, IntPtr sessionGuid);
        [PreserveSig] int GetBufferSize(out uint frames);
        [PreserveSig] int GetStreamLatency(out long latency);
        [PreserveSig] int GetCurrentPadding(out uint padding);
        [PreserveSig] int IsFormatSupported(int shareMode, IntPtr format, out IntPtr closest);
        [PreserveSig] int GetMixFormat(out IntPtr format);
        [PreserveSig] int GetDevicePeriod(out long defaultPeriod, out long minimumPeriod);
        [PreserveSig] int Start();
        [PreserveSig] int Stop();
        [PreserveSig] int Reset();
        [PreserveSig] int SetEventHandle(IntPtr handle);
        [PreserveSig] int GetService(ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object service);
    }

    [ComImport, Guid("F294ACFC-3146-4483-A7BF-ADDCA7C260E2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioRenderClient
    {
        [PreserveSig] int GetBuffer(uint frames, out IntPtr data);
        [PreserveSig] int ReleaseBuffer(uint frames, int flags);
    }

    [StructLayout(LayoutKind.Sequential, Pack = 1)]
    private struct WAVEFORMATEX
    {
        public ushort wFormatTag;
        public ushort nChannels;
        public uint nSamplesPerSec;
        public uint nAvgBytesPerSec;
        public ushort nBlockAlign;
        public ushort wBitsPerSample;
        public ushort cbSize;
    }

    [StructLayout(LayoutKind.Sequential, Pack = 1)]
    private struct WAVEFORMATEXTENSIBLE
    {
        public WAVEFORMATEX Format;
        public ushort wValidBitsPerSample;
        public uint dwChannelMask;
        public Guid SubFormat;
    }

    [DllImport("ole32.dll")]
    private static extern int CoInitializeEx(IntPtr reserved, uint coInit);

    [DllImport("ole32.dll")]
    private static extern void CoUninitialize();

    private static readonly PROPERTYKEY PKEY_Device_FriendlyName = new PROPERTYKEY
    {
        fmtid = new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"),
        pid = 14,
    };

    public sealed class EndpointInfo
    {
        public string Role;
        public string Id;
        public string Name;
        public override string ToString() { return Role + "='" + Name + "' (" + Id + ")"; }
    }

    private static IMMDeviceEnumerator Enumerator()
    {
        return (IMMDeviceEnumerator)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")));
    }

    private static string FriendlyName(IMMDevice device)
    {
        IPropertyStore store;
        int hr = device.OpenPropertyStore(STGM_READ, out store);
        if (hr != 0) return "unknown endpoint name";
        try
        {
            PROPERTYKEY key = PKEY_Device_FriendlyName;
            PROPVARIANT value;
            hr = store.GetValue(ref key, out value);
            if (hr != 0) return "unknown endpoint name";
            try
            {
                if (value.vt == 31 && value.pointer != IntPtr.Zero)
                    return Marshal.PtrToStringUni(value.pointer);
                return "unknown endpoint name";
            }
            finally { PropVariantClear(ref value); }
        }
        finally { Marshal.ReleaseComObject(store); }
    }

    private static EndpointInfo Describe(IMMDevice device, string role)
    {
        string id;
        int hr = device.GetId(out id);
        if (hr != 0) Marshal.ThrowExceptionForHR(hr);
        return new EndpointInfo { Role = role, Id = id, Name = FriendlyName(device) };
    }

    private static EndpointInfo Default(int role, string label)
    {
        IMMDeviceEnumerator enumerator = Enumerator();
        try
        {
            IMMDevice device;
            int hr = enumerator.GetDefaultAudioEndpoint(eRender, role, out device);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            return Describe(device, label);
        }
        finally { Marshal.ReleaseComObject(enumerator); }
    }

    public static EndpointInfo Multimedia() { return Default(eMultimedia, "multimedia"); }
    public static EndpointInfo Communications() { return Default(eCommunications, "communications"); }
    public static EndpointInfo Console() { return Default(eConsole, "console"); }

    public static List<EndpointInfo> ActiveRenderEndpoints()
    {
        var result = new List<EndpointInfo>();
        IMMDeviceEnumerator enumerator = Enumerator();
        try
        {
            IMMDeviceCollection collection;
            int hr = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE, out collection);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            try
            {
                uint count;
                collection.GetCount(out count);
                for (uint index = 0; index < count; index++)
                {
                    IMMDevice device;
                    if (collection.Item(index, out device) == 0)
                        result.Add(Describe(device, "render"));
                }
            }
            finally { Marshal.ReleaseComObject(collection); }
        }
        finally { Marshal.ReleaseComObject(enumerator); }
        return result;
    }

    public static void SetDefault(string deviceId, int role)
    {
        IPolicyConfig policy = (IPolicyConfig)Activator.CreateInstance(
            Type.GetTypeFromCLSID(new Guid("870AF99C-171D-4F9E-AF0D-E63DF40C2BC9")));
        try
        {
            int hr = policy.SetDefaultEndpoint(deviceId, role);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
        }
        finally { Marshal.ReleaseComObject(policy); }
    }

    private static double[] ReadMonoAsDouble(string path, out int sampleRate)
    {
        byte[] bytes = File.ReadAllBytes(path);
        if (bytes.Length < 44 || bytes[0] != (byte)'R' || bytes[1] != (byte)'I'
            || bytes[2] != (byte)'F' || bytes[3] != (byte)'F')
            throw new InvalidDataException("Not a RIFF WAV: " + path);
        int position = 12;
        ushort format = 0, channels = 0, bits = 0;
        sampleRate = 0;
        double[] samples = null;
        while (position + 8 <= bytes.Length)
        {
            string id = "" + (char)bytes[position] + (char)bytes[position + 1]
                + (char)bytes[position + 2] + (char)bytes[position + 3];
            int size = BitConverter.ToInt32(bytes, position + 4);
            int data = position + 8;
            if (id == "fmt ")
            {
                format = BitConverter.ToUInt16(bytes, data);
                channels = BitConverter.ToUInt16(bytes, data + 2);
                sampleRate = BitConverter.ToInt32(bytes, data + 4);
                bits = BitConverter.ToUInt16(bytes, data + 14);
            }
            else if (id == "data")
            {
                if (format != WAVE_FORMAT_PCM || channels == 0 || bits != 16)
                    throw new InvalidDataException("Only 16-bit PCM WAV is supported for the acceptance player.");
                int count = Math.Min(size, bytes.Length - data) / 2;
                samples = new double[count];
                for (int i = 0; i < count; i++)
                {
                    short value = BitConverter.ToInt16(bytes, data + i * 2);
                    samples[i] = value / 32768.0;
                }
                break;
            }
            position = data + size + (size & 1);
        }
        if (samples == null || samples.Length == 0) throw new InvalidDataException("WAV has no PCM data: " + path);
        if (channels > 1)
        {
            int frames = samples.Length / channels;
            var mono = new double[frames];
            for (int frame = 0; frame < frames; frame++)
            {
                double sum = 0;
                for (int channel = 0; channel < channels; channel++) sum += samples[frame * channels + channel];
                mono[frame] = sum / channels;
            }
            samples = mono;
        }
        return samples;
    }

    private static double[] Resample(double[] input, int sourceRate, int targetRate)
    {
        if (sourceRate == targetRate) return input;
        int outputLength = (int)Math.Round(input.Length * (double)targetRate / sourceRate);
        var output = new double[outputLength];
        double step = (double)sourceRate / targetRate;
        for (int i = 0; i < outputLength; i++)
        {
            double source = i * step;
            int left = (int)source;
            int right = Math.Min(left + 1, input.Length - 1);
            double fraction = source - left;
            output[i] = input[left] * (1 - fraction) + input[right] * fraction;
        }
        return output;
    }

    public static double WavSeconds(string path)
    {
        int rate;
        double[] samples = ReadMonoAsDouble(path, out rate);
        return (double)samples.Length / rate;
    }

    private static void PlayToEndpoint(string deviceId, string wavPath)
    {
        CoInitializeEx(IntPtr.Zero, 0);
        IMMDeviceEnumerator enumerator = null;
        IMMDevice device = null;
        object clientObject = null;
        object renderObject = null;
        try
        {
            enumerator = Enumerator();
            int hr = enumerator.GetDevice(deviceId, out device);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            Guid iid = new Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2");
            object activated;
            hr = device.Activate(ref iid, CLSCTX_ALL, IntPtr.Zero, out activated);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            clientObject = activated;
            var client = (IAudioClient)activated;
            IntPtr formatPointer;
            hr = client.GetMixFormat(out formatPointer);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            try
            {
                var format = (WAVEFORMATEX)Marshal.PtrToStructure(formatPointer, typeof(WAVEFORMATEX));
                ushort effectiveFormat = format.wFormatTag;
                if (format.wFormatTag == WAVE_FORMAT_EXTENSIBLE)
                {
                    var extensible = (WAVEFORMATEXTENSIBLE)Marshal.PtrToStructure(formatPointer, typeof(WAVEFORMATEXTENSIBLE));
                    if (extensible.SubFormat == new Guid("00000003-0000-0010-8000-00aa00389b71")) effectiveFormat = WAVE_FORMAT_IEEE_FLOAT;
                    else if (extensible.SubFormat == new Guid("00000001-0000-0010-8000-00aa00389b71")) effectiveFormat = WAVE_FORMAT_PCM;
                }
                if (format.nChannels == 0 || format.nSamplesPerSec == 0)
                    throw new InvalidOperationException("The endpoint mix format is invalid.");
                if (effectiveFormat == WAVE_FORMAT_IEEE_FLOAT && format.wBitsPerSample == 32) { }
                else if (effectiveFormat == WAVE_FORMAT_PCM && format.wBitsPerSample == 16) { }
                else throw new InvalidOperationException("Unsupported endpoint mix format: tag=" + effectiveFormat
                    + " bits=" + format.wBitsPerSample + " channels=" + format.nChannels + " rate=" + format.nSamplesPerSec);

                hr = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2000000, 0, formatPointer, IntPtr.Zero);
                if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                uint bufferFrames;
                hr = client.GetBufferSize(out bufferFrames);
                if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                Guid renderIid = new Guid("F294ACFC-3146-4483-A7BF-ADDCA7C260E2");
                object renderService;
                hr = client.GetService(ref renderIid, out renderService);
                if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                renderObject = renderService;
                var render = (IAudioRenderClient)renderService;

                int sourceRate;
                double[] source = ReadMonoAsDouble(wavPath, out sourceRate);
                double[] samples = Resample(source, sourceRate, (int)format.nSamplesPerSec);
                int channels = format.nChannels;
                int bytesPerSample = format.wBitsPerSample / 8;
                int blockAlign = bytesPerSample * channels;
                hr = client.Start();
                if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                int offset = 0;
                while (offset < samples.Length)
                {
                    uint padding;
                    hr = client.GetCurrentPadding(out padding);
                    if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                    int available = (int)bufferFrames - (int)padding;
                    if (available <= 0)
                    {
                        Thread.Sleep(5);
                        continue;
                    }
                    int frames = Math.Min(available, samples.Length - offset);
                    IntPtr data;
                    hr = render.GetBuffer((uint)frames, out data);
                    if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                    byte[] buffer = new byte[frames * blockAlign];
                    for (int frame = 0; frame < frames; frame++)
                    {
                        double value = Math.Max(-1.0, Math.Min(1.0, samples[offset + frame]));
                        for (int channel = 0; channel < channels; channel++)
                        {
                            int index = frame * blockAlign + channel * bytesPerSample;
                            if (effectiveFormat == WAVE_FORMAT_IEEE_FLOAT)
                                BitConverter.GetBytes((float)value).CopyTo(buffer, index);
                            else
                                BitConverter.GetBytes((short)(value * 32767.0)).CopyTo(buffer, index);
                        }
                    }
                    Marshal.Copy(buffer, 0, data, buffer.Length);
                    hr = render.ReleaseBuffer((uint)frames, 0);
                    if (hr != 0) Marshal.ThrowExceptionForHR(hr);
                    offset += frames;
                    Thread.Sleep((int)Math.Max(1, frames * 1000.0 / format.nSamplesPerSec * 0.8));
                }
                Thread.Sleep(300);
                client.Stop();
            }
            finally { Marshal.FreeCoTaskMem(formatPointer); }
        }
        finally
        {
            if (renderObject != null) Marshal.ReleaseComObject(renderObject);
            if (clientObject != null) Marshal.ReleaseComObject(clientObject);
            if (device != null) Marshal.ReleaseComObject(device);
            if (enumerator != null) Marshal.ReleaseComObject(enumerator);
            CoUninitialize();
        }
    }

    private static readonly object Gate = new object();
    private static Task playback;
    private static ManualResetEventSlim playbackDone;
    private static volatile string playbackError;

    public static void StartPlayback(string deviceId, string wavPath)
    {
        lock (Gate)
        {
            if (playback != null && !playback.IsCompleted) throw new InvalidOperationException("A QA playback is already running.");
            playbackError = null;
            playbackDone = new ManualResetEventSlim(false);
            playback = Task.Run(() =>
            {
                try { PlayToEndpoint(deviceId, wavPath); }
                catch (Exception error) { playbackError = error.ToString(); }
                finally { playbackDone.Set(); }
            });
        }
    }

    public static bool WaitPlayback(int milliseconds)
    {
        ManualResetEventSlim done;
        lock (Gate) done = playbackDone;
        return done != null && done.Wait(milliseconds);
    }

    public static string PlaybackError { get { return playbackError; } }
}
