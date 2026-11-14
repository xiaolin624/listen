param(
    [Parameter(Mandatory = $true)][string]$Corpus,
    [Parameter(Mandatory = $true)][string]$Runtime,
    [ValidateSet('whisper', 'sensevoice')][string]$Engine = 'whisper',
    [string[]]$ListenArgs = $null,
    [string]$Output = '',
    [string]$ListenBinary = '',
    [string]$Baseline = '',
    [int]$TimeoutSeconds = 180,
    [double]$SentenceDelaySeconds = 1,
    [switch]$ListOnly
)
# Replays a recorded meeting-corpus manifest through the default multimedia
# render endpoint while a running Listen captures and recognizes it. The harness
# never recognizes anything itself and never invents numbers: every value in the
# JSON result and in the markdown table comes from runtime\status.txt,
# runtime\current-result.txt and the recorded audio.
#
# The -Engine label names the engine under test. -ListenArgs supplies the command
# line used to launch Listen; omit it to attach to an already running instance:
#   * launch mode: -ListenArgs @('--engine', <whisper-cli.exe>, '--model', ...)
#   * SenseVoice after ticket #17: -ListenArgs @() (default flags, to be confirmed)
#   * attach mode: no -ListenArgs; the harness verifies the runtime's process_id.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
$repositoryRoot = Split-Path $commonGit -Parent
try {
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    $OutputEncoding = [Text.UTF8Encoding]::new($false)
} catch { }

function Quote-Argument([string]$Value) {
    if ($Value.Contains('"')) { throw 'Listen arguments must not contain a double quote.' }
    if ($Value -notmatch '\s') { return $Value }
    return '"' + ($Value -replace '(\\+)$', '$1$1') + '"'
}
function Read-Utf8File([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '' }
    return [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8)
}
function Get-StatusField([string]$Text, [string]$Name) {
    if ($Text -match ('(?m)^' + [regex]::Escape($Name) + '=(.*?)\r?$')) { return $Matches[1] }
    return ''
}
function Get-WavInfo([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $reader = New-Object IO.BinaryReader($stream)
        $riff = [Text.Encoding]::ASCII.GetString($reader.ReadBytes(4))
        [void]$reader.ReadUInt32()
        $wave = [Text.Encoding]::ASCII.GetString($reader.ReadBytes(4))
        if ($riff -ne 'RIFF' -or $wave -ne 'WAVE') { throw "Not a RIFF/WAVE file: $Path" }
        $sampleRate = 0
        $channels = 0
        $bits = 0
        $dataBytes = -1
        while ($stream.Position + 8 -le $stream.Length) {
            $chunkId = [Text.Encoding]::ASCII.GetString($reader.ReadBytes(4))
            $chunkSize = [long]$reader.ReadUInt32()
            if ($chunkId -eq 'fmt ') {
                $format = $reader.ReadUInt16()
                $channels = [int]$reader.ReadUInt16()
                $sampleRate = [int]$reader.ReadUInt32()
                [void]$reader.ReadUInt32()
                [void]$reader.ReadUInt16()
                $bits = [int]$reader.ReadUInt16()
                if ($format -ne 1) { throw "WAV is not integer PCM: $Path" }
                if ($chunkSize -gt 16) { [void]$reader.ReadBytes([int]($chunkSize - 16)) }
            } elseif ($chunkId -eq 'data') {
                $dataBytes = $chunkSize
                [void]$stream.Seek($chunkSize, [IO.SeekOrigin]::Current)
            } else {
                [void]$stream.Seek($chunkSize, [IO.SeekOrigin]::Current)
            }
            if ($chunkSize % 2 -eq 1) { [void]$stream.Seek(1, [IO.SeekOrigin]::Current) }
        }
        if ($dataBytes -lt 0) { throw "WAV has no data chunk: $Path" }
        $blockAlign = $channels * [int]($bits / 8)
        if ($blockAlign -le 0) { throw "Invalid WAV format: $Path" }
        $samples = [long]($dataBytes / $blockAlign)
        return [pscustomobject]@{
            sample_rate = $sampleRate
            channels    = $channels
            bits        = $bits
            samples     = $samples
            duration    = $samples / [double]$sampleRate
        }
    } finally {
        $stream.Dispose()
    }
}

