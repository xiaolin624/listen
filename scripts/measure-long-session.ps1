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
    [string]$VadModel = ''
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
    seconds = $Seconds
    threads = $Threads
    delay_ms = $DelayMs
    spool_limit_bytes = $SpoolBytes
    audio_boundary = 'substituted playback device, looping real PCM with wall-clock pacing; no audio output'
    production_path = 'Application::dispatch -> production segmented writer -> actual VAD/Whisper -> complete fallback result'
    resource_sampling = '3 second samples of harness and direct child processes; observed peaks can miss short child lifetimes; GPU is global device usage'
}
$metadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'metadata.json') -Encoding UTF8
$arguments = @(
    '--input', ('"{0}"' -f $InputWav), '--runtime', ('"{0}"' -f $runtime),
    '--engine', ('"{0}"' -f $Engine), '--model', ('"{0}"' -f $Model),
    '--vad-model', ('"{0}"' -f $VadModel), '--seconds', $Seconds.ToString([Globalization.CultureInfo]::InvariantCulture),
    '--threads', $Threads, '--delay-ms', $DelayMs, '--spool-bytes', $SpoolBytes
)
$process = Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
$process.Id | Set-Content -LiteralPath (Join-Path $OutputDirectory 'process-id.txt') -Encoding ASCII
$gpu = Get-Command nvidia-smi -ErrorAction SilentlyContinue
try {
    while (-not $process.HasExited) {
        $ids = @($process.Id)
        $ids += @(Get-CimInstance Win32_Process -Filter ('ParentProcessId = {0}' -f $process.Id) -ErrorAction SilentlyContinue | ForEach-Object { $_.ProcessId })
        $observed = @(Get-Process -Id $ids -ErrorAction SilentlyContinue | ForEach-Object {
            [ordered]@{ id = $_.Id; name = $_.ProcessName; working_set = $_.WorkingSet64; private_bytes = $_.PrivateMemorySize64; cpu_seconds = $_.CPU }
        })
        $record = [ordered]@{
            utc = [DateTime]::UtcNow.ToString('o')
            processes = $observed
            total_working_set = ($observed | Measure-Object -Property working_set -Sum).Sum
            total_private_bytes = ($observed | Measure-Object -Property private_bytes -Sum).Sum
        }
        if ($gpu) {
            $record.gpu = @(& $gpu.Source --query-gpu=utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits 2>$null)
        }
        $record | ConvertTo-Json -Depth 5 -Compress | Add-Content -LiteralPath $resources -Encoding UTF8
        Start-Sleep -Seconds 3
        $process.Refresh()
    }
    $process.WaitForExit()
    [ordered]@{ finished_utc = [DateTime]::UtcNow.ToString('o'); exit_code = $process.ExitCode } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'exit.json') -Encoding UTF8
    if ($process.ExitCode -ne 0) { throw "Long-session harness failed ($($process.ExitCode)); inspect $stdout and $stderr" }
    Write-Output $OutputDirectory
} finally {
    $process.Dispose()
}
