param(
    [Alias('Corpus')][string]$Definition = '',
    [string]$Output = '',
    [string]$Only = '',
    [double]$SentenceDelaySeconds = 2,
    [switch]$ListOnly,
    [string]$Recorder = ''
)
# Records the real Tencent-Meeting sentences in an explicit corpus (#22/#28).
# captured from the default multimedia render endpoint by capture_only.exe and is
# never recognized, transcribed or uploaded. The sentence text and all user-facing
# recording instructions live in quality\meeting-corpus.json so this script stays
# ASCII-only; the prompts below are English and the sentences are printed as-is.
#
# Each take is written to <Output>\<id>.wav with a matching sentence-<n>.txt and a
# manifest.json that records id, audio path, SHA-256, duration, reference and
# recorded_at. A full run refuses to report success while any sentence is missing.
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
    if ($Value.Contains('"')) { throw 'Capture paths must not contain a double quote.' }
    if ($Value -notmatch '\s') { return $Value }
    return '"' + ($Value -replace '(\\+)$', '$1$1') + '"'
}
function Join-Arguments([string[]]$Values) {
    $parts = @()
    foreach ($value in $Values) { $parts += (Quote-Argument $value) }
    return ($parts -join ' ')
}
function Write-Utf8Text([string]$Path, [string]$Content, [bool]$Bom) {
    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($Bom))
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

