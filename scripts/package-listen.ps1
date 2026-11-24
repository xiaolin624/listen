param(
    [string]$ToolsRoot = '',
    [string]$OutputDirectory = '',
    [string]$Version = '',
    [switch]$IncludeModels,
    [switch]$SkipBuild,
    [switch]$SkipValidation
)
# Build a directly runnable Listen directory. The output is self-contained except for
# Windows itself: listen.exe + libunwind.dll, the local SenseVoice q8 engine (the
# pinned sherpa-onnx / onnxruntime DLLs) with its optional model files, the cloud
# daily-default launcher, the explicitly labelled local fallback launcher, the local
# cloud configuration helper and guide, the user README, and the third-party notices
# together with the upstream licence texts. whisper.cpp is not packaged any more
# (#23). No unrelated engine examples or server binaries are copied, and no
# credential, recording or test-environment path is ever packaged.
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

$engineSource = Join-Path $ToolsRoot 'sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib'
# The application loads the C API and onnxruntime with LOAD_WITH_ALTERED_SEARCH_PATH,
# so both must sit in the same directory. The provider bridge ships beside them.
$engineFiles = @('sherpa-onnx-c-api.dll', 'onnxruntime.dll', 'onnxruntime_providers_shared.dll')

$modelsSource = Join-Path $ToolsRoot 'models\sense-voice'
$modelFiles = @('model.int8.onnx', 'tokens.txt', 'silero_vad.onnx')

# Every delivered engine and model file is pinned by SHA-256. These are the exact
# builds the application verifies before loading (app\listen\src\sensevoice.rs), so a
# mismatched local tool set is refused here instead of on the user's machine.
$pinnedHashes = @{
    'sherpa-onnx-c-api.dll'            = '0965A6E4C0254DA2BEFF489CF7A8AAFFAF7DC94F90B43B3E66448B8CAB4177EA'
    'onnxruntime.dll'                  = '7F66F939A881BAF4F46A2216496798EDF4A1429878B646D12674AA62F27D8A25'
    'onnxruntime_providers_shared.dll' = '551D0E1FE4C227D8542314BA718D52F4379E0C7BFE729A37C59833A884E27B4D'
    'model.int8.onnx'                  = 'C71F0CE00BEC95B07744E116345E33D8CBBE08CEF896382CF907BF4B51A2CD51'
    'tokens.txt'                       = 'F449EB28DC567533D7FA59BE34E2ABCA8784F771850C78A47FB731A31429A1DC'
    'silero_vad.onnx'                  = '9E2449E1087496D8D4CABA907F23E0BD3F78D91FA552479BB9C23AC09CBB1FD6'
}
foreach ($name in $engineFiles) {
    $source = Join-Path $engineSource $name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw ('Engine file is missing: ' + $source)
    }
    $actual = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
    if ($actual -ne $pinnedHashes[$name]) {
        throw ('Engine file is not the pinned build: ' + $source + ' (' + $actual + ')')
    }
}
if ($IncludeModels) {
    foreach ($name in $modelFiles) {
        $source = Join-Path $modelsSource $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw ('Model is missing: ' + $source) }
        $actual = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        if ($actual -ne $pinnedHashes[$name]) {
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
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDirectory 'engine\sherpa') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDirectory 'models\sense-voice') | Out-Null
[IO.File]::WriteAllText((Join-Path $OutputDirectory '.listen-package'), ('Listen package ' + $Version + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))

