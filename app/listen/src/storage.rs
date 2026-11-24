//! Storage accounting and bounds for pending capture audio (#36).
//!
//! The application owns one storage policy: how many bytes of not-yet-drained
//! capture audio may exist across **all** segments, how much free disk must remain
//! before a capture may start, when the user is warned first, and a separate budget
//! for the complete texts it saves.
//!
//! Accounting deliberately does not look at one "final WAV": every regular file
//! inside every `session-*` directory is counted, because that is where the cloud
//! PCM spool, the local overlapping recognition windows, temporary upload parts and
//! repair/retry copies live. Saved texts and the runtime index are **not** part of
//! the audio budget: they have their own bound, so the audio budget can never be
//! mistaken for a whole-process or whole-runtime-directory limit. In-memory audio is
//! bounded separately by the adapters (the local window queue and one cloud PCM
//! chunk); nothing here holds audio in memory.
//!
//! Limits are advisory before the fact and the hard stop is the write error itself:
//! at or over the budget, and at or below the free-disk floor, the application
//! refuses a new capture and stops a running one, keeping everything already
//! received instead of overwriting it.
//!
//! **Measuring is separate from deciding** (#49). [`MeasureBytes`] is the one
//! runtime-directory walk and [`FreeSpace`] the one volume probe; the crate-private
//! `Storage::status` is the pure judgement that turns a cached [`MeasuredBytes`]
//! plus the policy into the published [`StorageStatus`]. `Application::poll` only
//! advances state, so the 10 ms stop path never walks the runtime directory: the
//! desktop caller asks for a low-frequency measurement
//! ([`crate::Application::measure_storage_if_due`]), and the panel and `status.txt`
//! read the last one.
//!
//! This module never deletes anything. An explicit per-segment delete is the
//! application's decision (see `Application::dispatch(Control::DeleteSegment)`).

use crate::{AppResult, segments::SegmentStore};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

/// The default pending-audio budget: 1 GiB across every segment.
pub const DEFAULT_PENDING_AUDIO_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
/// The default free-disk floor: 1 GiB must remain on the runtime volume.
pub const DEFAULT_FREE_DISK_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;
/// The four-fifths default warning line for [`StoragePolicy::warning_at_bytes`].
const DEFAULT_WARNING_NUMERATOR: u64 = 4;
const DEFAULT_WARNING_DENOMINATOR: u64 = 5;
/// A separate, deliberately small bound for the complete texts the application
/// keeps (pending results and the current result). It is unrelated to the audio
/// budget and is never used to decide whether audio may be captured.
pub const DEFAULT_TEXT_BUDGET_BYTES: u64 = 4 * 1024 * 1024;

const SESSION_PREFIX: &str = "session-";
const CURRENT_RESULT: &str = "current-result.txt";
/// Root-level capture files that an adapter might publish outside a session
/// directory. The runtime directory's own text/state files are never part of the
/// audio budget.
const ROOT_AUDIO_EXTENSIONS: [&str; 2] = ["wav", "pcm"];

/// How much pending audio may exist, how much disk must remain, when to warn, and
/// the separate saved-text bound. Every field has a documented default and can be
/// set explicitly; `warning_at_bytes` defaults to four fifths of the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoragePolicy {
    pub pending_audio_budget_bytes: u64,
    /// Bytes of pending audio at which the approaching-limit warning starts.
    /// `None` uses four fifths of `pending_audio_budget_bytes`.
    pub warning_at_bytes: Option<u64>,
    pub free_disk_floor_bytes: u64,
    /// Bound for saved complete texts, independent of the audio budget.
    pub text_budget_bytes: u64,
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            pending_audio_budget_bytes: DEFAULT_PENDING_AUDIO_BUDGET_BYTES,
            warning_at_bytes: None,
            free_disk_floor_bytes: DEFAULT_FREE_DISK_FLOOR_BYTES,
            text_budget_bytes: DEFAULT_TEXT_BUDGET_BYTES,
        }
    }
}

