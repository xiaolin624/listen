//! One segment state transition, and the one place that decides what it means.
//!
//! A transition is a value: which segment ([`Transition::sequence`]), what fact it
//! becomes ([`Transition::state`] and the flags beside it) and why
//! ([`Transition::cause`]). `Application::apply` is the only place that applies one:
//! it derives `phase`, `recognition`, `detail`, `segments_dirty` and the durable record
//! from the transition. Whether a segment is the newest — the one allowed to drive the
//! global snapshot — is judged there once and passed in, never recomputed by a caller.
//!
//! [`decide`] holds the frozen rules as a pure function over plain inputs: which
//! sentence replaces another, when a stopped segment keeps its line, when a stable
//! sentence must not be downgraded, and whether the record is written or removed. It
//! needs no device, no runtime directory and no poll loop, so the segment state machine
//! can be driven directly.

use crate::{Delivery, Phase, SegmentState, recognition::SessionState};
use std::path::{Path, PathBuf};

/// The two sentences one transition produces: the segment's own durable status line
/// and the line the user reads in the capsule and the result panel. `None` means the
/// transition does not change that line at all (progress that only moves recognition
/// state, or a local-file problem whose snapshot line belongs to the operation-level
/// failure).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lines {
    pub segment: Option<String>,
    pub snapshot: Option<String>,
}

/// Where the sentence material comes from, gathered once by the application point.
#[derive(Clone, Copy, Default)]
pub(crate) struct Notes<'a> {
    /// The recovery fact recorded when this segment came back after a restart. It is
    /// kept on the segment's own line and is never erased by a later refresh.
    pub recovery: Option<&'a str>,
    /// Why the storage bounds stopped this capture, kept beside progress detail so a
    /// later refresh cannot turn it back into a normal capture message.
    pub storage: Option<&'a str>,
    /// The 术语表 note. Only the snapshot line carries it: the durable record never
    /// stores a display note.
    pub glossary: Option<&'a str>,
}

/// A partly written result kept with an interrupted capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Partial {
    pub chars: usize,
    /// Where the sentence says the incomplete text is stored.
    pub path: PathBuf,
}

