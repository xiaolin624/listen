//! Live, read-only checks using the existing logged-in CLI. This never changes
//! protection, reads a key, starts a model task or accepts a local verified flag.
use super::{ALI_MODEL, AliSettings, CloudError, FreeOnlyPolicy};
use serde_json::Value;
use std::{
    io::Read,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
                THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
            },
        },
    },
    core::PCWSTR,
};

const MAX_OUTPUT: u64 = 64 * 1024;
const QUERY_DEADLINE: Duration = Duration::from_secs(12);

/// The subprocess is an external dependency; fixtures replace only its outputs.
pub trait FreeOnlyStatusSource: Send + Sync {
    fn checked_at(&self) -> Result<SystemTime, CloudError> {
        Ok(SystemTime::now())
    }
    fn workspaces(&self) -> Result<Vec<u8>, CloudError>;
    fn quota(&self) -> Result<Vec<u8>, CloudError>;
}
pub(super) struct CliStatusSource;
impl FreeOnlyStatusSource for CliStatusSource {
    fn workspaces(&self) -> Result<Vec<u8>, CloudError> {
        query(&["workspace", "list"])
    }
    fn quota(&self) -> Result<Vec<u8>, CloudError> {
        query(&["usage", "free", "--model", ALI_MODEL])
    }
}

pub(super) fn verify(
    source: &dyn FreeOnlyStatusSource,
    settings: &AliSettings,
) -> Result<FreeOnlyPolicy, CloudError> {
    settings.validate()?;
    let workspaces = source.workspaces()?;
    let workspaces = parse(&workspaces)?;
    let entries = workspaces
        .as_array()
        .ok_or(CloudError::ProtectionUnavailable)?;
    let mut matching = 0;
    let mut defaults = 0;
    for entry in entries {
        let id = entry["workspaceId"]
            .as_str()
            .ok_or(CloudError::ProtectionUnavailable)?;
        let is_default = entry["default"]
            .as_bool()
            .ok_or(CloudError::ProtectionUnavailable)?;
        matching += usize::from(id == settings.workspace_id);
        defaults += usize::from(is_default);
        if is_default && id != settings.workspace_id {
            return Err(CloudError::ProtectionUnavailable);
        }
    }
    if matching != 1 || defaults != 1 {
        return Err(CloudError::ProtectionUnavailable);
    }
    let quota = single_record(source.quota()?)?;
    let now = source
        .checked_at()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CloudError::ProtectionUnavailable)?
        .as_secs();
    let expires = quota["expires"]
        .as_str()
        .and_then(date)
        .ok_or(CloudError::ProtectionUnavailable)?;
    let remaining = quota["remaining"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or(CloudError::ProtectionUnavailable)?;
    let total = quota["total"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or(CloudError::ProtectionUnavailable)?;
    if quota["model"].as_str() != Some(ALI_MODEL)
        || quota["type"].as_str() != Some("Audio")
        || quota["autoStop"].as_bool() != Some(true)
        || remaining > total
        // CLI exposes a date, not an expiry instant. Keep at least two Beijing
        // dates for the 1 GiB capture allowance and its bounded finalization.
        || expires <= beijing_date(now.checked_add(86400).ok_or(CloudError::ProtectionUnavailable)?)?
    {
        return Err(CloudError::ProtectionUnavailable);
    }
    Ok(FreeOnlyPolicy {
        model: settings.model.clone(),
        region: settings.region.clone(),
        evidence: "Live Beijing domestic CLI: unique matching default workspace; positive unexpired exact-model quota; autoStop=true. The actual key is authenticated by the selected workspace-exclusive WSS host before run-task. https://help.aliyun.com/zh/model-studio/new-free-quota/ ; https://help.aliyun.com/zh/model-studio/base-url ; https://help.aliyun.com/zh/model-studio/qwen-asr-message-websocket-api".into(),
        verified_on: format!("live read at Unix {now}"),
    })
}
fn parse(bytes: &[u8]) -> Result<Value, CloudError> {
    if bytes.len() as u64 > MAX_OUTPUT {
        return Err(CloudError::ProtectionUnavailable);
    }
    serde_json::from_slice(bytes).map_err(|_| CloudError::ProtectionUnavailable)
}
/// The installed CLI prints a one-element array for a single-model free quota.
/// Accept that real shape (and a bare object for compatibility), but never pick
/// among several records: an ambiguous answer must not authorize an upload.
fn single_record(bytes: Vec<u8>) -> Result<Value, CloudError> {
    match parse(&bytes)? {
        Value::Array(mut items) if items.len() == 1 => {
            items.pop().ok_or(CloudError::ProtectionUnavailable)
        }
        Value::Array(_) => Err(CloudError::ProtectionUnavailable),
        record => Ok(record),
    }
}
fn leap(year: u32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
fn month_days(year: u32, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if leap(year) { 29 } else { 28 }),
        _ => None,
    }
}
fn date(text: &str) -> Option<(u32, u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year = text[..4].parse().ok()?;
    let month = text[5..7].parse().ok()?;
    let day = text[8..].parse().ok()?;
    (year >= 1970 && day > 0 && day <= month_days(year, month)?).then_some((year, month, day))
}
fn beijing_date(seconds: u64) -> Result<(u32, u32, u32), CloudError> {
    let mut days = seconds
        .checked_add(8 * 3600)
        .ok_or(CloudError::ProtectionUnavailable)?
        / 86400;
    let mut year = 1970;
    while year < 9999 {
        let count = if leap(year) { 366 } else { 365 };
        if days < count {
            break;
        }
        days -= count;
        year += 1;
    }
    let mut month = 1;
    while month <= 12 {
        let count = u64::from(month_days(year, month).ok_or(CloudError::ProtectionUnavailable)?);
        if days < count {
            return Ok((year, month, days as u32 + 1));
        }
        days -= count;
        month += 1;
    }
    Err(CloudError::ProtectionUnavailable)
}

