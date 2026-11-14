//! The status capsule's state model (状态胶囊), independent of Windows.
//!
//! The capsule never shows text, numbers or live transcription: colour, shape and
//! motion are the only status a user gets. That makes the mapping from the
//! application snapshot to the capsule's visual state the one place worth unit
//! testing; the window itself is verified by the desktop acceptance script.

use listen::{Phase, Preparation, Snapshot};

/// What the resident capsule is showing right now. This is deliberately a
/// different vocabulary from `Phase`: `Result` and `NoSpeech` both mean "nothing is
/// happening, the tool is usable", while a pending or failed model preparation must
/// never look like that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleState {
    /// The local model is still loading (or the application is in its own
    /// first-use preparation). Capture may still be possible, but recognition is
    /// not available yet, so the capsule must not look ready.
    Preparing,
    /// Idle and usable: a capture can start.
    Ready,
    /// Playback audio is being captured.
    Capturing,
    /// Captured audio is being recognized locally.
    Processing,
    /// The last operation failed, or model preparation failed.
    Failure,
}

impl CapsuleState {
    /// The stable `status.txt` spelling.
    pub fn name(self) -> &'static str {
        match self {
            CapsuleState::Preparing => "Preparing",
            CapsuleState::Ready => "Ready",
            CapsuleState::Capturing => "Capturing",
            CapsuleState::Processing => "Processing",
            CapsuleState::Failure => "Failure",
        }
    }
}

/// The stable `status.txt` spelling of the preparation state.
pub fn preparation_name(preparation: Preparation) -> &'static str {
    match preparation {
        Preparation::Pending => "Pending",
        Preparation::Ready => "Ready",
        Preparation::Failed => "Failed",
    }
}

/// Map the application snapshot to the capsule's visual state.
///
/// Precedence, highest first:
///
/// 1. `Phase::Failure` wins over everything: a failed capture or a failed model
///    preparation is always visible.
/// 2. `Phase::Capturing` wins over preparation: the user pressed the hotkey, so
///    "capture is live" must be visible even while `Preparation::Pending`.
/// 3. `Phase::Processing`.
/// 4. A failed preparation in any other phase is `Failure`, never `Ready`: the
///    capsule must not invite the user to start a capture that cannot be
///    recognized.
/// 5. `Phase::Preparing`, or `Preparation::Pending` in an idle phase, is
///    `Preparing`.
/// 6. `Ready`, `Result` and `NoSpeech` with a ready preparation are `Ready`; the
///    last result is not something the capsule displays, so it stays calm.
pub fn capsule_state(snapshot: &Snapshot) -> CapsuleState {
    match snapshot.phase {
        Phase::Failure => CapsuleState::Failure,
        Phase::Capturing => CapsuleState::Capturing,
        Phase::Processing => CapsuleState::Processing,
        // Every remaining phase is idle with respect to capture, so preparation
        // decides between "not ready yet", "failed" and "ready".
        Phase::Preparing | Phase::Ready | Phase::Result | Phase::NoSpeech => {
            match snapshot.preparation {
                Preparation::Failed => CapsuleState::Failure,
                Preparation::Pending => CapsuleState::Preparing,
                // The application reports a finished preparation as `Ready`, so
                // `Phase::Preparing` here would only mean the phase update is one
                // poll behind; it must not look usable yet.
                Preparation::Ready if snapshot.phase == Phase::Preparing => CapsuleState::Preparing,
                Preparation::Ready => CapsuleState::Ready,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use listen::{Phase, Preparation, Snapshot, Timing, streaming::CaptureProgress};

    fn snapshot(phase: Phase, preparation: Preparation) -> Snapshot {
        Snapshot {
            phase,
            preparation,
            recognition: None,
            detail: String::new(),
            text: String::new(),
            retained_audio: Vec::new(),
            progress: CaptureProgress::default(),
            delivery: None,
            retry_available: false,
            endpoint: None,
            backend: None,
            timing: Timing::default(),
            refine: listen::Refine::Unavailable("not connected in this test".into()),
            refined_text: None,
            notice: None,
        }
    }

    const PHASES: [Phase; 7] = [
        Phase::Preparing,
        Phase::Ready,
        Phase::Capturing,
        Phase::Processing,
        Phase::Result,
        Phase::NoSpeech,
        Phase::Failure,
    ];
    const PREPARATIONS: [Preparation; 3] = [
        Preparation::Pending,
        Preparation::Ready,
        Preparation::Failed,
    ];

    #[test]
    fn every_phase_and_preparation_combination_maps_to_the_specified_capsule_state() {
        use CapsuleState as C;
        use Phase as P;
        use Preparation as R;
        // The specification table, written out as literals so the test does not
        // recompute the precedence it is checking.
        let table = [
            (P::Preparing, R::Pending, C::Preparing),
            (P::Preparing, R::Ready, C::Preparing),
            (P::Preparing, R::Failed, C::Failure),
            (P::Ready, R::Pending, C::Preparing),
            (P::Ready, R::Ready, C::Ready),
            (P::Ready, R::Failed, C::Failure),
            (P::Capturing, R::Pending, C::Capturing),
            (P::Capturing, R::Ready, C::Capturing),
            (P::Capturing, R::Failed, C::Capturing),
            (P::Processing, R::Pending, C::Processing),
            (P::Processing, R::Ready, C::Processing),
            (P::Processing, R::Failed, C::Processing),
            (P::Result, R::Pending, C::Preparing),
            (P::Result, R::Ready, C::Ready),
            (P::Result, R::Failed, C::Failure),
            (P::NoSpeech, R::Pending, C::Preparing),
            (P::NoSpeech, R::Ready, C::Ready),
            (P::NoSpeech, R::Failed, C::Failure),
            (P::Failure, R::Pending, C::Failure),
            (P::Failure, R::Ready, C::Failure),
            (P::Failure, R::Failed, C::Failure),
        ];
        assert_eq!(table.len(), PHASES.len() * PREPARATIONS.len());
        for (phase, preparation, expected) in table {
            let actual = capsule_state(&snapshot(phase.clone(), preparation));
            assert_eq!(
                actual, expected,
                "phase={phase:?} preparation={preparation:?}"
            );
        }
    }

    #[test]
    fn a_pending_or_failed_preparation_never_renders_the_capsule_as_ready() {
        for phase in &PHASES {
            for preparation in [Preparation::Pending, Preparation::Failed] {
                assert_ne!(
                    capsule_state(&snapshot(phase.clone(), preparation)),
                    CapsuleState::Ready,
                    "phase={phase:?} preparation={preparation:?} must not look ready"
                );
            }
        }
    }

    #[test]
    fn capture_started_while_the_model_is_still_preparing_shows_live_capture() {
        let snapshot = snapshot(Phase::Capturing, Preparation::Pending);
        assert_eq!(capsule_state(&snapshot), CapsuleState::Capturing);
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(CapsuleState::Preparing.name(), "Preparing");
        assert_eq!(CapsuleState::Ready.name(), "Ready");
        assert_eq!(CapsuleState::Capturing.name(), "Capturing");
        assert_eq!(CapsuleState::Processing.name(), "Processing");
        assert_eq!(CapsuleState::Failure.name(), "Failure");
        assert_eq!(preparation_name(Preparation::Pending), "Pending");
        assert_eq!(preparation_name(Preparation::Ready), "Ready");
        assert_eq!(preparation_name(Preparation::Failed), "Failed");
    }
}