$corpusPath = Join-Path $projectRoot 'quality\meeting-corpus.json'
if ($Definition) { $corpusPath = [IO.Path]::GetFullPath($Definition) }
if (-not (Test-Path -LiteralPath $corpusPath -PathType Leaf)) { throw "Corpus file is missing: $corpusPath" }
$corpus = [IO.File]::ReadAllText($corpusPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
$samples = @($corpus.samples)
$requiredCount = 10
if ($corpus.required_sample_count) { $requiredCount = [int]$corpus.required_sample_count }
if ($samples.Count -ne $requiredCount) { throw "Corpus must contain exactly $requiredCount sentences; found $($samples.Count)." }
$targetSeconds = 12
if ($corpus.target_seconds) { $targetSeconds = [int]$corpus.target_seconds }
$indexById = @{}
for ($i = 0; $i -lt $samples.Count; $i++) {
    $sample = $samples[$i]
    if (-not $sample.id -or -not $sample.spoken -or -not $sample.reference) {
        throw "Corpus sentence $($i + 1) is missing id, spoken or reference."
    }
    if ($sample.id -notmatch '^[a-z0-9][a-z0-9-]{0,79}$') { throw "Unsafe corpus id: $($sample.id)" }
    if ($indexById.ContainsKey($sample.id)) { throw "Duplicate corpus id: $($sample.id)" }
    $indexById[$sample.id] = $i + 1
}
if ($Only -and -not $indexById.ContainsKey($Only)) {
    throw "Unknown sentence id: $Only. Use -ListOnly to see the ids."
}
if ($SentenceDelaySeconds -lt 0 -or $SentenceDelaySeconds -gt 30) {
    throw 'SentenceDelaySeconds must be between 0 and 30.'
}
if (-not $Output) {
    $directory = 'meeting-corpus'
    if ($corpus.kind -eq 'real-meeting-evaluation') { $directory = 'cloud-meeting-corpus' }
    $Output = Join-Path (Join-Path $projectRoot '.artifacts') $directory
}
$Output = [IO.Path]::GetFullPath($Output)
$manifestPath = Join-Path $Output 'manifest.json'

$recorded = @{}
if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
    try {
        $previous = [IO.File]::ReadAllText($manifestPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    } catch {
        throw "Existing manifest cannot be read: $manifestPath. Move it aside to start over. ($($_.Exception.Message))"
    }
    if ($corpus.kind -eq 'real-meeting-evaluation' -and
        ($previous.kind -ne $corpus.kind -or $previous.corpus_sha256 -ne (Get-FileHash -LiteralPath $corpusPath -Algorithm SHA256).Hash.ToLowerInvariant())) {
        throw 'The existing recording manifest uses a different definition; preserve it and choose a new -Output directory.'
    }
    foreach ($entry in @($previous.samples)) {
        if ($entry -and $entry.id) { $recorded[$entry.id] = $entry }
    }
}

function Get-EntryAudioPath($Entry) {
    if ($Entry.file) {
        $candidate = Join-Path $Output $Entry.file
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    if ($Entry.audio -and (Test-Path -LiteralPath $Entry.audio -PathType Leaf)) { return $Entry.audio }
    return ''
}
function Test-RecordedEntry($Entry) {
    if (-not $Entry) { return $false }
    $audio = Get-EntryAudioPath $Entry
    if (-not $audio) { return $false }
    if ((Get-FileHash -LiteralPath $audio -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Entry.sha256) { return $false }
    try { $info = Get-WavInfo $audio } catch { return $false }
    return ($info.sample_rate -eq 16000 -and $info.channels -eq 1 -and $info.bits -eq 16 -and $info.samples -gt 0)
}
function New-Manifest($Corpus, $Recorded, [int]$Count, [string]$RecorderPath) {
    $entries = New-Object System.Collections.Generic.List[object]
    foreach ($sample in @($Corpus.samples)) {
        if ($Recorded.ContainsKey($sample.id)) { $entries.Add($Recorded[$sample.id]) }
    }
    return [ordered]@{
        corpus          = $corpusPath
        corpus_sha256   = (Get-FileHash -LiteralPath $corpusPath -Algorithm SHA256).Hash.ToLowerInvariant()
        corpus_version  = $Corpus.version
        kind            = $Corpus.kind
        sample_rate     = $Corpus.sample_rate
        channels        = $Corpus.channels
        bits_per_sample = $Corpus.bits_per_sample
        recorder        = $RecorderPath
        recording_chain = $(if ($Corpus.recording_chain) { $Corpus.recording_chain } else { 'phone in Tencent Meeting -> PC in the same meeting -> default multimedia render endpoint (headphones) -> capture_only loopback; no recognition' })
        sentence_count  = $Count
        generated_at    = (Get-Date).ToString('o')
        samples         = $entries.ToArray()
    }
}
function Save-Manifest($Manifest, [string]$Path) {
    $json = ConvertTo-Json -InputObject $Manifest -Depth 10
    Write-Utf8Text -Path $Path -Content ($json + [Environment]::NewLine) -Bom $false
}
function Resolve-Recorder([string]$ExplicitPath) {
    if ($ExplicitPath) {
        $resolved = [IO.Path]::GetFullPath($ExplicitPath)
        if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
            throw "Recorder not found: $resolved. Run .\scripts\build-recorder.ps1 first."
        }
        return $resolved
    }
    $candidates = @(
        (Join-Path $projectRoot 'app\listen\target\release\examples\capture_only.exe'),
        (Join-Path $repositoryRoot '.artifacts\cargo-target\release\examples\capture_only.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    Write-Output 'Recorder not built yet; building it with scripts\build-recorder.ps1 ...'
    $built = @()
    try { $built = @(& (Join-Path $PSScriptRoot 'build-recorder.ps1')) } catch {
        throw "Could not build the recorder: $($_.Exception.Message)"
    }
    $builtPath = @($built) | Where-Object { $_ } | Select-Object -Last 1
    foreach ($candidate in ($candidates + @([string]$builtPath))) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) { return $candidate }
    }
    throw 'Recorder still missing after the build; run .\scripts\build-recorder.ps1 manually.'
}

$presentBefore = 0
foreach ($sample in $samples) {
    if ($recorded.ContainsKey($sample.id) -and (Test-RecordedEntry $recorded[$sample.id])) { $presentBefore++ }
}
if ($ListOnly) {
    Write-Output ("Corpus: {0}" -f $corpusPath)
    Write-Output ("Output: {0}" -f $Output)
    Write-Output ("Recorded: {0} / {1}" -f $presentBefore, $samples.Count)
    for ($i = 0; $i -lt $samples.Count; $i++) {
        $sample = $samples[$i]
        $state = 'missing'
        if ($recorded.ContainsKey($sample.id)) {
            if (Test-RecordedEntry $recorded[$sample.id]) { $state = 'recorded' } else { $state = 'invalid or moved' }
        }
        Write-Output ("{0,2}. [{1}] {2}" -f ($i + 1), $state, $sample.id)
        Write-Output ("    " + $sample.spoken)
        if ($sample.focus) { Write-Output ("    " + $sample.focus) }
    }
    return
}

$RecorderPath = Resolve-Recorder -ExplicitPath $Recorder
$null = New-Item -ItemType Directory -Force -Path $Output
Write-Host ''
Write-Host '=== Listen meeting-corpus recording ===' -ForegroundColor Cyan
Write-Host ("Recorder: {0}" -f $RecorderPath)
Write-Host ("Output:   {0}" -f $Output)
Write-Host ''
foreach ($line in @($corpus.instructions)) { Write-Host ('  - ' + $line) }
Write-Host ''
$instructionText = ''
foreach ($line in @($corpus.instructions)) { $instructionText += ('- ' + $line + [Environment]::NewLine) }
Write-Utf8Text -Path (Join-Path $Output 'instructions.txt') -Content $instructionText -Bom $true
Write-Output ("Recording instructions were also written to {0}" -f (Join-Path $Output 'instructions.txt'))
Write-Output ("Progress before this session: {0} / {1} sentences recorded." -f $presentBefore, $samples.Count)

$allText = ''
for ($i = 0; $i -lt $samples.Count; $i++) {
    $allText += ('{0,2}. [{1}] {2}' -f ($i + 1), $samples[$i].id, $samples[$i].spoken) + [Environment]::NewLine + [Environment]::NewLine
}
Write-Utf8Text -Path (Join-Path $Output 'sentences.txt') -Content $allText -Bom $true

$selected = $samples
if ($Only) { $selected = @($samples | Where-Object { $_.id -eq $Only }) }

$aborted = $false
foreach ($sample in $selected) {
    $index = [int]$indexById[$sample.id]
    if (-not $Only -and $recorded.ContainsKey($sample.id) -and (Test-RecordedEntry $recorded[$sample.id])) {
        Write-Output ("Sentence {0} ({1}) is already recorded with a matching checksum; skipping. Use -Only {1} to redo it." -f $index, $sample.id)
        continue
    }

    $wavPath = Join-Path $Output ($sample.id + '.wav')
    $textPath = Join-Path $Output ('sentence-{0:D2}.txt' -f $index)
    $textBody = ('Sentence {0} / {1}' -f $index, $samples.Count) + [Environment]::NewLine +
        ('id: ' + $sample.id) + [Environment]::NewLine + [Environment]::NewLine +
        $sample.spoken + [Environment]::NewLine + [Environment]::NewLine +
        ('Focus: ' + $sample.focus) + [Environment]::NewLine
    Write-Utf8Text -Path $textPath -Content $textBody -Bom $true

    $accepted = $false
    while (-not $accepted) {
        Write-Host ''
        Write-Host ('-' * 72) -ForegroundColor DarkGray
        Write-Host ("Sentence {0} / {1}    id: {2}" -f $index, $samples.Count, $sample.id) -ForegroundColor Cyan
        Write-Host ''
        Write-Host $sample.spoken -ForegroundColor Yellow
        Write-Host ''
        if ($sample.focus) { Write-Host ('Focus: ' + $sample.focus) -ForegroundColor DarkGray }
        Write-Host ('Text file: ' + $textPath) -ForegroundColor DarkGray
        Write-Host ''
        $answer = Read-Host 'Press Enter to start recording this sentence, or type q to stop the session'
        if ($answer -match '^\s*[Qq]\s*$') { $aborted = $true; break }

        if ($SentenceDelaySeconds -gt 0) {
            Write-Host ("Recording starts in {0} seconds. Start reading when RECORDING appears." -f $SentenceDelaySeconds) -ForegroundColor DarkGray
            Start-Sleep -Seconds $SentenceDelaySeconds
        }
        $stamp = (Get-Date).ToString('yyyyMMdd-HHmmss')
        $runtimePath = Join-Path (Join-Path $Output 'capture-runtime') ('{0}-{1}' -f $sample.id, $stamp)
        $reportPath = Join-Path $runtimePath 'capture.json'
        Remove-Item -LiteralPath $wavPath -Force -ErrorAction SilentlyContinue

        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $RecorderPath
        $startInfo.Arguments = Join-Arguments @('--output', $wavPath, '--runtime', $runtimePath, '--report', $reportPath)
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardInput = $true
        $process = [Diagnostics.Process]::Start($startInfo)
        $captureReport = $null
        try {
            Write-Host 'RECORDING. Read the sentence now.' -ForegroundColor Green
            if (-not $process.WaitForExit(1500)) {
                [void](Read-Host 'Press Enter here when you have finished reading')
                try { $process.StandardInput.Close() } catch { }
                if (-not $process.WaitForExit(300000)) {
                    try { $process.Kill() } catch { }
                    throw 'capture_only did not stop within 300 seconds.'
                }
            }
            $exitCode = $process.ExitCode
        } finally {
            $process.Dispose()
        }

        $problem = ''
        $info = $null
        if ($exitCode -ne 0) {
            $problem = "The recorder failed with exit code $exitCode; this take was not accepted."
            if ($exitCode -eq -1073741515) {
                $problem += ' STATUS_DLL_NOT_FOUND: rebuild with .\scripts\build-recorder.ps1 so libunwind.dll sits next to capture_only.exe.'
            }
        } elseif (-not (Test-Path -LiteralPath $wavPath -PathType Leaf)) {
            $problem = "The recorder did not write $wavPath."
        } else {
            try { $info = Get-WavInfo $wavPath } catch { $problem = $_.Exception.Message }
            if (-not $problem) {
                if ($info.sample_rate -ne 16000 -or $info.channels -ne 1 -or $info.bits -ne 16) {
                    $problem = ("The take is not a 16 kHz mono 16-bit WAV ({0} Hz, {1} channel(s), {2} bit)." -f $info.sample_rate, $info.channels, $info.bits)
                } elseif ($info.duration -lt 1.0) {
                    $problem = ("The take is only {0:N2} s long, which is too short; record the whole sentence again." -f $info.duration)
                } elseif ($info.duration -gt ($targetSeconds + 3)) {
                    Write-Warning ("The take is {0:N2} s long; the corpus targets under {1} s per sentence. You can still accept it." -f $info.duration, $targetSeconds)
                }
            }
            if (-not $problem -and (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
                try { $captureReport = [IO.File]::ReadAllText($reportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json } catch { }
            }
        }
        if ($problem) {
            Write-Warning $problem
            if (Test-Path -LiteralPath $runtimePath) { Write-Warning ("Temporary capture files were kept for diagnosis: " + $runtimePath) }
            $answer = Read-Host 'Press Enter to try this sentence again, or type q to stop the session'
            if ($answer -match '^\s*[Qq]\s*$') { $aborted = $true; break }
            continue
        }

        $hash = (Get-FileHash -LiteralPath $wavPath -Algorithm SHA256).Hash.ToLowerInvariant()
        Write-Host ''
        Write-Host ("Recorded {0:N2} s ({1} samples, {2} Hz mono 16-bit), SHA-256 {3}" -f $info.duration, $info.samples, $info.sample_rate, $hash) -ForegroundColor Green
        if ($captureReport -and $captureReport.endpoint) {
            Write-Host ("Captured endpoint: {0} '{1}'" -f $captureReport.endpoint.role, $captureReport.endpoint.name) -ForegroundColor Green
        }
        if ($corpus.requires_reference_review) {
            Write-Host 'Replaying this take. Listen for missing words, repeats and changed meaning before accepting.'
            $player = New-Object System.Media.SoundPlayer($wavPath)
            try { $player.PlaySync() } finally { $player.Dispose() }
        }
        $answer = Read-Host 'Press Enter to accept this take, type r to re-record, or q to stop the session'
        if ($answer -match '^\s*[Qq]\s*$') { $aborted = $true; break }
        if ($answer -match '^\s*[Rr]\s*$') { continue }

        $entry = [ordered]@{
            id             = $sample.id
            index          = $index
            audio          = $wavPath
            file           = ($sample.id + '.wav')
            sha256         = $hash
            duration_seconds = [math]::Round($info.duration, 3)
            sample_rate    = $info.sample_rate
            channels       = $info.channels
            bits_per_sample = $info.bits
            spoken         = $sample.spoken
            reference      = $sample.reference
            focus          = $sample.focus
            coverage       = @($sample.coverage)
            expressions    = @($sample.expressions)
            capture_source = $(if ($captureReport) { [string]$captureReport.source } else { '' })
            recorded_at    = (Get-Date).ToString('o')
            endpoint_name  = ''
            endpoint_id    = ''
        }
        if ($captureReport -and $captureReport.endpoint) {
            $entry.endpoint_name = [string]$captureReport.endpoint.name
            $entry.endpoint_id = [string]$captureReport.endpoint.id
        }
        $recorded[$sample.id] = $entry
        $manifest = New-Manifest -Corpus $corpus -Recorded $recorded -Count $samples.Count -RecorderPath $RecorderPath
        Save-Manifest -Manifest $manifest -Path $manifestPath
        $captureRoot = [IO.Path]::GetFullPath((Join-Path $Output 'capture-runtime')) + [IO.Path]::DirectorySeparatorChar
        $resolvedRuntime = [IO.Path]::GetFullPath($runtimePath)
        if (-not $resolvedRuntime.StartsWith($captureRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Refusing to clean a capture directory outside this evaluation corpus.'
        }
        Remove-Item -LiteralPath $resolvedRuntime -Recurse -Force -ErrorAction SilentlyContinue
        $accepted = $true
        Write-Output ("Accepted {0}; manifest updated: {1}" -f $sample.id, $manifestPath)
    }
    if ($aborted) { break }
}

$missing = @()
foreach ($sample in $samples) {
    if (-not ($recorded.ContainsKey($sample.id) -and (Test-RecordedEntry $recorded[$sample.id]))) { $missing += $sample.id }
}
$present = $samples.Count - $missing.Count
Write-Host ''
if ($Only) {
    if ($missing -contains $Only) { throw "The requested sentence was not recorded: $Only" }
    Write-Output ("Recorded {0}. {1} / {2} sentences are now complete." -f $Only, $present, $samples.Count)
    if ($missing.Count -gt 0) { Write-Output ('Still missing: ' + ($missing -join ', ')) }
    return
}
if ($missing.Count -gt 0) {
    Write-Output ("Missing sentences ({0} / {1}): {2}" -f $missing.Count, $samples.Count, ($missing -join ', '))
    Write-Output 'Run this script again to record the missing sentences; a full recording is not complete yet.'
    exit 1
}
Write-Output ("All {0} sentences are recorded with matching checksums: {1}" -f $samples.Count, $manifestPath)
Write-Output 'Next: run .\scripts\benchmark-meeting-corpus.ps1 against this manifest. See docs\issue-22-recording-guide.md.'
