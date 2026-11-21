//! Durable delivery state and the late-backfill safety check (#34).
//!
//! A normal result is appended to the latest draft of the target bound at capture
//! start. A **late** result — one that had to wait for the network or was recovered
//! from retained audio — may only be appended when the original target still holds
//! exactly the draft recorded when that segment stopped. Anything else keeps the
//! complete text as its own pending result.
//!
//! This module owns the durable half of that decision:
//!
//! ```text
//! delivery.json
//!   version: 1
//!   segments: [{ sequence, state, baseline, baseline_problem, before, after, detail, updated_utc }]
//! ```
//!
//! * `pending`   — the stop-time draft was read; no write was decided yet.
//! * `started`   — one target write was attempted and its outcome is not known.
//! * `appended`  — a complete append was confirmed and read back; `before` and
//!   `after` are the complete target content around it.
//! * `fallback`  — deliberately nothing was written; the complete text stays pending.
//! * `uncertain` — a write may have happened but cannot be proven; never append again.
//!
//! The file is replaced atomically only after the in-memory state changed, so a
//! crash can lose the newest decision but never invent one. `started` and
//! `uncertain` are terminal for automatic appends: a lost write response, a
//! repeated stop, a repeated cloud event or a repeated poll must never append the
//! same segment twice. A target handle cannot be persisted, so this record makes no
//! promise across a restart: after a restart the recovered text is panel-only
//! (#35), and the states are kept so a restart cannot claim a delivery it cannot
//! prove.
//!
//! The whole draft is stored, not a hash, because a hash collision would turn an
//! unprovable draft into a safe-looking one. Editable drafts are small; the index
//! is replaced per decision and is not an audio store.
//!
//! **Bounded history (#38).** `before` and `after` hold the complete draft around
//! each confirmed append, so keeping every resolved segment forever would make the
//! file — and the two writes that bracket every delivery — grow with the session.
//! Resolved records are therefore pruned to a byte budget and a record count,
//! oldest first. Pruning is fail-closed: the late-append proof walks a chain of
//! `appended` records, so a missing link can only make [`LatePlan::Keep`] more
//! likely; it can never make an append look safe. `pending`, `started` and
//! `uncertain` records are never pruned — they are the safety markers that prevent
//! a second automatic write.

use crate::{AppResult, cloud::persist_result, normalize_newlines};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Bumping this is a compatibility decision: any other version is read as
/// unusable and is never silently rewritten.
const STORE_VERSION: u32 = 1;
const FILE: &str = "delivery.json";

const PENDING: &str = "pending";
const STARTED: &str = "started";
const APPENDED: &str = "appended";
const FALLBACK: &str = "fallback";
const UNCERTAIN: &str = "uncertain";

/// How long a late result waits, off the UI thread, for the draft read that was
/// started at Stop. A slow or hung editor must not delay Stop, and a draft that
/// cannot be proven unchanged must keep the result pending instead of guessing.
pub(crate) const DRAFT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounds on the resolved (`appended`/`fallback`) delivery history. Enough recent
/// confirmed appends remain to prove a backlog segment's draft, while the file and
/// the two saves around each delivery stay bounded through a long session.
const MAX_RESOLVED_RECORDS: usize = 64;
/// The newest resolved records are always kept, even under byte pressure: the most
/// recent append is the one a just-stopped segment's draft is most likely to sit on.
const MIN_RESOLVED_RECORDS: usize = 4;
/// The largest serialized `delivery.json` this module writes before pruning older
/// resolved records. Non-terminal safety records are never dropped to meet it.
/// Keeping it small matters: every delivery writes this file twice (before and
/// after the target write), so its size is a direct stop-path cost.
const MAX_BYTES: usize = 64 * 1024;

/// One segment's durable delivery state. Every field is complete content or a
/// user-readable reason; nothing here contains a credential.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sequence: u64,
    /// One of `pending`, `started`, `appended`, `fallback`, `uncertain`.
    state: String,
    /// The complete target draft read once, when this segment stopped. `None` when
    /// the read failed; a late result is then never appended.
    baseline: Option<String>,
    /// Why the stop-time draft could not be read.
    baseline_problem: Option<String>,
    /// For a confirmed append, the complete target content before and after it.
    before: Option<String>,
    after: Option<String>,
    /// What happened, in user-readable words. Never a credential or a vendor body.
    detail: String,
    updated_utc: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    segments: Vec<Record>,
}

/// What a late result may do, decided against the durable record.
pub(crate) enum LatePlan {
    /// The original target still holds exactly the stop-time draft plus the tool's
    /// own confirmed appends, so appending the complete text is safe.
    Append,
    /// The result stays its own pending result; `reason` completes the sentence
    /// "…, but {reason}, so nothing was written to the original target".
    Keep { reason: String },
}

