//! The one target write protocol.
//!
//! A result reaches an editor through one of three rules: a normal delivery
//! appends to the latest draft, a late delivery appends only to a draft proven
//! unchanged since its Stop, and a refine replaces the transcript an earlier
//! append left behind. The rules differ in the proof they demand before the write
//! and in the wording of their outcome; the write itself is the same protocol
//! everywhere:
//!
//! ```text
//! lock the one writer -> read the draft -> persist the declared marker ->
//! replace(previous, intended) -> read the target back -> record the outcome
//! ```
//!
//! This module owns all of it: the process-wide writer lock, the draft read, the
//! one append value (`draft + separator + text`), the read-back, the durable
//! `started`/`appended` markers and the classification of a failed write. A
//! caller only declares what it needs from the durable delivery record
//! ([`WriteRule`]) and turns [`Written`] into its own user-visible outcome.
//!
//! ADR 0005 (`docs/adr/0005-durable-segment-and-delivery-records.md`) applies on
//! every rule: the `started` marker is persisted before the target is written and
//! the confirmed append only after the read-back. A write whose `started` marker
//! cannot be persisted is refused when the rule requires that marker (late and
//! refined writes), because an unrecorded write could never be proven single
//! after a lost response. A normal delivery keeps its frozen best-effort rule: a
//! degraded delivery record is reported at startup, and refusing a normal append
//! for it is a product decision this module does not take.

use crate::{
    AppResult, Delivery, Target, delivery::DeliveryLedger, elapsed_ms, normalize_newlines,
};
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

/// Only one delivery may write to a target at a time. Segments finish at their own
/// pace, but one editor's read/modify/write pair must never interleave with another
/// segment's, or a confirmed append would be misread as a user edit. The lock is
/// taken on a delivery worker's thread, never on the application thread, so a slow
/// editor can never block a new capture.
static TARGET_WRITER: Mutex<()> = Mutex::new(());

/// The one separator between an existing draft and the text appended after it.
const SEPARATOR: &str = "\n";

/// The separator one append inserts before its text. An empty draft gets none, so
/// the delivered text never starts with a blank line.
pub(crate) fn separator(before: &str) -> &'static str {
    if before.is_empty() { "" } else { SEPARATOR }
}

/// The complete value one append writes: `before`, one separator, then `text`.
/// This is the only construction of an appended value.
pub(crate) fn append_value(before: &str, text: &str) -> String {
    if before.is_empty() {
        text.to_string()
    } else {
        format!("{before}{SEPARATOR}{text}")
    }
}

/// The text appended to `before` when `value` is exactly one append. The paste
/// fallback recovers the transcript with this, so building and taking apart an
/// appended value can never disagree about the separator.
pub(crate) fn appended_part<'a>(before: &str, value: &'a str) -> Option<&'a str> {
    if before.is_empty() {
        return Some(value);
    }
    value.strip_prefix(before)?.strip_prefix(SEPARATOR)
}

/// Whether the marker that says "one write started" must be durable before the
/// target may be written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StartMarker {
    /// Persist `started` when the record allows it and write anyway. Frozen normal
    /// delivery rule: a degraded delivery record is reported elsewhere and cannot
    /// prove anything either way, but it must not block a first automatic append.
    IfPossible,
    /// Refuse the write when `started` cannot be persisted. A late or refined write
    /// that is not marked could never be proven single after a lost response.
    Required,
}

/// What proves one write complete.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    /// The adapter's success, which its contract already defines as "confirmed by
    /// reading the complete replacement back" ([`Target::replace`]).
    Adapter,
    /// One explicit read-back of the complete replacement. A refine replaces text
    /// the user can see, so it proves the final content itself.
    ReadBack,
}

/// What the durable record must say when the target proves that the write left
/// nothing behind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unproven {
    /// The result is still this segment's pending result: record `fallback`.
    KeepPending,
    /// The write only tried to replace an earlier confirmed append: record that
    /// earlier append again, so a restart still reports it as confirmed instead of
    /// claiming a panel-only fallback for a result that did reach the target.
    RestoreAppend,
}

/// Everything one caller declares about a write. The rule is the whole difference
/// between the three call sites: which durable markers they need, what confirms the
/// write, and what an unproven write leaves in the record.
#[derive(Clone, Copy)]
pub(crate) struct WriteRule {
    start: StartMarker,
    confirm: Confirmation,
    unproven: Unproven,
}

impl WriteRule {
    /// A normal result: the latest draft is read at delivery time and appended to.
    pub(crate) fn delivery() -> Self {
        Self {
            start: StartMarker::IfPossible,
            confirm: Confirmation::Adapter,
            unproven: Unproven::KeepPending,
        }
    }

    /// A late result: the append is legal only against a draft that was proven
    /// unchanged, and it must be marked before it is written.
    pub(crate) fn late_delivery() -> Self {
        Self {
            start: StartMarker::Required,
            confirm: Confirmation::Adapter,
            unproven: Unproven::KeepPending,
        }
    }

