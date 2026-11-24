//! Restart recovery for unfinished recordings and pending results (#35).
//!
//! The runtime directory is the only thing that survives a restart. This module
//! turns what an interrupted process left there into the application's own state:
//!
//! * a segment whose complete text was saved is shown as its own pending result
//!   and is **never** recognized again;
//! * a segment that still only has its recording is validated and then handed
//!   back to the recognition adapter, oldest capture order first;
//! * a target handle cannot survive a restart, so nothing recovered here is ever
//!   written to a window or input target. The delivery record is read to say
//!   honestly whether a write was decided or may have happened (#34);
//! * audio is released only when the complete text is durably readable. A
//!   recording that cannot be recovered is kept, never deleted as "expired".
//!
//! Nothing here deletes a pending result and nothing expires by date.

use crate::{
    Delivery, SegmentState,
    delivery::{DeliveryState, recovered_delivery},
    durable::now_utc,
    segments::{Segment, SegmentStore},
};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
};

/// The continuous cloud spool and its two descriptors. The names are the durable
/// contract the device writes; recovery only reads them.
const PCM_FILE: &str = "playback.pcm";
const PCM_FORMAT_FILE: &str = "pcm-format.txt";
const PCM_STATE_FILE: &str = "playback.state";
/// The repair copy of a damaged continuous recording. It lives in the same segment
/// directory, so it counts toward the pending-audio budget and is released with
/// the segment once the complete text is saved.
const REPAIRED_PCM_FILE: &str = "recovered.pcm";
/// The rejoined copy of a local capture whose windows could not all be read.
const REPAIRED_WAV_FILE: &str = "recovered.wav";
const SESSION_PREFIX: &str = "session-";

/// Why a recovered segment is being shown exactly as it is. These are user-facing
/// sentences; none of them contains a credential or a vendor error body.
pub(crate) const RESUMED_DETAIL: &str = "Recovered after Listen restarted: the recording kept from the previous session is processed again automatically in the background, in capture order. No old window or input target is reused after a restart, so its complete result stays in this panel. Nothing is re-recorded and nothing is written automatically.";
pub(crate) const ORPHAN_RESUMED_DETAIL: &str = "Recovered after Listen restarted: this recording was in the runtime directory but not in the segment index, so its original capture order and fixed term snapshot were lost with the index. It is listed after the recorded segments and processed again automatically with the current 术语表. No old window or input target is reused, so its complete result stays in this panel. Nothing is re-recorded and nothing is written automatically.";
pub(crate) const RECOVERED_TEXT_DETAIL: &str = "Recovered after Listen restarted: this segment's complete result was saved by the previous session, so it is available here without being recognized again. Nothing was written to any window or input target after the restart.";
pub(crate) const RECOVERED_APPENDED_NOTE: &str = " The previous session had already appended this result to its target; the copy here is kept because it is the complete result of this segment.";
pub(crate) const UNKNOWN_DELIVERY_NOTE: &str = " The previous session had started writing to its target (or its outcome is unknown), so it is not written again automatically; check the original target before copying. This result may already be there once.";
pub(crate) const NO_TARGET_AFTER_RESTART: &str =
    "the input target bound when this segment started cannot be reused after Listen restarted";
pub(crate) const KEPT_AFTER_FAILED_RESUME: &str = " The recovered recording is kept on this machine; it is not retried automatically, so nothing is paid twice without your request.";

/// The audio the previous process left behind, validated against its own durable
/// descriptors. `path` is what the recognition adapter is handed; the original
/// files are never deleted here.
#[derive(Clone)]
pub(crate) struct RecoveredAudio {
    pub path: PathBuf,
    pub description: String,
    /// Why the recording is not proven complete, if it is not. An incomplete
    /// recording is still processed, but it is never presented as a complete
    /// recording or as a passing transcript. Never a credential.
    pub problem: Option<String>,
    /// Every audio file kept on disk for this segment, for the panel.
    pub retained: Vec<PathBuf>,
}

