# Issue #36 — storage warning, hard stop and explicit per-segment cleanup

Verification notes for `implement/issue-36-storage`, branched from
`integration/cloud-recognition` at `c1e3f47` (#32 + #33 + #37 merged).

## What the user gets

- **A pending-audio budget across every segment, not one file.** The default is
  1 GiB. Every regular file inside every `runtime\session-*\` directory counts —
  the continuous cloud PCM spool, the local overlapping recognition windows, the
  window manifest, temporary upload parts and repair/retry copies — plus root-level
  `.wav`/`.pcm` files. Saved text, `segments.json`, `latency.jsonl` and the glossary
  are deliberately **not** pending audio: they have their own bound, so the audio
  budget is never a whole-runtime-directory limit.
- **A warning before the hard stop.** At four fifths of the budget (configurable)
  the panel gets a storage line, `status.txt` reports `storage_level=warning`, and a
  `Notice::StorageWarning` sentence appears. A warning stops nothing.
- **Two independent hard stops.** At or over the budget (`storage_level=full`) or at
  or below the free-disk floor (`storage_level=disk_low`):
  - `Control::Start` is refused with a sentence that says no capture started and
    that nothing was deleted or overwritten;
  - a capture that is already running is stopped by the application, its
    `storage_note` is kept with the segment, and everything already received is
    retained and still processed when the recognition session confirms it.
- **Real storage faults, distinct from waits.** A failed save (either the segment
  text, `current-result.txt`, the PCM spool or a window writer) is reported as
  `Storage fault: …`, states that it is not a network wait and not a
  playback-device interruption, keeps the captured audio and writes nothing to the
  target. `Snapshot::storage.fault` keeps the last fault observable, and
  `status.txt` has `storage_fault=`.
- **Explicit, per-segment cleanup.** `Control::DeleteSegment(sequence)` deletes the
  pending recording and/or pending result of exactly one capture order. The tray
  menu offers 「清理待处理段」 with one item per eligible segment, and
  `listen.exe --control delete-segment:<n>` does the same from automation.
  - The live capture is refused (stop it first) and a result whose target write is
    in flight is refused until the write is confirmed, so a delete can never race a
    target write.
  - A segment that is still recovering is stopped and its session handle dropped
    first; its event channel is closed, so a late confirmation/failure can neither
    resurrect the segment nor write its target.
  - A segment whose result was already delivered has nothing pending and is
    refused; deleting never pretends to undo text already in the editor.
  - Deleting the current pending result clears `current-result.txt` too, so a
    restart cannot resurrect it. Other segments' text, audio and records are
    untouched. There is no date-based expiry.
- **Separate bounds.** Saved text has its own default 4 MiB budget
  (`StoragePolicy::text_budget_bytes`); going over it is a storage fault that keeps
  the audio. In-memory audio stays with the adapters (bounded local window queue,
  one PCM chunk), never with this budget.

## Configuration

| Flag | Default | Meaning |
| --- | --- | --- |
| `--pending-audio-budget-mib <MiB>` | 1024 | pending audio allowed across all segments |
| `--pending-audio-warn-mib <MiB>` | four fifths of the budget | warning line; clamped to the budget |
| `--free-disk-floor-mib <MiB>` | 1024 | free space that must remain on the runtime volume |

`StoragePolicy` (in `listen::storage`) carries the same values in bytes plus
`text_budget_bytes`. The per-capture PCM spool guard (1 GiB in `audio.rs`) remains as
the last-resort per-file stop; the configurable budget governs admission, warning and
the running-capture stop.

## Test evidence

`app/listen/tests/storage_bounds.rs` drives only `Application::dispatch`,
`poll` and `snapshot`; the budget, the free-space probe and the runtime directory
are injected, so no test creates real disk pressure:

1. pending audio is summed across segments, a full budget stops the live capture
   and refuses the next start while both segments' files stay on disk;
2. temporary uploads, nested window fragments and repair files count, not just the
   final WAV;
3. a warning appears before the limit and never refuses a capture;
4. a low free-disk reading refuses a start and keeps existing recordings;
5. a save failure reads as `Storage fault:`, keeps real retained audio and never
   writes the target;
6. audio over the budget does not block saving the complete text (separate
   bounds), and the audio is released only after the text file exists;
7. the text bound reports its own storage fault and keeps the audio;
8. the free-disk floor stops a running capture and keeps its received audio;
9. deleting a selected pending result removes only it, closes its session channel,
   keeps another pending result usable and cannot be resurrected by a late event;
10. deleting a recovering segment stops its later processing without stopping the
    live capture, and the new capture still delivers;
11. deleting a failed recording releases its retained audio and leaves the pending
    result and later captures usable;
12. the live capture cannot be deleted and keeps recording;
13. a result whose target write is in flight cannot be deleted; after delivery the
    delete refuses because nothing is pending, and the target text is untouched.

Unit tests in `listen::storage` cover the accounting split (audio vs. saved text), a
missing runtime directory, the warning clamp, the two independent hard stops and the
advisory storage-fault classifier (device interruptions and network waits are not
relabelled). `cargo fmt --check` passes.

## What a merger must know

- New module `app/listen/src/storage.rs` owns accounting, policy, the Windows
  free-space probe and the user-facing storage sentences. The application only
  calls it.
- `Application` gained `with_storage_policy` / `with_free_space` builders (#36 tests
  use them); `Snapshot` gained `storage: Option<StorageStatus>` and
  `SegmentSession` gained `storage_note: Option<String>`.
- `Control` gained `DeleteSegment(u64)`. `panel.rs` gained `storage_note`,
  `deletable_segments` and made `segment_label` public; `gui.rs` appends
  `storage_*` keys to `status.txt`, offers the tray cleanup submenu and parses
  `--control delete-segment:<n>`.
- This ticket does **not** implement restart recovery of those states (#35) or
  remove the per-file PCM guard; cross-restart review belongs to the restart and
  final-acceptance tickets.
- Not run here: real-volume disk-pressure end-to-end and a real editor delivery;
  both are covered by the controlled application tests above and by the final
  desktop acceptance task.
