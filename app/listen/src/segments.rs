//! Durable per-segment state for multi-segment capture (#33).
//!
//! Every start/stop is an independent segment: its audio stays in a session
//! directory, its complete text and its state/usage are recorded separately, and a
//! later segment never overwrites an earlier one.
//!
//! This module owns the on-disk shape **and** the one way it changes. [`Segment`] is
//! the authoritative representation of one segment: `segments.json` is that value
//! serialized (the frozen version-1 shape), the panel's summary is a projection of
//! it, and a restart scan rebuilds it from the index plus the text file. Runtime-only
//! state (a live recognition handle, a bound target, `Instant`s) belongs to the
//! application's own session, never here: a scan result must stay read-only.
//!
//! Layout in the runtime directory (all files, so `session_directories` style checks
//! on the audio lifecycle still mean what they meant):
//!
//! ```text
//! segments.json            versioned index, capture order, next sequence
//! segment-00000001.txt     the complete text of segment 1, once saved
//! session-<pid>-<nanos>/   the segment's audio, until the text is safely saved
//! ```
//!
//! Precedence: a segment's text file is the durable copy; the index points at it.
//! `segments.json` is replaced atomically only after the text is on disk, so an
//! interrupted save can lose the index entry but never the complete text file it
//! already wrote.
//!
//! # The one write path
//!
//! `SegmentStore` exposes named changes ([`Change`]) instead of a mutable record.
//! The caller says *what happened*; the store decides whether the index has to be
//! written, never writes while the record is degraded, and reports a write failure
//! exactly once. There is no `record_mut`: a caller cannot leave the index
//! half-updated by forgetting the persist, and cannot re-report the same failure.

use crate::{
    AppResult, Delivery, SegmentState,
    delivery::DeliveryState,
    durable::{VersionedRecord, atomic_write, now_utc, open_versioned},
    recognition::SegmentUsage,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Bumping this is a compatibility decision: an index with any other version is
/// read as unusable and is never silently rewritten.
const STORE_VERSION: u32 = 1;
const INDEX_FILE: &str = "segments.json";
const TEXT_PREFIX: &str = "segment-";
const TEXT_SUFFIX: &str = ".txt";

/// One segment's authoritative facts: everything that must survive a process
/// restart, in one place.
///
/// The on-disk shape is this value's `Serialize` (version 1, unchanged since #33):
/// `text_file`/`text_chars` are the durable pointers to the complete text, `text` is
/// that text once this process has read or produced it. A new durable field is added
/// here and nowhere else; only a field the panel shows also needs the one projection
/// ([`crate::SegmentSummary::of`]).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Segment {
    pub sequence: u64,
    /// The runtime session directory *name* that holds this segment's audio.
    pub directory: String,
    /// The stable spelling of the user-visible state (`capturing`,
    /// `waiting_for_network`, `processing`, `result`, `no_speech`, `failed`).
    #[serde(with = "state_name")]
    pub state: SegmentState,
    /// The user-readable state line, without a credential or vendor error body.
    pub detail: String,
    /// The complete text file name inside the runtime directory, once saved.
    pub text_file: Option<String>,
    pub text_chars: usize,
    /// A delivery decision, once one exists: `appended`, `fallback` or `uncertain`.
    /// Never a guess; the durable delivery record remains the authority for
    /// `pending`/`started` writes.
    #[serde(with = "delivery_name")]
    pub delivery: Option<Delivery>,
    /// What the engine recorded for this segment's paid requests, if anything.
    pub usage: Option<SegmentUsage>,
    /// The fixed 术语表 snapshot of this capture, as canonical terms. A segment
    /// recovered after a restart is processed again with exactly these terms, not
    /// with whatever the glossary file holds at the next launch (#35). An index
    /// written before this field existed loads as an empty snapshot.
    #[serde(default)]
    pub glossary_terms: Vec<String>,
    pub updated_utc: f64,
    /// The complete text itself, once this process has it. It is not stored in
    /// `segments.json`; the text file above is the durable copy.
    #[serde(skip)]
    pub text: Option<String>,
}

impl Segment {
    /// A capture that just started: its first durable record.
    pub fn capturing(
        sequence: u64,
        directory: &str,
        detail: &str,
        glossary_terms: &[String],
    ) -> Self {
        Self {
            sequence,
            directory: directory.into(),
            state: SegmentState::Capturing,
            detail: detail.into(),
            glossary_terms: glossary_terms.to_vec(),
            ..Self::default()
        }
    }
}

