//! `runtime\status.txt`: the machine-readable contract the desktop acceptance
//! scripts read.
//!
//! The text is a pure function of the application snapshot plus the facts only the
//! running process and its two windows know. Nothing here opens a window, touches a
//! file or reads a clock, so the key set, the key order and the conditional blocks
//! are pinned by unit tests instead of only by the `scripts\*.ps1` assertions. The
//! caller's whole job is to collect those facts and write the returned string.

use crate::{capsule::preparation_name, panel};
use listen::{Refine, Snapshot, UsageSummary};
use std::path::{Path, PathBuf};

/// The facts only the running process knows: where its durable current-result
/// mirror is, which 术语表 it resolved, and its own process id. They are inputs so
/// the report stays a pure function.
#[derive(Clone, Copy, Debug)]
pub struct HostFacts<'a> {
    /// The `current-result.txt` mirror this run reads and writes.
    pub current_result_path: &'a Path,
    /// The `status.txt` spelling of the active 术语表 (`none` when no file exists).
    pub glossary: &'a str,
    pub process_id: u32,
}

/// The facts the two windows own. `capsule` is the visual the capsule is actually
/// showing ([`crate::capsule::CapsuleState::name`]); `panel_visible` and
/// `panel_error` are the panel's own state. `report` flattens the error to one line.
#[derive(Clone, Copy, Debug)]
pub struct WindowFacts<'a> {
    pub panel_visible: bool,
    pub panel_error: &'a str,
    pub capsule: &'a str,
    pub startup_capsule_ms: u64,
    pub capture_excluded: bool,
    /// Whether the expanded result panel is hidden from screen capture. False
    /// before the panel window exists and when the platform refused; the key is
    /// reported either way, exactly like `capture_excluded`.
    pub panel_capture_excluded: bool,
}

/// A cheap identity of the report's inputs that live outside the snapshot.
///
/// [`report`] is a pure function of the snapshot plus these facts, so a caller that
/// already knows whether the snapshot changed can regenerate the whole file only
/// when this key changed, instead of formatting every key on each poll (#49). The
/// key owns copies so the caller can keep it between ticks; every fact either input
/// carries is part of the comparison, so a changed fact can never be skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactsKey {
    current_result_path: PathBuf,
    glossary: String,
    process_id: u32,
    panel_visible: bool,
    panel_error: String,
    capsule: String,
    startup_capsule_ms: u64,
    capture_excluded: bool,
    panel_capture_excluded: bool,
}

impl FactsKey {
    pub fn of(host: &HostFacts<'_>, window: &WindowFacts<'_>) -> Self {
        Self {
            current_result_path: host.current_result_path.to_path_buf(),
            glossary: host.glossary.to_owned(),
            process_id: host.process_id,
            panel_visible: window.panel_visible,
            panel_error: window.panel_error.to_owned(),
            capsule: window.capsule.to_owned(),
            startup_capsule_ms: window.startup_capsule_ms,
            capture_excluded: window.capture_excluded,
            panel_capture_excluded: window.panel_capture_excluded,
        }
    }
}

/// Whether the report's inputs changed, and the whole file therefore has to be
/// generated again.
///
/// The comparison is the values' own `PartialEq`, never a hand-written list of
/// fields: the complete [`Snapshot`] on one side, and `FactsKey` — a field-for-field
/// copy of [`HostFacts`] and [`WindowFacts`] — on the other. A field added to any of
/// the three inputs is compared the moment it exists, so a new input cannot be
/// silently left out of the rewrite decision.
pub fn report_inputs_differ(
    previous: Option<(&Snapshot, &FactsKey)>,
    current: (&Snapshot, &FactsKey),
) -> bool {
    previous != Some(current)
}