/// One segment the previous process left behind, as the application rebuilds it:
/// the authoritative facts plus what only the startup scan can know (the audio it
/// validated, the paths it kept, and the recovery sentence that must survive every
/// later status refresh).
pub(crate) struct RecoveredSegment {
    pub facts: Segment,
    pub audio: Option<RecoveredAudio>,
    pub retained_paths: Vec<PathBuf>,
    /// The validated capture directory whose received audio may be released because
    /// the complete text is durably readable. `None` keeps every recording.
    pub release: Option<PathBuf>,
    /// The recovery fact that must survive every later status refresh: a recovered
    /// segment's recording is not re-recorded, its old target is not reused, and an
    /// incomplete recording is never presented as a complete one.
    pub note: Option<String>,
}

impl RecoveredSegment {
    /// Whether this process should process the recording again automatically. Only a
    /// validated recording whose segment is still in flight qualifies; a failure is
    /// never retried because that would spend money without the user asking.
    pub fn resumable(&self) -> bool {
        self.audio.is_some() && self.facts.state == SegmentState::Processing
    }
}

/// The persistent sentence beside a recovered segment's own progress detail. The
/// adapter's status may change it, but never erase it.
pub(crate) fn recovery_note(problem: Option<&str>) -> String {
    let mut note = "Recovered after Listen restarted; the original input target cannot be reused, so this segment's result stays in the panel.".to_string();
    if let Some(problem) = problem {
        note.push_str(&incomplete_note(problem));
    }
    note
}

/// Whether this segment holds a continuous cloud recording, so quitting has to
/// wait for its writer to publish the end-of-recording marker.
pub(crate) fn has_cloud_spool(directory: &Path) -> bool {
    directory.join(PCM_FILE).is_file()
}

/// Whether the received audio of this segment was already closed durably. A
/// recording that was not closed by a normal stop still publishes what it
/// committed when its writer is dropped, so the marker is the signal either way.
pub(crate) fn audio_flushed(directory: &Path) -> bool {
    !has_cloud_spool(directory) || directory.join(PCM_STATE_FILE).is_file()
}

/// A complete text file the index does not describe: the previous process saved
/// the result and then stopped before its index entry was durable, or the index
/// itself was lost. Its sequence comes from the file name, so its identity is kept.
pub(crate) struct OrphanText {
    sequence: u64,
    name: String,
    text: String,
    /// The durable delivery decision for this capture order, when a record still
    /// proves one. A confirmed 精修 append survives its drained index entry here.
    delivery: Option<DeliveryState>,
}

/// A capture directory the index does not describe. Its original capture order was
/// lost with the index, so it is adopted after every recorded segment and said so.
pub(crate) struct OrphanDirectory {
    name: String,
    audio: Option<RecoveredAudio>,
    retained: Vec<PathBuf>,
}

/// Everything one startup scan decided.
pub(crate) struct Plan {
    pub segments: Vec<RecoveredSegment>,
    orphan_texts: Vec<OrphanText>,
    orphan_directories: Vec<OrphanDirectory>,
    /// Conditions the user must see: a corrupt index, a segment whose evidence was
    /// lost, an unreadable complete text. Nothing here was deleted.
    pub problems: Vec<String>,
}