/// Why a transition happened. Each variant owns the sentence rules for its cause.
#[derive(Clone, Debug)]
pub(crate) enum Cause<'a> {
    /// A live recognition adapter reported its own progress.
    Progress {
        detail: &'a str,
        session: SessionState,
    },
    /// A stable sentence was recognized: progress, never a deliverable result.
    Stable,
    /// The adapter reported its terminal fact; the result is being saved.
    Completed,
    /// The transition only moves the segment's own bookkeeping: the sentence the user
    /// reads is already correct (a recovered recording handed back to the adapter
    /// keeps its own recovery sentence).
    Silent,
    /// The segment was stopped (by the user, by the storage bounds, or for a retry)
    /// and its whole result is still being confirmed.
    Stopped { detail: &'a str },
    /// A complete result exists and was handed to the target (or could not be).
    Delivered {
        /// The delivery outcome's own sentence, as the delivery layer wrote it.
        detail: &'a str,
        delivery: Delivery,
        description: &'a str,
        /// The complete text, carried into the durable record.
        text: &'a str,
    },
    /// Silence: no result and no delivery.
    NoSpeech { description: &'a str },
    /// No complete result, and why.
    Failed {
        error: &'a str,
        /// The audio kept on this machine, listed for local recovery.
        retained: &'a [PathBuf],
        partial: Option<Partial>,
        /// Retry advice, or the fact that kept audio is not retried automatically.
        advice: Option<&'a str>,
        /// A recovery summary appended to the snapshot line only.
        summary: Option<&'a str>,
    },
    /// The segment's own line was restated by an explicit local action (a cleanup, a
    /// delete, a failed resume). It is not re-derived and it does not move the phase.
    Restated { line: &'a str },
    /// A complete result was kept, but a local file could not be removed. Only the
    /// segment's own line changes; the operation-level failure owns the snapshot line.
    CleanupFailed { previous: &'a str, error: &'a str },
}

impl Cause<'_> {
    /// The two sentences, composed in the documented order.
    pub(crate) fn lines(&self, notes: &Notes<'_>) -> Lines {
        match self {
            Self::Progress { detail, .. } => {
                let mut segment = (*detail).to_string();
                if let Some(note) = notes.storage {
                    segment.push('\n');
                    segment.push_str(note);
                }
                if let Some(note) = notes.recovery {
                    segment.push('\n');
                    segment.push_str(note);
                }
                let snapshot = match notes.glossary {
                    Some(note) => format!("{segment}\n{note}"),
                    None => segment.clone(),
                };
                Lines {
                    segment: Some(segment),
                    snapshot: Some(snapshot),
                }
            }
            Self::Stable | Self::Completed | Self::Silent => Lines::default(),
            Self::Stopped { detail } => Lines {
                segment: Some((*detail).to_string()),
                snapshot: Some((*detail).to_string()),
            },
            Self::Delivered {
                detail,
                description,
                ..
            } => Lines {
                segment: Some(append(format!("{detail}"), notes.recovery)),
                snapshot: Some(append(
                    format!("{detail}\nPlayback: {description}"),
                    notes.glossary,
                )),
            },
            Self::NoSpeech { description } => Lines {
                segment: Some(append(
                    format!(
                        "No valid speech detected; nothing was delivered.\nPlayback: {description}"
                    ),
                    notes.recovery,
                )),
                snapshot: Some(format!(
                    "No valid speech detected; nothing was delivered. The previous current result is unchanged.\nPlayback: {description}"
                )),
            },
            Self::Failed {
                error,
                retained,
                partial,
                advice,
                summary,
            } => {
                let base = failure_detail(error, retained);
                let segment = match partial {
                    Some(partial) => format!(
                        "{base}. The capture was interrupted, so this text is incomplete and kept with this segment: {}. This is not a completed transcription.",
                        partial.chars
                    ),
                    None => base.clone(),
                };
                // The recovery fact is stored with the segment and is never erased; the
                // advice and summary that follow it are appended verbatim by the caller,
                // so the existing sentence shape does not move.
                let mut segment = append(segment, notes.recovery);
                if let Some(advice) = *advice {
                    segment.push_str(advice);
                }
                let mut snapshot = match partial {
                    Some(partial) => format!(
                        "{base}. The capture was interrupted, so this text is incomplete: it is shown in the expanded result panel and stored at {}. This is not a completed transcription.",
                        partial.path.display()
                    ),
                    None => base,
                };
                if let Some(advice) = *advice {
                    snapshot.push_str(advice);
                }
                if let Some(summary) = *summary {
                    snapshot.push_str(summary);
                }
                Lines {
                    segment: Some(segment),
                    snapshot: Some(snapshot),
                }
            }
            Self::Restated { line } => Lines {
                segment: Some((*line).to_string()),
                snapshot: Some((*line).to_string()),
            },
            Self::CleanupFailed { previous, error } => Lines {
                segment: Some(format!(
                    "{previous}\nThe temporary audio for this segment could not be cleaned ({error}); the result is still complete."
                )),
                snapshot: None,
            },
        }
    }
}

impl Cause<'_> {
    /// What the global snapshot may show for the phase and the recognition progress.
    fn standing(&self) -> Standing {
        match self {
            Self::Progress { session, .. } => {
                // The durable state is mapped separately; here only the reading moves.
                Standing::Session(*session)
            }
            Self::Stable => Standing::Recognition(SessionState::Stable),
            Self::Completed => Standing::Recognition(SessionState::Complete),
            Self::Silent => Standing::Unchanged,
            Self::Stopped { .. } => Standing::Fixed(Phase::Processing, SessionState::AwaitingFinal),
            Self::Delivered { .. } => Standing::Fixed(Phase::Result, SessionState::Complete),
            Self::NoSpeech { .. } => Standing::Fixed(Phase::NoSpeech, SessionState::Complete),
            Self::Failed { .. } => Standing::Fixed(Phase::Failure, SessionState::Failed),
            Self::Restated { .. } | Self::CleanupFailed { .. } => Standing::Unchanged,
        }
    }
}

/// Append a note to a line, keeping the newline convention: a note always starts on
/// its own line.
fn append(mut line: String, note: Option<&str>) -> String {
    if let Some(note) = note {
        line.push('\n');
        line.push_str(note);
    }
    line
}