# --- text normalization and character error rate -----------------------------
# Same idea as .artifacts\engine-bench\cer.py: case-fold, Unicode NFKC (so
# full-width digits and punctuation become half-width), then drop every
# punctuation, symbol and whitespace character. Chinese characters, Latin letters
# and digits are compared.
function Normalize-Text([string]$Text) {
    if (-not $Text) { return '' }
    $value = $Text.ToLowerInvariant().Normalize([Text.NormalizationForm]::FormKC)
    $value = [regex]::Replace($value, '[\uFEFF\u200B\u200C\u200D]', '')
    return [regex]::Replace($value, '[\p{P}\p{S}\s]', '')
}
function Get-LevenshteinDistance([string]$Reference, [string]$Hypothesis) {
    $r = $Reference.ToCharArray()
    $h = $Hypothesis.ToCharArray()
    if ($r.Length -eq 0) { return $h.Length }
    $previous = New-Object 'int[]' ($h.Length + 1)
    for ($j = 0; $j -le $h.Length; $j++) { $previous[$j] = $j }
    for ($i = 1; $i -le $r.Length; $i++) {
        $current = New-Object 'int[]' ($h.Length + 1)
        $current[0] = $i
        for ($j = 1; $j -le $h.Length; $j++) {
            $cost = 1
            if ($r[$i - 1] -eq $h[$j - 1]) { $cost = 0 }
            $best = $previous[$j] + 1
            if (($current[$j - 1] + 1) -lt $best) { $best = $current[$j - 1] + 1 }
            if (($previous[$j - 1] + $cost) -lt $best) { $best = $previous[$j - 1] + $cost }
            $current[$j] = $best
        }
        $previous = $current
    }
    return $previous[$h.Length]
}
function Format-Percent($Value) {
    if ($null -eq $Value) { return 'n/a' }
    return ('{0:N1}%' -f ([double]$Value * 100))
}
function Format-Milliseconds($Value) {
    if ($null -eq $Value) { return 'n/a' }
    return ('{0:N0}' -f [double]$Value)
}
function Format-Seconds($Value) {
    if ($null -eq $Value) { return 'n/a' }
    return ('{0:N2}' -f [double]$Value)
}
function Format-MarkdownCell([string]$Text) {
    if (-not $Text) { return '' }
    $value = $Text -replace '\r?\n', ' '
    return ($value -replace '\|', '\|')
}
function Get-Mean($Values) {
    $items = @($Values | Where-Object { $null -ne $_ })
    if ($items.Count -eq 0) { return $null }
    return ($items | Measure-Object -Average).Average
}
function Get-Median($Values) {
    $items = @($Values | Where-Object { $null -ne $_ } | Sort-Object)
    if ($items.Count -eq 0) { return $null }
    $mid = [int][Math]::Floor($items.Count / 2)
    if ($items.Count % 2 -eq 1) { return [double]$items[$mid] }
    return ([double]$items[$mid - 1] + [double]$items[$mid]) / 2
}