/// The durable delivery record, shared by the application thread and every
/// delivery worker. One mutex keeps the in-memory state and the file consistent.
pub(crate) struct DeliveryLedger {
    runtime: PathBuf,
    data: Mutex<Data>,
    /// Wakes a late delivery waiting for the stop-time draft.
    changed: Condvar,
    /// A lock-free hint so the UI poll never takes the record mutex while a delivery
    /// worker is saving it.
    degraded: AtomicBool,
}

#[derive(Default)]
struct Data {
    segments: BTreeMap<u64, Record>,
    /// First durable-record problem. It is reported once and refuses automatic late
    /// appends: without the record, safety cannot be proven.
    problem: Option<String>,
}

impl DeliveryLedger {
    pub fn open(runtime: &Path) -> Self {
        let mut segments = BTreeMap::new();
        let mut problem = None;
        match fs::read(runtime.join(FILE)) {
            Ok(bytes) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) if stored.version == STORE_VERSION => {
                    for record in stored.segments {
                        if segments.insert(record.sequence, record).is_some() {
                            problem = Some(format!("{PROBLEM_PREFIX}{PROBLEM_SUFFIX}"));
                        }
                    }
                }
                Ok(stored) => {
                    problem = Some(format!(
                        "the delivery record uses an unsupported version ({}){PROBLEM_SUFFIX}",
                        stored.version
                    ));
                }
                Err(error) => {
                    problem = Some(format!(
                        "the delivery record could not be read ({error}){PROBLEM_SUFFIX}"
                    ));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                problem = Some(format!(
                    "the delivery record could not be read ({error}){PROBLEM_SUFFIX}"
                ));
            }
        }
        let degraded = problem.is_some();
        Self {
            runtime: runtime.to_path_buf(),
            data: Mutex::new(Data { segments, problem }),
            changed: Condvar::new(),
            degraded: AtomicBool::new(degraded),
        }
    }

    /// Why automatic late appends are refused, if the durable record is degraded.
    /// The lock-free flag keeps this cheap enough for every poll.
    pub fn problem(&self) -> Option<String> {
        if !self.degraded.load(Ordering::Acquire) {
            return None;
        }
        self.lock().problem.clone()
    }

    /// Record the draft read when a segment stopped. Only the first read is kept:
    /// a later read would describe a draft the user may already have edited.
    pub fn record_baseline(&self, sequence: u64, observed: AppResult<String>) {
        {
            let mut data = self.lock();
            let record = data.record(sequence);
            if record.baseline.is_some() || record.baseline_problem.is_some() {
                drop(data);
                self.changed.notify_all();
                return;
            }
            match observed {
                Ok(text) => record.baseline = Some(text),
                Err(error) => record.baseline_problem = Some(error),
            }
            record.updated_utc = now_utc();
            self.persist(&mut data);
        }
        self.changed.notify_all();
    }

    /// Record that no target was bound, so a late result fails closed at once
    /// instead of waiting for a draft read that can never succeed.
    pub fn record_no_target(&self, sequence: u64, reason: &str) {
        self.record_baseline(sequence, Err(reason.to_string()));
    }

    /// Wait off the UI thread for the stop-time draft. `Err` means the draft is
    /// unknown, so the late result must stay pending.
    pub fn wait_baseline(&self, sequence: u64) -> Result<String, String> {
        let deadline = Instant::now() + DRAFT_READ_TIMEOUT;
        let mut data = self.lock();
        loop {
            if let Some(record) = data.segments.get(&sequence) {
                if let Some(baseline) = &record.baseline {
                    return Ok(baseline.clone());
                }
                if let Some(problem) = &record.baseline_problem {
                    return Err(problem.clone());
                }
                if record.state != PENDING {
                    return Err(format!(
                        "the durable delivery record already reports '{}'",
                        record.state
                    ));
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "the draft recorded when this segment stopped could not be read within {} seconds",
                    DRAFT_READ_TIMEOUT.as_secs()
                ));
            }
            let (next, _) = self
                .changed
                .wait_timeout(data, deadline - now)
                .unwrap_or_else(|error| error.into_inner());
            data = next;
        }
    }

    /// Whether this segment's draft was already read (successfully or not). Used
    /// only to decide whether a retry still needs its one read.
    pub fn has_baseline_attempt(&self, sequence: u64) -> bool {
        self.lock()
            .segments
            .get(&sequence)
            .is_some_and(|record| record.baseline.is_some() || record.baseline_problem.is_some())
    }

    /// Decide what a late result may do. Only a durable `pending` record with a
    /// stop-time draft that is still exactly what the target holds — possibly with
    /// the tool's own confirmed appends on top — can append. Those appends are the
    /// difference between "Listen wrote here after this segment stopped" and "the
    /// user rewrote the draft"; both would otherwise look like a changed target.
    pub fn plan_late_append(&self, sequence: u64, current: &str) -> LatePlan {
        let data = self.lock();
        if let Some(problem) = &data.problem {
            return LatePlan::Keep {
                reason: problem.clone(),
            };
        }
        let Some(record) = data.segments.get(&sequence) else {
            return LatePlan::Keep {
                reason: "the durable delivery record for this segment is missing, so the original draft cannot be proven unchanged".into(),
            };
        };
        if record.state != PENDING {
            return LatePlan::Keep {
                reason: format!(
                    "this segment's delivery is already recorded as '{}', so it is never appended again",
                    record.state
                ),
            };
        }
        let Some(baseline) = &record.baseline else {
            return LatePlan::Keep {
                reason: record.baseline_problem.clone().unwrap_or_else(|| {
                    "the draft recorded when this segment stopped is unknown".into()
                }),
            };
        };
        let mut chain = normalize_newlines(baseline);
        loop {
            // Only a confirmed append whose complete "before" content is exactly
            // this chain can belong to the same target. A user edit makes the
            // chain stop, because no confirmed append starts from the edited text.
            let mut next: Option<String> = None;
            for other in data.segments.values() {
                if other.sequence == sequence || other.state != APPENDED {
                    continue;
                }
                let (Some(before), Some(after)) = (&other.before, &other.after) else {
                    continue;
                };
                if normalize_newlines(before) != chain {
                    continue;
                }
                let after = normalize_newlines(after);
                if after.chars().count() <= chain.chars().count() {
                    // An append always grows the draft; never follow a cycle.
                    continue;
                }
                match &next {
                    None => next = Some(after),
                    Some(existing) if *existing == after => {}
                    Some(_) => {
                        return LatePlan::Keep {
                            reason: "several of Listen's own confirmed appends fit this draft equally well, so the original draft cannot be proven unchanged".into(),
                        };
                    }
                }
            }
            match next {
                Some(after) => chain = after,
                None => break,
            }
        }
        if chain == normalize_newlines(current) {
            LatePlan::Append
        } else {
            LatePlan::Keep {
                reason: "the original target was changed after this segment stopped".into(),
            }
        }
    }

    /// Persist that one write started. A late result must not write when this
    /// marker cannot be saved: an unrecorded write could not be proven single.
    pub fn begin_append(&self, sequence: u64, before: &str) -> Result<(), String> {
        let mut data = self.lock();
        if let Some(problem) = &data.problem {
            return Err(problem.clone());
        }
        {
            let record = data.record(sequence);
            record.state = STARTED.into();
            record.before = Some(before.into());
            record.detail =
                "A write to the original target started; its complete result is not confirmed yet."
                    .into();
            record.updated_utc = now_utc();
        }
        let bytes = serialize_pruned(&mut data.segments);
        match persist_result(&self.runtime.join(FILE), &bytes) {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = format!(
                    "the delivery record could not be saved before writing ({error}), so the target was not written"
                );
                self.note(&mut data, reason.clone());
                Err(reason)
            }
        }
    }

    /// Record one confirmed append and the complete content around it.
    pub fn confirm_append(&self, sequence: u64, before: &str, after: &str) {
        let mut data = self.lock();
        {
            let record = data.record(sequence);
            record.state = APPENDED.into();
            record.before = Some(before.into());
            record.after = Some(after.into());
            record.detail =
                "The complete result was appended to the original target and read back in full."
                    .into();
            record.updated_utc = now_utc();
        }
        self.persist(&mut data);
    }

    /// Record that this segment stays a pending result and why. An append that was
    /// proven complete or uncertain is never downgraded.
    pub fn keep_pending(&self, sequence: u64, reason: &str) {
        let mut data = self.lock();
        let record = data.record(sequence);
        if record.state == UNCERTAIN || record.state == APPENDED {
            return;
        }
        record.state = FALLBACK.into();
        record.detail = reason.into();
        record.updated_utc = now_utc();
        self.persist(&mut data);
    }

    /// Record that a write may have happened but cannot be proven. This is
    /// terminal: the same segment is never appended again automatically.
    pub fn mark_uncertain(&self, sequence: u64, reason: &str) {
        let mut data = self.lock();
        {
            let record = data.record(sequence);
            record.state = UNCERTAIN.into();
            record.detail = reason.into();
            record.updated_utc = now_utc();
        }
        self.persist(&mut data);
    }

    /// Forget one segment after its retained audio was explicitly deleted, so no
    /// future retry can borrow a stale draft reference.
    pub fn forget(&self, sequence: u64) {
        let mut data = self.lock();
        if data.segments.remove(&sequence).is_none() {
            return;
        }
        self.persist(&mut data);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Replace the stored index with the current in-memory state. A record that
    /// could not be read is never rewritten: a future or corrupt file stays intact
    /// and the in-memory state is advisory only.
    fn persist(&self, data: &mut Data) {
        if data.problem.is_some() {
            return;
        }
        let bytes = serialize_pruned(&mut data.segments);
        if let Err(error) = persist_result(&self.runtime.join(FILE), &bytes) {
            self.note(
                data,
                format!("the delivery record could not be saved ({error})"),
            );
        }
    }

    fn note(&self, data: &mut Data, problem: String) {
        if data.problem.is_none() {
            data.problem = Some(problem);
        }
        self.degraded.store(true, Ordering::Release);
    }
}