impl Plan {
    /// Whether the runtime directory contained nothing to recover and nothing to
    /// report. A fresh runtime directory reaches this and recovery is a no-op.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
            && self.orphan_texts.is_empty()
            && self.orphan_directories.is_empty()
            && self.problems.is_empty()
    }

    /// Adopt the artifacts the index never described as ordinary recovered
    /// segments with their own durable records, and return the warnings the user
    /// must see. `terms` is the current 术语表: a lost index also lost the
    /// capture-time snapshot, and the detail says so.
    pub fn adopt(&mut self, store: &mut SegmentStore, terms: &[String]) -> Vec<String> {
        let mut warnings = Vec::new();
        for orphan in std::mem::take(&mut self.orphan_texts) {
            // A delivery record for this capture order is still the authority even
            // when the index entry it belonged to is gone: a confirmed 精修 append
            // must be read as confirmed, not reported as never written (#44, W3).
            let delivery = recovered_delivery(orphan.delivery);
            let note = Some(
                "Recovered after Listen restarted from a complete text the segment index did not describe; it stays in the panel and is never written to any old target."
                    .to_string(),
            );
            let facts = Segment {
                sequence: orphan.sequence,
                state: SegmentState::Result,
                detail: adopted_text_detail(delivery),
                text_file: Some(orphan.name),
                text_chars: orphan.text.chars().count(),
                text: Some(orphan.text),
                delivery: Some(delivery),
                glossary_terms: terms.to_vec(),
                updated_utc: now_utc(),
                ..Segment::default()
            };
            if let Some(error) = store.adopt(facts.clone()) {
                warnings.push(error);
            }
            self.segments.push(RecoveredSegment {
                facts,
                audio: None,
                retained_paths: Vec::new(),
                release: None,
                note,
            });
        }
        for orphan in std::mem::take(&mut self.orphan_directories) {
            let audio_problem = orphan
                .audio
                .as_ref()
                .and_then(|audio| audio.problem.clone());
            let (_, facts, error) = store.begin(|sequence| {
                let (state, detail) = match &orphan.audio {
                    Some(_) => {
                        let mut detail = ORPHAN_RESUMED_DETAIL.to_string();
                        if let Some(problem) = &audio_problem {
                            detail.push_str(&incomplete_note(problem));
                        }
                        (SegmentState::Processing, detail)
                    }
                    None => (
                        SegmentState::Failed,
                        format!(
                            "Segment {sequence} was found in the runtime directory but not in the segment index, and no readable audio was found. Nothing was deleted; delete this segment explicitly if it is no longer needed."
                        ),
                    ),
                };
                Segment {
                    sequence,
                    directory: orphan.name.clone(),
                    state,
                    detail,
                    glossary_terms: terms.to_vec(),
                    updated_utc: now_utc(),
                    ..Segment::default()
                }
            });
            if let Some(error) = error {
                warnings.push(error);
            }
            let note = (facts.state == SegmentState::Processing)
                .then(|| recovery_note(audio_problem.as_deref()));
            self.segments.push(RecoveredSegment {
                facts,
                audio: orphan.audio,
                retained_paths: orphan.retained,
                release: None,
                note,
            });
        }
        if self.segments.iter().any(|segment| {
            segment.facts.detail.starts_with(ORPHAN_RESUMED_DETAIL)
                || segment.facts.detail.contains("not in the segment index")
        }) {
            self.problems.push(
                "The segment index did not describe every leftover capture; those segments were recovered from the runtime directory and are listed after the recorded ones."
                    .into(),
            );
        }
        self.segments.sort_by_key(|segment| segment.facts.sequence);
        warnings
    }
}

/// The one readable sentence for an adopted complete text the index did not
/// describe. Only a proven decision is stated as one.
fn adopted_text_detail(delivery: Delivery) -> String {
    let kept = "The segment index did not describe this saved result, so its record is rebuilt here; it is kept in the panel and is never written again automatically.";
    let mut detail = RECOVERED_TEXT_DETAIL.to_string();
    match delivery {
        Delivery::Appended => detail.push_str(RECOVERED_APPENDED_NOTE),
        Delivery::Uncertain => detail.push_str(UNKNOWN_DELIVERY_NOTE),
        Delivery::Fallback => {}
    }
    detail.push(' ');
    detail.push_str(kept);
    detail
}

/// One recording that is processed again, with the capture-time terms.
pub(crate) struct Resume {
    pub sequence: u64,
    pub audio: RecoveredAudio,
    pub glossary_terms: Vec<String>,
}

/// What the panel's one headline says about a restart. The per-segment detail
/// carries the specifics.
pub(crate) struct Summary {
    pub resumed: usize,
    pub texts: usize,
    pub problems: Vec<String>,
}

/// The recovery state of one process: the ordered background queue and what the
/// startup scan found. It is deliberately not durable; the segment index is.
#[derive(Default)]
pub(crate) struct Recovery {
    queue: VecDeque<Resume>,
    summary: Option<Summary>,
}

impl Recovery {
    /// Adopt one scan: queue every segment that must be processed again, in
    /// capture order, and remember the user-visible summary.
    pub fn start(&mut self, segments: Vec<RecoveredSegment>, problems: Vec<String>) {
        let mut resumed = 0;
        let mut texts = 0;
        for segment in segments {
            if segment.resumable() {
                if let Some(audio) = segment.audio.clone() {
                    resumed += 1;
                    self.queue.push_back(Resume {
                        sequence: segment.facts.sequence,
                        audio,
                        glossary_terms: segment.facts.glossary_terms,
                    });
                }
            } else if segment.facts.text.is_some() {
                texts += 1;
            }
        }
        self.summary = Some(Summary {
            resumed,
            texts,
            problems,
        });
    }