fn query(arguments: &[&str]) -> Result<Vec<u8>, CloudError> {
    let mut child = Command::new("cmd.exe")
        .args(["/d", "/s", "/c", "bl.cmd"])
        .args(arguments)
        .args([
            "--console-region",
            "cn-beijing",
            "--console-site",
            "domestic",
            "--output",
            "json",
            "--timeout",
            "10",
        ])
        .env("BAILIAN_CALLER_TYPE", "script")
        .env("BAILIAN_CALLER_NAME", "codex")
        .env("NO_COLOR", "1")
        .env_remove("BAILIAN_SKILL_NAME")
        .env_remove("BAILIAN_SKILL_VERSION")
        .env_remove("LISTEN_ALI_API_KEY")
        .env_remove("BAILIAN_WORKSPACE_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // No CLI code can spawn an unowned descendant before Job assignment.
        .creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0)
        .spawn()
        .map_err(|_| CloudError::ProtectionUnavailable)?;
    let job = match QueryJob::new(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.try_wait();
            return Err(error);
        }
    };
    if let Err(error) = resume_query(&child) {
        drop(job);
        let _ = child.kill();
        let _ = child.try_wait();
        return Err(error);
    }
    let output = child
        .stdout
        .take()
        .ok_or(CloudError::ProtectionUnavailable)?;
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|_| CloudError::ProtectionUnavailable);
        let _ = send.send(result);
    });
    let started = Instant::now();
    let result = (|| {
        let mut output = None;
        loop {
            if let Ok(bytes) = receive.try_recv() {
                let bytes = bytes?;
                if bytes.len() as u64 > MAX_OUTPUT {
                    return Err(CloudError::ProtectionUnavailable);
                }
                output = Some(bytes);
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|_| CloudError::ProtectionUnavailable)?
            {
                if !status.success() {
                    return Err(CloudError::ProtectionUnavailable);
                }
                // The command has exited. Terminate any remaining descendants
                // before waiting for pipe EOF; they may still own inherited
                // stdout handles even though the shell returned successfully.
                unsafe {
                    let _ = TerminateJobObject(job.0, 1);
                }
                let bytes = match output {
                    Some(bytes) => bytes,
                    None => receive
                        .recv_timeout(Duration::from_millis(250))
                        .map_err(|_| CloudError::ProtectionUnavailable)??,
                };
                if bytes.len() as u64 > MAX_OUTPUT {
                    return Err(CloudError::ProtectionUnavailable);
                }
                return Ok(bytes);
            }
            if started.elapsed() >= QUERY_DEADLINE {
                return Err(CloudError::ProtectionUnavailable);
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    drop(job); // Kill the command tree on every exit, including timeout/overflow.
    let _ = child.try_wait();
    result
}
struct QueryJob(HANDLE);
struct QueryHandle(HANDLE);
impl Drop for QueryHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
fn resume_query(child: &Child) -> Result<(), CloudError> {
    let snapshot = QueryHandle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
            .map_err(|_| CloudError::ProtectionUnavailable)?,
    );
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe { Thread32First(snapshot.0, &mut entry) }
        .map_err(|_| CloudError::ProtectionUnavailable)?;
    let mut primary = None;
    loop {
        if entry.th32OwnerProcessID == child.id() {
            if primary.replace(entry.th32ThreadID).is_some() {
                // A new suspended command must have exactly one main thread.
                return Err(CloudError::ProtectionUnavailable);
            }
        }
        match unsafe { Thread32Next(snapshot.0, &mut entry) } {
            Ok(()) => (),
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(_) => return Err(CloudError::ProtectionUnavailable),
        }
    }
    let thread = QueryHandle(
        unsafe {
            OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                false,
                primary.ok_or(CloudError::ProtectionUnavailable)?,
            )
        }
        .map_err(|_| CloudError::ProtectionUnavailable)?,
    );
    // Verify ownership again after opening the enumerated thread ID. Never
    // resume an ambiguous/reused thread, or run before the Job is assigned.
    if unsafe { GetProcessIdOfThread(thread.0) } != child.id()
        || unsafe { ResumeThread(thread.0) } != 1
    {
        return Err(CloudError::ProtectionUnavailable);
    }
    Ok(())
}
impl QueryJob {
    fn new(child: &Child) -> Result<Self, CloudError> {
        let job = Self(
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .map_err(|_| CloudError::ProtectionUnavailable)?,
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(|_| CloudError::ProtectionUnavailable)?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle()))
                .map_err(|_| CloudError::ProtectionUnavailable)?;
        }
        Ok(job)
    }
}
impl Drop for QueryJob {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
            let _ = CloseHandle(self.0);
        }
    }
}