impl StoragePolicy {
    /// A policy with an explicit pending-audio budget and the default relative
    /// warning line, free-disk floor and text bound.
    pub fn with_budget_bytes(pending_audio_budget_bytes: u64) -> Self {
        Self {
            pending_audio_budget_bytes,
            ..Self::default()
        }
    }

    /// MiB helper for the CLI and tests. Saturates instead of overflowing.
    pub fn mib(mib: u64) -> u64 {
        mib.saturating_mul(1024 * 1024)
    }

    /// The effective warning line, never above the budget.
    pub fn warning_at_bytes(&self) -> u64 {
        let relative = self
            .pending_audio_budget_bytes
            .saturating_mul(DEFAULT_WARNING_NUMERATOR)
            / DEFAULT_WARNING_DENOMINATOR;
        self.warning_at_bytes
            .unwrap_or(relative)
            .min(self.pending_audio_budget_bytes)
    }
}

/// The single user-visible storage level. A warning is a reminder where capture
/// still works; `Full` and `DiskLow` are the two independent hard stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageLevel {
    Ok,
    Warning,
    Full,
    DiskLow,
}

impl StorageLevel {
    /// The stable `status.txt` spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Full => "full",
            Self::DiskLow => "disk_low",
        }
    }
}

/// One measurement of the storage conditions the application acts on. `fault` is
/// the last real write/save failure, so a storage fault is observable as itself and
/// never has to be inferred from a network or device message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageStatus {
    /// Pending capture audio across every segment, including temporary and repair
    /// files inside their session directories.
    pub pending_audio_bytes: u64,
    pub budget_bytes: u64,
    pub warning_at_bytes: u64,
    /// `None` when the platform refused to report free space; never treated as zero.
    pub free_disk_bytes: Option<u64>,
    pub free_disk_floor_bytes: u64,
    /// Saved complete texts (pending results plus the current result), separate
    /// from the audio budget.
    pub text_bytes: u64,
    pub text_budget_bytes: u64,
    /// The last storage write/save failure, already phrased for the user.
    pub fault: Option<String>,
}

impl StorageStatus {
    pub fn over_budget(&self) -> bool {
        self.pending_audio_bytes >= self.budget_bytes
    }

    pub fn near_limit(&self) -> bool {
        self.pending_audio_bytes >= self.warning_at_bytes
    }

    pub fn below_floor(&self) -> bool {
        self.free_disk_bytes
            .is_some_and(|free| free <= self.free_disk_floor_bytes)
    }

    /// Either hard stop: no new capture may start and a running one is stopped.
    pub fn blocking(&self) -> bool {
        self.over_budget() || self.below_floor()
    }

    pub fn level(&self) -> StorageLevel {
        if self.over_budget() {
            StorageLevel::Full
        } else if self.below_floor() {
            StorageLevel::DiskLow
        } else if self.near_limit() {
            StorageLevel::Warning
        } else {
            StorageLevel::Ok
        }
    }

    /// The one user-facing sentence for a warning. The hard stops are phrased by
    /// [`start_refusal`] and [`capture_stopped_detail`] instead, because they must
    /// also say what happened to the capture.
    pub fn warning_detail(&self) -> String {
        format!(
            "Pending capture audio is approaching the {} budget ({} already kept across all segments). At the budget, or when less than {} remains on the runtime disk, Listen stops starting new captures and keeps everything already received; delete segments you no longer need or raise the limit with --pending-audio-budget-mib.",
            human_bytes(self.budget_bytes),
            human_bytes(self.pending_audio_bytes),
            human_bytes(self.free_disk_floor_bytes)
        )
    }
}

/// Where the free space of the runtime volume comes from. Production reads the
/// operating system; tests inject a controlled value, so disk pressure is a state
/// and never a machine accident.
pub trait FreeSpace: Send + Sync {
    fn free_bytes(&self, directory: &Path) -> AppResult<u64>;
}