# --- corpus manifest ---------------------------------------------------------
$corpusPath = [IO.Path]::GetFullPath($Corpus)
if (Test-Path -LiteralPath $corpusPath -PathType Container) {
    $corpusDir = $corpusPath
    $manifestPath = Join-Path $corpusDir 'manifest.json'
} else {
    $manifestPath = $corpusPath
    $corpusDir = Split-Path $corpusPath -Parent
}
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "Corpus manifest is missing: $manifestPath" }
$manifest = Read-Utf8File $manifestPath | ConvertFrom-Json
$corpusSamples = @($manifest.samples)
if ($corpusSamples.Count -eq 0) { throw "Corpus manifest has no samples: $manifestPath" }
$baselineReceipt = $null
$baselineReceiptPath = ''
$baselineArtifactsVerified = $false
function Test-BaselineArtifacts($Receipt) {
    foreach ($property in $Receipt.artifacts.PSObject.Properties) {
        $artifact = $property.Value
        if (-not (Test-Path -LiteralPath $artifact.path -PathType Leaf) -or
            (Get-FileHash -LiteralPath $artifact.path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $artifact.sha256) {
            throw ("Baseline artifact changed: {0}. Rebuild into a new baseline directory; do not relabel this run." -f $property.Name)
        }
    }
}
if (-not $Baseline -and $manifest.baseline.file) { $Baseline = Join-Path $corpusDir $manifest.baseline.file }
if ($Baseline) {
    if ($PSBoundParameters.ContainsKey('ListenArgs') -or $ListenBinary) {
        throw '-Baseline supplies the fixed binary and arguments; omit -ListenArgs and -ListenBinary.'
    }
    if ($PSBoundParameters.ContainsKey('Engine') -and $Engine -ne 'sensevoice') { throw 'The fixed baseline uses SenseVoice.' }
    $baselineReceiptPath = [IO.Path]::GetFullPath($Baseline)
    $baselineReceipt = Read-Utf8File $baselineReceiptPath | ConvertFrom-Json
    if ($baselineReceipt.kind -ne 'local-baseline-build' -or $baselineReceipt.git_commit -ne '4b8a487e5e6cb5aaedf8bfe8084291cc24540545') {
        throw 'A fixed current-local baseline build receipt is required; fixtures cannot run as quality evidence.'
    }
    Test-BaselineArtifacts $baselineReceipt
    if ($manifest.baseline.sha256 -and
        (Get-FileHash -LiteralPath $baselineReceiptPath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest.baseline.sha256) {
        throw 'The baseline receipt differs from the frozen corpus.'
    }
    $baselineArtifactsVerified = $true
    $ListenBinary = [string]$baselineReceipt.artifacts.listen_binary.path
    $ListenArgs = @($baselineReceipt.listen_args)
    $Engine = 'sensevoice'
}
$Runtime = [IO.Path]::GetFullPath($Runtime)
$statusPath = Join-Path $Runtime 'status.txt'
$resultPath = Join-Path $Runtime 'current-result.txt'
if (-not $Output) {
    $Output = Join-Path (Join-Path $projectRoot '.artifacts\meeting-corpus-bench') ($Engine + '.json')
}
$Output = [IO.Path]::GetFullPath($Output)
$outputDir = Split-Path $Output -Parent
$docPath = Join-Path $projectRoot 'docs\issue-22-benchmark.md'
if ($TimeoutSeconds -lt 10 -or $TimeoutSeconds -gt 1800) { throw 'TimeoutSeconds must be between 10 and 1800.' }

if ($ListOnly) {
    Write-Output ("Corpus manifest: {0}" -f $manifestPath)
    Write-Output ("Samples: {0}" -f $corpusSamples.Count)
    Write-Output ("Engine label: {0}" -f $Engine)
    Write-Output ("Results would be written to: {0}" -f $Output)
    foreach ($sample in $corpusSamples) {
        $audio = ''
        if ($sample.file) { $audio = Join-Path $corpusDir $sample.file }
        elseif ($sample.audio) { $audio = $sample.audio }
        $state = 'missing'
        if ($audio -and (Test-Path -LiteralPath $audio -PathType Leaf)) { $state = 'present' }
        Write-Output ("{0,2}. [{1}] {2}  {3}" -f $sample.index, $state, $sample.id, $sample.reference)
    }
    return
}

function Resolve-ListenBinary([string]$ExplicitPath) {
    if ($ExplicitPath) {
        $resolved = [IO.Path]::GetFullPath($ExplicitPath)
        if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) { throw "Listen binary not found: $resolved" }
        return $resolved
    }
    foreach ($candidate in @(
            (Join-Path $projectRoot 'app\listen\target\release\listen.exe'),
            (Join-Path $repositoryRoot '.artifacts\cargo-target\release\listen.exe')
        )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    return ''
}

$launchedListen = $false
$listenProcess = $null
$listenPath = ''
$binaryVerified = $false
$launchMode = $PSBoundParameters.ContainsKey('ListenArgs') -or $null -ne $baselineReceipt

if ($launchMode) {
    $launchBinary = Resolve-ListenBinary -ExplicitPath $ListenBinary
    if (-not $launchBinary) { throw 'No built listen.exe was found. Build it or pass -ListenBinary.' }
    $running = Get-Process -Name listen -ErrorAction SilentlyContinue
    if ($running) {
        throw ("A Listen process is already running (PID {0}). Quit it first, or omit -ListenArgs to attach to it." -f @($running)[0].Id)
    }
    if (Test-Path -LiteralPath $statusPath -PathType Leaf) { Remove-Item -LiteralPath $statusPath -Force }
    $quoted = @()
    foreach ($argument in $ListenArgs) { $quoted += (Quote-Argument $argument) }
    $quoted += @('--runtime', (Quote-Argument $Runtime))
    Write-Output ("Launching Listen: {0} {1}" -f $launchBinary, ($quoted -join ' '))
    $listenProcess = Start-Process -FilePath $launchBinary -ArgumentList $quoted -WindowStyle Hidden -PassThru
    $launchedListen = $true
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    $listenStatus = ''
    while ($true) {
        Start-Sleep -Milliseconds 100
        if ($listenProcess.HasExited) {
            $hint = ''
            if ($listenProcess.ExitCode -eq -1073741515) {
                $hint = ' STATUS_DLL_NOT_FOUND: make sure libunwind.dll is next to listen.exe (build-listen.ps1 copies it).'
            }
            throw ("Listen exited during startup with code {0}.{1} Its window may have shown a configuration error; check the arguments." -f $listenProcess.ExitCode, $hint)
        }
        $listenStatus = Read-Utf8File $statusPath
        if ($listenStatus -match '(?m)^process_id=(\d+)') {
            if ([int]$Matches[1] -eq $listenProcess.Id) { break }
        }
        if ([DateTime]::UtcNow -gt $deadline) {
            try { $listenProcess.Kill() } catch { }
            throw ("Listen did not write {0} for its own process within 120 seconds." -f $statusPath)
        }
    }
    $launchedListen = $true
    $listenPath = $launchBinary
    $binaryVerified = $true
} else {
    $initialStatus = Read-Utf8File $statusPath
    if (-not $initialStatus) {
        throw ("No running Listen was found for runtime {0}. Start Listen with --runtime {0}, or pass -ListenArgs to let this harness launch it." -f $Runtime)
    }
    if ($initialStatus -notmatch '(?m)^process_id=(\d+)') {
        throw 'Rebuild and start Listen so status.txt identifies its live process.'
    }
    $reportedPid = [int]$Matches[1]
    $listenProcess = Get-Process -Id $reportedPid -ErrorAction SilentlyContinue
    if (-not $listenProcess) { throw ("status.txt in {0} belongs to process {1}, which is not running. Start Listen again or delete the stale status file." -f $Runtime, $reportedPid) }
    if ($listenProcess.ProcessName -ne 'listen') { throw ("The runtime belongs to process {0} ({1}), not a live listen.exe." -f $reportedPid, $listenProcess.ProcessName) }
    try { $listenPath = $listenProcess.Path } catch { $listenPath = '' }
    $expected = Resolve-ListenBinary -ExplicitPath $ListenBinary
    if ($ListenBinary) {
        if (-not $listenPath) { throw ("Cannot read the image path of the running Listen (PID {0}) to verify -ListenBinary." -f $reportedPid) }
        if ($listenPath -ine $expected) { throw ("The runtime belongs to {0}, not the requested binary {1}." -f $listenPath, $expected) }
        $binaryVerified = $true
    } elseif ($expected) {
        if (-not $listenPath) {
            Write-Warning ("Could not read the image path of the running Listen; -ListenBinary was not verified.")
        } elseif ($listenPath -ine $expected) {
            Write-Warning ("The running Listen is {0}, not the local build {1}. The engine label {2} is whatever that build was started with; the app's backend note is recorded in the results." -f $listenPath, $expected, $Engine)
        } else {
            $binaryVerified = $true
        }
    } else {
        Write-Warning "No local listen.exe build was found to compare with the running process; only the live process identity was verified."
    }
}
if (-not $listenPath) {
    $listenPath = Resolve-ListenBinary -ExplicitPath $ListenBinary
    if (-not $listenPath) { throw 'Cannot find a listen.exe to send --control commands.' }
}
$controlBinary = $listenPath

function Invoke-Control([string]$Command) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $controlBinary
    $info.Arguments = '--control ' + $Command
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardError = $true
    $info.RedirectStandardOutput = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(15000)) {
            try { $process.Kill() } catch { }
            throw ("Listen control '{0}' did not exit within 15 seconds." -f $Command)
        }
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) {
            throw ("Listen control '{0}' failed ({1}): {2}" -f $Command, $process.ExitCode, $stderr)
        }
        if ($stderr) { Write-Output $stderr }
    } finally {
        $process.Dispose()
    }
}
function Get-StatusText { return (Read-Utf8File $statusPath) }
function Wait-ForPhase([string]$Pattern, [int]$Seconds, [string]$What) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ($true) {
        Start-Sleep -Milliseconds 100
        $status = Get-StatusText
        if ($status -match $Pattern) { return $status }
        if ($status -match '(?m)^phase=Failure' -and $Pattern -notmatch 'Failure') { return $status }
        if ([DateTime]::UtcNow -gt $deadline) {
            throw ("Timed out waiting for {0}. Last status: {1}" -f $What, $status)
        }
    }
}
function Clear-FailedCapture {
    Invoke-Control 'cleanup'
    Start-Sleep -Milliseconds 500
    $status = Get-StatusText
    if ($status -match '(?m)^retry_available=true') {
        throw ("Listen still retains audio for a local retry after cleanup, so the benchmark cannot continue. Runtime: {0}" -f $Runtime)
    }
}

