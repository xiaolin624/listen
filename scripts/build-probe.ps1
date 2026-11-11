$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$env:CARGO_HOME = Join-Path $projectRoot '.tools\cargo'
$env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnullvm'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = Join-Path $env:RUSTUP_HOME 'toolchains\stable-x86_64-pc-windows-gnullvm\lib\rustlib\x86_64-pc-windows-gnullvm\bin\rust-lld.exe'
$env:RUSTFLAGS = '-C link-self-contained=yes'
& (Join-Path $env:CARGO_HOME 'bin\cargo.exe') build --locked --release --manifest-path (Join-Path $projectRoot 'experiments\windows-probe\Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Probe build failed' }
$runtime = Join-Path $env:RUSTUP_HOME 'toolchains\stable-x86_64-pc-windows-gnullvm\bin\libunwind.dll'
Copy-Item -LiteralPath $runtime -Destination (Join-Path $projectRoot 'experiments\windows-probe\target\release\libunwind.dll') -Force
