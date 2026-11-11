[CmdletBinding()]
param([switch]$CheckOnly)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$gitPath = Join-Path $projectRoot '.git'
$repairRoot = Join-Path $projectRoot '.scratch\environment-repair'
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\GitHub CLI'
$portableGh = Join-Path $installRoot 'bin\gh.exe'
$initialGhCommand = Get-Command gh -ErrorAction SilentlyContinue
$initialProcessPath = $env:Path

if (-not ('EnvironmentRepair.NativeAccess' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace EnvironmentRepair {
    public static class NativeAccess {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        private static extern IntPtr CreateFileW(string path, uint access,
            uint share, IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll")]
        private static extern bool CloseHandle(IntPtr handle);
        public static int CheckDaclWrite(string path) {
            IntPtr handle = CreateFileW(path, 0x60000, 7, IntPtr.Zero, 3,
                0x02000000, IntPtr.Zero);
            if (handle == new IntPtr(-1)) return Marshal.GetLastWin32Error();
            CloseHandle(handle);
            return 0;
        }
    }
}
'@
}

function Find-Gh {
    $command = Get-Command gh.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in @($portableGh,
        "$env:ProgramFiles\GitHub CLI\gh.exe",
        "${env:ProgramFiles(x86)}\GitHub CLI\gh.exe",
        "$env:LOCALAPPDATA\Microsoft\WinGet\Links\gh.exe")) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    return $null
}

function Test-Environment {
    $failed = $false
    $ghExe = Find-Gh
    if ($ghExe) {
        & $ghExe --version
        if ($LASTEXITCODE -ne 0) { $failed = $true }
    } else { Write-Output 'FAIL: GitHub CLI executable not found'; $failed = $true }
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Write-Output 'FAIL: gh cannot be resolved through process PATH'
        $failed = $true
    }
    if (Test-Path -LiteralPath $gitPath -PathType Container) {
        $accessError = [EnvironmentRepair.NativeAccess]::CheckDaclWrite($gitPath)
        Write-Output "Git metadata READ_CONTROL/WRITE_DAC error: $accessError"
        if ($accessError -ne 0) { $failed = $true }
        & git -C $projectRoot status --short
        if ($LASTEXITCODE -ne 0) { $failed = $true }
    } else { Write-Output 'FAIL: Git metadata missing'; $failed = $true }
    return (-not $failed)
}

if ($CheckOnly) {
    $results = @(Test-Environment)
    $passed = $results[-1]
    $results | Select-Object -SkipLast 1 | Write-Output
    if (-not $passed) { exit 1 }
    exit 0
}

