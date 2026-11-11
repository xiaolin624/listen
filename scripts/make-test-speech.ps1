$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$artifactDirectory = Join-Path $projectRoot '.artifacts\verification'
$null = New-Item -ItemType Directory -Force -Path $artifactDirectory
Add-Type -AssemblyName System.Speech
$speaker = New-Object System.Speech.Synthesis.SpeechSynthesizer
try {
    $voice = $speaker.GetInstalledVoices() | Where-Object { $_.Enabled -and $_.VoiceInfo.Culture.Name -eq 'zh-CN' } | Select-Object -First 1
    if (-not $voice) { throw 'An installed offline zh-CN Windows speech voice is required' }
    $speaker.SelectVoice($voice.VoiceInfo.Name)
    $speaker.Rate = 0
    $format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
    $output = Join-Path $artifactDirectory 'synthetic-zh.wav'
    $speaker.SetOutputToWaveFile($output, $format)
    $text = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $projectRoot 'experiments\windows-probe\sample-zh.txt')
    $speaker.Speak($text.Trim())
    Write-Output "Generated offline test speech: $output"
} finally {
    $speaker.Dispose()
}
