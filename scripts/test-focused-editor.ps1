param(
    [Parameter(Mandatory = $true)][string]$Handshake,
    [string]$ToolsRoot = '',
    [switch]$ExpectFallback
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
# The normal suite also prepares the cached compiler environment. The ignored
# flow below alone accesses the operator-selected external editor.
& (Join-Path $PSScriptRoot 'build-listen.ps1') -ToolsRoot $ToolsRoot -Task Test
$env:LISTEN_EDITOR_HANDSHAKE = [IO.Path]::GetFullPath($Handshake)
$env:LISTEN_EDITOR_EXPECT_FALLBACK = if ($ExpectFallback) { '1' } else { '0' }
$cargo = Join-Path $env:CARGO_HOME 'bin\cargo.exe'
$manifest = Join-Path $projectRoot 'app\listen\Cargo.toml'
& $cargo test --offline --locked --manifest-path $manifest --test capture_to_copy selected_generic_editor_receives_complete_output_through_the_real_target_adapter -- --ignored --nocapture --test-threads=1
if ($LASTEXITCODE -ne 0) { throw "Focused editor complete-flow test failed ($LASTEXITCODE)" }