impl Data {
    /// The record for one capture order, created empty when absent. Every state
    /// change is written under the same ledger mutex.
    fn record(&mut self, sequence: u64) -> &mut Record {
        self.segments.entry(sequence).or_insert_with(|| Record {
            sequence,
            state: PENDING.into(),
            baseline: None,
            baseline_problem: None,
            before: None,
            after: None,
            detail: String::new(),
            updated_utc: now_utc(),
        })
    }
}

const PROBLEM_PREFIX: &str = "the delivery record contains two entries for one segment";
const PROBLEM_SUFFIX: &str =
    "; recovered results will not be appended to their original targets automatically";

/// The stored representation, in capture order, so a human can read it.
fn serialize(segments: &BTreeMap<u64, Record>) -> Vec<u8> {
    let stored = Stored {
        version: STORE_VERSION,
        segments: segments.values().cloned().collect(),
    };
    serde_json::to_vec(&stored).unwrap_or_default()
}

/// Serialize while keeping the resolved history bounded. The common small case is
/// one serialization; pruning starts only when the count or the byte budget is
/// exceeded.
fn serialize_pruned(segments: &mut BTreeMap<u64, Record>) -> Vec<u8> {
    let resolved = resolved_sequences(segments);
    if resolved.len() <= MAX_RESOLVED_RECORDS {
        let bytes = serialize(segments);
        if bytes.len() <= MAX_BYTES {
            return bytes;
        }
    }
    prune(segments, &resolved);
    serialize(segments)
}