    /// A refine: replace the transcript of an earlier confirmed append with the
    /// refined text, and prove the replacement by reading it back.
    pub(crate) fn refine() -> Self {
        Self {
            start: StartMarker::Required,
            confirm: Confirmation::ReadBack,
            unproven: Unproven::RestoreAppend,
        }
    }
}

/// The appended value one write stores: `base`, one separator, then `text`. A
/// normal append uses the draft read by [`TargetWriteSession::open`] as `base`; a
/// refine uses the draft its earlier append sat on, so only the transcript is
/// replaced and the user's own content is rebuilt exactly as it was.
pub(crate) struct AppendValue<'a> {
    pub(crate) base: &'a str,
    pub(crate) text: &'a str,
}

/// What one write attempt leaves for the caller. The timings are the protocol's own
/// measurements, so a caller never re-measures the lock, the read or the save.
pub(crate) struct WriteAttempt {
    pub(crate) outcome: Written,
    /// Time spent in the adapter's one write attempt; absent when nothing was
    /// written to the target.
    pub(crate) write_ms: Option<u64>,
    /// Time spent saving the durable markers around this write.
    pub(crate) save_ms: Option<u64>,
}

/// The classified result of one write attempt.
pub(crate) enum Written {
    /// The write is complete and confirmed. `before` and `after` are the complete
    /// target content around it, which a later refine has to protect.
    Confirmed { before: String, after: String },
    /// The rule required a durable marker that could not be persisted. Nothing was
    /// written to the target and the caller must not retry it.
    Refused { reason: String },
    /// The write was attempted and no complete write can be claimed. `delivery` is
    /// [`Delivery::Fallback`] only when a read-back proved the target still holds
    /// its previous content; `detail` always says what the target actually holds.
    Unconfirmed { delivery: Delivery, detail: String },
}

/// One target write session: the writer lock is held from [`open`] to the drop of
/// this value, so the draft read here is still the draft the write replaces.
///
/// [`open`]: TargetWriteSession::open
pub(crate) struct TargetWriteSession {
    _writer: MutexGuard<'static, ()>,
    target: Arc<Mutex<Box<dyn Target>>>,
    /// The complete draft read immediately after the lock. It is the content the
    /// one write replaces and the "before" content of the recorded append.
    previous: String,
    lock_ms: u64,
    read_ms: u64,
}

impl TargetWriteSession {
    /// Take the one writer lock and read the draft the write will replace.
    pub(crate) fn open(target: Arc<Mutex<Box<dyn Target>>>) -> AppResult<Self> {
        let lock_started = Instant::now();
        let writer = TARGET_WRITER
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let lock_ms = elapsed_ms(lock_started);
        let read_started = Instant::now();
        let previous = read(&target)?;
        let read_ms = elapsed_ms(read_started);
        Ok(Self {
            _writer: writer,
            target,
            previous,
            lock_ms,
            read_ms,
        })
    }

    /// The complete draft read when this session opened.
    pub(crate) fn draft(&self) -> &str {
        &self.previous
    }

    pub(crate) fn lock_ms(&self) -> u64 {
        self.lock_ms
    }

    pub(crate) fn read_ms(&self) -> u64 {
        self.read_ms
    }

    /// One write: persist the rule's marker, replace the draft read by [`open`],
    /// read the target back, record the outcome and classify anything that is not
    /// a confirmed complete write. The write is never repeated.
    ///
    /// [`open`]: TargetWriteSession::open
    pub(crate) fn write(
        &self,
        append: AppendValue<'_>,
        ledger: &DeliveryLedger,
        sequence: u64,
        rule: WriteRule,
    ) -> WriteAttempt {
        let intended = append_value(append.base, append.text);
        // The fail-closed marker must be on disk before the write, and the confirmed
        // append on disk before the outcome is trusted. Both are measured: this is the
        // part of a write that grows with confirmed history.
        let save_started = Instant::now();
        let begin = ledger.begin_append(sequence, &self.previous);
        let begin_ms = elapsed_ms(save_started);
        if let Err(reason) = begin
            && rule.start == StartMarker::Required
        {
            return WriteAttempt {
                outcome: Written::Refused { reason },
                write_ms: None,
                save_ms: None,
            };
        }
        let write_started = Instant::now();
        let written = lock(&self.target).replace(&self.previous, &intended);
        let write_ms = elapsed_ms(write_started);
        // Exactly one read-back. A delivery whose adapter confirmed the complete
        // replacement needs none (its contract already read it back); every other
        // write reads the target once, and that same reading both confirms a refine
        // and diagnoses every failure.
        let readback = match &written {
            Ok(()) if rule.confirm == Confirmation::Adapter => None,
            _ => Some(read(&self.target)),
        };
        let confirmed = written.is_ok()
            && match &readback {
                None => true,
                Some(Ok(value)) => normalize_newlines(value) == normalize_newlines(&intended),
                Some(Err(_)) => false,
            };
        if confirmed {
            let save_started = Instant::now();
            ledger.confirm_append(sequence, &self.previous, &intended);
            let confirm_ms = elapsed_ms(save_started);
            return WriteAttempt {
                outcome: Written::Confirmed {
                    before: self.previous.clone(),
                    after: intended,
                },
                write_ms: Some(write_ms),
                save_ms: Some(begin_ms + confirm_ms),
            };
        }
        let observed = readback.expect("an unconfirmed write is always read back");
        let error = match written {
            Err(error) => error,
            // Only a read-back rule can get here with a successful adapter call.
            Ok(()) => "the target did not confirm the complete replacement".to_string(),
        };
        let failure = failed_write(&self.previous, &intended, &error, observed);
        if failure.uncertain {
            ledger.mark_uncertain(sequence, &failure.detail);
        } else {
            match rule.unproven {
                Unproven::KeepPending => ledger.keep_pending(sequence, &failure.detail),
                Unproven::RestoreAppend => {
                    ledger.confirm_append(sequence, append.base, &self.previous)
                }
            }
        }
        WriteAttempt {
            outcome: Written::Unconfirmed {
                delivery: failure.delivery,
                detail: failure.detail,
            },
            write_ms: Some(write_ms),
            save_ms: Some(begin_ms),
        }
    }
}

