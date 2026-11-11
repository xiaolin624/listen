param([string]$ToolsRoot = '')
$ErrorActionPreference = 'Stop'
if (-not $ToolsRoot) {
    $projectRoot = Split-Path $PSScriptRoot -Parent
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
    $ToolsRoot = Join-Path (Split-Path $commonGit -Parent) '.tools'
}
$directory = Join-Path ([IO.Path]::GetFullPath($ToolsRoot)) 'models'
$null = New-Item -ItemType Directory -Force -Path $directory
# Official model repositories used by whisper.cpp's v1.8.2 download scripts.
# Revisions and hashes are pinned to their LFS metadata, not a moving main branch.
$models = @(
    @{
        name = 'ggml-small.bin'
        sha256 = '1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b'
        url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin'
    },
    @{
        name = 'ggml-silero-v5.1.2.bin'
        sha256 = '29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf'
        url = 'https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b/ggml-silero-v5.1.2.bin'
    }
)
$ProgressPreference = 'SilentlyContinue'
foreach ($model in $models) {
    $destination = Join-Path $directory $model.name
    if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
        $partial = $destination + '.download'
        Invoke-WebRequest -UseBasicParsing -Uri $model.url -OutFile $partial
        if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $model.sha256) {
            throw "Local model checksum mismatch: $partial"
        }
        Move-Item -LiteralPath $partial -Destination $destination
    }
    if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $model.sha256) {
        throw "Existing local model checksum mismatch; inspect it before replacing: $destination"
    }
    Write-Output "Verified local model: $destination"
}
