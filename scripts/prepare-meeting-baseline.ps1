param([string]$Output = '', [string]$ToolsRoot = '')
# Builds the accepted pre-cloud baseline in its own detached worktree. It never
# starts Listen, records/plays audio, changes the daily launcher, or uploads.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$baselineCommit = '4b8a487e5e6cb5aaedf8bfe8084291cc24540545'
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
if (-not $Output) { $Output = Join-Path $repositoryRoot '.artifacts\cloud-evaluation\baseline-4b8a487' }
if (-not $ToolsRoot) { $ToolsRoot = Join-Path $repositoryRoot '.tools' }
$Output = [IO.Path]::GetFullPath($Output)
$ToolsRoot = [IO.Path]::GetFullPath($ToolsRoot)
$receiptPath = Join-Path $Output 'baseline.json'
if (Test-Path -LiteralPath $receiptPath) { throw "Baseline already pinned: $receiptPath. Use a new output directory to rebuild." }
$source = Join-Path $Output 'source'
$null = New-Item -ItemType Directory -Force -Path $Output
if (-not (Test-Path -LiteralPath $source)) {
    & git -C $repositoryRoot worktree add --detach $source $baselineCommit
    if ($LASTEXITCODE -ne 0) { throw 'Could not create baseline worktree.' }
}
$actualCommit = & git -C $source rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $baselineCommit) { throw 'The baseline source is not the fixed pre-cloud commit.' }
$dirty = & git -C $source status --porcelain
if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Baseline source must be clean. Preserve changes and use another output directory.' }
$buildLog = Join-Path $Output 'build.log'
$savedTargetDirectory = $env:CARGO_TARGET_DIR
try {
    # The pinned build script expects its default target directory when copying
    # libunwind; isolate it from any caller's shared CARGO_TARGET_DIR.
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    & (Join-Path $source 'scripts\build-listen.ps1') -ToolsRoot $ToolsRoot -Task Build *>&1 |
        Tee-Object -FilePath $buildLog | ForEach-Object { Write-Host $_ }
} finally {
    if ($null -ne $savedTargetDirectory) { $env:CARGO_TARGET_DIR = $savedTargetDirectory }
    else { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
}
if ((& git -C $source status --porcelain)) { throw 'Baseline source changed during build; no receipt created.' }
$release = Join-Path $source 'app\listen\target\release'
$models = Join-Path $ToolsRoot 'models\sense-voice'
$library = Join-Path $ToolsRoot 'sherpa-onnx\sherpa-onnx-v2.0.1-win-x64-shared-MT-Release\lib'
$glossary = Join-Path $Output 'glossary.txt'
Copy-Item -LiteralPath (Join-Path $source 'packaging\glossary.txt') -Destination $glossary
$paths = [ordered]@{
    listen_binary = Join-Path $release 'listen.exe'
    libunwind = Join-Path $release 'libunwind.dll'
    model = Join-Path $models 'model.int8.onnx'
    tokens = Join-Path $models 'tokens.txt'
    vad = Join-Path $models 'silero_vad.onnx'
    sherpa = Join-Path $library 'sherpa-onnx-c-api.dll'
    onnxruntime = Join-Path $library 'onnxruntime.dll'
    glossary = $glossary
    build_log = $buildLog
}
$artifacts = [ordered]@{}
foreach ($role in $paths.Keys) {
    $path = [IO.Path]::GetFullPath($paths[$role])
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing baseline artifact $role at $path" }
    $artifacts[$role] = [ordered]@{ path = $path; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$receipt = [ordered]@{
    kind = 'local-baseline-build'
    git_commit = $baselineCommit
    source = $source
    source_clean = $true
    build_succeeded = $true
    built_at = (Get-Date).ToString('o')
    engine = 'sensevoice'
    configuration = 'SenseVoice q8; Silero VAD; language auto; CPU; 6 threads; explicit fixed glossary; no refine'
    artifacts = $artifacts
    listen_args = @('--engine', 'sensevoice', '--sherpa-lib', $library,
        '--sensevoice-model', $paths.model, '--sensevoice-tokens', $paths.tokens,
        '--sensevoice-vad', $paths.vad, '--sensevoice-language', 'auto',
        '--threads', '6', '--glossary', $glossary, '--hidden')
}
[IO.File]::WriteAllText($receiptPath, (($receipt | ConvertTo-Json -Depth 10) + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
Write-Output "Pinned current local baseline: $receiptPath"
Write-Output 'No recognition run has been performed. Preserve this directory and the referenced model/library files.'