/// Read the live draft through the bound adapter. The target mutex is held only for
/// the length of one adapter call.
fn read(target: &Arc<Mutex<Box<dyn Target>>>) -> AppResult<String> {
    lock(target).content()
}

fn lock(target: &Arc<Mutex<Box<dyn Target>>>) -> MutexGuard<'_, Box<dyn Target>> {
    target.lock().unwrap_or_else(|error| error.into_inner())
}

/// One failed write's honest report, including whether anything about it is still
/// uncertain. `uncertain` is true whenever the target may hold a complete or partial
/// write: the segment is then never written again automatically.
struct FailedWrite {
    delivery: Delivery,
    detail: String,
    uncertain: bool,
}

/// Reports what the target actually holds after a failed write, without ever claiming
/// a complete delivery. The read-back distinguishes a refused write from a truncated
/// one, a late complete write, and a possibly partial one. A write that may have
/// landed is reported as [`Delivery::Uncertain`]; only a read-back proving the target
/// still holds its previous content may claim [`Delivery::Fallback`].
fn failed_write(
    previous: &str,
    intended: &str,
    error: &str,
    observed: AppResult<String>,
) -> FailedWrite {
    let advice = "Check the original target before copying; output is not retried automatically and the original text is not reverted.";
    let fallback = "The complete result is in the expanded result panel.";
    let (detail, uncertain) = match observed {
        Err(read_error) => (
            format!(
                "The write to the original target could not be confirmed ({error}); the target could not be read back ({read_error}). {advice} {fallback}"
            ),
            true,
        ),
        Ok(observed) => {
            // Line endings are not part of the delivered text's meaning.
            let observed = normalize_newlines(&observed);
            let previous = normalize_newlines(previous);
            let intended = normalize_newlines(intended);
            if observed == previous {
                (
                    format!(
                        "The original target still holds its previous content, so nothing was written to it ({error}). {fallback} Review and copy it to the intended input."
                    ),
                    false,
                )
            } else if observed == intended {
                (
                    format!(
                        "The write to the original target could not be confirmed ({error}), but it now contains the complete appended text. {advice} A copy remains in Listen."
                    ),
                    true,
                )
            } else if !observed.is_empty() && intended.starts_with(&observed) {
                (
                    format!(
                        "The original target holds only {} of {} characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. {advice} {fallback}",
                        observed.chars().count(),
                        intended.chars().count()
                    ),
                    true,
                )
            } else {
                (
                    format!(
                        "The write to the original target could not be confirmed ({error}); the target content changed and may be partially written. {advice} {fallback}"
                    ),
                    true,
                )
            }
        }
    };
    FailedWrite {
        delivery: if uncertain {
            Delivery::Uncertain
        } else {
            Delivery::Fallback
        },
        detail,
        uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_append_value_has_one_separator_and_round_trips() {
        assert_eq!(append_value("", "轉寫"), "轉寫");
        assert_eq!(append_value("草稿", "轉寫"), "草稿\n轉寫");
        assert_eq!(separator(""), "");
        assert_eq!(separator("草稿"), "\n");
        for (before, text) in [("", "轉寫"), ("草稿", "轉寫"), ("第一行\r\n第二行", "轉寫")]
        {
            let value = append_value(before, text);
            assert_eq!(
                appended_part(before, &value),
                Some(text),
                "the appended part must come back out of the same value"
            );
        }
        // A value that is not one append on this draft is never guessed apart.
        assert_eq!(appended_part("草稿", "别的草稿\n轉寫"), None);
        assert_eq!(appended_part("草稿", "草稿轉寫"), None);
    }
}