/// What the global snapshot may show for the phase and the recognition progress.
enum Standing {
    /// Leave both alone.
    Unchanged,
    /// A session-reported state: the phase moves to processing on a stop; the
    /// recognition state is mirrored unless it would downgrade a stable sentence.
    Session(SessionState),
    /// Mirror the reported session state, but do not move the phase.
    Recognition(SessionState),
    /// A terminal fact: this phase and this recognition state.
    Fixed(Phase, SessionState),
}

/// What the caller asks the durable record to do, before the transition rules run.
///
/// This is an **input**, not the write itself: [`decide`] turns it into a [`Record`],
/// the only value `Application::apply` executes. Keeping the two stages as separate
/// types is what keeps `Status` and `StatusIfChanged` from becoming two names for the
/// same write at the application point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RecordRequest {
    /// Nothing durable changed.
    #[default]
    Keep,
    /// Write the state line unconditionally (its detail may have moved while the
    /// durable state itself did not).
    Status,
    /// Write the state line only when the durable state itself changed. A live
    /// adapter's progress tick is the common case: its sentence moves every tick and
    /// must not rewrite `segments.json` for it.
    StatusIfChanged,
    /// Write the complete delivered fact (text, delivery and the line).
    Delivered,
    /// The segment has nothing pending any more; drop its record.
    Remove,
}

/// What the one application point actually does with the durable record.
///
/// This is the **output** of [`decide`]: the conditional request was resolved into
/// one of these four writes, so `Application::apply` has one arm per real write and
/// no two variants that do the same thing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Record {
    /// Nothing durable changed.
    #[default]
    Keep,
    /// Write the state line.
    Status,
    /// Write the complete delivered fact (text, delivery and the line).
    Delivered,
    /// The segment has nothing pending any more; drop its record.
    Remove,
}

/// One transition: what the segment becomes, why, and what that means durably.
#[derive(Clone, Debug)]
pub(crate) struct Transition<'a> {
    pub sequence: u64,
    /// The durable state the segment becomes; `None` keeps it.
    pub state: Option<SegmentState>,
    /// The session-side mirror of the same transition; `None` keeps it.
    pub session: Option<SessionState>,
    /// Explicit flag changes applied with the same transition.
    pub terminal: Option<bool>,
    pub stopped: Option<bool>,
    pub finalizing: Option<bool>,
    pub cause: Cause<'a>,
    /// What this transition asks the durable record to do; [`decide`] resolves it
    /// into a [`Record`] using the frozen rules.
    pub record: RecordRequest,
    /// The user asked for a 术语表 reload and it changed in this tick. A progress tick
    /// may then refresh a stopped segment's sentence.
    pub preparation_changed: bool,
}

/// The frozen inputs the rules need besides the transition itself.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Context {
    /// The segment is stopped after this transition, so its sentence is being held
    /// stable.
    pub stopped: bool,
    /// The segment was already stopped before this transition. Only the transition
    /// into the stop itself may move the phase.
    pub was_stopped: bool,
    /// The durable state changed with this transition.
    pub state_changed: bool,
    /// What the snapshot showed as recognition progress before this transition.
    pub snapshot_recognition: Option<SessionState>,
}

/// What the one application point should do. `None` means "leave it as it is".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Decision {
    pub phase: Option<Phase>,
    pub recognition: Option<SessionState>,
    /// The replacement snapshot sentence, if the current one must change.
    pub detail: Option<String>,
    pub record: Record,
    /// Whether the durable state moved, so the panel projection is now stale.
    pub state_changed: bool,
}

/// The durable state a session-reported status implies, keeping a terminal fact
/// terminal: a session reports `Complete`/`Failed` once its terminal event is queued,
/// and the segment becomes a result (or a failure) when that event is applied.
pub(crate) fn state_for_status(current: SegmentState, session: SessionState) -> SegmentState {
    if matches!(
        current,
        SegmentState::Result | SegmentState::NoSpeech | SegmentState::Failed
    ) {
        return current;
    }
    match session {
        SessionState::WaitingForNetwork => SegmentState::WaitingForNetwork,
        SessionState::Capturing | SessionState::Stable => SegmentState::Capturing,
        SessionState::Preparing
        | SessionState::AwaitingFinal
        | SessionState::Complete
        | SessionState::Failed => SegmentState::Processing,
    }
}

