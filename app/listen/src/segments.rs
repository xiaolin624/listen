//! Durable per-segment state for multi-segment capture (#33).
//!
//! Every start/stop is an independent segment: its audio stays in a session
//! directory, its complete text and its state/usage are recorded separately, and a
//! later segment never overwrites an earlier one. This module owns the on-disk
//! shape only; the application decides what a segment means.
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

use crate::{AppResult, cloud::persist_result, recognition::SegmentUsage};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Bumping this is a compatibility decision: an index with any other version is
/// read as unusable and is never silently rewritten.
const STORE_VERSION: u32 = 1;
const INDEX_FILE: &str = "segments.json";
const TEXT_PREFIX: &str = "segment-";
const TEXT_SUFFIX: &str = ".txt";

/// One segment as it must survive a process restart: where its audio is, what
/// state it reached, where its complete text is, and what it may cost.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRecord {
    pub sequence: u64,
    /// The runtime session directory name that holds this segment's audio.
    pub directory: String,
    /// The stable spelling of the user-visible state (`capturing`,
    /// `waiting_for_network`, `processing`, `result`, `no_speech`, `failed`).
    pub state: String,
    /// The user-readable state line, without a credential or vendor error body.
    pub detail: String,
    /// The complete text file name inside the runtime directory, once saved.
    pub text_file: Option<String>,
    pub text_chars: usize,
    /// `appended` or `fallback` once a delivery was decided; never a guess.
    pub delivery: Option<String>,
    /// What the engine recorded for this segment's paid requests, if anything.
    pub usage: Option<SegmentUsage>,
    /// The fixed 术语表 snapshot of this capture, as canonical terms. A segment
    /// recovered after a restart is processed again with exactly these terms, not
    /// with whatever the glossary file holds at the next launch (#35). An index
    /// written before this field existed loads as an empty snapshot.
    #[serde(default)]
    pub glossary_terms: Vec<String>,
    pub updated_utc: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    next_sequence: u64,
    segments: Vec<SegmentRecord>,
}

/// The runtime-directory segment store. It is used from the application thread
/// only, so index writes are naturally serialized; workers only touch their own
/// text file through [`SegmentStore::write_text`].
pub(crate) struct SegmentStore {
    runtime: PathBuf,
    next_sequence: u64,
    segments: Vec<SegmentRecord>,
    /// Why the stored index could not be used, if it could not. The segments are
    /// still captured; only the durable record is degraded and says so.
    problem: Option<String>,
}

impl SegmentStore {
    pub fn open(runtime: &Path) -> Self {
        let mut segments = Vec::new();
        let mut next_sequence = 1;
        let mut problem = None;
        match fs::read(runtime.join(INDEX_FILE)) {
            Ok(bytes) => match serde_json::from_slice::<Index>(&bytes) {
                Ok(index) if index.version == STORE_VERSION => {
                    segments = index.segments;
                    next_sequence = index.next_sequence.max(1);
                }
                Ok(index) => {
                    problem = Some(format!(
                        "the multi-segment record uses an unsupported version ({}); new segments are still captured but their recovery record is not updated",
                        index.version
                    ));
                }
                Err(error) => {
                    problem = Some(format!(
                        "the multi-segment record could not be read ({error}); new segments are still captured but their recovery record is not updated"
                    ));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                problem = Some(format!(
                    "the multi-segment record could not be read ({error}); new segments are still captured but their recovery record is not updated"
                ));
            }
        }
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
            segments,
            problem,
        }
    }

    /// Why the durable record is degraded, if it is. Never includes a credential.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Every stored record, oldest capture order first.
    pub fn records(&self) -> &[SegmentRecord] {
        &self.segments
    }

    /// The complete-text file name for one sequence, relative to the runtime
    /// directory. Stable, sortable and independent of every other segment.
    pub fn text_name(sequence: u64) -> String {
        format!("{TEXT_PREFIX}{sequence:08}{TEXT_SUFFIX}")
    }

    /// The next capture order that no record and no saved text owns. Recovery
    /// uses it for a recording whose index entry was lost; a new capture uses it
    /// through [`SegmentStore::begin`], so neither can reuse a sequence.
    pub fn allocate(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.saturating_add(1);
        sequence
    }

    /// Allocate the next capture order and start its durable record. The sequence
    /// is returned even when the record cannot be written, so the in-memory capture
    /// is never lost to a storage problem.
    pub fn begin(
        &mut self,
        directory: &Path,
        detail: &str,
        glossary_terms: &[String],
    ) -> (u64, Option<String>) {
        let sequence = self.allocate();
        self.segments.push(SegmentRecord {
            sequence,
            directory: directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned(),
            state: "capturing".into(),
            detail: detail.into(),
            text_file: None,
            text_chars: 0,
            delivery: None,
            usage: None,
            glossary_terms: glossary_terms.to_vec(),
            updated_utc: now_utc(),
        });
        let error = self.persist().err();
        (sequence, error)
    }

