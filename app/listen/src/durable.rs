//! The shared plumbing behind the runtime directory's durable files.
//!
//! `segments.json` and `delivery.json` are both replaced atomically and both carry a
//! version that decides whether the record may be used at all. Keeping the open,
//! parse and version-check rules in one place makes the two records fail closed the
//! same way; the on-disk schemas and their messages stay in their own modules.
//!
//! The atomic replace lives here too. It is what "durable" means in this tool — the
//! complete bytes reach the disk before the old file is replaced, and a reader that
//! happens to hold the destination open cannot turn a proven write into a false
//! failure — so every module that stores a record (including the cloud trial
//! ledger and the received-audio descriptors) writes through [`atomic_write`]
//! instead of inventing its own replace.

use serde::de::DeserializeOwned;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Wall-clock time a record was last updated, for diagnostics and recovery ordering.
pub(crate) fn now_utc() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

/// A process-unique identifier, for a temporary file name or a cloud task identity.
pub(crate) fn unique_id() -> std::io::Result<String> {
    #[cfg(windows)]
    {
        let guid = unsafe { windows::Win32::System::Com::CoCreateGuid() }
            .map_err(std::io::Error::other)?;
        Ok(format!("{guid:?}"))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "{:032x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(std::io::Error::other)?
                .as_nanos()
        ))
    }
}

/// A stored record whose own field says which schema version it uses.
pub(crate) trait VersionedRecord: DeserializeOwned {
    const VERSION: u32;
    fn version(&self) -> u32;
}

/// Read one versioned record. `Ok(None)` means the file does not exist yet (an
/// ordinary first run); every unreadable, malformed or unsupported record is
/// reported as a problem and is never silently rewritten. `consequence` completes
/// the sentence with what the user loses while the record is degraded.
pub(crate) fn open_versioned<T: VersionedRecord>(
    path: &Path,
    label: &str,
    consequence: &str,
) -> (Option<T>, Option<String>) {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
            Ok(record) if record.version() == T::VERSION => (Some(record), None),
            Ok(record) => (
                None,
                Some(format!(
                    "{label} uses an unsupported version ({}){consequence}",
                    record.version()
                )),
            ),
            Err(error) => (
                None,
                Some(format!("{label} could not be read ({error}){consequence}")),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => (
            None,
            Some(format!("{label} could not be read ({error}){consequence}")),
        ),
    }
}

/// Replace one file with the complete bytes, durably.
///
/// The bytes are written to a process-unique temporary file next to the
/// destination, flushed to the disk, and only then renamed over it. A crash before
/// the rename leaves the old file; a crash after it leaves the new one. No reader
/// ever sees a half-written record.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", unique_id()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| replace_file(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Replace the destination with the already synced temporary file.
///
/// On Windows `MoveFileExW(REPLACE_EXISTING)` can fail with a sharing violation
/// while another process (or a reader that is still closing) holds the destination
/// open. The delivery record and `current-result.txt` are read by a recovery scan
/// and by the verification scripts, so a transient reader must not turn a proven
/// append into a fail-closed "the delivery record could not be saved" error. The
/// retry only repeats the atomic rename: the write and its `sync_all` already
/// succeeded, and a failed rename never partially replaces the destination. Any
/// error other than the two transient sharing codes is returned immediately.
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::{os::windows::ffi::OsStrExt, thread, time::Duration};
        use windows::{
            Win32::{
                Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION},
                Storage::FileSystem::{
                    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                },
            },
            core::PCWSTR,
        };
        const ATTEMPTS: usize = 3;
        const RETRY: Duration = Duration::from_millis(2);
        let transient = |error: &windows::core::Error| {
            let denied: windows::core::HRESULT = ERROR_ACCESS_DENIED.into();
            let sharing: windows::core::HRESULT = ERROR_SHARING_VIOLATION.into();
            let code = error.code();
            code == denied || code == sharing
        };
        let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        for attempt in 1..=ATTEMPTS {
            let result = unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(destination.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) if attempt < ATTEMPTS && transient(&error) => {
                    thread::sleep(RETRY);
                }
                Err(error) => return Err(std::io::Error::other(error)),
            }
        }
        unreachable!("the bounded replace loop always returns")
    }
    #[cfg(not(windows))]
    {
        fs::rename(temporary, path)
    }
}
