param(
    [string]$ToolsRoot = '',
    [ValidateSet('Build', 'BuildHarness', 'Test', 'Quality', 'Format', 'CheckFormat')][string]$Task = 'Build'
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $ToolsRoot) {
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Pass -ToolsRoot with the directory containing cargo and rustup.' }
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
# cargo writes its progress to stderr. Under `$ErrorActionPreference = 'Stop'`
# PowerShell turns that into a terminating error, so a successful build used to
# be reported as a failure. Keep the strict policy for everything else and only
# relax it around the native call, then judge the result by its exit code.
$strict = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    switch ($Task) {
        'Build' { & $cargo build --offline --locked --release --manifest-path $manifest 2>&1 | ForEach-Object { Write-Host $_ } }
        'BuildHarness' { & $cargo build --offline --locked --release --manifest-path $manifest --example long_session 2>&1 | ForEach-Object { Write-Host $_ } }
        'Test' { & $cargo test --offline --locked --manifest-path $manifest -- --nocapture 2>&1 | ForEach-Object { Write-Host $_ } }
        'Quality' { & $cargo test --offline --locked --manifest-path $manifest --test local_quality -- --ignored --nocapture 2>&1 | ForEach-Object { Write-Host $_ } }
        'Format' { & $cargo fmt --manifest-path $manifest 2>&1 | ForEach-Object { Write-Host $_ } }
        'CheckFormat' { & $cargo fmt --manifest-path $manifest --check 2>&1 | ForEach-Object { Write-Host $_ } }
    }
    $code = $LASTEXITCODE
}
finally {
    $ErrorActionPreference = $strict
}
if ($code -ne 0) { throw "Listen $Task failed ($code)" }
if ($Task -eq 'Build') {
    $output = Join-Path $projectRoot 'app\listen\target\release'
    Copy-Item -LiteralPath (Join-Path $toolchain 'bin\libunwind.dll') -Destination $output -Force
    Write-Output (Join-Path $output 'listen.exe')
}
if ($Task -eq 'BuildHarness') {
    $output = Join-Path $projectRoot 'app\listen\target\release\examples'
    Copy-Item -LiteralPath (Join-Path $toolchain 'bin\libunwind.dll') -Destination $output -Force
    Write-Output (Join-Path $output 'long_session.exe')
}
