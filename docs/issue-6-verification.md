# Issue #6 verification (implementation checkpoint, incomplete)

Branch: `implement/issue-6-long-sessions`; worktree: `E:\luyin\.worktrees\issue-6`.
Base `integration/issue-3-native-target` confirmed at `e4f93bf` before changes.

The confirmed business seam remains `Application::dispatch` (Start/Stop/Toggle),
`poll`, and the observable snapshot/result/audio lifecycle. External playback
and local model replacements drive deterministic PCM and timestamped words;
tests use the same `SegmentedAudioWriter` as WASAPI, with smaller windows.
No separate private-function business test seam was introduced.

## TDD evidence

- RED: first complete-flow long-session test failed to compile because streaming
  handoff/progress and timestamped local results did not exist.
- GREEN: the same test observes two recognized windows while still Capturing,
  no text publication, a spoken word crossing the window boundary, correct
  overlap ordering, final tail, cleanup and repeated Stop without duplicate text.
- RED: stopping before one complete window exposed an unsigned subtraction
  underflow; the test observed Failure instead of its complete short result.
- GREEN: short final windows no longer advance a nonexistent next window; final
  progress includes the tail and the released spool.
- RED: storage exhaustion fixture required an absent explicit spool budget.
- GREEN: bounded storage interruption reports Failure and retains a readable WAV
  containing all 160 accepted samples.
- Additional complete-flow regression: blocked local inference saturates the
  bounded path queue, visibly interrupts capture, then retains the recognized
  checkpoint and the unqueued WAV after already queued work drains.

`scripts/build-listen.ps1 -Task Test` passed: 7 existing + 4 long-session tests.
`scripts/build-listen.ps1 -Task Format` completed. Shared cached toolchain at
`E:\luyin\.tools`; no model/audio playback/GUI/device changes in these checks.

## Implementation at this checkpoint

Production capture now writes 30-second 16 kHz mono WAV windows with 2 seconds
overlap, uses one recognition worker while capturing, and bounds the waiting
path queue to 8 windows plus the running window and the pressure-retained
window. The PCM overlap buffer is bounded to 32,000 samples. A separate audio
spool budget is 9,600,440 bytes. These are resource limits, not a session timer.
Recognized audio is released only after an atomic local transcript checkpoint;
failures retain the checkpoint and remaining WAVs. No subtitles are published.

## Outstanding before completion

- Merge the latest integration tip containing #3's native Target interface and
  #9's quality/no-speech behavior, keeping delivery exclusively at final output.
- Implement real Whisper token timestamps (the default adapter is currently
  whole-window text; it cannot yet establish real boundary correctness).
- Add the requested silent real-wall-clock known-PCM + actual Whisper harness,
  metrics collection and pressure controls; exercise it with a short smoke run.
- Add exact-boundary / legitimate repeated-word regression coverage, finish
  resource/recovery robustness, README, and final format/test/release checks.
- Root must run and record actual 60-minute validation and short real WASAPI /
  desktop output acceptance. No 60-minute test has been run or claimed.