/// Apply the frozen rules to one transition.
pub(crate) fn decide(transition: &Transition<'_>, lines: &Lines, context: &Context) -> Decision {
    let mut decision = Decision {
        record: match transition.record {
            RecordRequest::StatusIfChanged if !context.state_changed => Record::Keep,
            RecordRequest::StatusIfChanged | RecordRequest::Status => Record::Status,
            RecordRequest::Keep => Record::Keep,
            RecordRequest::Delivered => Record::Delivered,
            RecordRequest::Remove => Record::Remove,
        },
        state_changed: context.state_changed,
        ..Decision::default()
    };
    match transition.cause.standing() {
        Standing::Unchanged => {
            decision.detail = lines.snapshot.clone();
        }
        Standing::Session(session) => {
            // A transition into waiting-for-network must be visible even after the
            // stop, when the detail is otherwise held stable. A normal stop keeps the
            // last capture detail, which carries the backlog/interruption note.
            let changed = context.snapshot_recognition != Some(session);
            let waiting_now = session == SessionState::WaitingForNetwork;
            if !context.stopped || transition.preparation_changed || (waiting_now && changed) {
                decision.detail = lines.snapshot.clone();
            }
            // A stable sentence stays observable until the next session event: a
            // status refresh must not turn it into a completed result. A waiting or
            // failed state still supersedes it.
            let keep_stable = context.snapshot_recognition == Some(SessionState::Stable)
                && !context.stopped
                && session == SessionState::Capturing;
            if !keep_stable {
                decision.recognition = Some(session);
            }
            // The live capture only moves to processing when the adapter reports that
            // it is waiting for the final confirmation.
            if session == SessionState::AwaitingFinal && !context.was_stopped {
                decision.phase = Some(Phase::Processing);
            }
        }
        Standing::Recognition(session) => {
            // The adapter reported a terminal fact or a stable sentence. The stable
            // case is only ever applied while the segment is not stopped (its event is
            // guarded); a completed result is reached from a stopped segment.
            decision.recognition = Some(session);
        }
        Standing::Fixed(phase, session) => {
            decision.phase = Some(phase);
            decision.recognition = Some(session);
            decision.detail = lines.snapshot.clone();
        }
    }
    decision
}