/// The real probe for the volume that holds the runtime directory.
pub struct DiskFreeSpace;

impl FreeSpace for DiskFreeSpace {
    fn free_bytes(&self, directory: &Path) -> AppResult<u64> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::PCWSTR};
            // The runtime directory may not exist yet; the volume of its nearest
            // existing ancestor is the same one.
            let mut existing = directory.to_path_buf();
            while !existing.exists() {
                match existing.parent() {
                    Some(parent) => existing = parent.to_path_buf(),
                    None => return Err("the runtime volume could not be identified".into()),
                }
            }
            let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut free = 0u64;
            let mut total = 0u64;
            unsafe {
                GetDiskFreeSpaceExW(
                    PCWSTR(wide.as_ptr()),
                    Some(&mut free as *mut u64),
                    None,
                    Some(&mut total as *mut u64),
                )
                .map_err(|error| {
                    format!(
                        "Windows could not report the free space of {}: {error}",
                        existing.display()
                    )
                })?;
            }
            Ok(free)
        }
        #[cfg(not(windows))]
        {
            let _ = directory;
            Err("Free disk space is only checked on Windows".into())
        }
    }
}

/// The result of the one on-disk walk behind the storage bounds: pending capture
/// audio across every segment and the saved complete text. It is plain data, so the
/// published [`StorageStatus`] stays a pure function of a cached measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeasuredBytes {
    pub pending_audio_bytes: u64,
    pub text_bytes: u64,
}

/// The runtime-directory walk behind [`MeasuredBytes`]. Production reads the disk;
/// tests inject a fake, so "a poll never measures" is a call count and storage
/// conditions stay a state instead of a machine accident.
pub trait MeasureBytes: Send + Sync {
    fn measure(&self, runtime: &Path) -> MeasuredBytes;
}

/// The real walk: every `session-*` tree (recursively, whatever its files are)
/// plus root-level capture files, and the saved text files beside them.
pub struct DiskBytes;

impl MeasureBytes for DiskBytes {
    fn measure(&self, runtime: &Path) -> MeasuredBytes {
        MeasuredBytes {
            pending_audio_bytes: pending_audio_bytes(runtime),
            text_bytes: text_bytes(runtime),
        }
    }
}

/// The application's storage policy plus the last fault it recorded.
pub(crate) struct Storage {
    policy: StoragePolicy,
    free_space: Arc<dyn FreeSpace>,
    bytes: Arc<dyn MeasureBytes>,
    fault: Option<String>,
}

impl Storage {
    pub fn new() -> Self {
        Self {
            policy: StoragePolicy::default(),
            free_space: Arc::new(DiskFreeSpace),
            bytes: Arc::new(DiskBytes),
            fault: None,
        }
    }

    pub fn set_policy(&mut self, policy: StoragePolicy) {
        self.policy = policy;
    }

    pub fn set_free_space(&mut self, free_space: Arc<dyn FreeSpace>) {
        self.free_space = free_space;
    }

    pub fn set_measure(&mut self, bytes: Arc<dyn MeasureBytes>) {
        self.bytes = bytes;
    }

    /// Record the last real storage write/save failure and return the user-facing
    /// form of it. Nothing here is ever phrased as a network wait.
    pub fn note_fault(&mut self, fault: String) -> String {
        self.fault = Some(fault.clone());
        fault
    }

    pub fn clear_fault(&mut self) {
        self.fault = None;
    }

    /// Measure the bounds now: one runtime-directory walk plus the free-space
    /// probe. This is the one measurement the published status comes from, and no
    /// `poll` asks for it (#49); the desktop caller asks for it explicitly or on a
    /// low-frequency tick.
    pub fn measure(&self, runtime: &Path) -> StorageStatus {
        let bytes = self.bytes.measure(runtime);
        let free_disk_bytes = self.free_space.free_bytes(runtime).ok();
        self.status(bytes, free_disk_bytes)
    }

