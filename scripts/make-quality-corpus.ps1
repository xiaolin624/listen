param([string]$Output = '')
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $Output) {
    $commonGit = & git -C $projectRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the shared repository root.' }
    $Output = Join-Path (Split-Path $commonGit -Parent) '.artifacts\quality-corpus'
}
$Output = [IO.Path]::GetFullPath($Output)
$null = New-Item -ItemType Directory -Force -Path $Output
$corpus = Get-Content -LiteralPath (Join-Path $projectRoot 'quality\corpus.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Add-Type -AssemblyName System.Speech
Add-Type -TypeDefinition @'
using System;
using System.IO;
public static class QualityWave {
    public static void Write(string path, string kind, int seconds, int rate) {
        int length = seconds * rate;
        using (var w = new BinaryWriter(File.Create(path))) {
            w.Write(System.Text.Encoding.ASCII.GetBytes("RIFF")); w.Write(36 + length * 2);
            w.Write(System.Text.Encoding.ASCII.GetBytes("WAVEfmt ")); w.Write(16);
            w.Write((short)1); w.Write((short)1); w.Write(rate); w.Write(rate * 2);
            w.Write((short)2); w.Write((short)16);
            w.Write(System.Text.Encoding.ASCII.GetBytes("data")); w.Write(length * 2);
            double[] notes = {261.625565, 329.627557, 391.995436, 523.251131,
                              440.0, 349.228231, 293.664768, 391.995436};
            for (int i = 0; i < length; i++) {
                double t = (double)i / rate, value = 0;
                if (kind == "instrumental") {
                    double beat = t % 0.5, f = notes[(int)(t / 0.5) % notes.Length];
                    double envelope = Math.Min(1, beat / 0.015) * Math.Exp(-4 * beat);
                    value = envelope * (0.22 * Math.Sin(2 * Math.PI * f * t)
                        + 0.07 * Math.Sin(4 * Math.PI * f * t)
                        + 0.04 * Math.Sin(2 * Math.PI * f / 2 * t));
                } else if (kind == "notification") {
                    double pulse = t - Math.Floor(t / 3) * 3;
                    if (pulse < 0.5) {
                        double envelope = Math.Min(1, pulse / 0.005) * Math.Exp(-8 * pulse);
                        value = envelope * (0.25 * Math.Sin(2 * Math.PI * 880 * t)
                            + 0.12 * Math.Sin(2 * Math.PI * 1320 * t));
                    }
                }
                w.Write((short)Math.Round(value * 32767));
            }
        }
    }
}
'@
$speaker = New-Object System.Speech.Synthesis.SpeechSynthesizer
$manifestSamples = @()
try {
    $format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo($corpus.sample_rate, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
    # Pin voice names: silently substituting another installed voice changes the corpus.
    $voices = @{ 'zh-CN' = 'Microsoft Huihui Desktop'; 'en-US' = 'Microsoft Zira Desktop' }
    foreach ($sample in $corpus.samples) {
        $path = Join-Path $Output ($sample.id + '.wav')
        if ($sample.kind -eq 'speech') {
            $builder = New-Object System.Speech.Synthesis.PromptBuilder
            foreach ($part in $sample.parts) {
                $voiceName = $voices[$part.culture]
                $available = $speaker.GetInstalledVoices() | Where-Object { $_.Enabled -and $_.VoiceInfo.Name -eq $voiceName }
                if (-not $available) { throw "Required offline voice is missing: $voiceName" }
                $builder.StartVoice($voiceName)
                $builder.AppendText($part.text)
                $builder.EndVoice()
            }
            $speaker.Rate = 0
            $speaker.SetOutputToWaveFile($path, $format)
            $speaker.Speak($builder)
            $speaker.SetOutputToNull()
        } else {
            [QualityWave]::Write($path, $sample.kind, $sample.seconds, $corpus.sample_rate)
        }
        $manifestSamples += [ordered]@{
            id = $sample.id; kind = $sample.kind; reference = $sample.reference
            audio = $path; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $manifest = [ordered]@{
        corpus_version = $corpus.version
        generator = 'System.Speech offline, rate 0; deterministic PCM non-speech generator'
        voices = $voices
        sample_rate = $corpus.sample_rate
        samples = $manifestSamples
    }
    $manifest | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $Output 'manifest.json') -Encoding UTF8
    Write-Output "Generated offline quality corpus without playback: $(Join-Path $Output 'manifest.json')"
} finally {
    $speaker.Dispose()
}