/// One failure message with the retained audio paths, never a credential.
pub(crate) fn failure_detail(error: &str, retained: &[PathBuf]) -> String {
    let error = error.trim_end_matches('.').to_string();
    if retained.is_empty() {
        error
    } else {
        format!(
            "{error}. Audio retained for local recovery: {}",
            retained
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// The segment's own line when silence could not be cleaned up. Nothing was delivered
/// and the previous current result is kept either way.
pub(crate) fn no_speech_cleanup_line(error: &str) -> String {
    format!(
        "No valid speech detected, but this segment's temporary audio could not be cleaned: {error}"
    )
}

/// Where one segment's temporary audio lives, for the failure sentence. A path that
/// cannot be named is reported as the runtime directory itself, exactly like the
/// failure sentence before this module existed.
pub(crate) fn partial_result_path(directory: &Path) -> PathBuf {
    directory.join("partial-result.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered_lines() -> Lines {
        Cause::Delivered {
            detail: "The complete result was appended to the original target.",
            delivery: Delivery::Appended,
            description: "test playback",
            text: "第一行",
        }
        .lines(&Notes::default())
    }

    /// The narrow seam: only the newest segment's transition may move `phase`,
    /// `recognition` and `detail`. `primary` is an input, judged once by the application
    /// point — a caller cannot recompute it here.
    #[test]
    fn a_transition_decision_is_independent_of_whether_the_segment_is_newest() {
        let transition = Transition {
            sequence: 3,
            state: Some(SegmentState::Result),
            session: Some(SessionState::Complete),
            terminal: None,
            stopped: None,
            finalizing: None,
            cause: Cause::Delivered {
                detail: "The complete result was appended to the original target.",
                delivery: Delivery::Appended,
                description: "test playback",
                text: "第一行",
            },
            preparation_changed: false,
            record: RecordRequest::Remove,
        };
        let lines = delivered_lines();
        let decision = decide(&transition, &lines, &Context::default());
        assert_eq!(decision.phase, Some(Phase::Result));
        assert_eq!(decision.recognition, Some(SessionState::Complete));
        assert!(
            decision
                .detail
                .unwrap()
                .contains("\nPlayback: test playback")
        );
        assert_eq!(decision.record, Record::Remove);
    }

    /// A stored failed segment's line is kept while a later progress tick arrives, but
    /// a transition into waiting-for-network is still shown: the user must learn that
    /// the stop is a network wait, not a normal end.
    #[test]
    fn a_stopped_segments_progress_tick_keeps_its_line_until_the_network_wait_changes() {
        let progress = |session| Cause::Progress {
            detail: "reconnecting",
            session,
        };
        let lines = progress(SessionState::WaitingForNetwork).lines(&Notes::default());
        let held = decide(
            &transition_with(Cause::Progress {
                detail: "buffering",
                session: SessionState::Capturing,
            }),
            &progress(SessionState::Capturing).lines(&Notes::default()),
            &Context {
                stopped: true,
                ..Context::default()
            },
        );
        assert_eq!(held.detail, None, "a stopped segment keeps its sentence");
        assert_eq!(held.phase, None);

        let shown = decide(
            &transition_with(progress(SessionState::WaitingForNetwork)),
            &lines,
            &Context {
                stopped: true,
                snapshot_recognition: Some(SessionState::AwaitingFinal),
                ..Context::default()
            },
        );
        assert_eq!(
            shown.detail.as_deref(),
            Some("reconnecting"),
            "the network wait is shown even after the stop"
        );
        assert_eq!(shown.recognition, Some(SessionState::WaitingForNetwork));
    }

    /// A stable sentence is observable until the next session event: a plain
    /// capturing tick must not downgrade it.
    #[test]
    fn a_capturing_tick_never_downgrades_a_stable_sentence() {
        let decision = decide(
            &transition_with(Cause::Progress {
                detail: "buffering",
                session: SessionState::Capturing,
            }),
            &Cause::Progress {
                detail: "buffering",
                session: SessionState::Capturing,
            }
            .lines(&Notes::default()),
            &Context {
                snapshot_recognition: Some(SessionState::Stable),
                ..Context::default()
            },
        );
        assert_eq!(decision.recognition, None, "Stable stays observable");
        assert!(decision.detail.is_some());
    }

    /// The recovery fact is kept on the segment's own line and in the snapshot line;
    /// the storage reason that stopped a capture is kept for the same reason.
    #[test]
    fn recovery_and_storage_notes_are_composed_into_the_segment_line() {
        let notes = Notes {
            recovery: Some("Recovered after Listen restarted"),
            storage: Some("the pending-audio budget was reached"),
            glossary: Some("Glossary correction is unavailable"),
        };
        let lines = Cause::Progress {
            detail: "capturing",
            session: SessionState::Capturing,
        }
        .lines(&notes);
        let segment = lines.segment.unwrap();
        assert_eq!(
            segment,
            "capturing\nthe pending-audio budget was reached\nRecovered after Listen restarted"
        );
        let snapshot = lines.snapshot.unwrap();
        assert!(snapshot.ends_with("\nGlossary correction is unavailable"));
        assert!(snapshot.contains("Recovered after Listen restarted"));
    }

    /// A failure names the reason, the audio kept on this machine and — when the
    /// capture was interrupted — the incomplete text. The two lines differ exactly
    /// where the user acts differently: the snapshot says where the incomplete text is
    /// stored and carries the recovery summary, the record keeps the fact itself.
    #[test]
    fn a_failure_line_names_the_reason_the_kept_audio_and_the_partial_text() {
        let retained = vec![PathBuf::from(r"C:\runtime\session-1\audio.wav")];
        let cause = Cause::Failed {
            error: "the recognition engine lost the connection",
            retained: &retained,
            partial: Some(Partial {
                chars: 42,
                path: PathBuf::from(r"C:\runtime\session-1\partial-result.txt"),
            }),
            advice: Some("The audio from this capture is retained."),
            summary: Some("1 segment was recovered."),
        };
        let lines = cause.lines(&Notes {
            recovery: Some("Recovered after Listen restarted"),
            ..Notes::default()
        });
        let segment = lines.segment.unwrap();
        assert!(segment.starts_with(
            "the recognition engine lost the connection. Audio retained for local recovery:"
        ));
        assert!(segment.contains("session-1\\audio.wav"));
        assert!(segment.contains("this text is incomplete and kept with this segment: 42"));
        assert!(segment.contains("The audio from this capture is retained."));
        assert!(segment.contains("Recovered after Listen restarted"));
        assert!(
            !segment.contains("1 segment was recovered"),
            "summary is panel-only"
        );

        let snapshot = lines.snapshot.unwrap();
        assert!(snapshot.contains("it is shown in the expanded result panel and stored at"));
        assert!(snapshot.contains("partial-result.txt"));
        assert!(snapshot.contains("1 segment was recovered."));
        assert!(
            !snapshot.contains("Recovered after Listen restarted"),
            "the snapshot line does not duplicate the stored recovery fact"
        );
    }

    /// A recorded delivery keeps the sentence the delivery layer wrote, adds the
    /// playback device to the snapshot line, and appends the 术语表 note only there.
    #[test]
    fn the_delivered_snapshot_line_names_the_playback_device_and_the_glossary_note() {
        let lines = Cause::Delivered {
            detail: "The complete result was appended to the original target.",
            delivery: Delivery::Appended,
            description: "Test Speakers",
            text: "第一行",
        }
        .lines(&Notes {
            glossary: Some("Glossary correction is unavailable"),
            ..Notes::default()
        });
        assert_eq!(
            lines.segment.as_deref(),
            Some("The complete result was appended to the original target.")
        );
        assert_eq!(
            lines.snapshot.as_deref(),
            Some(
                "The complete result was appended to the original target.\nPlayback: Test Speakers\nGlossary correction is unavailable"
            )
        );
    }

    /// The persistence rule: a live progress tick only writes when the durable state
    /// moved; a terminal fact always writes; a drained result drops the record.
    #[test]
    fn the_record_follows_the_transition_and_only_a_state_change_forces_a_tick_write() {
        let progress = Cause::Progress {
            detail: "capturing",
            session: SessionState::Capturing,
        };
        let lines = progress.lines(&Notes::default());
        let conditional = Transition {
            sequence: 1,
            state: Some(SegmentState::Capturing),
            session: Some(SessionState::Capturing),
            terminal: None,
            stopped: None,
            finalizing: None,
            preparation_changed: false,
            cause: progress,
            record: RecordRequest::StatusIfChanged,
        };
        let unchanged = decide(&conditional, &lines, &Context::default());
        assert_eq!(unchanged.record, Record::Keep);
        let changed = decide(
            &conditional,
            &lines,
            &Context {
                state_changed: true,
                ..Context::default()
            },
        );
        assert_eq!(changed.record, Record::Status);
        assert!(changed.state_changed);
    }

    /// A session's terminal names never overwrite a segment that already reached a
    /// terminal fact of its own.
    #[test]
    fn a_live_status_never_reopens_a_terminal_segment() {
        assert_eq!(
            state_for_status(SegmentState::Failed, SessionState::Complete),
            SegmentState::Failed
        );
        assert_eq!(
            state_for_status(SegmentState::Result, SessionState::Capturing),
            SegmentState::Result
        );
        assert_eq!(
            state_for_status(SegmentState::Capturing, SessionState::WaitingForNetwork),
            SegmentState::WaitingForNetwork
        );
        assert_eq!(
            state_for_status(SegmentState::Capturing, SessionState::AwaitingFinal),
            SegmentState::Processing
        );
    }

    fn transition_with(cause: Cause<'static>) -> Transition<'static> {
        Transition {
            sequence: 1,
            state: None,
            session: None,
            terminal: None,
            stopped: None,
            finalizing: None,
            preparation_changed: false,
            cause,
            record: RecordRequest::Keep,
        }
    }
}