    /// The pure judgement: a cached measurement plus the policy become the
    /// published status. It has no runtime path and performs no I/O, so a state-only
    /// tick can read the last measurement without walking the directory (#49).
    pub fn status(&self, bytes: MeasuredBytes, free_disk_bytes: Option<u64>) -> StorageStatus {
        StorageStatus {
            pending_audio_bytes: bytes.pending_audio_bytes,
            budget_bytes: self.policy.pending_audio_budget_bytes,
            warning_at_bytes: self.policy.warning_at_bytes(),
            free_disk_bytes,
            free_disk_floor_bytes: self.policy.free_disk_floor_bytes,
            text_bytes: bytes.text_bytes,
            text_budget_bytes: self.policy.text_budget_bytes,
            fault: self.fault.clone(),
        }
    }

    /// Refuse to save one complete result when the **text** bound is already
    /// exceeded. This is a separate bound from the audio budget: it never stops a
    /// capture, and the captured audio is kept because the text is not durable.
    ///
    /// It measures the saved text itself instead of reading the cached status: the
    /// bound is a write-time safety check (it decides whether *this* result may be
    /// written), so it must not act on a value measured up to a tick earlier. It runs
    /// once per saved result — not on a poll tick that saves nothing (#49).
    pub fn check_text_budget(&self, runtime: &Path, new_text: &str) -> Result<(), String> {
        let budget = self.policy.text_budget_bytes;
        let needed = text_bytes(runtime).saturating_add(new_text.len() as u64);
        if needed > budget {
            return Err(format!(
                "Storage fault (text): {} of saved text plus this complete result would need {}, over the separate {} text budget. The captured audio is kept on this machine because the text was not saved; nothing was written to the target.",
                human_bytes(text_bytes(runtime)),
                human_bytes(needed),
                human_bytes(budget)
            ));
        }
        Ok(())
    }
}

/// The reason a new capture was refused, if the current measurement blocks one.
/// It always says that nothing was deleted or overwritten.
pub(crate) fn start_refusal(status: &StorageStatus) -> Option<String> {
    if status.over_budget() {
        return Some(format!(
            "Storage is full: {} of pending capture audio is kept across all segments, at or over the {} budget. No new capture was started, and no previous recording, pending result or multi-segment record was deleted or overwritten. Delete segments you no longer need (tray menu 「清理待处理段」 or --control delete-segment:<n>) or raise the budget with --pending-audio-budget-mib.",
            human_bytes(status.pending_audio_bytes),
            human_bytes(status.budget_bytes)
        ));
    }
    if status.below_floor() {
        return Some(format!(
            "The runtime disk has only {} free, at or below the {} free-disk floor. No new capture was started, and every existing recording, pending result and multi-segment record is kept. This is a local storage condition, not a network wait or a playback-device interruption; free disk space and try again.",
            human_bytes(status.free_disk_bytes.unwrap_or(0)),
            human_bytes(status.free_disk_floor_bytes)
        ));
    }
    None
}

/// Why a running capture was stopped by the storage bounds, phrased so the user
/// knows the received audio is kept and captured audio is not being overwritten.
pub(crate) fn capture_stopped_detail(status: &StorageStatus) -> String {
    if status.over_budget() {
        format!(
            "The pending-audio storage budget was reached while this segment was being captured: {} is kept across all segments, at or over the {} budget. Capture was stopped; the playback audio already received is retained on this machine and is still processed when the recognition session confirms it. Every older segment and pending result is kept. This is a local storage limit, not a network wait; delete segments you no longer need or raise the budget before starting another capture.",
            human_bytes(status.pending_audio_bytes),
            human_bytes(status.budget_bytes)
        )
    } else {
        format!(
            "The runtime disk fell to {} free, at or below the {} free-disk floor, while this segment was being captured. Capture was stopped; the playback audio already received is retained on this machine and is still processed when the recognition session confirms it. Every older segment and pending result is kept. This is a local storage condition, not a network wait or a playback-device interruption.",
            human_bytes(status.free_disk_bytes.unwrap_or(0)),
            human_bytes(status.free_disk_floor_bytes)
        )
    }
}

