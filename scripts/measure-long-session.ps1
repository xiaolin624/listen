param(
    [Parameter(Mandatory = $true)][string]$InputWav,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$ToolsRoot = '',
    [double]$Seconds = 3600,
    [int]$Threads = 6,
    [long]$DelayMs = 0,
    [long]$SpoolBytes = 9600440,
    [string]$Model = '',
    [string]$Engine = '',
    [string]$VadModel = '',
    [ValidateSet('host', 'cli')][string]$Backend = 'host'
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $ToolsRoot) {
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Pass -ToolsRoot explicitly.' }
    $ToolsRoot = Join-Path (Split-Path $commonGit -Parent) '.tools'
}
$ToolsRoot = [IO.Path]::GetFullPath($ToolsRoot)
if (-not $Model) { $Model = Join-Path $ToolsRoot 'models\ggml-small.bin' }
if (-not $Engine) { $Engine = Join-Path $ToolsRoot 'whisper-v1.8.2-cpu\bin\Release\whisper-cli.exe' }
if (-not $VadModel) { $VadModel = Join-Path $ToolsRoot 'models\ggml-silero-v5.1.2.bin' }
$InputWav = (Get-Item -LiteralPath $InputWav).FullName
$Model = (Get-Item -LiteralPath $Model).FullName
$Engine = (Get-Item -LiteralPath $Engine).FullName
$VadModel = (Get-Item -LiteralPath $VadModel).FullName
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$runtime = Join-Path $OutputDirectory 'runtime'
$stdout = Join-Path $OutputDirectory 'flow.ndjson'
$stderr = Join-Path $OutputDirectory 'stderr.txt'
$resources = Join-Path $OutputDirectory 'resources.ndjson'
if ((Test-Path -LiteralPath $stdout) -or (Test-Path -LiteralPath $runtime)) {
    throw 'Use a fresh OutputDirectory so previous measurements are not overwritten.'
}
$exe = Join-Path $projectRoot 'app\listen\target\release\examples\long_session.exe'
if (-not (Test-Path -LiteralPath $exe)) { throw 'Run scripts/build-listen.ps1 -Task BuildHarness first.' }
$metadata = [ordered]@{
    started_utc = [DateTime]::UtcNow.ToString('o')
    source = $InputWav
    source_sha256 = (Get-FileHash -LiteralPath $InputWav -Algorithm SHA256).Hash
    model = $Model
    model_sha256 = (Get-FileHash -LiteralPath $Model -Algorithm SHA256).Hash
    vad_model = $VadModel
    vad_sha256 = (Get-FileHash -LiteralPath $VadModel -Algorithm SHA256).Hash
    engine = $Engine
    backend = $Backend
    seconds = $Seconds
    threads = $Threads
    delay_ms = $DelayMs
    spool_limit_bytes = $SpoolBytes
    audio_boundary = 'substituted playback device, looping real PCM with wall-clock pacing; no audio output'
    production_path = 'Application::dispatch -> production segmented writer -> actual VAD/Whisper -> complete fallback result'
    resource_sampling = '3 second samples of harness and direct child processes; observed peaks can miss short child lifetimes; GPU is global device usage'
    success_criterion = 'the harness terminal NDJSON record reports phase Result with terminal true'
}
$metadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'metadata.json') -Encoding UTF8
$arguments = @(
    '--input', ('"{0}"' -f $InputWav), '--runtime', ('"{0}"' -f $runtime),
    '--engine', ('"{0}"' -f $Engine), '--model', ('"{0}"' -f $Model),
    '--vad-model', ('"{0}"' -f $VadModel), '--backend', $Backend, '--seconds', $Seconds.ToString([Globalization.CultureInfo]::InvariantCulture),
    '--threads', $Threads, '--delay-ms', $DelayMs, '--spool-bytes', $SpoolBytes
)
# Windows PowerShell rebuilds a case-insensitive environment dictionary when it
# redirects a child, so an inherited block holding both HTTP_PROXY and http_proxy
# makes Start-Process fail before the harness starts. Drop the case duplicates once.
$duplicates = [Environment]::GetEnvironmentVariables().Keys |
    Group-Object { $_.ToUpperInvariant() } | Where-Object { $_.Count -gt 1 }
foreach ($group in $duplicates) {
    foreach ($name in $group.Group) {
        if ($name -cne $group.Group[0]) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    }
}
$process = Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
$process.Id | Set-Content -LiteralPath (Join-Path $OutputDirectory 'process-id.txt') -Encoding ASCII
$gpu = Get-Command nvidia-smi -ErrorAction SilentlyContinue
try {
    while (-not $process.HasExited) {
        $ids = @($process.Id)
        $ids += @(Get-CimInstance Win32_Process -Filter ('ParentProcessId = {0}' -f $process.Id) -ErrorAction SilentlyContinue | ForEach-Object { $_.ProcessId })
        $observed = @()
        $totalWorking = [long]0
        $totalPrivate = [long]0
        foreach ($item in @(Get-Process -Id $ids -ErrorAction SilentlyContinue)) {
            $observed += [ordered]@{ id = $item.Id; name = $item.ProcessName; working_set = $item.WorkingSet64; private_bytes = $item.PrivateMemorySize64; cpu_seconds = $item.CPU }
            $totalWorking += $item.WorkingSet64
            $totalPrivate += $item.PrivateMemorySize64
        }
        $record = [ordered]@{
            utc = [DateTime]::UtcNow.ToString('o')
            processes = $observed
            total_working_set = $totalWorking
            total_private_bytes = $totalPrivate
        }
        if ($gpu) {
            $record.gpu = @(& $gpu.Source --query-gpu=utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits 2>$null)
        }
        $record | ConvertTo-Json -Depth 5 -Compress | Add-Content -LiteralPath $resources -Encoding UTF8
        Start-Sleep -Seconds 3
        $process.Refresh()
    }
    $process.WaitForExit()
    # Windows PowerShell does not expose ExitCode for a redirected Start-Process
    # child, so decide from the harness's own terminal record and keep stderr.txt
    # for the failure message.
    $terminal = @(Get-Content -LiteralPath $stdout -Encoding UTF8 | Where-Object { $_ -match '^\{' })[-1] | ConvertFrom-Json
    [ordered]@{
        finished_utc = [DateTime]::UtcNow.ToString('o')
        phase = $terminal.phase
        terminal = $terminal.terminal
        recognized_segments = $terminal.recognized_segments
        stop_to_result_seconds = $terminal.stop_to_result_seconds
        stop_to_result_ms = $terminal.timing_stop_to_result_ms
        finalize_ms = $terminal.timing_finalize_ms
        recognition_ms = $terminal.timing_recognition_ms
        last_window_wait_ms = $terminal.timing_last_window_wait_ms
        last_window_inference_ms = $terminal.timing_last_window_inference_ms
        preparation_ms = $terminal.preparation_ms
        backend = $terminal.backend
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'exit.json') -Encoding UTF8
    if ($terminal.phase -ne 'Result' -or $terminal.terminal -ne $true) {
        throw "Long-session harness did not reach a complete result; inspect $stdout and $stderr"
    }
    Write-Output $OutputDirectory
} finally {
    # A failed sample loop must not leave the measurement running unattended.
    if (-not $process.HasExited) {
        try { $process.Kill() } catch { }
        $process.WaitForExit()
    }
    $process.Dispose()
}
