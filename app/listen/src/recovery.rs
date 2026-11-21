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
    delivery::DeliveryState,
    recognition::SegmentUsage,
    segments::{SegmentRecord, SegmentStore},
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
const SESSION_PREFIX: &str = "session-";
const SEGMENT_TEXT_PREFIX: &str = "segment-";
const SEGMENT_TEXT_SUFFIX: &str = ".txt";

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

/// One segment the previous process left behind, as the application rebuilds it.
pub(crate) struct RecoveredSegment {
    pub sequence: u64,
    pub directory: Option<PathBuf>,
    pub state: SegmentState,
    pub detail: String,
    pub text: Option<String>,
    pub delivery: Option<Delivery>,
    pub usage: Option<SegmentUsage>,
    pub glossary_terms: Vec<String>,
    pub audio: Option<RecoveredAudio>,
    pub retained_paths: Vec<PathBuf>,
    /// Whether this process should process the recording again automatically.
    pub resumable: bool,
    /// Whether the complete text is durably readable, so the audio may be released.
    pub release_audio: bool,
}

/// A complete text file the index does not describe: the previous process saved
/// the result and then stopped before its index entry was durable, or the index
/// itself was lost. Its sequence comes from the file name, so its identity is kept.
pub(crate) struct OrphanText {
    sequence: u64,
    name: String,
    text: String,
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
            let detail = format!(
                "{RECOVERED_TEXT_DETAIL} The segment index did not describe this saved result, so its delivery state is unknown; it is kept in the panel and is never written again automatically."
            );
            let delivery = Delivery::Uncertain;
            self.segments.push(RecoveredSegment {
                sequence: orphan.sequence,
                directory: None,
                state: SegmentState::Result,
                detail: detail.clone(),
                text: Some(orphan.text.clone()),
                delivery: Some(delivery),
                usage: None,
                glossary_terms: terms.to_vec(),
                audio: None,
                retained_paths: Vec::new(),
                resumable: false,
                release_audio: false,
            });
            let record = SegmentRecord {
                sequence: orphan.sequence,
                directory: String::new(),
                state: SegmentState::Result.name().to_owned(),
                detail,
                text_file: Some(orphan.name),
                text_chars: orphan.text.chars().count(),
                delivery: Some("uncertain".into()),
                usage: None,
                glossary_terms: terms.to_vec(),
                updated_utc: crate::segments::now_utc(),
            };
            if let Some(error) = store.upsert(record) {
                warnings.push(error);
            }
        }
        for orphan in std::mem::take(&mut self.orphan_directories) {
            let sequence = store.allocate();
            let (state, detail, delivery, resumable) = match &orphan.audio {
                Some(audio) => {
                    let mut detail = ORPHAN_RESUMED_DETAIL.to_string();
                    if let Some(problem) = &audio.problem {
                        detail.push_str(&incomplete_note(problem));
                    }
                    (SegmentState::Processing, detail, None, true)
                }
                None => (
                    SegmentState::Failed,
                    format!(
                        "Segment {sequence} was found in the runtime directory but not in the segment index, and no readable audio was found. Nothing was deleted; delete this segment explicitly if it is no longer needed."
                    ),
                    None,
                    false,
                ),
            };
            self.segments.push(RecoveredSegment {
                sequence,
                directory: Some(Path::new(&orphan.name).to_path_buf()),
                state,
                detail: detail.clone(),
                text: None,
                delivery,
                usage: None,
                glossary_terms: terms.to_vec(),
                audio: orphan.audio,
                retained_paths: orphan.retained,
                resumable,
                release_audio: false,
            });
            let record = SegmentRecord {
                sequence,
                directory: orphan.name,
                state: state.name().to_owned(),
                detail,
                text_file: None,
                text_chars: 0,
                delivery: None,
                usage: None,
                glossary_terms: terms.to_vec(),
                updated_utc: crate::segments::now_utc(),
            };
            if let Some(error) = store.upsert(record) {
                warnings.push(error);
            }
        }
        if self.segments.iter().any(|segment| {
            segment.detail.starts_with(ORPHAN_RESUMED_DETAIL)
                || segment.detail.contains("not in the segment index")
        }) {
            self.problems.push(
                "The segment index did not describe every leftover capture; those segments were recovered from the runtime directory and are listed after the recorded ones."
                    .into(),
            );
        }
        self.segments.sort_by_key(|segment| segment.sequence);
        warnings
    }
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
            if segment.resumable {
                if let Some(audio) = segment.audio.clone() {
                    resumed += 1;
                    self.queue.push_back(Resume {
                        sequence: segment.sequence,
                        audio,
                        glossary_terms: segment.glossary_terms,
                    });
                }
            } else if segment.text.is_some() {
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
/// `delivery_problem` says the durable delivery record could not be read at all.
/// Every recovered text is then `Uncertain`: a write may have happened and this
/// process cannot prove it, so it is never appended automatically. A delivery
/// record that was pruned away is treated exactly like a missing one — panel-only,
/// fail-closed.
pub(crate) fn plan(
    runtime: &Path,
    records: &[SegmentRecord],
    deliveries: &BTreeMap<u64, DeliveryState>,
    delivery_problem: bool,
) -> Plan {
    let mut sorted: Vec<&SegmentRecord> = records.iter().collect();
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
        let delivery_state = deliveries.get(&sequence).copied();
        let recorded_delivery = parse_delivery(record.delivery.as_deref());
        let delivery = delivery_state.or(recorded_delivery);
        let resumable_state = matches!(
            record.state.as_str(),
            "capturing" | "waiting_for_network" | "processing"
        );
        let (state, detail, delivery, resumable, release_audio) = match text.as_ref() {
            Some(_) => {
                let delivery = recovered_delivery(delivery, delivery_problem);
                let mut detail = RECOVERED_TEXT_DETAIL.to_string();
                if delivery == Delivery::Appended {
                    detail.push_str(RECOVERED_APPENDED_NOTE);
                } else if delivery == Delivery::Uncertain {
                    detail.push_str(UNKNOWN_DELIVERY_NOTE);
                }
                (SegmentState::Result, detail, Some(delivery), false, true)
            }
            None => match audio.as_ref() {
                Some(audio) if resumable_state => {
                    let mut detail = RESUMED_DETAIL.to_string();
                    if let Some(problem) = &audio.problem {
                        detail.push_str(&incomplete_note(problem));
                    }
                    (SegmentState::Processing, detail, None, true, false)
                }
                Some(audio) => {
                    // The recording is recoverable but the segment is not in flight:
                    // a failure stays a failure. It is never retried automatically
                    // because that would spend money without the user asking.
                    let mut detail = if record.state == "no_speech" {
                        "The previous session recorded no speech for this segment, but its audio is still kept on this machine.".to_string()
                    } else {
                        format!(
                            "The previous session ended with this segment recorded as '{}'. {} The recording is kept on this machine.",
                            record.state, record.detail
                        )
                    };
                    if let Some(problem) = &audio.problem {
                        detail.push_str(&incomplete_note(problem));
                    }
                    let state = if record.state == "no_speech" {
                        SegmentState::NoSpeech
                    } else {
                        SegmentState::Failed
                    };
                    (state, detail, None, false, false)
                }
                None if directory.is_some() => {
                    // A directory with nothing processable in it. Do not delete it:
                    // it may be the only remaining evidence of the attempt.
                    let detail = format!(
                        "Segment {sequence} was recovered from the previous Listen session, but no readable audio and no complete text were found. Nothing was deleted; delete this segment explicitly if it is no longer needed."
                    );
                    problems.push(detail.clone());
                    (SegmentState::Failed, detail, None, false, false)
                }
                None => {
                    let detail = if record.state == "failed" {
                        format!(
                            "Segment {sequence} failed in the previous Listen session; {}. No pending recording or complete text was left to recover.",
                            record.detail
                        )
                    } else {
                        format!(
                            "Segment {sequence} has no readable recording and no saved complete text, so nothing could be recovered. Nothing was deleted."
                        )
                    };
                    (SegmentState::Failed, detail, None, false, false)
                }
            },
        };
        segments.push(RecoveredSegment {
            sequence,
            directory,
            state,
            detail,
            text,
            delivery,
            usage: record.usage.clone(),
            glossary_terms: record.glossary_terms.clone(),
            audio,
            retained_paths,
            resumable,
            release_audio,
        });
    }
    // Artifacts the index never described: a complete text saved just before the
    // index write was lost, or a recording whose index entry never landed. They are
    // recovered instead of being treated as orphans on disk.
    let (orphan_texts, orphan_directories) = scan_orphans(runtime, &known_sequences, &known_directories);
    Plan {
        segments,
        orphan_texts,
        orphan_directories,
        problems,
    }
}

