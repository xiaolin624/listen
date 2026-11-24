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
use std::path::Path;

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
}

/// The complete `runtime\status.txt` text for one snapshot.
///
/// This is the one place the machine-readable contract is serialized. The key set,
/// the key order, the three conditional blocks (usage / storage / #38 latency) and
/// the one-line rule for free-text values are pinned by the tests in this module;
/// `scripts\verify-panel.ps1`'s `$requiredKeys` is the same 29 keys the desktop
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
        "phase={:?}\ndetail={}\nendpoint_role={}\nendpoint_id={}\nendpoint_name={}\ncurrent_result={}\nretained_audio={}\nretry_available={}\nbackend={}\nrecognition_mode={}\nglossary={}\npreparation={}\nrefine={}\nrefine_reason={}\nnotice={}\ncapsule={}\npanel_visible={}\npanel_error={}\nstartup_capsule_ms={}\ncapsule_capture_excluded={}\ntiming_capture_start_ms={}\ntiming_preparation_ms={}\ntiming_stop_to_result_ms={}\ntiming_finalize_ms={}\ntiming_recognition_ms={}\ntiming_last_window_wait_ms={}\ntiming_last_window_inference_ms={}\ntiming_save_ms={}\ntiming_delivery_ms={}\ntiming_recognition_windows={}\nprocess_id={}\nrecognition={}\nwaiting_segments={}\npending_results={}\n",
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
        Delivery, Notice, Phase, Preparation, Refine, SegmentState, SegmentSummary, Snapshot,
        Timing, UsageSummary,
        recognition::{RecognitionMode, SessionState},
        streaming::CaptureProgress,
    };
    use std::path::{Path, PathBuf};

    /// The 34 unconditional keys, in the order they were written before this wave.
    const BASE_KEYS: [&str; 34] = [
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

    /// `scripts\verify-panel.ps1`'s `$requiredKeys`, copied verbatim. These 29 keys,
    /// in this order, are the desktop acceptance contract.
    const REQUIRED_KEYS: [&str; 29] = [
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
        }
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
    fn every_block_together_is_the_frozen_sixty_eight_keys_and_the_required_twenty_nine() {
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
        assert_eq!(keys.len(), 68);

        // No key is emitted twice; the contract is a set with one defined order.
        let unique: std::collections::HashSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len(), "a key was emitted twice");

        // The desktop acceptance script reads these 29 keys by name and order.
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