/// The capture orders of resolved records, oldest first (a `BTreeMap` iterates in
/// key order). Non-terminal records are never candidates.
fn resolved_sequences(segments: &BTreeMap<u64, Record>) -> Vec<u64> {
    segments
        .iter()
        .filter(|(_, record)| is_resolved(&record.state))
        .map(|(sequence, _)| *sequence)
        .collect()
}

/// A resolved record is history: it proves something about a past delivery but can
/// be dropped without ever permitting another write. Fail-closed pruning relies on
/// this split, not on the sequence number.
fn is_resolved(state: &str) -> bool {
    state == APPENDED || state == FALLBACK
}

/// Drop the oldest resolved records until the count and byte bounds hold. At least
/// [`MIN_RESOLVED_RECORDS`] of the newest resolved records always stay, so the
/// immediately preceding append is still available to prove a draft even when the
/// drafts are large. A file that is still over budget because of non-terminal
/// records is left that way: those records are the duplicate-append guards.
fn prune(segments: &mut BTreeMap<u64, Record>, resolved: &[u64]) {
    let excess = resolved.len().saturating_sub(MAX_RESOLVED_RECORDS);
    for sequence in &resolved[..excess] {
        segments.remove(sequence);
    }
    // Drop the oldest resolved records in halves rather than one at a time: a
    // serialization per removal would itself become the stop-path cost. At least
    // MIN_RESOLVED_RECORDS of the newest are always kept, so the immediately
    // preceding append can still prove a draft.
    loop {
        if serialize(segments).len() <= MAX_BYTES {
            break;
        }
        let remaining = resolved_sequences(segments);
        if remaining.len() <= MIN_RESOLVED_RECORDS {
            break;
        }
        let droppable = remaining.len() - MIN_RESOLVED_RECORDS;
        let count = (droppable / 2).max(1).min(droppable);
        for sequence in &remaining[..count] {
            segments.remove(sequence);
        }
    }
}

fn now_utc() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}