    /// Add or replace one record that recovery reconstructed from files on disk.
    /// A record the index never described (an interrupted index save, a corrupt
    /// index) keeps its own stable capture order and is never merged into another
    /// segment. Returns why the index could not be written, if it could not.
    pub fn upsert(&mut self, record: SegmentRecord) -> Option<String> {
        self.next_sequence = self.next_sequence.max(record.sequence.saturating_add(1));
        match self
            .segments
            .iter_mut()
            .find(|stored| stored.sequence == record.sequence)
        {
            Some(stored) => *stored = record,
            None => self.segments.push(record),
        }
        self.persist().err()
    }

    /// The record of one capture order, if it is still stored.
    pub fn record(&self, sequence: u64) -> Option<&SegmentRecord> {
        self.segments
            .iter()
            .find(|record| record.sequence == sequence)
    }

    pub fn record_mut(&mut self, sequence: u64) -> Option<&mut SegmentRecord> {
        self.segments
            .iter_mut()
            .find(|record| record.sequence == sequence)
    }

    /// Replace the stored index with the current records.
    pub fn persist(&mut self) -> AppResult<()> {
        if let Some(problem) = &self.problem {
            return Err(problem.clone());
        }
        let index = Index {
            version: STORE_VERSION,
            next_sequence: self.next_sequence,
            segments: self.segments.clone(),
        };
        let bytes = serde_json::to_vec(&index).map_err(|error| error.to_string())?;
        persist_result(&self.runtime.join(INDEX_FILE), &bytes)
            .map_err(|error| format!("Could not save the multi-segment record: {error}"))
    }

    /// Write one segment's complete text. This is the reliable per-segment save;
    /// the caller must not release that segment's audio before it succeeds.
    pub fn write_text(&self, sequence: u64, text: &str) -> AppResult<String> {
        let name = Self::text_name(sequence);
        persist_result(&self.runtime.join(&name), text.as_bytes()).map_err(|error| {
            format!("Could not save segment {sequence}'s complete result: {error}")
        })?;
        Ok(name)
    }

    /// Forget one drained segment and its text file. Only called after the result
    /// was safely delivered (appended) or explicitly cleaned; a pending result is
    /// never removed by the application on its own.
    pub fn remove(&mut self, sequence: u64) -> AppResult<()> {
        if let Some(record) = self.record_mut(sequence) {
            if let Some(name) = record.text_file.clone() {
                let _ = fs::remove_file(self.runtime.join(name));
            }
        }
        self.segments.retain(|record| record.sequence != sequence);
        self.persist()
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
        .filter_map(|name| {
            name.strip_prefix(TEXT_PREFIX)?
                .strip_suffix(TEXT_SUFFIX)?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

/// The wall-clock time a record was last updated, for diagnostics and recovery
/// ordering. The capture sequence stays the authority for order.
pub(crate) fn now_utc() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[test]
    fn a_saved_index_keeps_capture_order_text_files_and_usage() {
        let runtime = Runtime::new();
        let mut store = SegmentStore::open(&runtime.0);
        let terms = vec!["DeepSeek Harness".to_string(), "WASAPI".to_string()];
        let (first, error) = store.begin(&runtime.0.join("session-1"), "capturing", &terms);
        assert!(error.is_none());
        let text = store.write_text(first, "第一段完整结果。").unwrap();
        let record = store.record_mut(first).unwrap();
        record.state = "result".into();
        record.text_file = Some(text.clone());
        record.text_chars = "第一段完整结果。".chars().count();
        record.delivery = Some("fallback".into());
        record.usage = Some(SegmentUsage {
            provider: "Alibaba Model Studio".into(),
            model: "controlled".into(),
            region: "cn-beijing".into(),
            requests: 2,
            estimated_nanoyuan: None,
            unknown: true,
        });
        store.persist().unwrap();
        let (second, error) = store.begin(&runtime.0.join("session-2"), "capturing", &terms);
        assert!(error.is_none());
        assert_eq!((first, second), (1, 2));

        let reopened = SegmentStore::open(&runtime.0);
        let records = reopened.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[0].state, "result");
        assert_eq!(records[0].delivery.as_deref(), Some("fallback"));
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
        let (sequence, _) = store.begin(&runtime.0.join("session-new"), "capturing", &[]);
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
        let (sequence, error) = store.begin(&runtime.0.join("session-new"), "capturing", &[]);
        assert_eq!(sequence, 1);
        assert!(error.is_some(), "the degraded record must be reported");
        assert_eq!(
            fs::read_to_string(&index).unwrap(),
            r#"{"version":99,"next_sequence":4,"segments":[]}"#,
            "a future index is not silently downgraded"
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
    }
}