# Wait out model preparation before the first capture, exactly like the
# acceptance driver for issue #9.
$preparationDeadline = [DateTime]::UtcNow.AddSeconds(300)
while ($true) {
    $status = Get-StatusText
    if (-not $status) { throw ("Listen did not write {0} within 300 seconds." -f $statusPath) }
    if ($status -notmatch '(?m)^phase=Preparing') { break }
    if ([DateTime]::UtcNow -gt $preparationDeadline) {
        throw ("Listen is still preparing its local model after 300 seconds. Last status: {0}" -f $status)
    }
    Start-Sleep -Milliseconds 200
}
if ($status -match '(?m)^phase=Failure') { throw ("Listen reports a failure before the benchmark starts: {0}" -f $status) }
if ($status -match '(?m)^phase=(Capturing|Processing)') { throw 'Listen is already capturing or processing; finish that session before the benchmark.' }
if ($status -match '(?m)^retry_available=true') { Clear-FailedCapture }

$startedAt = (Get-Date).ToString('o')
$results = New-Object System.Collections.Generic.List[object]
$fatal = ''
$index = 0
try {
    foreach ($sample in $corpusSamples) {
        $index++
        $captureInvoked = $false
        $stopwatch = $null
        $audio = ''
        if ($sample.file) { $audio = Join-Path $corpusDir $sample.file }
        elseif ($sample.audio) { $audio = [IO.Path]::GetFullPath($sample.audio) }
        if (-not $audio -or -not (Test-Path -LiteralPath $audio -PathType Leaf)) {
            throw ("Audio for sentence '{0}' is missing: {1}" -f $sample.id, $audio)
        }
        $info = Get-WavInfo $audio
        if ($info.sample_rate -ne 16000 -or $info.channels -ne 1 -or $info.bits -ne 16) {
            throw ("Audio for sentence '{0}' is not a 16 kHz mono 16-bit PCM WAV: {1}" -f $sample.id, $audio)
        }
        $sha = (Get-FileHash -LiteralPath $audio -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($sample.sha256 -and $sha -ne $sample.sha256) {
            throw ("Corpus checksum changed for sentence '{0}': expected {1}, got {2}." -f $sample.id, $sample.sha256, $sha)
        }
        if (-not $sample.reference) { throw ("Corpus sentence '{0}' has no reference text." -f $sample.id) }

        $before = Read-Utf8File $resultPath
        Invoke-Control 'start'
        $captureInvoked = $true
        $status = Wait-ForPhase '^phase=Capturing' 10 'the Capturing phase'
        if ($status -match '(?m)^phase=Failure') { throw ("Capture could not start: {0}" -f (Get-StatusField $status 'detail')) }
        Write-Host ("[{0}/{1}] {2}: playing {3} ({4} s) ..." -f $index, $corpusSamples.Count, $sample.id, $audio, (Format-Seconds $info.duration))
        $player = New-Object System.Media.SoundPlayer($audio)
        $player.Load()
        try { $player.PlaySync() } finally { $player.Dispose() }
        Start-Sleep -Milliseconds 250
        $stopwatch = [Diagnostics.Stopwatch]::StartNew()
        Invoke-Control 'stop'
        $status = Wait-ForPhase '^phase=(Result|NoSpeech|Failure)' $TimeoutSeconds 'a terminal phase'
        $stopwatch.Stop()
        $phase = Get-StatusField $status 'phase'
        $detail = Get-StatusField $status 'detail'
        $backend = Get-StatusField $status 'backend'
        $endpointName = Get-StatusField $status 'endpoint_name'
        $effectiveGlossary = Get-StatusField $status 'glossary'
        if ($baselineReceipt -and ($backend -notmatch '^SenseVoice q8' -or
            $effectiveGlossary -ine $baselineReceipt.artifacts.glossary.path)) {
            $baselineArtifactsVerified = $false
            throw 'The running backend or effective glossary does not match the fixed baseline.'
        }
        $timingText = Get-StatusField $status 'timing_stop_to_result_ms'
        $timingMs = $null
        if ($timingText -match '^\d+$') { $timingMs = [long]$timingText }
        $text = ''
        $errorText = ''
        if ($phase -eq 'Result') {
            $text = Read-Utf8File $resultPath
        } elseif ($phase -eq 'Failure') {
            $errorText = $detail
        }
        $reference = $sample.reference
        $normalizedReference = Normalize-Text $reference
        $normalizedText = Normalize-Text $text
        $distance = $normalizedReference.Length
        if ($phase -eq 'Result') {
            $distance = Get-LevenshteinDistance -Reference $normalizedReference -Hypothesis $normalizedText
        }
        $cer = $null
        if ($normalizedReference.Length -gt 0) { $cer = $distance / [double]$normalizedReference.Length }
        $entry = [ordered]@{
            id                     = $sample.id
            index                  = $index
            reference              = $reference
            audio                  = $audio
            audio_duration_seconds = [math]::Round($info.duration, 3)
            sha256                 = $sha
            phase                  = $phase
            text                   = $text
            error                  = $errorText
            reference_chars        = $normalizedReference.Length
            distance_chars         = $distance
            cer                    = $cer
            accuracy               = $(if ($null -ne $cer) { [Math]::Max(0.0, 1.0 - $cer) } else { $null })
            stop_to_result_ms      = $timingMs
            wall_stop_to_result_ms = [math]::Round($stopwatch.Elapsed.TotalMilliseconds, 1)
            backend                = $backend
            glossary               = $effectiveGlossary
            endpoint_name          = $endpointName
            detail                 = $detail
        }
        $results.Add($entry)
        $line = "[{0}/{1}] {2}: {3}, CER {4}, stop->result {5} ms" -f $index, $corpusSamples.Count, $sample.id, $phase, (Format-Percent $cer), (Format-Milliseconds $timingMs)
        Write-Host $line -ForegroundColor $(if ($phase -eq 'Result') { 'Green' } elseif ($phase -eq 'Failure') { 'Red' } else { 'Yellow' })
        if ($phase -eq 'Result' -and $text) { Write-Host ('    ' + $text) -ForegroundColor DarkGray }
        if ($phase -eq 'Failure') { Write-Host ('    ' + $errorText) -ForegroundColor Red; Clear-FailedCapture }
        if ($SentenceDelaySeconds -gt 0) { Start-Sleep -Milliseconds ([int]($SentenceDelaySeconds * 1000)) }
    }
} catch {
    $fatal = $_.Exception.Message
} finally {
    if ($launchedListen -and $listenProcess) {
        try {
            if (-not $listenProcess.HasExited) {
                Invoke-Control 'quit'
                if (-not $listenProcess.WaitForExit(15000)) {
                    try { $listenProcess.Kill() } catch { }
                }
            }
        } catch {
            try { if (-not $listenProcess.HasExited) { $listenProcess.Kill() } } catch { }
        }
    }
}

# A fatal capture/control/timeout error must not silently shrink the denominator.
# Retain every identity, including the interrupted sample and untouched tail.
foreach ($pendingSample in $corpusSamples) {
    if (@($results | Where-Object { $_.id -eq $pendingSample.id }).Count -gt 0) { continue }
    $pendingReference = Normalize-Text $pendingSample.reference
    $pendingPhase = 'NotRun'
    $pendingWall = $null
    if ($sample -and $pendingSample.id -eq $sample.id -and $captureInvoked) {
        $pendingPhase = 'Failure'
        if ($fatal -match 'Timed out') { $pendingPhase = 'Timeout' }
        if ($stopwatch) { $pendingWall = [math]::Round($stopwatch.Elapsed.TotalMilliseconds, 1) }
        else { $pendingWall = 0 }
    }
    $results.Add([ordered]@{
        id = $pendingSample.id; index = [array]::IndexOf($corpusSamples, $pendingSample) + 1
        reference = $pendingSample.reference; sha256 = $pendingSample.sha256
        phase = $pendingPhase; text = ''; error = $fatal
        reference_chars = $pendingReference.Length; distance_chars = $pendingReference.Length
        cer = 1.0; accuracy = 0.0; stop_to_result_ms = $null; wall_stop_to_result_ms = $pendingWall
        backend = ''; endpoint_name = ''; glossary = ''; detail = $fatal
    })
}
if ($baselineReceipt) {
    try { Test-BaselineArtifacts $baselineReceipt } catch {
        $baselineArtifactsVerified = $false
        $fatal = ($fatal + ' ' + $_.Exception.Message).Trim()
    }
}

# --- aggregate ---------------------------------------------------------------
$scored = @($results | Where-Object { $_.reference_chars -gt 0 })
$totalReferenceChars = 0
$totalDistance = 0
$latencies = @()
$wallLatencies = @()
$recognized = 0
$noSpeech = 0
$failed = 0
foreach ($entry in $scored) {
    $totalReferenceChars += $entry.reference_chars
    $totalDistance += $entry.distance_chars
    if ($null -ne $entry.stop_to_result_ms) { $latencies += [double]$entry.stop_to_result_ms }
    if ($null -ne $entry.wall_stop_to_result_ms) { $wallLatencies += [double]$entry.wall_stop_to_result_ms }
    if ($entry.phase -eq 'Result') { $recognized++ }
    elseif ($entry.phase -eq 'NoSpeech') { $noSpeech++ }
    else { $failed++ }
}
$accuracy = $null
if ($totalReferenceChars -gt 0) {
    # Clamp only the reported accuracy; per-sentence CER itself stays unclamped so
    # an insertion-heavy result is still visible as CER above 100%.
    $accuracy = [Math]::Max(0.0, 1.0 - ($totalDistance / [double]$totalReferenceChars))
}
$meanSentenceAccuracy = Get-Mean @($scored | ForEach-Object { $_.accuracy })
$meanLatency = Get-Mean $latencies
$medianLatency = Get-Median $latencies
$meanWallLatency = Get-Mean $wallLatencies

$terms = @('DeepSeek Harness', 'WASAPI', 'CUDA', 'SenseVoice', 'sherpa-onnx')
$properNouns = @()
$expectedTotal = 0
$hitTotal = 0
foreach ($term in $terms) {
    $normalizedTerm = Normalize-Text $term
    $expected = @()
    $hits = @()
    $missed = @()
    foreach ($entry in $scored) {
        if ((Normalize-Text $entry.reference).Contains($normalizedTerm)) {
            $expected += $entry.id
            if ($entry.phase -eq 'Result' -and (Normalize-Text $entry.text).Contains($normalizedTerm)) {
                $hits += $entry.id
            } else {
                $missed += $entry.id
            }
        }
    }
    $expectedTotal += $expected.Count
    $hitTotal += $hits.Count
    $recall = $null
    if ($expected.Count -gt 0) { $recall = $hits.Count / [double]$expected.Count }
    $properNouns += [ordered]@{
        term             = $term
        expected_samples = $expected
        hits             = $hits
        missed           = $missed
        recall           = $recall
    }
}
$overallRecall = $null
if ($expectedTotal -gt 0) { $overallRecall = $hitTotal / [double]$expectedTotal }

$summary = [ordered]@{
    sample_count                 = $results.Count
    scored_count                 = $scored.Count
    recognized_count             = $recognized
    no_speech_count              = $noSpeech
    failed_count                 = $failed
    reference_chars              = $totalReferenceChars
    distance_chars               = $totalDistance
    chinese_accuracy             = $accuracy
    mean_sentence_accuracy       = $meanSentenceAccuracy
    mean_stop_to_result_ms       = $meanLatency
    median_stop_to_result_ms     = $medianLatency
    latency_sample_count         = $latencies.Count
    mean_wall_stop_to_result_ms  = $meanWallLatency
    proper_noun_recall           = $overallRecall
    proper_noun_expected_count   = $expectedTotal
    proper_noun_hit_count        = $hitTotal
    accuracy_definition          = '1 - (sum Levenshtein distance / sum reference characters) over every corpus sentence; punctuation, symbols and whitespace are removed first. Recognition failures and no-speech count as all characters wrong. The aggregate accuracy is clamped at 0; per-sentence CER is not clamped.'
}
$result = [ordered]@{
    corpus_manifest       = $manifestPath
    corpus_manifest_sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    baseline_receipt_sha256 = $(if ($baselineReceiptPath) { (Get-FileHash -LiteralPath $baselineReceiptPath -Algorithm SHA256).Hash.ToLowerInvariant() } else { $null })
    baseline_artifacts_verified = $baselineArtifactsVerified
    corpus_dir            = $corpusDir
    engine                = $Engine
    engine_label_source   = $(if ($launchMode) { 'launch arguments' } else { 'operator (attached instance)' })
    listen_mode           = $(if ($launchMode) { 'launched' } else { 'attached' })
    listen_args           = $(if ($launchMode) { @($ListenArgs) } else { $null })
    listen_binary         = $listenPath
    listen_binary_sha256  = (Get-FileHash -LiteralPath $listenPath -Algorithm SHA256).Hash.ToLowerInvariant()
    listen_binary_verified = $binaryVerified
    runtime               = $Runtime
    started_at            = $startedAt
    completed_at          = (Get-Date).ToString('o')
    timeout_seconds       = $TimeoutSeconds
    fatal_error           = $(if ($fatal) { $fatal } else { $null })
    summary               = $summary
    proper_nouns          = $properNouns
    samples               = $results.ToArray()
}
if (-not (Test-Path -LiteralPath $outputDir -PathType Container)) { $null = New-Item -ItemType Directory -Force -Path $outputDir }
$json = ConvertTo-Json -InputObject $result -Depth 12
[IO.File]::WriteAllText($Output, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))

