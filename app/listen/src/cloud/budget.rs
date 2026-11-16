//! One ledger for the entire two-provider trial, independently of runtime paths.
//! Only verified Token bounds or checked free-only protection admit a task.
use super::{AliSettings, CloudError, FreeOnlyPolicy, TrialCostPolicy, VerifiedBounds};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub const TRIAL_CAP_NANOYUAN: u64 = 50_000_000_000;
const INPUT_NANOYUAN_PER_TOKEN: u64 = 6_000;
const OUTPUT_NANOYUAN_PER_TOKEN: u64 = 4_500;

#[derive(Clone)]
pub struct TrialLedger {
    directory: PathBuf,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub duration: Option<f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub task_id: String,
    pub segment_id: String,
    pub provider: String,
    pub model: String,
    pub region: String,
    pub reserved_nanoyuan: u64,
    pub estimated_nanoyuan: Option<u64>,
    pub billed_nanoyuan: Option<u64>,
    pub usage: Usage,
    pub reported_final_usage: Option<Usage>,
    pub final_usage: bool,
    pub unknown_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<VerifiedBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_only: Option<FreeOnlyPolicy>,
    pub price_source: String,
    pub price_verified_on: String,
}
#[derive(Clone, Debug)]
pub struct LedgerSnapshot {
    pub committed_nanoyuan: u64,
    pub cap_nanoyuan: u64,
    pub frozen: bool,
    pub attempts: Vec<Attempt>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Data {
    version: u32,
    cap_nanoyuan: u64,
    frozen: bool,
    attempts: Vec<Attempt>,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            version: 1,
            cap_nanoyuan: TRIAL_CAP_NANOYUAN,
            frozen: false,
            attempts: vec![],
        }
    }
}
struct Lock {
    _file: File,
    #[cfg(not(windows))]
    path: PathBuf,
}
#[cfg(not(windows))]
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
impl TrialLedger {
    /// An explicit path is for controlled tests and future providers. Production
    /// always uses `shared_local`, never the selectable recording runtime.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn shared_local() -> Result<Self, CloudError> {
        let root = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .ok_or(CloudError::Ledger)?;
        Ok(Self::new(PathBuf::from(root).join("Listen/cloud-trial")))
    }
    fn lock(&self) -> Result<Lock, CloudError> {
        fs::create_dir_all(&self.directory).map_err(|_| CloudError::Ledger)?;
        let path = self.directory.join("ledger.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.create(true).share_mode(0);
        }
        #[cfg(not(windows))]
        {
            options.create_new(true);
        }
        let _file = options.open(&path).map_err(|_| CloudError::LedgerBusy)?;
        Ok(Lock {
            _file,
            #[cfg(not(windows))]
            path,
        })
    }
    fn read(&self) -> Result<Data, CloudError> {
        let path = self.directory.join("ledger.json");
        match fs::read(&path) {
            Ok(bytes) if bytes.len() <= 8 * 1024 * 1024 => {
                let data: Data = serde_json::from_slice(&bytes).map_err(|_| CloudError::Ledger)?;
                if data.version != 1
                    || data.cap_nanoyuan != TRIAL_CAP_NANOYUAN
                    || data.attempts.len() > 10_000
                    || data.attempts.iter().any(|attempt| !valid_policy(attempt))
                {
                    return Err(CloudError::Ledger);
                }
                Ok(data)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Data::default()),
            _ => Err(CloudError::Ledger),
        }
    }
    fn write(&self, data: &Data) -> Result<(), CloudError> {
        let bytes = serde_json::to_vec_pretty(data).map_err(|_| CloudError::Ledger)?;
        atomic_write(&self.directory.join("ledger.json"), &bytes).map_err(|_| CloudError::Ledger)
    }
    pub fn snapshot(&self) -> Result<LedgerSnapshot, CloudError> {
        let _lock = self.lock()?;
        let data = self.read()?;
        Ok(LedgerSnapshot {
            committed_nanoyuan: committed(&data)?,
            cap_nanoyuan: data.cap_nanoyuan,
            frozen: data.frozen,
            attempts: data.attempts,
        })
    }
    pub(crate) fn reserve(
        &self,
        task_id: &str,
        segment_id: &str,
        settings: &AliSettings,
        policy: TrialCostPolicy,
    ) -> Result<(), CloudError> {
        let (amount, bounds, free_only, price_source, price_verified_on) = match policy {
            TrialCostPolicy::TokenBounds(bounds) => {
                let amount = token_reservation(&bounds)?;
                (amount, Some(bounds), None,
                 "https://help.aliyun.com/zh/model-studio/model-pricing (input CNY 6 / million tokens; output CNY 4.5 / million tokens)".to_owned(),
                 "2026-11-14".to_owned())
            }
            TrialCostPolicy::FreeOnly(protection) => {
                validate_free_only(&protection, &settings.model, &settings.region)?;
                let verified_on = protection.verified_on.clone();
                (0, None, Some(protection),
                 "https://help.aliyun.com/zh/model-studio/new-free-quota/ (active model-scoped free-only protection)".to_owned(),
                 verified_on)
            }
        };
        let _lock = self.lock()?;
        let mut data = self.read()?;
        if data.frozen {
            return Err(CloudError::UsageMismatch);
        }
        if committed(&data)?
            .checked_add(amount)
            .ok_or(CloudError::Ledger)?
            > TRIAL_CAP_NANOYUAN
        {
            return Err(CloudError::BudgetExhausted);
        }
        if data.attempts.iter().any(|a| a.task_id == task_id) {
            return Err(CloudError::Ledger);
        }
        let estimated_nanoyuan = free_only.as_ref().map(|_| 0);
        let unknown_reason = if free_only.is_some() {
            "Provider usage and the actual billed charge have not been verified; active free-only protection limits additional cash charges to zero"
        } else {
            "Final cumulative provider usage has not been confirmed; the full reservation remains committed"
        };
        data.attempts.push(Attempt {
            task_id: task_id.into(),
            segment_id: segment_id.into(),
            provider: "alibaba-model-studio".into(),
            model: settings.model.clone(),
            region: settings.region.clone(),
            reserved_nanoyuan: amount,
            estimated_nanoyuan,
            billed_nanoyuan: None,
            usage: Usage::default(),
            reported_final_usage: None,
            final_usage: false,
            unknown_reason: Some(unknown_reason.into()),
            bounds,
            free_only,
            price_source,
            price_verified_on,
        });
        self.write(&data)
    }
    pub(crate) fn observe(
        &self,
        task_id: &str,
        usage: Usage,
        final_usage: bool,
    ) -> Result<(), CloudError> {
        let _lock = self.lock()?;
        let mut data = self.read()?;
        let attempt = data
            .attempts
            .iter_mut()
            .find(|a| a.task_id == task_id)
            .ok_or(CloudError::Ledger)?;
        let previous = attempt.usage.clone();
        let complete = usage
            .input_tokens
            .zip(usage.output_tokens)
            .zip(usage.total_tokens);
        let within_bounds = |input, output| {
            attempt.bounds.as_ref().is_none_or(|bounds| {
                input <= bounds.max_input_tokens && output <= bounds.max_output_tokens
            })
        };
        let mismatch = attempt.bounds.as_ref().is_some_and(|bounds| {
            usage
                .input_tokens
                .is_some_and(|v| v > bounds.max_input_tokens)
                || usage
                    .output_tokens
                    .is_some_and(|v| v > bounds.max_output_tokens)
        }) || complete
            .is_some_and(|((input, output), total)| input.checked_add(output) != Some(total))
            || (final_usage
                && complete.is_some_and(|((input, output), total)| {
                    previous.input_tokens.is_some_and(|v| input < v)
                        || previous.output_tokens.is_some_and(|v| output < v)
                        || previous.total_tokens.is_some_and(|v| total < v)
                }));
        attempt.usage.input_tokens = maximum(previous.input_tokens, usage.input_tokens);
        attempt.usage.output_tokens = maximum(previous.output_tokens, usage.output_tokens);
        attempt.usage.total_tokens = maximum(previous.total_tokens, usage.total_tokens);
        attempt.usage.duration = match (previous.duration, usage.duration) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if final_usage {
            attempt.reported_final_usage = Some(usage.clone());
            if let (Some(input), Some(output), Some(total)) =
                (usage.input_tokens, usage.output_tokens, usage.total_tokens)
            {
                if input.checked_add(output) == Some(total)
                    && within_bounds(input, output)
                    && previous.input_tokens.is_none_or(|v| input >= v)
                    && previous.output_tokens.is_none_or(|v| output >= v)
                    && previous.total_tokens.is_none_or(|v| total >= v)
                {
                    attempt.estimated_nanoyuan = Some(if attempt.free_only.is_some() {
                        0
                    } else {
                        price(input, output)?
                    });
                    attempt.final_usage = true;
                    attempt.unknown_reason = attempt.free_only.as_ref().map(|_| {
                        "Actual billed charge has not been verified; active free-only protection limits additional cash charges to zero".into()
                    });
                }
            }
        }
        if mismatch {
            attempt.final_usage = false;
            attempt.estimated_nanoyuan = None;
            attempt.unknown_reason = Some("Usage contradicted the verified contract; the full reservation remains held pending billing review".into());
            data.frozen = true;
        }
        self.write(&data)?;
        if mismatch {
            Err(CloudError::UsageMismatch)
        } else {
            Ok(())
        }
    }
}
fn token_reservation(bounds: &VerifiedBounds) -> Result<u64, CloudError> {
    let amount = price(bounds.max_input_tokens, bounds.max_output_tokens)?;
    if amount == 0
        || bounds.max_pcm_bytes == 0
        || bounds.max_pcm_bytes % 2 != 0
        || bounds.evidence.trim().is_empty()
        || bounds.verified_on.trim().is_empty()
    {
        return Err(CloudError::BudgetUnknown);
    }
    Ok(amount)
}
fn validate_free_only(
    protection: &FreeOnlyPolicy,
    model: &str,
    region: &str,
) -> Result<(), CloudError> {
    if protection.model != model
        || protection.region != region
        || protection.evidence.trim().is_empty()
        || protection.verified_on.trim().is_empty()
    {
        return Err(CloudError::BudgetUnknown);
    }
    Ok(())
}
fn valid_policy(attempt: &Attempt) -> bool {
    match (&attempt.bounds, &attempt.free_only) {
        // Version 1 TokenBounds records predate the optional free_only field.
        (Some(bounds), None) => {
            token_reservation(bounds).is_ok_and(|amount| amount == attempt.reserved_nanoyuan)
        }
        (None, Some(protection)) => {
            validate_free_only(protection, &attempt.model, &attempt.region).is_ok()
                && attempt.reserved_nanoyuan == 0
                && attempt.estimated_nanoyuan.is_none_or(|amount| amount == 0)
                && attempt.billed_nanoyuan.is_none_or(|amount| amount == 0)
        }
        _ => false,
    }
}
fn maximum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}
fn price(input: u64, output: u64) -> Result<u64, CloudError> {
    input
        .checked_mul(INPUT_NANOYUAN_PER_TOKEN)
        .and_then(|i| {
            output
                .checked_mul(OUTPUT_NANOYUAN_PER_TOKEN)
                .and_then(|o| i.checked_add(o))
        })
        .ok_or(CloudError::BudgetUnknown)
}
fn committed(data: &Data) -> Result<u64, CloudError> {
    data.attempts.iter().try_fold(0u64, |sum, a| {
        sum.checked_add(if a.final_usage {
            a.estimated_nanoyuan.ok_or(CloudError::Ledger)?
        } else {
            a.reserved_nanoyuan
        })
        .ok_or(CloudError::Ledger)
    })
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!(
        "{}.tmp",
        super::task_id().map_err(std::io::Error::other)?
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                use windows::{
                    Win32::Storage::FileSystem::{
                        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                    },
                    core::PCWSTR,
                };
                let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
                let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                unsafe {
                    MoveFileExW(
                        PCWSTR(source.as_ptr()),
                        PCWSTR(destination.as_ptr()),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                }
                .map_err(std::io::Error::other)
            }
            #[cfg(not(windows))]
            {
                fs::rename(&temporary, path)
            }
        });
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