    /// The next recording to process, oldest capture order first. The caller only
    /// asks when no recognition session is in flight, so one old segment at a time
    /// is processed and a new capture always gets the processing capacity first.
    pub fn take_next(&mut self) -> Option<Resume> {
        self.queue.pop_front()
    }

    /// A segment the user deleted must never be processed again.
    pub fn forget(&mut self, sequence: u64) {
        self.queue.retain(|resume| resume.sequence != sequence);
    }

    pub fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref()
    }
}

/// The one sentence the panel shows at startup. It never claims a delivery: a
/// recovered result is always a panel result.
pub(crate) fn summary_detail(summary: &Summary) -> String {
    let mut parts = Vec::new();
    if summary.resumed > 0 {
        parts.push(format!(
            "Recovered {} unfinished segment{} from the previous Listen session after a restart; {} processed again automatically in capture order, one at a time, so a new capture always has the processing capacity first.",
            summary.resumed,
            plural(summary.resumed),
            if summary.resumed == 1 {
                "it is"
            } else {
                "they are"
            },
        ));
    }
    if summary.texts > 0 {
        parts.push(format!(
            "Recovered {} complete pending result{} from the previous Listen session after a restart; {} in this panel to take and {} recognized again.",
            summary.texts,
            plural(summary.texts),
            if summary.texts == 1 {
                "it is"
            } else {
                "they are"
            },
            if summary.texts == 1 {
                "is not"
            } else {
                "are not"
            },
        ));
    }
    let mut detail = parts.join(" ");
    if !detail.is_empty() {
        detail.push_str(
            " No old window or input target is reused after a restart, so nothing recovered is written automatically.",
        );
    }
    for problem in &summary.problems {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(problem);
    }
    detail
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Scan the runtime directory together with the stored segment records and the
/// durable delivery states, and decide what each leftover segment is.
///
/// A delivery record that is missing, unreadable or pruned proves nothing about the
/// original target, so every recovered text in that position is `Uncertain` and is
/// never appended automatically. Only an explicit `fallback` record proves that
/// nothing was written.
pub(crate) fn plan(
    runtime: &Path,
    records: &[Segment],
    deliveries: &BTreeMap<u64, DeliveryState>,
) -> Plan {
    let mut sorted: Vec<&Segment> = records.iter().collect();
    sorted.sort_by_key(|record| record.sequence);
    let mut segments = Vec::new();
    let mut problems = Vec::new();
    let known_sequences: Vec<u64> = records.iter().map(|record| record.sequence).collect();
    let known_directories: Vec<String> = records
        .iter()
        .map(|record| record.directory.clone())
        .collect();
    for record in sorted {
        let sequence = record.sequence;
        let directory = safe_directory(runtime, &record.directory);
        if directory.is_none() && !record.directory.is_empty() {
            problems.push(format!(
                "Segment {sequence} records a capture directory that cannot be used ({:?}); nothing in it was deleted.",
                record.directory
            ));
        }
        let text = read_text(runtime, record);
        // A saved complete text is the recovery basis; the audio is no longer needed
        // to recognize it again, so only its release is decided here.
        let audio = match &text {
            Some(_) => None,
            None => directory.as_deref().and_then(recovered_audio),
        };
        let retained_paths = match &text {
            Some(_) => Vec::new(),
            None => directory
                .as_deref()
                .map(crate::audio_paths)
                .unwrap_or_default(),
        };
        // The live delivery record is the authority; the index's own delivery field
        // is only the fallback for when the ledger no longer has the entry.
        let delivery = match deliveries.get(&sequence).copied() {
            Some(state) => recovered_delivery(Some(state)),
            None => record.delivery.unwrap_or(Delivery::Uncertain),
        };
        let text_is_some = text.is_some();
        let audio_problem = audio.as_ref().and_then(|audio| audio.problem.clone());
        let resumable_state = record.state.in_flight();
        let (state, detail, delivery, release) = match text.as_ref() {
            Some(_) => {
                let mut detail = RECOVERED_TEXT_DETAIL.to_string();
                if delivery == Delivery::Appended {
                    detail.push_str(RECOVERED_APPENDED_NOTE);
                } else if delivery == Delivery::Uncertain {
                    detail.push_str(UNKNOWN_DELIVERY_NOTE);
                }
                (
                    SegmentState::Result,
                    detail,
                    Some(delivery),
                    directory.clone(),
                )
            }
            None => match audio.as_ref() {
                Some(audio) if resumable_state => {
                    let mut detail = RESUMED_DETAIL.to_string();
                    if let Some(problem) = &audio.problem {
                        detail.push_str(&incomplete_note(problem));
                    }
                    (SegmentState::Processing, detail, None, None)
                }
                Some(audio) => {
                    // The recording is recoverable but the segment is not in flight:
                    // a failure stays a failure. It is never retried automatically
                    // because that would spend money without the user asking.
                    let mut detail = if record.state == SegmentState::NoSpeech {
                        "The previous session recorded no speech for this segment, but its audio is still kept on this machine.".to_string()
                    } else {
                        format!(
                            "The previous session ended with this segment recorded as '{}'. {} The recording is kept on this machine.",
                            record.state.name(),
                            record.detail
                        )
                    };
                    if let Some(problem) = &audio.problem {
                        detail.push_str(&incomplete_note(problem));
                    }
                    let state = if record.state == SegmentState::NoSpeech {
                        SegmentState::NoSpeech
                    } else {
                        SegmentState::Failed
                    };
                    (state, detail, None, None)
                }
                None if directory.is_some() => {
                    // A directory with nothing processable in it. Do not delete it:
                    // it may be the only remaining evidence of the attempt.
                    let detail = format!(
                        "Segment {sequence} was recovered from the previous Listen session, but no readable audio and no complete text were found. Nothing was deleted; delete this segment explicitly if it is no longer needed."
                    );
                    problems.push(detail.clone());
                    (SegmentState::Failed, detail, None, None)
                }
                None => {
                    let delivered = delivery == Delivery::Appended;
                    let detail = if delivered {
                        // The crash landed between deleting this segment's text and
                        // dropping its index entry. The result was already delivered
                        // and is not pending; do not resurrect it as a failure.
                        format!(
                            "Segment {sequence}'s complete result was already delivered to its target by the previous session, and no pending recording or separate text copy was left. Nothing is pending for this segment."
                        )
                    } else if record.state == SegmentState::Failed {
                        format!(
                            "Segment {sequence} failed in the previous Listen session; {}. No pending recording or complete text was left to recover.",
                            record.detail
                        )
                    } else {
                        format!(
                            "Segment {sequence} has no readable recording and no saved complete text, so nothing could be recovered. Nothing was deleted."
                        )
                    };
                    let state = if delivered {
                        SegmentState::Result
                    } else {
                        SegmentState::Failed
                    };
                    let delivery = delivered.then_some(Delivery::Appended);
                    (state, detail, delivery, None)
                }
            },
        };
        let resumable = audio.is_some() && state == SegmentState::Processing;
        let note = if text_is_some {
            Some(
                "Recovered after Listen restarted; this complete result stays in the panel and is never written to the old target automatically."
                    .to_string(),
            )
        } else if resumable {
            Some(recovery_note(audio_problem.as_deref()))
        } else {
            None
        };
        // Rebuild the facts from the record and override only what the scan
        // decided. Every other durable fact (the capture directory, the term
        // snapshot, the cost, the delivery pointer) is carried by the record, so a
        // field added to `Segment` reaches recovery without being listed here.
        let mut facts = record.clone();
        facts.state = state;
        facts.detail = detail;
        if text.is_some() {
            // A readable complete text is the durable copy: the record points at the
            // one file name the naming convention owns.
            facts.text_file = Some(SegmentStore::text_name(sequence));
            facts.text_chars = text.as_ref().map(|text| text.chars().count()).unwrap_or(0);
        }
        facts.delivery = delivery;
        facts.text = text;
        segments.push(RecoveredSegment {
            facts,
            audio,
            retained_paths,
            release,
            note,
        });
    }
    // Artifacts the index never described: a complete text saved just before the
    // index write was lost, or a recording whose index entry never landed. They are
    // recovered instead of being treated as orphans on disk.
    let (orphan_texts, orphan_directories) =
        scan_orphans(runtime, &known_sequences, &known_directories, deliveries);
    Plan {
        segments,
        orphan_texts,
        orphan_directories,
        problems,
    }
}

/// Every complete text file and capture directory the index did not describe.
fn scan_orphans(
    runtime: &Path,
    known_sequences: &[u64],
    known_directories: &[String],
    deliveries: &BTreeMap<u64, DeliveryState>,
) -> (Vec<OrphanText>, Vec<OrphanDirectory>) {
    let Ok(entries) = fs::read_dir(runtime) else {
        return (Vec::new(), Vec::new());
    };
    let mut texts = Vec::new();
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() && name.starts_with(SESSION_PREFIX) {
            if known_directories.iter().any(|known| known == &name) {
                continue;
            }
            let audio = recovered_audio(&path);
            directories.push(OrphanDirectory {
                name,
                audio,
                retained: crate::audio_paths(&path),
            });
            continue;
        }
        let Some(sequence) = SegmentStore::text_sequence(&name) else {
            continue;
        };
        if known_sequences.contains(&sequence) {
            continue;
        }
        // An empty file is not a complete result; nothing is deleted, but it is not
        // presented as pending text either.
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        texts.push(OrphanText {
            sequence,
            name,
            text,
            // A drained index entry can leave this complete text behind (a confirmed
            // 精修 writes it again). The delivery record still knows what happened.
            delivery: deliveries.get(&sequence).copied(),
        });
    }
    texts.sort_by_key(|orphan| orphan.sequence);
    directories.sort_by(|left, right| left.name.cmp(&right.name));
    (texts, directories)
}

