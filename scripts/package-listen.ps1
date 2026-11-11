param(
    [string]$ToolsRoot = '',
    [string]$OutputDirectory = '',
    [string]$Version = '',
    [switch]$IncludeModels,
    [switch]$SkipBuild,
    [switch]$SkipValidation
)
# Build a directly runnable Listen directory. The output is self-contained except for
# Windows itself: listen.exe + libunwind.dll, the local whisper.cpp engine and its
# actual adjacent DLLs, an optional copy of the two models, a double-click launcher
# and the user README. No unrelated engine examples or server binaries are copied.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$projectRoot = Split-Path $PSScriptRoot -Parent
$packagingRoot = Join-Path $projectRoot 'packaging'
$cargoPath = Join-Path $projectRoot 'app\listen\Cargo.toml'

if (-not $ToolsRoot) {
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Pass -ToolsRoot with the directory containing cargo and rustup.' }
    $ToolsRoot = Join-Path (Split-Path $commonGit -Parent) '.tools'
}
$ToolsRoot = [IO.Path]::GetFullPath($ToolsRoot)
if (-not (Test-Path -LiteralPath $ToolsRoot -PathType Container)) { throw ('Tools root is missing: ' + $ToolsRoot) }

if (-not $Version) {
    $cargo = [IO.File]::ReadAllText($cargoPath)
    $match = [regex]::Match($cargo, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw 'Cannot read the Listen version from app\listen\Cargo.toml.' }
    $Version = $match.Groups[1].Value
}
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$') {
    throw ('Unsupported version format: ' + $Version)
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $projectRoot ('dist\Listen-' + $Version)
}
elseif (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $PWD $OutputDirectory
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$leaf = Split-Path $OutputDirectory -Leaf
if ($leaf -notmatch '^Listen-') {
    throw 'The output directory leaf must start with "Listen-" so the cleanup cannot remove an unrelated directory.'
}
if ((Split-Path $OutputDirectory -Parent) -eq $OutputDirectory) { throw 'Refusing to use a drive root as the output directory.' }

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build-listen.ps1') -ToolsRoot $ToolsRoot -Task Build
    if ($LASTEXITCODE -ne 0) { throw ('Listen build failed (' + $LASTEXITCODE + ')') }
}
$release = Join-Path $projectRoot 'app\listen\target\release'
$listen = Join-Path $release 'listen.exe'
if (-not (Test-Path -LiteralPath $listen -PathType Leaf)) { throw ('Built listen.exe is missing: ' + $listen) }
$libunwind = Join-Path $release 'libunwind.dll'
if (-not (Test-Path -LiteralPath $libunwind -PathType Leaf)) {
    $libunwind = Join-Path $ToolsRoot 'rustup\toolchains\stable-x86_64-pc-windows-gnullvm\bin\libunwind.dll'
}
if (-not (Test-Path -LiteralPath $libunwind -PathType Leaf)) { throw 'Cannot locate the gnullvm libunwind.dll.' }

$engineSource = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release'
$engineFiles = @(
    'whisper-cli.exe',
    'whisper.dll',
    'ggml.dll',
    'ggml-cpu.dll',
    'ggml-base.dll',
    'vad-speech-segments.exe'
)
$vcRuntime = @('MSVCP140.dll', 'VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'VCOMP140.DLL')
$system32 = Join-Path $env:WINDIR 'System32'
foreach ($name in $engineFiles) {
    if (-not (Test-Path -LiteralPath (Join-Path $engineSource $name) -PathType Leaf)) {
        throw ('Engine file is missing: ' + (Join-Path $engineSource $name))
    }
}
foreach ($name in $vcRuntime) {
    if (-not (Test-Path -LiteralPath (Join-Path $system32 $name) -PathType Leaf)) {
        throw ('The Visual C++ runtime is missing on this build host: ' + $name)
    }
}

$modelHashes = @{
    'ggml-small.bin' = '1BE3A9B2063867B937E64E2EC7483364A79917E157FA98C5D94B5C1FFFEA987B'
    'ggml-silero-v5.1.2.bin' = '29940D98D42B91FBD05CE489F3ECF7C72F0A42F027E4875919A28FB4C04EA2CF'
}
$modelsSource = Join-Path $ToolsRoot 'models'
if ($IncludeModels) {
    foreach ($name in @('ggml-small.bin', 'ggml-silero-v5.1.2.bin')) {
        $source = Join-Path $modelsSource $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw ('Model is missing: ' + $source) }
        $actual = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        if ($actual -ne $modelHashes[$name]) {
            throw ('Model hash does not match the documented model: ' + $source + ' (' + $actual + ')')
        }
    }
}