/// A recovered result's delivery state. A missing record (never written, pruned, or
/// an unreadable ledger) can never prove that nothing was written, so it is
/// reported as uncertain instead of claiming a panel-only fallback.
fn recovered_delivery(state: Option<DeliveryState>, ledger_problem: bool) -> Delivery {
    match state {
        Some(DeliveryState::Appended) => Delivery::Appended,
        Some(DeliveryState::Started) | Some(DeliveryState::Uncertain) => Delivery::Uncertain,
        Some(DeliveryState::Pending) | Some(DeliveryState::Fallback) => Delivery::Fallback,
        None if ledger_problem => Delivery::Uncertain,
        None => Delivery::Fallback,
    }
}

/// Every complete text file and capture directory the index did not describe.
fn scan_orphans(
    runtime: &Path,
    known_sequences: &[u64],
    known_directories: &[String],
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
        let Some(sequence) = name
            .strip_prefix(SEGMENT_TEXT_PREFIX)
            .and_then(|rest| rest.strip_suffix(SEGMENT_TEXT_SUFFIX))
            .and_then(|digits| digits.parse::<u64>().ok())
        else {
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
fn read_text(runtime: &Path, record: &SegmentRecord) -> Option<String> {
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
fn safe_directory(runtime: &Path, name: &str) -> Option<PathBuf> {
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

fn parse_delivery(recorded: Option<&str>) -> Option<DeliveryState> {
    match recorded {
        Some("appended") => Some(DeliveryState::Appended),
        Some("fallback") => Some(DeliveryState::Fallback),
        Some("started") => Some(DeliveryState::Started),
        Some("uncertain") => Some(DeliveryState::Uncertain),
        Some(_) | None => None,
    }
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
    let mut problem: Option<String> = None;
    let committed = match &manifest {
        Some(manifest) => {
            if !manifest.clean_stop {
                problem = Some("the recording was not closed by a normal stop".into());
            }
            if manifest.committed_bytes != len {
                problem.get_or_insert_with(|| {
                    format!(
                        "the end marker reports {} committed bytes but {len} bytes are readable",
                        manifest.committed_bytes
                    )
                });
            }
            manifest.committed_bytes.min(len)
        }
        None => {
            problem = Some(
                "the previous session ended without an end-of-recording marker, so the recording cannot be proven complete"
                    .into(),
            );
            len
        }
    };
    if !pcm_format_ok(directory) {
        problem.get_or_insert_with(|| {
            "the recording has no matching 16 kHz mono 16-bit format descriptor".into()
        });
    }
    let usable = (committed / 2) * 2;
    if usable != len {
        problem.get_or_insert_with(|| {
            format!("only {usable} of {len} readable bytes are whole audio samples")
        });
    }
    if usable == 0 || usable != len {
        // Recovering the readable prefix needs a repair copy, which the next
        // recovery slice adds together with its test.
        return None;
    }
    Some(RecoveredAudio {
        path: pcm,
        description: format!(
            "Recovered continuous playback recording ({:.1} s, 16 kHz mono)",
            usable as f64 / 2.0 / 16_000.0
        ),
        problem,
        retained: crate::audio_paths(directory),
    })
}

/// A local capture's recognition windows are rejoined by the local adapter's own
/// helper, so recovery never has to understand the WAV layout.
fn local_audio(directory: &Path) -> Option<RecoveredAudio> {
    let path = crate::streaming::retained_audio_path(directory).ok()?;
    let bytes = fs::metadata(&path).map(|metadata| metadata.len()).ok()?;
    if bytes == 0 {
        return None;
    }
    Some(RecoveredAudio {
        path,
        description: "Recovered local playback recording".into(),
        problem: None,
        retained: crate::audio_paths(directory),
    })
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