/// Why an unfinished recording is still processed but must not be presented as a
/// complete recording or a passing transcript.
pub(crate) fn incomplete_note(problem: &str) -> String {
    format!(
        " The recovered recording is incomplete or cannot be proven complete ({problem}); only the readable part is processed, and this is neither a complete recording nor a passing transcript."
    )
}

/// The complete text this segment already owns, if it is readable.
fn read_text(runtime: &Path, record: &Segment) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(name) = &record.text_file {
        candidates.push(name.clone());
    }
    candidates.push(crate::segments::SegmentStore::text_name(record.sequence));
    candidates.dedup();
    for name in candidates {
        if Path::new(&name).components().count() != 1 {
            continue;
        }
        let Ok(text) = fs::read_to_string(runtime.join(&name)) else {
            continue;
        };
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

/// A directory field from the index is only usable when it is one plain name
/// inside the runtime directory. A corrupted index must never make recovery read
/// or delete something outside it.
pub(crate) fn safe_directory(runtime: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.contains(['/', '\\'])
        || !name.starts_with(SESSION_PREFIX)
        || Path::new(name).components().count() != 1
    {
        return None;
    }
    let path = runtime.join(name);
    path.is_dir().then_some(path)
}

/// The continuous recording of one cloud capture, validated against the
/// descriptors the device wrote beside it.
fn recovered_audio(directory: &Path) -> Option<RecoveredAudio> {
    let pcm = directory.join(PCM_FILE);
    let len = fs::metadata(&pcm)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    // A local capture uses one WAV per recognition window instead; it is handled
    // separately so the two layouts never address each other's files.
    if len == 0 {
        return local_audio(directory);
    }
    let manifest = read_pcm_state(directory);
    let mut problems: Vec<String> = Vec::new();
    let committed = match &manifest {
        Some(manifest) => {
            if !manifest.clean_stop {
                problems.push("the recording was not closed by a normal stop".into());
            }
            if manifest.committed_bytes != len {
                problems.push(format!(
                    "the end marker reports {} committed bytes but {len} bytes are readable",
                    manifest.committed_bytes
                ));
            }
            manifest.committed_bytes.min(len)
        }
        None => {
            problems.push(
                "the previous session ended without an end-of-recording marker, so the recording cannot be proven complete"
                    .into(),
            );
            len
        }
    };
    if !pcm_format_ok(directory) {
        problems.push("the recording has no matching 16 kHz mono 16-bit format descriptor".into());
    }
    let usable = (committed / 2) * 2;
    if usable != len {
        problems.push(format!(
            "only {usable} of {len} readable bytes are whole audio samples"
        ));
    }
    let mut problem = (!problems.is_empty()).then(|| problems.join("; "));
    if usable == 0 {
        return None;
    }
    // The recognition adapter can only resume a whole-sample stream, so a torn or
    // over-long spool is copied up to the readable whole samples. The original
    // stays where it is: it is still the only recovery basis until the complete
    // text is saved.
    let (path, repaired) = if usable == len {
        (pcm, false)
    } else {
        match repair_copy(&pcm, directory, usable) {
            Ok(path) => (path, true),
            Err(error) => {
                problem.get_or_insert_with(|| {
                    format!("the readable audio could not be copied for recovery ({error})")
                });
                return None;
            }
        }
    };
    let mut description = format!(
        "Recovered continuous playback recording ({:.1} s, 16 kHz mono)",
        usable as f64 / 2.0 / 16_000.0
    );
    if repaired {
        description.push_str("; only the readable part is repaired and processed");
    }
    Some(RecoveredAudio {
        path,
        description,
        problem,
        retained: crate::audio_paths(directory),
    })
}

/// Copy the readable prefix of a damaged continuous recording into a separate file
/// inside the same segment directory. The copy is bounded by the committed bytes
/// and is streamed, so a large spool is never held in memory.
fn repair_copy(pcm: &Path, directory: &Path, usable: u64) -> Result<PathBuf, String> {
    let mut source = std::fs::File::open(pcm)
        .map_err(|error| format!("the damaged recording could not be read: {error}"))?;
    let path = directory.join(REPAIRED_PCM_FILE);
    let mut target = std::fs::File::create(&path)
        .map_err(|error| format!("the repaired recording could not be created: {error}"))?;
    std::io::copy(&mut std::io::Read::take(&mut source, usable), &mut target)
        .map_err(|error| format!("the repaired recording could not be written: {error}"))?;
    target
        .sync_all()
        .map_err(|error| format!("the repaired recording could not be saved: {error}"))?;
    Ok(path)
}

/// A local capture's recognition windows. The adapter's own rejoin helper is used
/// first; when a torn window makes that refuse the whole capture, the readable
/// windows are rejoined here instead, so an abnormal exit still recovers every
/// whole sample it can and the result is marked incomplete.
fn local_audio(directory: &Path) -> Option<RecoveredAudio> {
    let whole = directory.join("retry-audio.wav");
    if let Some(bytes) = readable_wav_bytes(&whole) {
        return Some(RecoveredAudio {
            path: whole,
            description: format!(
                "Recovered local playback recording ({:.1} s)",
                bytes as f64 / 2.0 / 16_000.0
            ),
            problem: None,
            retained: crate::audio_paths(directory),
        });
    }
    let mut windows: Vec<PathBuf> = fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("segment-"))
        })
        .map(|path| path.join("playback.wav"))
        .filter(|path| path.is_file())
        .collect();
    windows.sort();
    if windows.is_empty() {
        // No window at all: the adapter's helper may still know a stored rejoin.
        let path = crate::streaming::retained_audio_path(directory).ok()?;
        let bytes = readable_wav_bytes(&path)?;
        return Some(RecoveredAudio {
            path,
            description: format!(
                "Recovered local playback recording ({:.1} s)",
                bytes as f64 / 2.0 / 16_000.0
            ),
            problem: None,
            retained: crate::audio_paths(directory),
        });
    }
    let mut readable: Vec<(PathBuf, Vec<i16>)> = Vec::new();
    let mut problem: Option<String> = None;
    for window in &windows {
        match read_window(window) {
            Some(samples) => readable.push((window.clone(), samples)),
            None => {
                problem.get_or_insert_with(|| {
                    format!(
                        "a recognition window of the previous session was torn and is not audio; {} of {} readable windows are used",
                        readable.len(),
                        windows.len()
                    )
                });
            }
        }
    }
    if readable.is_empty() {
        return None;
    }
    let overlap = capture_overlap(directory);
    if overlap.is_none() {
        problem.get_or_insert_with(|| {
            "the window overlap of the previous session is missing; the readable windows are joined without overlap removal".into()
        });
    }
    let path = match rejoin_windows(directory, &readable, overlap.unwrap_or(0)) {
        Ok(path) => path,
        Err(error) => {
            problem.get_or_insert_with(|| {
                format!("the readable windows could not be joined ({error})")
            });
            return None;
        }
    };
    let bytes = readable_wav_bytes(&path).unwrap_or(0);
    Some(RecoveredAudio {
        path,
        description: format!(
            "Recovered local playback recording ({:.1} s, rejoined from {} readable window(s))",
            bytes as f64 / 2.0 / 16_000.0,
            readable.len()
        ),
        problem,
        retained: crate::audio_paths(directory),
    })
}