/// The complete `runtime\status.txt` text for one snapshot.
///
/// This is the one place the machine-readable contract is serialized. The key set,
/// the key order, the three conditional blocks (usage / storage / #38 latency) and
/// the one-line rule for free-text values are pinned by the tests in this module;
/// `scripts\verify-panel.ps1`'s `$requiredKeys` is the same 30 keys the desktop
/// acceptance run asserts. Every key here existed before this function did, with the
/// same spelling and the same order.
pub fn report(snapshot: &Snapshot, host: &HostFacts<'_>, window: &WindowFacts<'_>) -> String {
    let timing = &snapshot.timing;
    // The file is a key=value contract: an embedded newline would invent a line no
    // key owns, so every free-text value is flattened in exactly one place.
    let one_line = |text: &str| text.replace('\r', "").replace('\n', " ");
    // Only a 精修 state that stopped explains itself.
    let refine_reason = match &snapshot.refine {
        Refine::Unavailable(reason)
        | Refine::NotApplied(reason)
        | Refine::Unconfirmed(reason)
        | Refine::Failed(reason) => one_line(reason),
        _ => String::new(),
    };
    let notice = snapshot
        .notice
        .as_ref()
        .map(|notice| one_line(&notice.text()))
        .unwrap_or_default();
    let milliseconds =
        |value: Option<u64>| value.map(|value| value.to_string()).unwrap_or_default();
    let mut status = format!(
        "phase={:?}\ndetail={}\nendpoint_role={}\nendpoint_id={}\nendpoint_name={}\ncurrent_result={}\nretained_audio={}\nretry_available={}\nbackend={}\nrecognition_mode={}\nglossary={}\npreparation={}\nrefine={}\nrefine_reason={}\nnotice={}\ncapsule={}\npanel_visible={}\npanel_error={}\nstartup_capsule_ms={}\ncapsule_capture_excluded={}\npanel_capture_excluded={}\ntiming_capture_start_ms={}\ntiming_preparation_ms={}\ntiming_stop_to_result_ms={}\ntiming_finalize_ms={}\ntiming_recognition_ms={}\ntiming_last_window_wait_ms={}\ntiming_last_window_inference_ms={}\ntiming_save_ms={}\ntiming_delivery_ms={}\ntiming_recognition_windows={}\nprocess_id={}\nrecognition={}\nwaiting_segments={}\npending_results={}\n",
        snapshot.phase,
        snapshot.detail,
        snapshot
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.role.as_str())
            .unwrap_or(""),
        snapshot
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.id.as_str())
            .unwrap_or(""),
        snapshot
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.name.as_str())
            .unwrap_or(""),
        host.current_result_path.display(),
        snapshot
            .retained_audio
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" | "),
        snapshot.retry_available,
        snapshot.backend.as_deref().unwrap_or(""),
        // The selected path is a fact, not an inference from the backend wording:
        // `local` is the explicit fallback, `cloud-trial` the daily default.
        snapshot.mode.name(),
        host.glossary,
        preparation_name(snapshot.preparation),
        snapshot.refine.name(),
        refine_reason,
        notice,
        window.capsule,
        if window.panel_visible { 1 } else { 0 },
        one_line(window.panel_error),
        window.startup_capsule_ms,
        if window.capture_excluded { 1 } else { 0 },
        if window.panel_capture_excluded { 1 } else { 0 },
        milliseconds(timing.capture_start_ms),
        milliseconds(timing.preparation_ms),
        milliseconds(timing.stop_to_result_ms),
        milliseconds(timing.finalize_ms),
        milliseconds(timing.recognition_ms),
        milliseconds(timing.last_window_wait_ms),
        milliseconds(timing.last_window_inference_ms),
        milliseconds(timing.save_ms),
        milliseconds(timing.delivery_ms),
        timing.recognition_windows,
        host.process_id,
        snapshot
            .recognition
            .map(|state| state.name())
            .unwrap_or("none"),
        // `waiting_segments` counts every in-flight segment; the panel's own
        // `segments_note` excludes the newest. That difference is a known product
        // question (#33), kept exactly as it was.
        snapshot
            .segments
            .iter()
            .filter(|segment| segment.state.in_flight())
            .count(),
        panel::pending_results(snapshot).len(),
    );
    // The cumulative use of this tool's own cloud requests. `usage_available=0`
    // means the engine keeps no cloud ledger (local recognition), which is not the
    // same as `usage_requests=0`: a cloud adapter always reports a summary, and an
    // unreadable ledger reports `usage_unreadable=1` instead of zero.
    status.push_str(&match &snapshot.usage {
        Some(usage) => format!(
            "usage_available=1\nusage_unreadable={}\nusage_requests={}\nusage_confirmed_nanoyuan={}\nusage_estimated_nanoyuan={}\nusage_unknown_requests={}\nusage_retried_segments={}\nusage_undated_requests={}\nusage_month_nanoyuan={}\nusage_month_reminder={}\nusage_month_reminder_nanoyuan={}\nusage_trial_cap_nanoyuan={}\n",
            if usage.unreadable { 1 } else { 0 },
            usage.requests,
            usage.confirmed_nanoyuan,
            usage.estimated_nanoyuan,
            usage.unknown_requests,
            usage.retried_segments,
            usage.undated_requests,
            usage.month_nanoyuan(),
            if usage.reminder_reached() { 1 } else { 0 },
            UsageSummary::MONTH_REMINDER_NANOYUAN,
            usage.trial_cap_nanoyuan,
        ),
        None => "usage_available=0\n".into(),
    });
    // The storage bounds are appended for the same reason: `status.txt` is a
    // machine-readable contract and existing keys never change meaning. A warning is
    // not a stop, and a real save failure is reported as a fault instead of being
    // inferred from a phase.
    status.push_str(&match &snapshot.storage {
        Some(storage) => format!(
            "storage_level={}\nstorage_pending_audio_bytes={}\nstorage_budget_bytes={}\nstorage_warning_bytes={}\nstorage_free_disk_bytes={}\nstorage_free_disk_floor_bytes={}\nstorage_text_bytes={}\nstorage_text_budget_bytes={}\nstorage_fault={}\n",
            storage.level().name(),
            storage.pending_audio_bytes,
            storage.budget_bytes,
            storage.warning_at_bytes,
            storage
                .free_disk_bytes
                .map(|bytes| bytes.to_string())
                .unwrap_or_default(),
            storage.free_disk_floor_bytes,
            storage.text_bytes,
            storage.text_budget_bytes,
            one_line(storage.fault.as_deref().unwrap_or("")),
        ),
        None => "storage_level=none\n".into(),
    });
    // #38: the one stop-to-confirmed-backfill number and the stages behind it are
    // appended. Existing keys keep their exact meaning, so scripts that read
    // `timing_stop_to_result_ms` are unaffected.
    status.push_str(&format!(
        "timing_stop_to_confirmed_ms={}\ntiming_cloud_connect_ms={}\ntiming_first_task_started_ms={}\ntiming_cloud_drain_ms={}\ntiming_cloud_final_ms={}\ntiming_event_wait_ms={}\ntiming_delivery_queue_ms={}\ntiming_apply_wait_ms={}\ntiming_schedule_ms={}\ntiming_delivery_lock_ms={}\ntiming_delivery_read_ms={}\ntiming_delivery_write_ms={}\ntiming_delivery_save_ms={}\n",
        milliseconds(timing.stop_to_confirmed_ms),
        milliseconds(timing.cloud_connect_ms),
        milliseconds(timing.first_task_started_ms),
        milliseconds(timing.cloud_drain_ms),
        milliseconds(timing.cloud_final_ms),
        milliseconds(timing.event_wait_ms),
        milliseconds(timing.delivery_queue_ms),
        milliseconds(timing.apply_wait_ms),
        milliseconds(timing.schedule_ms),
        milliseconds(timing.delivery_lock_ms),
        milliseconds(timing.delivery_read_ms),
        milliseconds(timing.delivery_write_ms),
        milliseconds(timing.delivery_save_ms),
    ));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use listen::{
        CaptureEndpoint, Delivery, Notice, Phase, Preparation, Refine, SegmentState,
        SegmentSummary, Snapshot, Timing, UsageSummary,
        recognition::{RecognitionMode, SessionState},
        streaming::CaptureProgress,
    };
    use std::path::{Path, PathBuf};

    /// The 35 unconditional keys, in the order they were written before this wave.
    const BASE_KEYS: [&str; 35] = [
        "phase",
        "detail",
        "endpoint_role",
        "endpoint_id",
        "endpoint_name",
        "current_result",
        "retained_audio",
        "retry_available",
        "backend",
        "recognition_mode",
        "glossary",
        "preparation",
        "refine",
        "refine_reason",
        "notice",
        "capsule",
        "panel_visible",
        "panel_error",
        "startup_capsule_ms",
        "capsule_capture_excluded",
        "panel_capture_excluded",
        "timing_capture_start_ms",
        "timing_preparation_ms",
        "timing_stop_to_result_ms",
        "timing_finalize_ms",
        "timing_recognition_ms",
        "timing_last_window_wait_ms",
        "timing_last_window_inference_ms",
        "timing_save_ms",
        "timing_delivery_ms",
        "timing_recognition_windows",
        "process_id",
        "recognition",
        "waiting_segments",
        "pending_results",
    ];

    /// The cloud-usage block: 12 keys when an engine reports a ledger.
    const USAGE_KEYS: [&str; 12] = [
        "usage_available",
        "usage_unreadable",
        "usage_requests",
        "usage_confirmed_nanoyuan",
        "usage_estimated_nanoyuan",
        "usage_unknown_requests",
        "usage_retried_segments",
        "usage_undated_requests",
        "usage_month_nanoyuan",
        "usage_month_reminder",
        "usage_month_reminder_nanoyuan",
        "usage_trial_cap_nanoyuan",
    ];

    /// The storage block: 9 keys when a measurement exists.
    const STORAGE_KEYS: [&str; 9] = [
        "storage_level",
        "storage_pending_audio_bytes",
        "storage_budget_bytes",
        "storage_warning_bytes",
        "storage_free_disk_bytes",
        "storage_free_disk_floor_bytes",
        "storage_text_bytes",
        "storage_text_budget_bytes",
        "storage_fault",
    ];

    /// The #38 latency block, always appended.
    const TIMING_KEYS: [&str; 13] = [
        "timing_stop_to_confirmed_ms",
        "timing_cloud_connect_ms",
        "timing_first_task_started_ms",
        "timing_cloud_drain_ms",
        "timing_cloud_final_ms",
        "timing_event_wait_ms",
        "timing_delivery_queue_ms",
        "timing_apply_wait_ms",
        "timing_schedule_ms",
        "timing_delivery_lock_ms",
        "timing_delivery_read_ms",
        "timing_delivery_write_ms",
        "timing_delivery_save_ms",
    ];

    /// `scripts\verify-panel.ps1`'s `$requiredKeys`, copied verbatim. These 30 keys,
    /// in this order, are the desktop acceptance contract.
    const REQUIRED_KEYS: [&str; 30] = [
        "phase",
        "detail",
        "endpoint_role",
        "endpoint_id",
        "endpoint_name",
        "current_result",
        "retained_audio",
        "retry_available",
        "backend",
        "preparation",
        "refine",
        "refine_reason",
        "notice",
        "capsule",
        "panel_visible",
        "panel_error",
        "startup_capsule_ms",
        "capsule_capture_excluded",
        "panel_capture_excluded",
        "timing_capture_start_ms",
        "timing_preparation_ms",
        "timing_stop_to_result_ms",
        "timing_finalize_ms",
        "timing_recognition_ms",
        "timing_last_window_wait_ms",
        "timing_last_window_inference_ms",
        "timing_save_ms",
        "timing_delivery_ms",
        "timing_recognition_windows",
        "process_id",
    ];

    fn snapshot() -> Snapshot {
        Snapshot {
            phase: Phase::Ready,
            preparation: Preparation::Ready,
            recognition: Some(SessionState::Stable),
            detail: "Ready detail".into(),
            text: String::new(),
            retained_audio: Vec::new(),
            progress: CaptureProgress::default(),
            delivery: None,
            retry_available: false,
            endpoint: None,
            backend: Some("Cloud trial: Alibaba Model Studio".into()),
            timing: Timing::default(),
            refine: Refine::Unavailable("no port".into()),
            refined_text: None,
            notice: None,
            usage: None,
            segments: Vec::new(),
            storage: None,
            mode: RecognitionMode::Local,
            current_sequence: None,
        }
    }

    fn host() -> HostFacts<'static> {
        HostFacts {
            current_result_path: Path::new(r"C:\runtime\current-result.txt"),
            glossary: "none",
            process_id: 4242,
        }
    }

    fn window() -> WindowFacts<'static> {
        WindowFacts {
            panel_visible: true,
            panel_error: "",
            capsule: "Ready",
            startup_capsule_ms: 42,
            capture_excluded: false,
            panel_capture_excluded: false,
        }
    }

    #[test]
    fn the_cheap_report_key_changes_with_every_fact_outside_the_snapshot() {
        let base = FactsKey::of(&host(), &window());
        assert_eq!(
            base,
            FactsKey::of(&host(), &window()),
            "identical facts are the same key"
        );

        for changed in [
            HostFacts {
                current_result_path: Path::new(r"C:\runtime\other-result.txt"),
                ..host()
            },
            HostFacts {
                glossary: "glossary.txt",
                ..host()
            },
            HostFacts {
                process_id: 4243,
                ..host()
            },
        ] {
            assert_ne!(
                base,
                FactsKey::of(&changed, &window()),
                "a changed host fact must be a new key: {changed:?}"
            );
        }

        for changed in [
            WindowFacts {
                panel_visible: false,
                ..window()
            },
            WindowFacts {
                panel_error: "panel failed",
                ..window()
            },
            WindowFacts {
                capsule: "Failure",
                ..window()
            },
            WindowFacts {
                startup_capsule_ms: 43,
                ..window()
            },
            WindowFacts {
                capture_excluded: true,
                ..window()
            },
            WindowFacts {
                panel_capture_excluded: true,
                ..window()
            },
        ] {
            assert_ne!(
                base,
                FactsKey::of(&host(), &changed),
                "a changed window fact must be a new key: {changed:?}"
            );
        }
    }

    #[test]
    fn a_change_in_the_snapshot_host_facts_or_window_facts_requires_a_new_report() {
        let base = snapshot();
        let base_host = host();
        let base_window = window();
        let base_facts = FactsKey::of(&base_host, &base_window);
        assert!(
            !report_inputs_differ(Some((&base, &base_facts)), (&base, &base_facts)),
            "identical inputs are the same report"
        );

        // One changed value from each of the three inputs. The value comparison
        // decides a new report, and the text really differs, so the file is rewritten.
        let mut changed_snapshot = snapshot();
        changed_snapshot.detail = "a different sentence".into();
        assert!(
            report_inputs_differ(Some((&base, &base_facts)), (&changed_snapshot, &base_facts)),
            "the whole snapshot is part of the comparison"
        );
        assert_ne!(
            report(&base, &base_host, &base_window),
            report(&changed_snapshot, &base_host, &base_window),
            "a changed snapshot value writes new status text"
        );

        let changed_host = HostFacts {
            glossary: "glossary.txt",
            ..base_host
        };
        let changed_host_facts = FactsKey::of(&changed_host, &base_window);
        assert!(
            report_inputs_differ(Some((&base, &base_facts)), (&base, &changed_host_facts)),
            "every host fact is in the comparison"
        );
        assert_ne!(
            report(&base, &base_host, &base_window),
            report(&base, &changed_host, &base_window),
            "a changed host fact writes new status text"
        );

        let changed_window = WindowFacts {
            capsule: "Failure",
            ..base_window
        };
        let changed_window_facts = FactsKey::of(&base_host, &changed_window);
        assert!(
            report_inputs_differ(Some((&base, &base_facts)), (&base, &changed_window_facts)),
            "every window fact is in the comparison"
        );
        assert_ne!(
            report(&base, &base_host, &base_window),
            report(&base, &base_host, &changed_window),
            "a changed window fact writes new status text"
        );
    }

    /// The hand-written field list missed exactly these three values before F6
    /// (`endpoint`, `mode`, `current_sequence`), so they get an explicit regression
    /// test on the value comparison.
    #[test]
    fn the_fields_the_hand_written_comparison_missed_now_change_the_report() {
        let base = snapshot();
        let base_facts = FactsKey::of(&host(), &window());

        let mut with_endpoint = snapshot();
        with_endpoint.endpoint = Some(CaptureEndpoint {
            role: "multimedia".into(),
            id: "endpoint-id".into(),
            name: "Speakers".into(),
        });
        assert!(report_inputs_differ(
            Some((&base, &base_facts)),
            (&with_endpoint, &base_facts)
        ));
        assert_ne!(
            report(&base, &host(), &window()),
            report(&with_endpoint, &host(), &window())
        );

        let mut cloud = snapshot();
        cloud.mode = RecognitionMode::CloudTrial;
        assert!(report_inputs_differ(
            Some((&base, &base_facts)),
            (&cloud, &base_facts)
        ));
        assert_ne!(
            report(&base, &host(), &window()),
            report(&cloud, &host(), &window())
        );

        let pending = |sequence: u64| SegmentSummary {
            sequence,
            state: SegmentState::Result,
            detail: String::new(),
            text: "待取结果".into(),
            delivery: Some(Delivery::Fallback),
            usage: None,
        };
        let mut without_identity = snapshot();
        without_identity.segments = (1..=3).map(pending).collect();
        let mut with_identity = without_identity.clone();
        with_identity.current_sequence = Some(2);
        assert!(report_inputs_differ(
            Some((&without_identity, &base_facts)),
            (&with_identity, &base_facts)
        ));
        assert!(
            report(&without_identity, &host(), &window()).contains("\npending_results=3\n"),
            "all three results are pending without an identity"
        );
        assert!(
            report(&with_identity, &host(), &window()).contains("\npending_results=2\n"),
            "the current result is excluded by its sequence"
        );
    }

    /// Every emitted line is `key=value`; `detail` is kept to one line in these
    /// fixtures so a key can never be mistaken for a value.
    fn keys(report: &str) -> Vec<&str> {
        report
            .lines()
            .map(|line| {
                line.split_once('=')
                    .unwrap_or_else(|| panic!("not a key=value line: {line:?}"))
                    .0
            })
            .collect()
    }

    fn storage() -> listen::storage::StorageStatus {
        listen::storage::StorageStatus {
            pending_audio_bytes: 1_000,
            budget_bytes: 2_000,
            warning_at_bytes: 1_600,
            free_disk_bytes: Some(3_000),
            free_disk_floor_bytes: 500,
            text_bytes: 10,
            text_budget_bytes: 4_000,
            fault: Some("disk full".into()),
        }
    }

    #[test]
    fn without_usage_or_storage_the_key_set_and_order_are_unchanged() {
        let report = report(&snapshot(), &host(), &window());
        let mut expected: Vec<&str> = BASE_KEYS.to_vec();
        // Both conditional blocks fall back to their single always-present key.
        expected.push("usage_available");
        expected.push("storage_level");
        expected.extend(TIMING_KEYS);
        assert_eq!(keys(&report), expected);
    }

    #[test]
    fn a_usage_ledger_inserts_its_twelve_keys_before_the_storage_block() {
        let mut state = snapshot();
        state.usage = Some(UsageSummary::default());
        let report = report(&state, &host(), &window());
        let mut expected: Vec<&str> = BASE_KEYS.to_vec();
        expected.extend(USAGE_KEYS);
        expected.push("storage_level");
        expected.extend(TIMING_KEYS);
        assert_eq!(keys(&report), expected);
    }

    #[test]
    fn a_storage_measurement_inserts_its_nine_keys_before_the_timing_block() {
        let mut state = snapshot();
        state.storage = Some(storage());
        let report = report(&state, &host(), &window());
        let mut expected: Vec<&str> = BASE_KEYS.to_vec();
        expected.push("usage_available");
        expected.extend(STORAGE_KEYS);
        expected.extend(TIMING_KEYS);
        assert_eq!(keys(&report), expected);
    }

    #[test]
    fn every_block_together_is_the_frozen_sixty_nine_keys_and_the_required_thirty() {
        let mut state = snapshot();
        state.usage = Some(UsageSummary::default());
        state.storage = Some(storage());
        let report = report(&state, &host(), &window());
        let keys = keys(&report);

        let mut expected: Vec<&str> = BASE_KEYS.to_vec();
        expected.extend(USAGE_KEYS);
        expected.extend(STORAGE_KEYS);
        expected.extend(TIMING_KEYS);
        assert_eq!(keys, expected);
        assert_eq!(keys.len(), 69);

        // No key is emitted twice; the contract is a set with one defined order.
        let unique: std::collections::HashSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len(), "a key was emitted twice");

        // The desktop acceptance script reads these 30 keys by name and order.
        let required: Vec<&str> = keys
            .iter()
            .copied()
            .filter(|key| REQUIRED_KEYS.contains(key))
            .collect();
        assert_eq!(required, REQUIRED_KEYS.to_vec());
    }

    #[test]
    fn every_value_keeps_its_spelling_and_stays_on_one_line() {
        let mut state = snapshot();
        state.mode = RecognitionMode::CloudTrial;
        state.text = "完整结果".into();
        state.current_sequence = Some(2);
        state.retained_audio = vec![
            PathBuf::from(r"C:\runtime\a.wav"),
            PathBuf::from(r"C:\runtime\b.wav"),
        ];
        state.retry_available = true;
        state.refine = Refine::Failed("network\r\ndown".into());
        state.notice = Some(Notice::CopyFailed("locked\r\nby another app".into()));
        state.timing.recognition_ms = Some(1234);
        state.segments = vec![
            SegmentSummary {
                sequence: 1,
                state: SegmentState::Processing,
                detail: String::new(),
                text: String::new(),
                delivery: None,
                usage: None,
            },
            SegmentSummary {
                sequence: 2,
                state: SegmentState::Result,
                detail: String::new(),
                text: "完整结果".into(),
                delivery: Some(Delivery::Fallback),
                usage: None,
            },
            SegmentSummary {
                sequence: 3,
                state: SegmentState::Result,
                detail: String::new(),
                text: "另一段待取结果".into(),
                delivery: Some(Delivery::Fallback),
                usage: None,
            },
        ];
        let window = WindowFacts {
            panel_visible: true,
            panel_error: "panel\r\nfailed",
            capsule: "Processing",
            startup_capsule_ms: 42,
            capture_excluded: true,
            panel_capture_excluded: true,
        };
        let report = report(&state, &host(), &window);

        assert!(report.starts_with("phase=Ready\n"), "{report}");
        assert!(report.contains("\ndetail=Ready detail\n"), "{report}");
        assert!(report.contains("\nendpoint_role=\n"), "{report}");
        assert!(
            report.contains("\ncurrent_result=C:\\runtime\\current-result.txt\n"),
            "{report}"
        );
        assert!(
            report.contains("\nretained_audio=C:\\runtime\\a.wav | C:\\runtime\\b.wav\n"),
            "{report}"
        );
        assert!(report.contains("\nretry_available=true\n"), "{report}");
        assert!(
            report.contains("\nbackend=Cloud trial: Alibaba Model Studio\n"),
            "{report}"
        );
        // The mode is its own fact and keeps the ADR 0006 spelling.
        assert!(
            report.contains("\nrecognition_mode=cloud-trial\n"),
            "{report}"
        );
        assert!(report.contains("\nglossary=none\n"), "{report}");
        assert!(report.contains("\npreparation=Ready\n"), "{report}");
        assert!(report.contains("\nrefine=failed\n"), "{report}");
        // A stopped refine reason and a notice are flattened to one line.
        assert!(
            report.contains("\nrefine_reason=network down\n"),
            "{report}"
        );
        assert!(
            report.contains("\nnotice=Could not copy the result: locked by another app."),
            "{report}"
        );
        assert!(report.contains("\ncapsule=Processing\n"), "{report}");
        assert!(report.contains("\npanel_visible=1\n"), "{report}");
        assert!(report.contains("\npanel_error=panel failed\n"), "{report}");
        assert!(report.contains("\nstartup_capsule_ms=42\n"), "{report}");
        assert!(
            report.contains("\ncapsule_capture_excluded=1\n"),
            "{report}"
        );
        assert!(report.contains("\npanel_capture_excluded=1\n"), "{report}");
        assert!(
            report.contains("\ntiming_recognition_ms=1234\n"),
            "{report}"
        );
        // A missing measurement is empty, never a zero.
        assert!(report.contains("\ntiming_preparation_ms=\n"), "{report}");
        assert!(
            report.contains("\ntiming_recognition_windows=0\n"),
            "{report}"
        );
        assert!(report.contains("\nprocess_id=4242\n"), "{report}");
        assert!(report.contains("\nrecognition=stable\n"), "{report}");
        // `waiting_segments` counts every in-flight segment; `pending_results`
        // counts the results the panel still offers (the current one is not one).
        assert!(report.contains("\nwaiting_segments=1\n"), "{report}");
        assert!(report.contains("\npending_results=1\n"), "{report}");
        assert!(report.ends_with("\n"), "{report}");
    }

    /// The expanded result panel is hidden from screen capture exactly like the
    /// capsule (#54). The key is always present: `0` before the window exists or
    /// when the platform refused, `1` once `SetWindowDisplayAffinity` succeeded.
    #[test]
    fn the_panel_capture_exclusion_is_always_reported_as_zero_or_one() {
        for (panel_capture_excluded, expected) in [(false, "0"), (true, "1")] {
            let window = WindowFacts {
                panel_capture_excluded,
                ..window()
            };
            let report = report(&snapshot(), &host(), &window);
            assert!(
                report.contains(&format!("\npanel_capture_excluded={expected}\n")),
                "panel_capture_excluded={expected} must be reported: {report}"
            );
        }
    }

    #[test]
    fn the_local_and_cloud_spellings_are_the_only_two_mode_values() {
        for (mode, expected) in [
            (RecognitionMode::Local, "local"),
            (RecognitionMode::CloudTrial, "cloud-trial"),
        ] {
            let mut state = snapshot();
            state.mode = mode;
            let report = report(&state, &host(), &window());
            assert!(
                report.contains(&format!("\nrecognition_mode={expected}\n")),
                "{report}"
            );
        }
    }

    #[test]
    fn the_backend_wording_changes_nothing_but_the_backend_value() {
        // The wave's contract: `backend` is display only. Two snapshots that differ
        // only in that wording must produce the same file once the value is set
        // aside, including the `recognition_mode` line.
        const PREFIXED: &str = "Cloud trial: Alibaba Model Studio";
        const REWORDED: &str = "Alibaba Model Studio (wording changed)";
        for mode in [RecognitionMode::Local, RecognitionMode::CloudTrial] {
            let mut prefixed = snapshot();
            prefixed.mode = mode;
            prefixed.backend = Some(PREFIXED.into());
            let mut reworded = snapshot();
            reworded.mode = mode;
            reworded.backend = Some(REWORDED.into());
            assert_eq!(
                report(&prefixed, &host(), &window()).replace(PREFIXED, "<backend>"),
                report(&reworded, &host(), &window()).replace(REWORDED, "<backend>"),
                "mode={mode:?}"
            );
        }
    }
}