if ($baselineReceipt) {
    # #26 uses the stricter content-CER policy and expression review. The legacy
    # #22 metrics above remain readable for old consumers but are not acceptance
    # evidence, and must not overwrite the old two-engine comparison document.
    $evaluationPath = Join-Path $outputDir ([IO.Path]::GetFileNameWithoutExtension($Output) + '-evaluation.json')
    & python (Join-Path $PSScriptRoot 'evaluate-meeting-corpus.py') score --manifest $manifestPath --results $Output --output $evaluationPath
    if ($LASTEXITCODE -ne 0) { throw 'Could not evaluate the fixed baseline; raw results were preserved.' }
    Write-Output ("Baseline raw results: {0}" -f $Output)
    Write-Output ("Authoritative content CER and pending expression reviews: {0}" -f $evaluationPath)
    if ($fatal) { throw $fatal }
    return
}

# --- console report ----------------------------------------------------------
Write-Host ''
Write-Host ("Engine: {0} ({1}); results: {2}" -f $Engine, $result.engine_label_source, $Output) -ForegroundColor Cyan
Write-Host ("Corpus: {0}" -f $manifestPath)
Write-Host ''
$header = '| # | id | phase | CER | stop->result ms | recognized text |'
$separator = '| ---: | --- | --- | ---: | ---: | --- |'
Write-Output $header
Write-Output $separator
foreach ($entry in $results) {
    $cellText = $entry.text
    if ($entry.phase -eq 'Failure') { $cellText = 'FAILED: ' + $entry.error }
    Write-Output ("| {0} | {1} | {2} | {3} | {4} | {5} |" -f $entry.index, $entry.id, $entry.phase, (Format-Percent $entry.cer), (Format-Milliseconds $entry.stop_to_result_ms), (Format-MarkdownCell $cellText))
}
Write-Host ''
Write-Output ("Chinese accuracy (1 - micro CER): {0} ({1} / {2} characters wrong)" -f (Format-Percent $accuracy), $totalDistance, $totalReferenceChars)
Write-Output ("Mean sentence accuracy: {0}" -f (Format-Percent $meanSentenceAccuracy))
Write-Output ("Stop->result: mean {0} ms, median {1} ms (n={2}); wall-clock mean {3} ms" -f (Format-Milliseconds $meanLatency), (Format-Milliseconds $medianLatency), $latencies.Count, (Format-Milliseconds $meanWallLatency))
Write-Output ("Phases: Result {0}, NoSpeech {1}, Failure {2}" -f $recognized, $noSpeech, $failed)
foreach ($noun in $properNouns) {
    Write-Output ("Proper noun {0}: {1} / {2} {3}" -f $noun.term, $noun.hits.Count, $noun.expected_samples.Count, $(if ($noun.missed.Count -gt 0) { '(missed: ' + ($noun.missed -join ', ') + ')' } else { '' }))
}
Write-Output ("Proper-noun recall: {0} ({1} / {2})" -f (Format-Percent $overallRecall), $hitTotal, $expectedTotal)
if ($fatal) { Write-Host ("FATAL: " + $fatal) -ForegroundColor Red }