/// Turn a known storage write/save failure into a user-facing sentence that is
/// unmistakably a storage fault. `None` means the message is not a known storage
/// failure and must keep its original text (for example a device interruption).
pub(crate) fn storage_fault_message(error: &str) -> Option<String> {
    if !describes_storage_fault(error) {
        return None;
    }
    let error = error.trim().trim_end_matches('.');
    if error.starts_with("Storage fault") {
        // Already phrased (for example the separate text bound); never double-wrap.
        return Some(error.to_string());
    }
    Some(format!(
        "Storage fault: {error}. This is a local storage failure, not a network wait and not a playback-device interruption. The captured audio is kept on this machine and nothing was written to the target."
    ))
}

/// Whether an error text describes a local storage failure. It covers the
/// application's own save path and the messages its adapters produce for the PCM
/// spool and window writer, plus the operating system's out-of-space errors. It is
/// deliberately narrow so a device or network message is never relabelled.
pub(crate) fn describes_storage_fault(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "storage fault",
        "storage limit",
        "storage budget",
        "could not save",
        "cannot persist",
        "cannot create retained",
        "cannot save pcm",
        "cannot write",
        "no space left",
        "not enough space",
        "disk is full",
        "os error 112",
        "os error 28",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

/// Every byte of pending capture audio under the runtime directory: all regular
/// files inside every `session-*` directory (recursively, whatever their names) plus
/// root-level `.wav`/`.pcm` capture files. Saved text, the segment index and logs are
/// excluded: they belong to their own bound.
pub(crate) fn pending_audio_bytes(runtime: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(runtime) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if name.starts_with(SESSION_PREFIX) {
            total = total.saturating_add(tree_bytes(&path));
        } else if path.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ROOT_AUDIO_EXTENSIONS
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
        {
            total = total.saturating_add(file_bytes(&path));
        }
    }
    total
}

/// Every byte of saved complete text the application keeps: one file per pending
/// result plus the current result. This is the input to the separate text bound.
pub(crate) fn text_bytes(runtime: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(runtime) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == CURRENT_RESULT || SegmentStore::is_text_name(&name)
        })
        .map(|entry| file_bytes(&entry.path()))
        .fold(0u64, u64::saturating_add)
}

fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // A directory entry's own size is not storage; only file content is.
            if path.is_dir() {
                pending.push(path);
            } else {
                total = total.saturating_add(file_bytes(&path));
            }
        }
    }
    total
}