if (Test-Path -LiteralPath $OutputDirectory) {
    # Only remove a directory that this script created. The leaf-name check alone
    # must never authorize deleting an unrelated "Listen-notes" or "Listen-backup".
    $owned = $false
    $manifestPath = Join-Path $OutputDirectory 'manifest.json'
    if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
        try {
            $existing = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
            $owned = ([string]$existing.product -eq 'Listen')
        }
        catch { $owned = $false }
    }
    if (-not $owned) {
        $markerPath = Join-Path $OutputDirectory '.listen-package'
        if (Test-Path -LiteralPath $markerPath -PathType Leaf) {
            try {
                $owned = ([IO.File]::ReadAllText($markerPath).Trim() -eq ('Listen package ' + $Version))
            }
            catch { $owned = $false }
        }
    }
    if (-not $owned) {
        throw ('Refusing to remove ' + $OutputDirectory + ': it contains neither a Listen manifest nor a Listen package marker. Remove it manually if it is not a Listen package.')
    }
    Remove-Item -LiteralPath $OutputDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDirectory 'engine') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDirectory 'models') | Out-Null
[IO.File]::WriteAllText((Join-Path $OutputDirectory '.listen-package'), ('Listen package ' + $Version + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))

Copy-Item -LiteralPath $listen -Destination (Join-Path $OutputDirectory 'listen.exe')
Copy-Item -LiteralPath $libunwind -Destination (Join-Path $OutputDirectory 'libunwind.dll')
foreach ($name in $engineFiles) {
    Copy-Item -LiteralPath (Join-Path $engineSource $name) -Destination (Join-Path $OutputDirectory ('engine\' + $name))
}
foreach ($name in $vcRuntime) {
    Copy-Item -LiteralPath (Join-Path $system32 $name) -Destination (Join-Path $OutputDirectory ('engine\' + $name))
}
if ($IncludeModels) {
    foreach ($name in @('ggml-small.bin', 'ggml-silero-v5.1.2.bin')) {
        Copy-Item -LiteralPath (Join-Path $modelsSource $name) -Destination (Join-Path $OutputDirectory ('models\' + $name))
    }
}
else {
    Copy-Item -LiteralPath (Join-Path $packagingRoot 'models-README.txt') -Destination (Join-Path $OutputDirectory 'models\README.txt')
}

Copy-Item -LiteralPath (Join-Path $packagingRoot 'README.md') -Destination (Join-Path $OutputDirectory 'README.md')
$launchers = @(Get-ChildItem -LiteralPath $packagingRoot -Filter '*.cmd' -File)
if ($launchers.Count -ne 1) { throw ('packaging must contain exactly one launcher .cmd; found ' + $launchers.Count) }
Copy-Item -LiteralPath $launchers[0].FullName -Destination (Join-Path $OutputDirectory $launchers[0].Name)

$gitCommit = (& git -C $projectRoot rev-parse HEAD 2>$null)
$gitBranch = (& git -C $projectRoot rev-parse --abbrev-ref HEAD 2>$null)
$gitDirty = [bool](& git -C $projectRoot status --porcelain --untracked-files=no 2>$null)
$builtUtc = [DateTime]::UtcNow.ToString('o')
$versionText = @(
    'Listen ' + $Version,
    'Built (UTC): ' + $builtUtc,
    'Source commit: ' + $gitCommit,
    'Models included: ' + $(if ($IncludeModels) { 'yes' } else { 'no' }),
    'Engine: whisper.cpp v1.8.2 CPU',
    ''
) -join [Environment]::NewLine
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'VERSION.txt'), $versionText, [Text.UTF8Encoding]::new($false))

$entries = [Collections.Generic.List[object]]::new()
foreach ($file in @(Get-ChildItem -LiteralPath $OutputDirectory -Recurse -File | Sort-Object FullName)) {
    if ($file.Name -eq 'manifest.json' -and $file.DirectoryName -eq $OutputDirectory) { continue }
    # The package marker authorizes a later clean rebuild; it is not a delivered file.
    if ($file.Name -eq '.listen-package' -and $file.DirectoryName -eq $OutputDirectory) { continue }
    $relative = $file.FullName.Substring($OutputDirectory.Length + 1)
    $entries.Add([ordered]@{
            path = $relative
            bytes = $file.Length
            sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
        })
}
$manifest = [ordered]@{
    product = 'Listen'
    version = $Version
    built_utc = $builtUtc
    git_commit = [string]$gitCommit
    git_branch = [string]$gitBranch
    git_dirty_at_package_time = $gitDirty
    included_models = [bool]$IncludeModels
    engine_source = 'whisper.cpp v1.8.2 CPU'
    engine_files = @($engineFiles + $vcRuntime)
    model_source = if ($IncludeModels) { $modelsSource + ' (verified SHA-256)' } else { 'not included; see README.md' }
    files = @($entries.ToArray())
}
$manifestJson = $manifest | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'manifest.json'), $manifestJson, [Text.UTF8Encoding]::new($false))

$totalBytes = [long]0
foreach ($entry in $entries) { $totalBytes += [long]$entry.bytes }
Write-Output ('Package: ' + $OutputDirectory)
Write-Output ('Version: ' + $Version + '; models included: ' + [bool]$IncludeModels)
Write-Output ('Files: ' + $entries.Count + '; bytes (without manifest): ' + $totalBytes)

if (-not $SkipValidation) {
    $evidenceDir = Join-Path $projectRoot '.artifacts\issue-11\package'
    $evidence = Join-Path $evidenceDir ('verify-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
    & (Join-Path $PSScriptRoot 'verify-listen-package.ps1') -PackageRoot $OutputDirectory -Evidence $evidence
    Write-Output ('Verification evidence: ' + $evidence)
}