# --- comparison document (only with both engines on the same corpus) ---------
function Get-EngineFile([string]$Name) { return (Join-Path $outputDir ($Name + '.json')) }
$whisperFile = Get-EngineFile 'whisper'
$sensevoiceFile = Get-EngineFile 'sensevoice'
$comparisonReady = $false
$whisperResult = $null
$sensevoiceResult = $null
if ((Test-Path -LiteralPath $whisperFile -PathType Leaf) -and (Test-Path -LiteralPath $sensevoiceFile -PathType Leaf)) {
    try {
        $whisperResult = Read-Utf8File $whisperFile | ConvertFrom-Json
        $sensevoiceResult = Read-Utf8File $sensevoiceFile | ConvertFrom-Json
    } catch {
        $whisperResult = $null
        $sensevoiceResult = $null
    }
    if ($whisperResult -and $sensevoiceResult) {
        $whisperManifest = [IO.Path]::GetFullPath($whisperResult.corpus_manifest)
        $sensevoiceManifest = [IO.Path]::GetFullPath($sensevoiceResult.corpus_manifest)
        if ($whisperManifest -ine $sensevoiceManifest) {
            Write-Warning ("Both result files exist, but they used different corpus manifests ('{0}' vs '{1}'); docs\issue-22-benchmark.md is not written." -f $whisperManifest, $sensevoiceManifest)
        } elseif (@($whisperResult.samples).Count -eq 0 -or @($sensevoiceResult.samples).Count -eq 0) {
            Write-Warning 'Both result files exist, but at least one contains no samples; docs\issue-22-benchmark.md is not written.'
        } else {
            $comparisonReady = $true
        }
    }
}
if (-not $comparisonReady) {
    $missing = @()
    if (-not (Test-Path -LiteralPath $whisperFile -PathType Leaf)) { $missing += 'whisper' }
    if (-not (Test-Path -LiteralPath $sensevoiceFile -PathType Leaf)) { $missing += 'sensevoice' }
    Write-Host ''
    if ($missing.Count -gt 0) {
        Write-Output ("Comparison not written: docs\issue-22-benchmark.md requires results from both engines. Missing: {0}." -f ($missing -join ', '))
        if ($missing -contains 'sensevoice') { Write-Output ("  Run -Engine sensevoice to produce {0}. Until ticket #17 merges, the SenseVoice invocation is the app's default flags (to be confirmed)." -f $sensevoiceFile) }
        if ($missing -contains 'whisper') { Write-Output ("  Run -Engine whisper to produce {0}." -f $whisperFile) }
    } else {
        Write-Output 'Comparison not written: the two result files are not a valid pair (different corpus or empty samples).'
    }
} else {
    $now = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss zzz')
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add('# Issue #22 benchmark: real Tencent-Meeting corpus')
    $lines.Add('')
    $lines.Add(('Generated by `scripts\benchmark-meeting-corpus.ps1` on {0}. This file is written only when both' -f $now))
    $lines.Add('engine result files exist for the same corpus manifest. Every number below is copied from those')
    $lines.Add('JSON files; nothing is estimated.')
    $lines.Add('')
    $lines.Add('## Provenance')
    $lines.Add('')
    $lines.Add('| item | whisper | sensevoice |')
    $lines.Add('| --- | --- | --- |')
    $lines.Add(('| result file | `{0}` | `{1}` |' -f $whisperFile, $sensevoiceFile))
    $lines.Add(('| corpus manifest | `{0}` | `{1}` |' -f $whisperResult.corpus_manifest, $sensevoiceResult.corpus_manifest))
    $lines.Add(('| listen mode | {0} | {1} |' -f $whisperResult.listen_mode, $sensevoiceResult.listen_mode))
    $lines.Add(('| engine label source | {0} | {1} |' -f $whisperResult.engine_label_source, $sensevoiceResult.engine_label_source))
    $lines.Add(('| runtime | `{0}` | `{1}` |' -f $whisperResult.runtime, $sensevoiceResult.runtime))
    $lines.Add(('| completed at | {0} | {1} |' -f $whisperResult.completed_at, $sensevoiceResult.completed_at))
    $lines.Add('')
    $lines.Add('## Headline comparison')
    $lines.Add('')
    $lines.Add('| metric | whisper | sensevoice |')
    $lines.Add('| --- | ---: | ---: |')
    $lines.Add(('| Chinese accuracy (1 - micro CER) | {0} | {1} |' -f (Format-Percent $whisperResult.summary.chinese_accuracy), (Format-Percent $sensevoiceResult.summary.chinese_accuracy)))
    $lines.Add(('| mean sentence accuracy | {0} | {1} |' -f (Format-Percent $whisperResult.summary.mean_sentence_accuracy), (Format-Percent $sensevoiceResult.summary.mean_sentence_accuracy)))
    $lines.Add(('| mean stop->result | {0} ms | {1} ms |' -f (Format-Milliseconds $whisperResult.summary.mean_stop_to_result_ms), (Format-Milliseconds $sensevoiceResult.summary.mean_stop_to_result_ms)))
    $lines.Add(('| median stop->result | {0} ms | {1} ms |' -f (Format-Milliseconds $whisperResult.summary.median_stop_to_result_ms), (Format-Milliseconds $sensevoiceResult.summary.median_stop_to_result_ms)))
    $lines.Add(('| mean wall-clock stop->result | {0} ms | {1} ms |' -f (Format-Milliseconds $whisperResult.summary.mean_wall_stop_to_result_ms), (Format-Milliseconds $sensevoiceResult.summary.mean_wall_stop_to_result_ms)))
    $lines.Add(('| proper-noun recall | {0} ({1}/{2}) | {3} ({4}/{5}) |' -f (Format-Percent $whisperResult.summary.proper_noun_recall), $whisperResult.summary.proper_noun_hit_count, $whisperResult.summary.proper_noun_expected_count, (Format-Percent $sensevoiceResult.summary.proper_noun_recall), $sensevoiceResult.summary.proper_noun_hit_count, $sensevoiceResult.summary.proper_noun_expected_count))
    $lines.Add(('| samples (recognized / no-speech / failed) | {0} / {1} / {2} | {3} / {4} / {5} |' -f $whisperResult.summary.recognized_count, $whisperResult.summary.no_speech_count, $whisperResult.summary.failed_count, $sensevoiceResult.summary.recognized_count, $sensevoiceResult.summary.no_speech_count, $sensevoiceResult.summary.failed_count))
    $lines.Add('')
    $lines.Add('## Proper nouns')
    $lines.Add('')
    $lines.Add('| term | whisper hits | sensevoice hits | expected sentences |')
    $lines.Add('| --- | ---: | ---: | --- |')
    for ($i = 0; $i -lt $whisperResult.proper_nouns.Count; $i++) {
        $wh = $whisperResult.proper_nouns[$i]
        $sv = $sensevoiceResult.proper_nouns[$i]
        $lines.Add(('| {0} | {1} / {2} | {3} / {4} | {5} |' -f $wh.term, $wh.hits.Count, $wh.expected_samples.Count, $sv.hits.Count, $sv.expected_samples.Count, ($wh.expected_samples -join ', ')))
    }
    $lines.Add('')
    $lines.Add('## Per-sentence results')
    foreach ($pair in @(
            @{ name = 'whisper'; data = $whisperResult },
            @{ name = 'sensevoice'; data = $sensevoiceResult }
        )) {
        $lines.Add('')
        $lines.Add(('### ' + $pair.name))
        $lines.Add('')
        $lines.Add('| # | id | phase | CER | stop->result ms | recognized text |')
        $lines.Add('| ---: | --- | --- | ---: | ---: | --- |')
        foreach ($entry in @($pair.data.samples)) {
            $cell = $entry.text
            if ($entry.phase -eq 'Failure') { $cell = 'FAILED: ' + $entry.error }
            $lines.Add(("| {0} | {1} | {2} | {3} | {4} | {5} |" -f $entry.index, $entry.id, $entry.phase, (Format-Percent $entry.cer), (Format-Milliseconds $entry.stop_to_result_ms), (Format-MarkdownCell $cell)))
        }
    }
    $lines.Add('')
    $lines.Add('## Notes')
    $lines.Add('')
    $lines.Add('- `Chinese accuracy` is 1 - (sum edit distance / sum reference characters) after removing punctuation, symbols and spaces. Recognition failures and no-speech count as all characters wrong; the aggregate is clamped at 0 while per-sentence CER is not.')
    $lines.Add('- `stop->result` is `timing_stop_to_result_ms` reported by Listen; the wall-clock column also includes control-process latency.')
    $lines.Add('- The engine label is operator supplied in attached mode; the app''s own `backend=` note is stored with every sample in the JSON results.')
    $lines.Add('- The corpus was recorded through the Tencent-Meeting link; playback during this benchmark uses the default multimedia render endpoint, not the meeting application.')
    if ($whisperResult.fatal_error -or $sensevoiceResult.fatal_error) {
        $lines.Add('- A run reported a fatal error; see `fatal_error` in the result JSON. The affected numbers may be incomplete.')
    }
    $null = New-Item -ItemType Directory -Force -Path (Split-Path $docPath -Parent)
    [IO.File]::WriteAllText($docPath, (($lines -join [Environment]::NewLine) + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
    Write-Host ''
    Write-Output ("Both engine results are present; wrote comparison to {0}" -f $docPath)
}

if ($fatal) { throw $fatal }