fn file_bytes(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Runtime(PathBuf);
    impl Runtime {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "listen-storage-unit-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    fn write(path: PathBuf, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![3u8; bytes]).unwrap();
    }

    #[test]
    fn pending_audio_is_the_sum_of_every_session_file_and_excludes_saved_text() {
        let runtime = Runtime::new();
        let audio = runtime.0.join("session-1-1");
        write(audio.join("playback.pcm"), 100);
        write(audio.join("segment-00000000/playback.wav"), 200);
        write(runtime.0.join("session-2-2/upload-0001.part"), 300);
        // Saved text and the index are not audio.
        write(runtime.0.join("segment-00000001.txt"), 5_000);
        write(runtime.0.join("current-result.txt"), 4_000);
        write(runtime.0.join("segments.json"), 1_000);
        assert_eq!(pending_audio_bytes(&runtime.0), 600);
        assert_eq!(text_bytes(&runtime.0), 9_000);
    }

    #[test]
    fn a_missing_runtime_directory_measures_zero_instead_of_inventing_space() {
        let runtime = Runtime::new();
        let missing = runtime.0.join("not-created-yet");
        assert_eq!(pending_audio_bytes(&missing), 0);
        assert_eq!(text_bytes(&missing), 0);
    }

    #[test]
    fn the_warning_line_defaults_to_four_fifths_and_never_exceeds_the_budget() {
        let policy = StoragePolicy::with_budget_bytes(1_000);
        assert_eq!(policy.warning_at_bytes(), 800);
        let explicit = StoragePolicy {
            warning_at_bytes: Some(5_000),
            ..StoragePolicy::with_budget_bytes(1_000)
        };
        assert_eq!(explicit.warning_at_bytes(), 1_000);
        assert_eq!(StoragePolicy::default().warning_at_bytes(), 858_993_459);
    }

    #[test]
    fn the_two_hard_stops_are_independent_and_a_warning_is_neither() {
        let warning = StorageStatus {
            pending_audio_bytes: 850,
            budget_bytes: 1_000,
            warning_at_bytes: 800,
            free_disk_bytes: Some(10_000),
            free_disk_floor_bytes: 1_000,
            text_bytes: 0,
            text_budget_bytes: 100,
            fault: None,
        };
        assert_eq!(warning.level(), StorageLevel::Warning);
        assert!(!warning.blocking());
        assert!(warning.near_limit());

        let full = StorageStatus {
            pending_audio_bytes: 1_000,
            ..warning.clone()
        };
        assert_eq!(full.level(), StorageLevel::Full);
        assert!(full.blocking());
        assert!(!full.below_floor());

        let disk_low = StorageStatus {
            pending_audio_bytes: 10,
            free_disk_bytes: Some(1_000),
            ..warning.clone()
        };
        assert_eq!(disk_low.level(), StorageLevel::DiskLow);
        assert!(disk_low.blocking());
        assert!(!disk_low.over_budget());

        // An unreadable free-space probe is not "zero free".
        let unknown = StorageStatus {
            free_disk_bytes: None,
            ..warning
        };
        assert!(!unknown.below_floor());
        assert_eq!(unknown.level(), StorageLevel::Warning);
    }

    #[test]
    fn a_storage_fault_is_recognised_and_a_device_interruption_is_not() {
        assert!(describes_storage_fault(
            "Cannot persist received PCM; capture is incomplete: There is not enough space on the disk. (os error 112)"
        ));
        assert!(describes_storage_fault(
            "Could not save segment 3's complete result: Access is denied. (os error 5)"
        ));
        assert!(!describes_storage_fault(
            "Playback capture from the default multimedia render endpoint was interrupted; received audio is retained"
        ));
        assert!(!describes_storage_fault(
            "Waiting for the network; the received playback is saved on this machine"
        ));
        let message =
            storage_fault_message("Could not save the current result: disk full").unwrap();
        assert!(message.starts_with("Storage fault:"));
        assert!(message.contains("not a network wait"));
        assert!(message.contains("audio is kept"));
    }

    #[test]
    fn the_text_bound_is_separate_from_the_audio_budget() {
        let runtime = Runtime::new();
        let mut storage = Storage::new();
        storage.set_policy(StoragePolicy {
            pending_audio_budget_bytes: 0,
            text_budget_bytes: 20,
            ..StoragePolicy::default()
        });
        // The audio budget is already exceeded, but saving a short text is legal:
        // the two bounds never stand in for each other.
        write(runtime.0.join("session-1-1/playback.pcm"), 100);
        assert!(storage.check_text_budget(&runtime.0, "short").is_ok());
        let refused = storage
            .check_text_budget(
                &runtime.0,
                "this result is clearly longer than twenty bytes",
            )
            .expect_err("the text bound must refuse");
        assert!(refused.contains("Storage fault (text)"), "{refused}");
        assert!(refused.contains("separate"), "{refused}");
    }
}
