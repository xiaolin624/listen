[CmdletBinding()]
param(
    [string]$WorkspaceId = '',
    [switch]$ShowStatus,
    [switch]$RemoveKey,
    [switch]$SelfTest
)
$ErrorActionPreference = 'Stop'
if (@($ShowStatus, $RemoveKey, $SelfTest).Where({ $_ }).Count -gt 1) {
    throw 'Choose only one of -ShowStatus, -RemoveKey or -SelfTest.'
}
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is unavailable.' }

if (-not ('ListenCloudCredentialStore' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Security;
using System.Text;
public static class ListenCloudCredentialStore {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct Credential {
        public uint Flags, Type;
        [MarshalAs(UnmanagedType.LPWStr)] public string TargetName;
        [MarshalAs(UnmanagedType.LPWStr)] public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist, AttributeCount;
        public IntPtr Attributes;
        [MarshalAs(UnmanagedType.LPWStr)] public string TargetAlias;
        [MarshalAs(UnmanagedType.LPWStr)] public string UserName;
    }
    [DllImport("advapi32.dll", EntryPoint="CredWriteW", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool Write(ref Credential credential, uint flags);
    [DllImport("advapi32.dll", EntryPoint="CredReadW", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool Read(string target, uint type, uint flags, out IntPtr credential);
    [DllImport("advapi32.dll", EntryPoint="CredDeleteW", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool Delete(string target, uint type, uint flags);
    [DllImport("advapi32.dll")] private static extern void CredFree(IntPtr credential);
    public static bool Exists(string target) {
        IntPtr pointer;
        if (!Read(target, 1, 0, out pointer)) {
            if (Marshal.GetLastWin32Error() == 1168) return false;
            throw new InvalidOperationException("Windows Credential Manager could not be read.");
        }
        CredFree(pointer);
        return true;
    }
    public static void Remove(string target) {
        if (!Delete(target, 1, 0) && Marshal.GetLastWin32Error() != 1168)
            throw new InvalidOperationException("Windows Credential Manager could not remove the key.");
    }
    public static void Store(string target, SecureString secret) {
        if (secret == null || secret.Length == 0 || secret.Length > 4096)
            throw new InvalidOperationException("Enter a non-empty API key of at most 4096 ASCII characters.");
        IntPtr clear = IntPtr.Zero, blob = IntPtr.Zero;
        char[] chars = new char[secret.Length];
        byte[] bytes = null;
        try {
            clear = Marshal.SecureStringToBSTR(secret);
            for (int i=0; i<chars.Length; i++) {
                chars[i] = (char)Marshal.ReadInt16(clear, i*2);
                if (chars[i] < 33 || chars[i] > 126)
                    throw new InvalidOperationException("The API key must contain only printable ASCII without spaces or line breaks.");
            }
            bytes = Encoding.UTF8.GetBytes(chars);
            blob = Marshal.AllocCoTaskMem(bytes.Length);
            Marshal.Copy(bytes, 0, blob, bytes.Length);
            Credential item = new Credential { Type=1, TargetName=target, CredentialBlobSize=(uint)bytes.Length,
                CredentialBlob=blob, Persist=2, UserName="Listen cloud trial" };
            if (!Write(ref item, 0)) throw new InvalidOperationException("Windows Credential Manager could not store the key.");
        } finally {
            Array.Clear(chars, 0, chars.Length);
            if (bytes != null) Array.Clear(bytes, 0, bytes.Length);
            if (blob != IntPtr.Zero) {
                for (int i=0; i<secret.Length; i++) Marshal.WriteByte(blob, i, 0);
                Marshal.FreeCoTaskMem(blob);
            }
            if (clear != IntPtr.Zero) Marshal.ZeroFreeBSTR(clear);
        }
    }
    public static void SelfTest() {
        string target = "Listen/CloudTrial/OfflineSelfTest/" + Guid.NewGuid().ToString();
        using (SecureString secret = new SecureString()) {
            foreach (char value in "offline-credential-self-test") secret.AppendChar(value);
            try {
                Store(target, secret);
                IntPtr pointer;
                if (!Read(target, 1, 0, out pointer)) throw new InvalidOperationException("Credential self-test read failed.");
                try {
                    Credential item = (Credential)Marshal.PtrToStructure(pointer, typeof(Credential));
                    byte[] actual = new byte[item.CredentialBlobSize];
                    Marshal.Copy(item.CredentialBlob, actual, 0, actual.Length);
                    if (Encoding.UTF8.GetString(actual) != "offline-credential-self-test")
                        throw new InvalidOperationException("Credential self-test readback differed.");
                    Array.Clear(actual, 0, actual.Length);
                } finally { CredFree(pointer); }
            } finally { Remove(target); }
            if (Exists(target)) throw new InvalidOperationException("Credential self-test cleanup failed.");
        }
    }
}
'@
}

$target = 'Listen/CloudTrial/Alibaba/cn-beijing'
$directory = Join-Path $env:LOCALAPPDATA 'Listen\cloud-trial'
$configuration = Join-Path $directory 'ali.json'
$ledgerPath = Join-Path $directory 'ledger.json'
if ($SelfTest) {
    [ListenCloudCredentialStore]::SelfTest()
    Write-Output 'Credential Manager self-test passed; temporary entry removed. No network or paid call was made.'
    return
}
if ($RemoveKey) {
    [ListenCloudCredentialStore]::Remove($target)
    Write-Output 'The Listen Alibaba trial credential was removed. The shared cost ledger was preserved.'
    return
}
if ($ShowStatus) {
    Write-Output 'Provider: Alibaba Model Studio (explicit trial only)'
    Write-Output 'Model: qwen-audio-3.1-asr-flash-message; region: cn-beijing'
    Write-Output ('Credential Manager: ' + $(if ([ListenCloudCredentialStore]::Exists($target)) { 'configured' } else { 'missing' }))
    Write-Output ('Process key injection: ' + $(if (Test-Path Env:LISTEN_ALI_API_KEY) { 'present (takes priority; value omitted)' } else { 'absent' }))
    Write-Output ('Nonsecret configuration: ' + $(if (Test-Path -LiteralPath $configuration) { $configuration } else { 'missing; run this helper without -ShowStatus' }))
    if (Test-Path -LiteralPath $ledgerPath) {
        try {
            $ledger = Get-Content -LiteralPath $ledgerPath -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($ledger.version -ne 1 -or $ledger.cap_nanoyuan -ne 50000000000) { throw 'Invalid ledger.' }
            [decimal]$committed = 0
            foreach ($attempt in $ledger.attempts) {
                if ($attempt.final_usage -and $null -ne $attempt.estimated_nanoyuan) { $committed += [decimal]$attempt.estimated_nanoyuan }
                else { $committed += [decimal]$attempt.reserved_nanoyuan }
            }
            Write-Output ('Shared two-provider trial commitment: CNY {0:F9} / 50; frozen={1}' -f ($committed / 1000000000), $ledger.frozen)
        } catch { throw 'The shared ledger is unreadable or invalid; preserve it. The application will refuse new paid tasks.' }
    } else { Write-Output 'Shared two-provider trial ledger: no task reserved yet (cap CNY 50).' }
    Write-Output 'Production requests remain blocked: Message has no verified per-task input/output token upper bounds. No key or console budget setting bypasses this protection.'
    return
}

if (-not $WorkspaceId) { $WorkspaceId = Read-Host 'Alibaba Beijing Workspace ID (not the API key)' }
if ($WorkspaceId -cnotmatch '^[A-Za-z0-9-]{1,64}$') { throw 'Workspace ID must contain 1-64 ASCII letters, digits or hyphens.' }
$secret = Read-Host 'Alibaba Beijing API key (hidden; never pass it as a command argument)' -AsSecureString
try { [ListenCloudCredentialStore]::Store($target, $secret) }
finally { if ($secret) { $secret.Dispose() } }
[IO.Directory]::CreateDirectory($directory) | Out-Null
$settings = [ordered]@{ workspace_id=$WorkspaceId; model='qwen-audio-3.1-asr-flash-message'; region='cn-beijing' }
$bytes = [Text.UTF8Encoding]::new($false).GetBytes(($settings | ConvertTo-Json))
$temporary = Join-Path $directory ('ali.' + [Guid]::NewGuid().ToString('N') + '.tmp')
try {
    $file = [IO.FileStream]::new($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $file.Write($bytes, 0, $bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
    if (Test-Path -LiteralPath $configuration) { [IO.File]::Replace($temporary, $configuration, $null) }
    else { [IO.File]::Move($temporary, $configuration) }
} finally { if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force } }
Write-Output 'Alibaba trial configured in Windows Credential Manager for the current user. Restart Listen to use it.'
Write-Output 'Production calls remain blocked until a defensible Message cost bound is verified; the default local engine is unchanged.'