/// The frozen spelling of a state in `segments.json`.
///
/// An unrecognized name makes the whole index unreadable instead of being guessed
/// into a conservative-looking state: `open_versioned` reports the degradation, the
/// file is left byte-for-byte as it is, and the store refuses every write until the
/// user repairs or removes the record (ADR 0005). Recovery can still adopt durable
/// text and audio from the runtime directory as orphans, exactly as it does for an
/// index this version cannot read at all.
mod state_name {
    use super::SegmentState;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        state: &SegmentState,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(state.name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SegmentState, D::Error> {
        let name = String::deserialize(deserializer)?;
        SegmentState::from_name(&name).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown segment state {name:?}; this version understands \
                 capturing, waiting_for_network, processing, result, no_speech and failed, \
                 so the stored spelling is left unchanged instead of being guessed"
            ))
        })
    }
}

/// The frozen spelling of a resolved delivery decision in `segments.json`.
///
/// Like [`super::DeliveryState::from_name`] on the `delivery.json` side, an unknown
/// spelling is never guessed into a safer-looking state: it makes this index
/// unreadable and is reported and left as stored.
mod delivery_name {
    use super::{Delivery, DeliveryState};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        delivery: &Option<Delivery>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match delivery {
            Some(delivery) => serializer.serialize_some(DeliveryState::from(*delivery).name()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Delivery>, D::Error> {
        let name = Option::<String>::deserialize(deserializer)?;
        match name {
            None => Ok(None),
            Some(name) => match DeliveryState::from_name(&name) {
                Some(state) => Ok(Some(Delivery::from(state))),
                None => Err(D::Error::custom(format!(
                    "unknown delivery {name:?}; this version understands appended, fallback \
                     and uncertain, plus the legacy pending/started names (read as uncertain), \
                     so the stored spelling is left unchanged instead of being guessed"
                ))),
            },
        }
    }
}

/// One named change to one segment's durable record. The caller says what happened;
/// the store decides whether that needs a write.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// This segment's own state line changed (`state` + `detail`).
    Status {
        sequence: u64,
        state: SegmentState,
        detail: String,
    },
    /// The complete text was saved as this segment's own text file; `current` says
    /// the text is also the current result (`current-result.txt`). The record stores
    /// where the text is, not its content: the content is this process's own and is
    /// set where the application obtains it (a delivery, a failure, a 精修 or a
    /// recovery scan).
    Text {
        sequence: u64,
        chars: usize,
        current: bool,
    },
    /// What the engine metered for this segment's paid requests.
    Usage { sequence: u64, usage: SegmentUsage },
    /// Restart recovery rebuilt this segment from the files on disk. Only the parts
    /// that could be rebuilt are written; the rest keeps what the index said.
    Recovered {
        sequence: u64,
        state: SegmentState,
        detail: String,
        text: Option<String>,
        delivery: Option<Delivery>,
    },
    /// The capture-time term snapshot a lost index entry did not describe.
    GlossaryTerms { sequence: u64, terms: Vec<String> },
    /// A finished delivery: the outcome, its detail, the complete text and where the
    /// text is.
    Delivered {
        sequence: u64,
        state: SegmentState,
        detail: String,
        text: String,
        delivery: Delivery,
    },
    /// Which capture owns `current-result.txt`, for an index that did not record it.
    CurrentResult { sequence: u64 },
}

impl Change {
    /// The capture order this change names.
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Status { sequence, .. }
            | Self::Text { sequence, .. }
            | Self::Usage { sequence, .. }
            | Self::Recovered { sequence, .. }
            | Self::GlossaryTerms { sequence, .. }
            | Self::Delivered { sequence, .. }
            | Self::CurrentResult { sequence } => *sequence,
        }
    }

    /// The capture order that must become the current result, if this change says so.
    fn current_result(&self) -> Option<u64> {
        match self {
            Self::Text {
                sequence,
                current: true,
                ..
            }
            | Self::CurrentResult { sequence } => Some(*sequence),
            _ => None,
        }
    }

    /// Write this change into one segment's facts. `updated_utc` is the store's
    /// business, so the same change can be applied to the application's copy too.
    pub fn write(&self, record: &mut Segment) {
        match self {
            Self::Status { state, detail, .. } => {
                record.state = *state;
                record.detail = detail.clone();
            }
            Self::Text {
                sequence, chars, ..
            } => {
                record.text_file = Some(SegmentStore::text_name(*sequence));
                record.text_chars = *chars;
            }
            Self::Usage { usage, .. } => record.usage = Some(usage.clone()),
            Self::Recovered {
                state,
                detail,
                text,
                delivery,
                ..
            } => {
                record.state = *state;
                record.detail = detail.clone();
                if let Some(text) = text {
                    record.text_file = Some(SegmentStore::text_name(record.sequence));
                    record.text_chars = text.chars().count();
                    record.text = Some(text.clone());
                }
                if let Some(delivery) = delivery {
                    record.delivery = Some(*delivery);
                }
            }
            Self::GlossaryTerms { terms, .. } => record.glossary_terms = terms.clone(),
            Self::Delivered {
                state,
                detail,
                text,
                delivery,
                ..
            } => {
                record.state = *state;
                record.detail = detail.clone();
                record.delivery = Some(*delivery);
                record.text_file = Some(SegmentStore::text_name(record.sequence));
                record.text_chars = text.chars().count();
                record.text = Some(text.clone());
            }
            Self::CurrentResult { .. } => {}
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    next_sequence: u64,
    /// The capture order whose complete text is the current result (#35). It is
    /// persisted with the text file so a restart knows which segment owns
    /// `current-result.txt`; an older segment completing later must never take it.
    /// An index written before this field existed loads as `0`.
    #[serde(default)]
    current_sequence: u64,
    segments: Vec<Segment>,
}

impl VersionedRecord for Index {
    const VERSION: u32 = STORE_VERSION;

    fn version(&self) -> u32 {
        self.version
    }
}

/// The runtime-directory segment store. It is used from the application thread
/// only, so index writes are naturally serialized; workers only read their own text
/// file through [`SegmentStore::write_text`].
pub(crate) struct SegmentStore {
    runtime: PathBuf,
    next_sequence: u64,
    current_sequence: u64,
    segments: Vec<Segment>,
    /// Why the stored index could not be used, if it could not. The segments are
    /// still captured; only the durable record is degraded and says so.
    problem: Option<String>,
    /// Whether a record problem was already handed to the caller. The store reports
    /// a degraded or failing index once; every later change is still applied in
    /// memory and stays silent, exactly like the single problem line the panel shows.
    reported: bool,
}

impl SegmentStore {
    pub fn open(runtime: &Path) -> Self {
        let (index, problem) = open_versioned::<Index>(
            &runtime.join(INDEX_FILE),
            "the multi-segment record",
            "; new segments are still captured but their recovery record is not updated",
        );
        let (segments, next_sequence, current_sequence) = match index {
            Some(index) => (
                index.segments,
                index.next_sequence.max(1),
                index.current_sequence,
            ),
            None => (Vec::new(), 1, 0),
        };
        // Never reuse a sequence that already owns a text file, even when the index
        // itself was lost or unusable.
        let highest = segments
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or(0)
            .max(highest_saved_sequence(runtime));
        let next_sequence = next_sequence.max(highest.saturating_add(1));
        Self {
            runtime: runtime.to_path_buf(),
            next_sequence,
            current_sequence,
            segments,
            problem,
            reported: false,
        }
    }

    /// Why the durable record is degraded, if it is. Never includes a credential.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The capture order whose complete text is the current result, as recorded
    /// when that result was saved. `0` means the record does not say.
    pub fn current_sequence(&self) -> u64 {
        self.current_sequence
    }

    /// Every stored segment, oldest capture order first.
    pub fn records(&self) -> &[Segment] {
        &self.segments
    }

    /// The complete-text file name for one sequence, relative to the runtime
    /// directory. Stable, sortable and independent of every other segment.
    pub fn text_name(sequence: u64) -> String {
        format!("{TEXT_PREFIX}{sequence:08}{TEXT_SUFFIX}")
    }

    /// The capture order encoded in a `segment-NNNNNNNN.txt` name, if the name is
    /// one. This is the one place that knows the naming convention.
    pub(crate) fn text_sequence(name: &str) -> Option<u64> {
        name.strip_prefix(TEXT_PREFIX)?
            .strip_suffix(TEXT_SUFFIX)?
            .parse()
            .ok()
    }

    /// Whether a runtime-directory entry is one segment's complete-text file.
    pub(crate) fn is_text_name(name: &str) -> bool {
        Self::text_sequence(name).is_some()
    }

    /// Start one durable segment: allocate the next capture order no record and no
    /// saved text owns, build its facts and write them. The sequence, the stored
    /// facts and any record problem are returned; the in-memory capture is never
    /// lost to a storage problem.
    pub fn begin(&mut self, build: impl FnOnce(u64) -> Segment) -> (u64, Segment, Option<String>) {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.saturating_add(1);
        let segment = build(sequence);
        self.segments.push(segment.clone());
        (sequence, segment, self.persist())
    }

    /// Adopt one segment the startup scan rebuilt. A record the index never
    /// described (an interrupted index save, a corrupt index) keeps its own stable
    /// capture order and is never merged into another segment.
    pub fn adopt(&mut self, segment: Segment) -> Option<String> {
        self.next_sequence = self.next_sequence.max(segment.sequence.saturating_add(1));
        match self
            .segments
            .iter_mut()
            .find(|stored| stored.sequence == segment.sequence)
        {
            Some(stored) => *stored = segment,
            None => self.segments.push(segment),
        }
        self.persist()
    }

    /// Apply one named change. Nothing is written when the change leaves the record
    /// as it was, and nothing is written at all while the record is degraded; a
    /// write failure is returned once and never repeated.
    pub fn apply(&mut self, change: Change) -> Option<String> {
        let mut changed = false;
        if let Some(record) = self
            .segments
            .iter_mut()
            .find(|record| record.sequence == change.sequence())
        {
            let before = record.clone();
            change.write(record);
            if *record != before {
                record.updated_utc = now_utc();
                changed = true;
            }
        }
        if let Some(sequence) = change.current_result()
            && self.current_sequence != sequence
        {
            self.current_sequence = sequence;
            changed = true;
        }
        if !changed {
            return None;
        }
        self.persist()
    }

    /// Write one segment's complete text. This is the reliable per-segment save;
    /// the caller must not release that segment's audio before it succeeds.
    pub fn write_text(&self, sequence: u64, text: &str) -> AppResult<String> {
        let name = Self::text_name(sequence);
        atomic_write(&self.runtime.join(&name), text.as_bytes()).map_err(|error| {
            format!("Could not save segment {sequence}'s complete result: {error}")
        })?;
        Ok(name)
    }

    /// Forget one drained segment and its text file. Only called after the result
    /// was safely delivered (appended) or explicitly cleaned; a pending result is
    /// never removed by the application on its own.
    pub fn remove(&mut self, sequence: u64) -> Option<String> {
        if let Some(name) = self
            .segments
            .iter()
            .find(|record| record.sequence == sequence)
            .and_then(|record| record.text_file.as_deref())
            .filter(|name| Path::new(name).components().count() == 1)
        {
            let _ = fs::remove_file(self.runtime.join(name));
        }
        self.segments.retain(|record| record.sequence != sequence);
        self.persist()
    }

    /// Replace the stored index with the current records. The one place that writes
    /// `segments.json`.
    fn persist(&mut self) -> Option<String> {
        if let Some(problem) = &self.problem {
            return self.report(problem.clone());
        }
        let index = Index {
            version: STORE_VERSION,
            next_sequence: self.next_sequence,
            current_sequence: self.current_sequence,
            segments: self.segments.clone(),
        };
        let bytes = match serde_json::to_vec(&index) {
            Ok(bytes) => bytes,
            Err(error) => return self.report(error.to_string()),
        };
        match atomic_write(&self.runtime.join(INDEX_FILE), &bytes) {
            Ok(()) => None,
            Err(error) => self.report(format!("Could not save the multi-segment record: {error}")),
        }
    }

    /// Hand one record problem to the caller, at most once per process.
    fn report(&mut self, error: String) -> Option<String> {
        if self.reported {
            return None;
        }
        self.reported = true;
        Some(error)
    }
}

/// The highest sequence that already owns a text file. Used only to avoid reusing
/// a sequence when the index was lost or unreadable.
fn highest_saved_sequence(runtime: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(runtime) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| SegmentStore::text_sequence(&name))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Delivery, SegmentState, SegmentSummary};
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Runtime(PathBuf);
    impl Runtime {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "listen-segment-store-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn usage(requests: u32) -> SegmentUsage {
        SegmentUsage {
            provider: "Alibaba Model Studio".into(),
            model: "controlled".into(),
            region: "cn-beijing".into(),
            requests,
            estimated_nanoyuan: None,
            unknown: true,
        }
    }

    /// `apply` returns the one problem worth reporting; `None` means the change was
    /// stored (or was already the record's state).
    trait Applied {
        fn applied(self);
    }
    impl Applied for Option<String> {
        fn applied(self) {
            assert!(self.is_none(), "the record is writable: {self:?}");
        }
    }

    #[test]
    fn a_saved_index_keeps_capture_order_text_files_and_usage() {
        let runtime = Runtime::new();
        let mut store = SegmentStore::open(&runtime.0);
        let terms = vec!["DeepSeek Harness".to_string(), "WASAPI".to_string()];
        let (first, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-1", "capturing", &terms));
        assert!(error.is_none());
        let text = store.write_text(first, "第一段完整结果。").unwrap();
        store
            .apply(Change::Text {
                sequence: first,
                chars: "第一段完整结果。".chars().count(),
                current: false,
            })
            .applied();
        store
            .apply(Change::Delivered {
                sequence: first,
                state: SegmentState::Result,
                detail: "stopped".into(),
                text: "第一段完整结果。".into(),
                delivery: Delivery::Fallback,
            })
            .applied();
        store
            .apply(Change::Usage {
                sequence: first,
                usage: usage(2),
            })
            .applied();
        let (second, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-2", "capturing", &terms));
        assert!(error.is_none());
        assert_eq!((first, second), (1, 2));

        let reopened = SegmentStore::open(&runtime.0);
        let records = reopened.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[0].state, SegmentState::Result);
        assert_eq!(records[0].delivery, Some(Delivery::Fallback));
        assert_eq!(records[0].text_file.as_deref(), Some(text.as_str()));
        assert_eq!(
            records[0].usage.as_ref().map(|usage| usage.requests),
            Some(2)
        );
        assert_eq!(records[0].glossary_terms, terms);
        assert_eq!(
            fs::read_to_string(runtime.0.join(&text)).unwrap(),
            "第一段完整结果。"
        );
        assert_eq!(records[1].sequence, 2);
    }

    #[test]
    fn a_new_segment_never_reuses_the_sequence_of_a_saved_text() {
        let runtime = Runtime::new();
        // No index, only a complete text file left by an interrupted run.
        fs::write(runtime.0.join(SegmentStore::text_name(7)), "旧的待取结果").unwrap();
        let mut store = SegmentStore::open(&runtime.0);
        let (sequence, _, _) =
            store.begin(|sequence| Segment::capturing(sequence, "session-new", "capturing", &[]));
        assert_eq!(
            sequence, 8,
            "an existing text file must never be overwritten"
        );
        assert_eq!(
            fs::read_to_string(runtime.0.join(SegmentStore::text_name(7))).unwrap(),
            "旧的待取结果"
        );
    }

    #[test]
    fn an_unsupported_index_is_reported_and_never_rewritten() {
        let runtime = Runtime::new();
        let index = runtime.0.join(INDEX_FILE);
        fs::write(&index, r#"{"version":99,"next_sequence":4,"segments":[]}"#).unwrap();
        let mut store = SegmentStore::open(&runtime.0);
        assert!(store.problem().is_some(), "{:?}", store.problem());
        let (sequence, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-new", "capturing", &[]));
        assert_eq!(sequence, 1);
        assert!(error.is_some(), "the degraded record must be reported");
        assert_eq!(
            fs::read_to_string(&index).unwrap(),
            r#"{"version":99,"next_sequence":4,"segments":[]}"#,
            "a future index is not silently downgraded"
        );
    }

    #[test]
    fn a_degraded_index_is_never_rewritten_and_reports_the_degradation_once() {
        let runtime = Runtime::new();
        let index = runtime.0.join(INDEX_FILE);
        let future = r#"{"version":99,"next_sequence":4,"segments":[]}"#;
        fs::write(&index, future).unwrap();
        let mut store = SegmentStore::open(&runtime.0);
        let (sequence, _, first) =
            store.begin(|sequence| Segment::capturing(sequence, "session-1", "capturing", &[]));
        assert!(first.is_some(), "the degradation is reported once");
        let second = store.apply(Change::Status {
            sequence,
            state: SegmentState::Processing,
            detail: "stopped".into(),
        });
        assert!(second.is_none(), "the same problem is never reported twice");
        assert_eq!(
            store.records()[0].state,
            SegmentState::Processing,
            "a degraded record still applies in memory"
        );
        assert_eq!(
            fs::read_to_string(&index).unwrap(),
            future,
            "a degraded store never writes"
        );
    }

    #[test]
    fn an_unknown_state_spelling_is_reported_and_never_written_back() {
        let runtime = Runtime::new();
        let index = runtime.0.join(INDEX_FILE);
        let stored = r#"{"version":1,"next_sequence":3,"current_sequence":1,"segments":[{"sequence":1,"directory":"session-1","state":"archived","detail":"kept exactly as stored","text_file":null,"text_chars":0,"delivery":null,"usage":null,"glossary_terms":[],"updated_utc":1.0}]}"#;
        fs::write(&index, stored).unwrap();

        let mut store = SegmentStore::open(&runtime.0);
        let problem = store
            .problem()
            .expect("an unknown state spelling must be reported");
        assert!(
            problem.contains("archived"),
            "the degradation names the exact spelling: {problem}"
        );

        // Any write path is refused while the record is degraded: `begin` asks the
        // store to persist, gets the one problem line, and the file keeps its bytes.
        let (_, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-new", "capturing", &[]));
        assert!(
            error.is_some(),
            "the degraded record must report the refusal"
        );
        assert_eq!(
            fs::read_to_string(&index).unwrap(),
            stored,
            "an unknown spelling is never guessed into `failed` and written back"
        );
    }

    #[test]
    fn an_unknown_delivery_spelling_is_reported_and_never_written_back() {
        let runtime = Runtime::new();
        let index = runtime.0.join(INDEX_FILE);
        let stored = r#"{"version":1,"next_sequence":2,"segments":[{"sequence":1,"directory":"session-1","state":"result","detail":"kept exactly as stored","text_file":null,"text_chars":0,"delivery":"maybe","usage":null,"glossary_terms":[],"updated_utc":1.0}]}"#;
        fs::write(&index, stored).unwrap();

        let mut store = SegmentStore::open(&runtime.0);
        let problem = store
            .problem()
            .expect("an unknown delivery spelling must be reported");
        assert!(
            problem.contains("maybe"),
            "the degradation names the exact spelling: {problem}"
        );

        let (_, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-new", "capturing", &[]));
        assert!(
            error.is_some(),
            "the degraded record must report the refusal"
        );
        assert_eq!(
            fs::read_to_string(&index).unwrap(),
            stored,
            "an unknown spelling is never guessed into `uncertain` and written back"
        );
    }

    #[test]
    fn a_hand_repaired_index_with_a_known_spelling_loads_and_writes_again() {
        let runtime = Runtime::new();
        let index = runtime.0.join(INDEX_FILE);
        let unknown = r#"{"version":1,"next_sequence":2,"segments":[{"sequence":1,"directory":"session-1","state":"archived","detail":"kept exactly as stored","text_file":null,"text_chars":0,"delivery":null,"usage":null,"glossary_terms":[],"updated_utc":1.0}]}"#;
        fs::write(&index, unknown).unwrap();
        assert!(
            SegmentStore::open(&runtime.0).problem().is_some(),
            "the hand edit starts from the degraded record"
        );

        // The user edits the one spelling by hand. Reopening is enough: the file is
        // the only authority, and nothing about the unknown value survives in memory.
        let repaired = unknown.replace(r#""state":"archived""#, r#""state":"failed""#);
        fs::write(&index, &repaired).unwrap();
        let mut store = SegmentStore::open(&runtime.0);
        assert!(store.problem().is_none(), "{:?}", store.problem());
        assert_eq!(store.records().len(), 1);
        assert_eq!(store.records()[0].state, SegmentState::Failed);

        store
            .apply(Change::Status {
                sequence: 1,
                state: SegmentState::Failed,
                detail: "repaired by hand".into(),
            })
            .applied();
        let written = fs::read_to_string(&index).unwrap();
        assert!(
            written.contains(r#""detail":"repaired by hand""#),
            "the repaired store writes again: {written}"
        );
        assert_eq!(
            SegmentStore::open(&runtime.0).records()[0].detail,
            "repaired by hand",
            "the write is the same durable record a restart reads"
        );
    }

    #[test]
    fn a_failed_index_write_is_reported_once_and_the_change_is_kept() {
        let runtime = Runtime::new();
        let mut store = SegmentStore::open(&runtime.0);
        let (sequence, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-1", "capturing", &[]));
        assert!(error.is_none());
        // Make the index unwritable as a file: the atomic replace cannot land, while
        // every other part of the store keeps working.
        let index = runtime.0.join(INDEX_FILE);
        fs::remove_file(&index).unwrap();
        fs::create_dir(&index).unwrap();
        let first = store.apply(Change::Status {
            sequence,
            state: SegmentState::Processing,
            detail: "stopped".into(),
        });
        assert!(first.is_some(), "a failed write is reported");
        let second = store.apply(Change::Status {
            sequence,
            state: SegmentState::Result,
            detail: "done".into(),
        });
        assert!(
            second.is_none(),
            "the same write failure is never reported twice"
        );
        assert_eq!(
            store.records()[0].state,
            SegmentState::Result,
            "the in-memory record keeps every change"
        );
    }

    #[test]
    fn an_index_from_before_the_term_snapshot_still_loads_with_an_empty_snapshot() {
        // The additive field must not make the existing version-1 index unusable.
        let runtime = Runtime::new();
        fs::write(
            runtime.0.join(INDEX_FILE),
            r#"{"version":1,"next_sequence":2,"segments":[{"sequence":1,"directory":"session-1","state":"processing","detail":"stopped","text_file":null,"text_chars":0,"delivery":null,"usage":null,"updated_utc":1.0}]}"#,
        )
        .unwrap();
        let store = SegmentStore::open(&runtime.0);
        assert!(store.problem().is_none(), "{:?}", store.problem());
        assert_eq!(store.records().len(), 1);
        assert!(store.records()[0].glossary_terms.is_empty());
        assert_eq!(store.records()[0].state, SegmentState::Processing);
        assert_eq!(store.records()[0].delivery, None);
    }

    #[test]
    fn the_stored_index_keeps_its_frozen_version_1_shape() {
        // The on-disk shape is a compatibility decision (ADR 0005): every key and
        // every value spelling below is what an older Listen reads. This literal is
        // an independent statement of the format, not a copy of the writer.
        let runtime = Runtime::new();
        let mut store = SegmentStore::open(&runtime.0);
        let (sequence, _, error) =
            store.begin(|sequence| Segment::capturing(sequence, "session-42", "capturing", &[]));
        assert!(error.is_none());
        store.write_text(sequence, "固化的结果").unwrap();
        store
            .apply(Change::Delivered {
                sequence,
                state: SegmentState::Result,
                detail: "stopped".into(),
                text: "固化的结果".into(),
                delivery: Delivery::Fallback,
            })
            .applied();
        store
            .apply(Change::Usage {
                sequence,
                usage: SegmentUsage {
                    provider: "Alibaba Model Studio".into(),
                    model: "controlled".into(),
                    region: "cn-beijing".into(),
                    requests: 2,
                    estimated_nanoyuan: None,
                    unknown: true,
                },
            })
            .applied();
        store
            .apply(Change::GlossaryTerms {
                sequence,
                terms: vec!["WASAPI".into()],
            })
            .applied();

        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(runtime.0.join(INDEX_FILE)).unwrap()).unwrap();
        // The update time is wall-clock; the format only requires the key to be a
        // number. Every other key and spelling is compared against the literal below.
        assert!(stored["segments"][0]["updated_utc"].is_f64(), "{stored}");
        stored["segments"][0]
            .as_object_mut()
            .unwrap()
            .remove("updated_utc");
        assert_eq!(
            stored,
            serde_json::json!({
                "version": 1,
                "next_sequence": 2,
                "current_sequence": 0,
                "segments": [{
                    "sequence": 1,
                    "directory": "session-42",
                    "state": "result",
                    "detail": "stopped",
                    "text_file": "segment-00000001.txt",
                    "text_chars": 5,
                    "delivery": "fallback",
                    "usage": {
                        "provider": "Alibaba Model Studio",
                        "model": "controlled",
                        "region": "cn-beijing",
                        "requests": 2,
                        "estimated_nanoyuan": null,
                        "unknown": true,
                    },
                    "glossary_terms": ["WASAPI"],
                }],
            }),
            "{stored}"
        );
    }

    #[test]
    fn every_fact_reaches_the_index_the_panel_and_a_restart_scan() {
        // The four directions of the one authoritative representation: into the
        // index, out of it, projected to the panel, and rebuilt by a restart scan.
        // A field added to `Segment` is the only production edit these assertions
        // ask for; every direction reads the same value.
        let runtime = Runtime::new();
        let mut store = SegmentStore::open(&runtime.0);
        let terms = vec!["WASAPI".to_string()];
        let (sequence, facts, _) =
            store.begin(|sequence| Segment::capturing(sequence, "session-5", "capturing", &terms));
        store.write_text(sequence, "完整结果").unwrap();
        store
            .apply(Change::Text {
                sequence,
                chars: "完整结果".chars().count(),
                current: true,
            })
            .applied();
        store
            .apply(Change::Usage {
                sequence,
                usage: usage(3),
            })
            .applied();
        store
            .apply(Change::Status {
                sequence,
                state: SegmentState::Processing,
                detail: "the result is being confirmed".into(),
            })
            .applied();
        let stored = store.records()[0].clone();
        assert_eq!(stored.sequence, facts.sequence);
        assert_ne!(stored, facts, "the change must be visible in the record");

        // Out of the index: a fresh store reads the same facts back.
        let reopened = SegmentStore::open(&runtime.0);
        let read_back = &reopened.records()[0];
        assert_eq!(read_back.sequence, stored.sequence);
        assert_eq!(read_back.directory, stored.directory);
        assert_eq!(read_back.state, stored.state);
        assert_eq!(read_back.detail, stored.detail);
        assert_eq!(read_back.text_file, stored.text_file);
        assert_eq!(read_back.text_chars, stored.text_chars);
        assert_eq!(read_back.delivery, stored.delivery);
        assert_eq!(read_back.usage, stored.usage);
        assert_eq!(read_back.glossary_terms, stored.glossary_terms);
        assert_eq!(
            read_back.text, None,
            "the text file is not read by opening the index"
        );

        // Projected to the panel.
        let summary = SegmentSummary::of(&stored);
        assert_eq!(summary.sequence, stored.sequence);
        assert_eq!(summary.state, stored.state);
        assert_eq!(summary.detail, stored.detail);
        assert_eq!(summary.text, stored.text.clone().unwrap_or_default());
        assert_eq!(summary.delivery, stored.delivery);
        assert_eq!(summary.usage, stored.usage);

        // Rebuilt from a restart scan: the facts the index records are the facts the
        // scan rebuilds (the state line is the scan's own recovery sentence).
        let plan = crate::recovery::plan(&runtime.0, reopened.records(), &BTreeMap::new());
        let recovered = &plan.segments[0];
        assert_eq!(recovered.facts.sequence, stored.sequence);
        assert_eq!(recovered.facts.directory, stored.directory);
        assert_eq!(recovered.facts.text, Some("完整结果".to_string()));
        assert_eq!(recovered.facts.text_file, stored.text_file);
        assert_eq!(recovered.facts.text_chars, stored.text_chars);
        // The scan decides the delivery from the durable delivery record: nothing is
        // proven here, so an unproven write stays uncertain instead of being read as
        // "no delivery happened".
        assert_eq!(recovered.facts.delivery, Some(Delivery::Uncertain));
        assert_eq!(stored.delivery, None);
        assert_eq!(recovered.facts.usage, stored.usage);
        assert_eq!(recovered.facts.glossary_terms, stored.glossary_terms);
    }

    #[test]
    fn removing_one_segment_never_reads_or_deletes_outside_the_runtime_directory() {
        let runtime = Runtime::new();
        fs::write(
            runtime.0.join(INDEX_FILE),
            r#"{"version":1,"next_sequence":2,"segments":[{"sequence":1,"directory":"","state":"result","detail":"x","text_file":"../outside.txt","text_chars":1,"delivery":"uncertain","usage":null,"updated_utc":1.0}]}"#,
        )
        .unwrap();
        let outside = runtime.0.parent().unwrap().join("outside.txt");
        fs::write(&outside, "must survive").unwrap();
        let mut store = SegmentStore::open(&runtime.0);
        assert!(store.problem().is_none(), "{:?}", store.problem());
        store.remove(1);
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "must survive",
            "a corrupt text pointer is never followed outside the runtime directory"
        );
        assert!(store.records().is_empty());
        let _ = fs::remove_file(&outside);
    }
}