/// The whole, readable content of one local recognition window.
fn read_window(path: &Path) -> Option<Vec<i16>> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 || spec.bits_per_sample != 16 {
        return None;
    }
    reader.samples::<i16>().collect::<Result<Vec<_>, _>>().ok()
}

/// One WAV file's total audio bytes, if its header and content are readable.
fn readable_wav_bytes(path: &Path) -> Option<u64> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 || spec.bits_per_sample != 16 {
        return None;
    }
    let samples = reader.samples::<i16>().count();
    (samples > 0).then_some(samples as u64 * 2)
}

fn capture_overlap(directory: &Path) -> Option<usize> {
    let manifest = fs::read_to_string(directory.join("capture-manifest.txt")).ok()?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("overlap_samples="))
        .and_then(|value| value.trim().parse::<usize>().ok())
}

/// Rejoin readable windows into one WAV, trimming the shared overlap so a word
/// that spans a boundary is not repeated.
fn rejoin_windows(
    directory: &Path,
    windows: &[(PathBuf, Vec<i16>)],
    overlap: usize,
) -> Result<PathBuf, String> {
    let path = directory.join(REPAIRED_WAV_FILE);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec)
        .map_err(|error| format!("the rejoined recording could not be created: {error}"))?;
    let mut written = 0usize;
    for (index, (_, samples)) in windows.iter().enumerate() {
        let skip = if index == 0 {
            0
        } else {
            overlap.min(samples.len())
        };
        if skip < overlap && index > 0 {
            // A window shorter than the overlap cannot be trimmed exactly; keep it
            // whole rather than dropping its end.
            for sample in samples {
                writer
                    .write_sample(*sample)
                    .map_err(|error| error.to_string())?;
                written += 1;
            }
            continue;
        }
        for sample in &samples[skip..] {
            writer
                .write_sample(*sample)
                .map_err(|error| error.to_string())?;
            written += 1;
        }
    }
    writer
        .finalize()
        .map_err(|error| format!("the rejoined recording could not be saved: {error}"))?;
    if written == 0 {
        return Err("no readable audio samples remained".into());
    }
    Ok(path)
}

struct PcmState {
    committed_bytes: u64,
    clean_stop: bool,
}

fn read_pcm_state(directory: &Path) -> Option<PcmState> {
    let text = fs::read_to_string(directory.join(PCM_STATE_FILE)).ok()?;
    let mut committed_bytes = None;
    let mut clean_stop = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "committed_bytes" => committed_bytes = value.trim().parse::<u64>().ok(),
            "clean_stop" => {
                clean_stop = match value.trim() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    Some(PcmState {
        committed_bytes: committed_bytes?,
        clean_stop: clean_stop?,
    })
}

/// Whether the recording's own format descriptor describes the audio the cloud
/// adapter can resume. A missing or mismatching descriptor is an incomplete
/// recording, not a silently assumed one.
fn pcm_format_ok(directory: &Path) -> bool {
    let Ok(text) = fs::read_to_string(directory.join(PCM_FORMAT_FILE)) else {
        return false;
    };
    let mut sample_rate = false;
    let mut channels = false;
    let mut bits = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "sample_rate" => sample_rate = value.trim() == "16000",
            "channels" => channels = value.trim() == "1",
            "bits" => bits = value.trim() == "16",
            _ => {}
        }
    }
    sample_rate && channels && bits
}