Copy-Item -LiteralPath $listen -Destination (Join-Path $OutputDirectory 'listen.exe')
Copy-Item -LiteralPath $libunwind -Destination (Join-Path $OutputDirectory 'libunwind.dll')
foreach ($name in $engineFiles) {
    Copy-Item -LiteralPath (Join-Path $engineSource $name) -Destination (Join-Path $OutputDirectory ('engine\sherpa\' + $name))
}
if ($IncludeModels) {
    foreach ($name in $modelFiles) {
        Copy-Item -LiteralPath (Join-Path $modelsSource $name) -Destination (Join-Path $OutputDirectory ('models\sense-voice\' + $name))
    }
}
else {
    Copy-Item -LiteralPath (Join-Path $packagingRoot 'models-README.txt') -Destination (Join-Path $OutputDirectory 'models\README.txt')
}

Copy-Item -LiteralPath (Join-Path $packagingRoot 'README.md') -Destination (Join-Path $OutputDirectory 'README.md')
Copy-Item -LiteralPath (Join-Path $packagingRoot 'CLOUD-TRIAL.md') -Destination (Join-Path $OutputDirectory 'CLOUD-TRIAL.md')
# The FunASR/SenseVoice attribution and the upstream licence texts travel with the
# package; the notices file names each file and where it came from.
Copy-Item -LiteralPath (Join-Path $packagingRoot 'THIRD-PARTY-NOTICES.md') -Destination (Join-Path $OutputDirectory 'THIRD-PARTY-NOTICES.md')
Copy-Item -LiteralPath (Join-Path $packagingRoot 'licenses') -Destination (Join-Path $OutputDirectory 'licenses') -Recurse
Copy-Item -LiteralPath (Join-Path $packagingRoot 'glossary.txt') -Destination (Join-Path $OutputDirectory 'glossary.txt')
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDirectory 'scripts') | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'configure-cloud-trial.ps1') -Destination (Join-Path $OutputDirectory 'scripts\configure-cloud-trial.ps1')
# Exactly two launchers: the daily cloud default and the explicitly labelled local
# fallback. They are classified by their arguments so this script itself stays
# ASCII-only.
$launchers = @(Get-ChildItem -LiteralPath $packagingRoot -Filter '*.cmd' -File)
if ($launchers.Count -ne 2) { throw ('packaging must contain exactly two launchers (.cmd: daily cloud default and explicit local fallback); found ' + $launchers.Count) }
$launcherInfo = @($launchers | ForEach-Object {
        [pscustomobject]@{ File = $_; Text = [IO.File]::ReadAllText($_.FullName) }
    })
$localLaunchers = @($launcherInfo | Where-Object { $_.Text -match '--engine' -and $_.Text -notmatch '--cloud-trial' })
$defaultLaunchers = @($launcherInfo | Where-Object { $_.Text -notmatch '--engine' })
if ($localLaunchers.Count -ne 1 -or $defaultLaunchers.Count -ne 1) {
    throw 'packaging must contain exactly one local fallback launcher (--engine, no --cloud-trial) and exactly one default launcher (no --engine).'
}
if ($localLaunchers[0].Text -notmatch '--runtime\s+"%~dp0runtime"') {
    throw 'The local fallback launcher must use the packaged local runtime directory.'
}
if ($defaultLaunchers[0].Text -notmatch '--runtime\s+"%~dp0runtime-cloud"') {
    throw 'The daily default launcher must use the cloud runtime directory.'
}
foreach ($launcher in $launcherInfo) {
    Copy-Item -LiteralPath $launcher.File.FullName -Destination (Join-Path $OutputDirectory $launcher.File.Name)
}

$gitCommit = (& git -C $projectRoot rev-parse HEAD 2>$null)
$gitBranch = (& git -C $projectRoot rev-parse --abbrev-ref HEAD 2>$null)
$gitDirty = [bool](& git -C $projectRoot status --porcelain --untracked-files=no 2>$null)
$builtUtc = [DateTime]::UtcNow.ToString('o')
$versionText = @(
    'Listen ' + $Version,
    'Built (UTC): ' + $builtUtc,
    'Source commit: ' + $gitCommit,
    'Models included: ' + $(if ($IncludeModels) { 'yes' } else { 'no' }),
    'Daily default: cloud (Alibaba Model Studio / qwen-audio-3.1-asr-flash-message / cn-beijing; free-only protection verified before every request)',
    'Local fallback (explicit only): SenseVoice q8 (sherpa-onnx, CPU, AVX2 required); start it with the explicitly labelled local launcher',
    'Cloud trial: single vendor, shared CNY 50 ledger; CNY 100/month is a reminder, never an automatic stop',
    'Engine: SenseVoice q8 (sherpa-onnx, CPU, AVX2 required)',
    'Third-party notices: THIRD-PARTY-NOTICES.md (FunASR MODEL_LICENSE for the SenseVoice weights; Apache License 2.0 for sherpa-onnx; MIT for onnxruntime)',
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
    # The daily default is the accepted cloud path (#40); local recognition is an
    # explicit fallback selected by its own labelled launcher.
    default_mode = 'cloud'
    cloud_trial = [ordered]@{
        selected_by_default = $true
        provider = 'Alibaba Model Studio'
        model = 'qwen-audio-3.1-asr-flash-message'
        region = 'cn-beijing'
        configure = 'scripts/configure-cloud-trial.ps1'
        guide = 'CLOUD-TRIAL.md'
    }
    local_fallback = [ordered]@{
        explicit_only = $true
        launcher = $localLaunchers[0].File.Name
        runtime = 'runtime'
        engine = 'SenseVoice q8 (sherpa-onnx, CPU, AVX2 required)'
    }
    engine_source = 'SenseVoice q8 (sherpa-onnx) CPU; AVX2 required'
    engine_files = @($engineFiles)
    third_party_notices = 'THIRD-PARTY-NOTICES.md'
    licenses = @('licenses/FunASR-MODEL_LICENSE.txt', 'licenses/sherpa-onnx-LICENSE.txt', 'licenses/onnxruntime-LICENSE.txt')
    # Keep host-specific build paths out of the delivered manifest.
    model_source = if ($IncludeModels) { 'local tools models (verified SHA-256)' } else { 'not included; see README.md' }
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
    $evidenceDir = Join-Path $projectRoot '.artifacts\issue-23\package'
    $evidence = Join-Path $evidenceDir ('verify-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
    & (Join-Path $PSScriptRoot 'verify-listen-package.ps1') -PackageRoot $OutputDirectory -Evidence $evidence
    Write-Output ('Verification evidence: ' + $evidence)
}
