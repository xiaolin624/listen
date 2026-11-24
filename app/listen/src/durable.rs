//! The small shared plumbing behind the runtime directory's versioned JSON records.
//!
//! `segments.json` and `delivery.json` are both replaced atomically and both carry a
//! version that decides whether the record may be used at all. Keeping the open,
//! parse and version-check rules in one place makes the two records fail closed the
//! same way; the on-disk schemas and their messages stay in their own modules.

use serde::de::DeserializeOwned;
use std::{
    fs,
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