$backupRoot = Join-Path $repairRoot ('backups\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
$null = New-Item -ItemType Directory -Path $backupRoot -Force
$utf8 = [Text.UTF8Encoding]::new($false)
$previousUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$before = @{ userPath = $previousUserPath; gitOwner = $null }
if (Test-Path -LiteralPath $gitPath -PathType Container) {
    $before.gitOwner = (Get-Acl -LiteralPath $gitPath).Owner
}
[IO.File]::WriteAllText((Join-Path $backupRoot 'before.json'), ($before | ConvertTo-Json), $utf8)

if ((Test-Path -LiteralPath $gitPath -PathType Container) -and
    ([EnvironmentRepair.NativeAccess]::CheckDaclWrite($gitPath) -ne 0)) {
    if ((Get-Item -Force -LiteralPath $gitPath).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Refusing to move linked Git metadata.'
    }
    $refFiles = @(Get-ChildItem -LiteralPath (Join-Path $gitPath 'refs') -Recurse -File)
    $objectFiles = @(Get-ChildItem -LiteralPath (Join-Path $gitPath 'objects') -Recurse -File)
    if ($refFiles.Count -or $objectFiles.Count -or
        (Test-Path -LiteralPath (Join-Path $gitPath 'index')) -or
        (Test-Path -LiteralPath (Join-Path $gitPath 'packed-refs'))) {
        throw 'Repository contains history or staged work; automatic metadata replacement stopped.'
    }
    $backupGit = [IO.Path]::GetFullPath((Join-Path $backupRoot 'git-metadata'))
    $projectPrefix = $projectRoot.TrimEnd('\') + '\'
    if (-not $backupGit.StartsWith($projectPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFullPath($gitPath) -ne ($projectPrefix + '.git')) {
        throw 'Git backup path escaped the project.'
    }
    Move-Item -LiteralPath $gitPath -Destination $backupGit
    & git init -b main $projectRoot
    if ($LASTEXITCODE -ne 0) { throw "git init failed; original metadata retained at $backupGit" }
    Copy-Item -LiteralPath (Join-Path $backupGit 'config') -Destination (Join-Path $gitPath 'config')
    Write-Output "Original Git metadata preserved: $backupGit"
}

$remotes = @(& git -C $projectRoot remote)
if ($LASTEXITCODE -ne 0) { throw 'Could not read Git remotes.' }
if ('origin' -notin $remotes) {
    & git -C $projectRoot remote add origin https://github.com/xiaolin624/listen.git
    if ($LASTEXITCODE -ne 0) { throw 'Could not attach GitHub origin.' }
} else {
    $existingOrigin = & git -C $projectRoot remote get-url origin
    if ($LASTEXITCODE -ne 0) { throw 'Could not read origin URL.' }
    if ($existingOrigin.TrimEnd('/') -notin @('https://github.com/xiaolin624/listen', 'https://github.com/xiaolin624/listen.git')) {
        throw "Unexpected origin retained: $existingOrigin"
    }
}

$excludePath = Join-Path $gitPath 'info\exclude'
$excludeText = [IO.File]::ReadAllText($excludePath)
if ($excludeText -notmatch '(?m)^/\.scratch/environment-repair/\s*$') {
    [IO.File]::AppendAllText($excludePath, "`n/.scratch/environment-repair/`n", $utf8)
}

$ghExe = Find-Gh
if (-not $ghExe) {
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/cli/cli/releases/latest' -TimeoutSec 45
    $architecture = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } elseif ([Environment]::Is64BitOperatingSystem) { 'amd64' } else { '386' }
    $asset = $release.assets | Where-Object { $_.name -match "_windows_$architecture\.zip$" } | Select-Object -First 1
    $checksumAsset = $release.assets | Where-Object { $_.name -match 'checksums\.txt$' } | Select-Object -First 1
    if (-not $asset -or -not $checksumAsset) { throw 'Official CLI release assets not found.' }
    $zipPath = Join-Path $backupRoot $asset.name
    $checksumsPath = Join-Path $backupRoot $checksumAsset.name
    Invoke-WebRequest -UseBasicParsing -Uri $asset.browser_download_url -OutFile $zipPath -TimeoutSec 120
    Invoke-WebRequest -UseBasicParsing -Uri $checksumAsset.browser_download_url -OutFile $checksumsPath -TimeoutSec 45
    $checksumLine = Get-Content -LiteralPath $checksumsPath | Where-Object { $_ -match ('\s+\*?' + [regex]::Escape($asset.name) + '$') } | Select-Object -First 1
    if (-not $checksumLine) { throw 'CLI archive checksum was not published.' }
    $expectedHash = ($checksumLine -split '\s+')[0]
    if ((Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'CLI archive checksum verification failed.'
    }
    if ($asset.digest -and $asset.digest.StartsWith('sha256:') -and
        $expectedHash -ne $asset.digest.Substring(7)) { throw 'Release asset digest mismatch.' }
    $null = New-Item -ItemType Directory -Path $installRoot -Force
    Expand-Archive -LiteralPath $zipPath -DestinationPath $installRoot -Force
    $ghExe = Find-Gh
    if (-not $ghExe) { throw 'CLI archive extracted but gh.exe was not found.' }
}

$ghDirectory = Split-Path -Parent $ghExe
if (@($previousUserPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $ghDirectory }).Count -eq 0) {
    $updatedUserPath = $previousUserPath.TrimEnd(';') + ';' + $ghDirectory + ';'
    [Environment]::SetEnvironmentVariable('Path', $updatedUserPath, 'User')
}

# Existing applications keep their old PATH. A normal command shim in the
# user's existing npm command directory lets those applications find gh too.
if (-not $initialGhCommand) {
    $commandDirectory = Join-Path $env:APPDATA 'npm'
    $directoryWasOnPath = @($initialProcessPath -split ';' | Where-Object {
        $_.TrimEnd('\') -ieq $commandDirectory
    }).Count -gt 0
    $shimPath = Join-Path $commandDirectory 'gh.cmd'
    if ($directoryWasOnPath -and (Test-Path -LiteralPath $commandDirectory -PathType Container) -and
        -not (Test-Path -LiteralPath $shimPath)) {
        [IO.File]::WriteAllText($shimPath, "@echo off`r`n`"$ghExe`" %*`r`n", $utf8)
        Write-Output "Compatibility command for existing terminals: $shimPath"
    }
}
$env:Path = $ghDirectory + ';' + $env:Path
Write-Output "GitHub CLI: $ghExe"
Write-Output "Repair backup: $backupRoot"
$results = @(Test-Environment)
$passed = $results[-1]
$results | Select-Object -SkipLast 1 | Write-Output
if (-not $passed) { throw 'Environment verification failed.' }
Write-Output 'PASS: CLI, PATH, Git ownership, and Git metadata ACL access.'
