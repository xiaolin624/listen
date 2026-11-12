param(
    [string]$ToolsRoot = ''
)
# Builds the meeting-corpus recorder example (app\listen\examples\capture_only.rs)
# with the same offline toolchain settings as build-listen.ps1. The normal
# release build does not include examples, and the gnullvm toolchain needs
# libunwind.dll next to the produced executable.
#
# This script deliberately does not use $ErrorActionPreference='Stop': cargo
# writes its progress to stderr and that would turn a successful build into a
# terminating error. The exit code is checked explicitly instead.
$ErrorActionPreference = 'Continue'
$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $ToolsRoot) {
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root; pass -ToolsRoot.' }
    $ToolsRoot = Join-Path (Split-Path $commonGit -Parent) '.tools'
}
$ToolsRoot = [IO.Path]::GetFullPath($ToolsRoot)
$env:CARGO_HOME = Join-Path $ToolsRoot 'cargo'
$env:RUSTUP_HOME = Join-Path $ToolsRoot 'rustup'
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnullvm'
$toolchain = Join-Path $env:RUSTUP_HOME 'toolchains\stable-x86_64-pc-windows-gnullvm'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = Join-Path $toolchain 'lib\rustlib\x86_64-pc-windows-gnullvm\bin\rust-lld.exe'
$env:RUSTFLAGS = '-C link-self-contained=yes'
$env:PATH = (Join-Path $toolchain 'bin') + ';' + $env:PATH
$cargo = Join-Path $env:CARGO_HOME 'bin\cargo.exe'
$manifest = Join-Path $projectRoot 'app\listen\Cargo.toml'
& $cargo build --offline --locked --release --manifest-path $manifest --example capture_only
$code = $LASTEXITCODE
if ($code -ne 0) { throw "Building capture_only failed ($code)." }
$output = Join-Path $projectRoot 'app\listen\target\release\examples'
$dll = Join-Path $toolchain 'bin\libunwind.dll'
if (Test-Path -LiteralPath $dll -PathType Leaf) {
    Copy-Item -LiteralPath $dll -Destination $output -Force
}
Write-Output (Join-Path $output 'capture_only.exe')
