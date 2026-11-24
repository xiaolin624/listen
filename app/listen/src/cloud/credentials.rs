//! Current-user Windows Credential Manager. The JSON contains only nonsecrets.
use super::{AliSettings, CloudError, CredentialSource, Secret};
use std::{fs, path::PathBuf};
use windows::{
    Win32::{
        Foundation::ERROR_NOT_FOUND,
        Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW},
    },
    core::{HRESULT, PCWSTR},
};

pub const CREDENTIAL_TARGET: &str = "Listen/CloudTrial/Alibaba/cn-beijing";
pub struct LocalCredentials;
impl CredentialSource for LocalCredentials {
    fn load(&self) -> Result<Secret, CloudError> {
        // Explicit process injection wins; an invalid/empty injection must not
        // silently select another credential. Values are never formatted/logged.
        match std::env::var("LISTEN_ALI_API_KEY") {
            Ok(key) => return Secret::new(key),
            Err(std::env::VarError::NotUnicode(_)) => return Err(CloudError::Credential),
            Err(std::env::VarError::NotPresent) => {}
        }
        let target: Vec<u16> = CREDENTIAL_TARGET.encode_utf16().chain(Some(0)).collect();
        let mut credential = std::ptr::null_mut::<CREDENTIALW>();
        if let Err(error) = unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
        } {
            return Err(if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
                CloudError::MissingKey
            } else {
                CloudError::Credential
            });
        }
        struct Buffer(*mut CREDENTIALW);
        impl Drop for Buffer {
            fn drop(&mut self) {
                unsafe {
                    CredFree(self.0.cast());
                }
            }
        }
        let buffer = Buffer(credential);
        if buffer.0.is_null() {
            return Err(CloudError::Credential);
        }
        let credential = unsafe { &*buffer.0 };
        if credential.CredentialBlobSize == 0
            || credential.CredentialBlobSize > 4096
            || credential.CredentialBlob.is_null()
        {
            return Err(CloudError::Credential);
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
        };
        Secret::new(String::from_utf8(bytes.to_vec()).map_err(|_| CloudError::Credential)?)
    }
}
pub fn local_settings() -> Result<AliSettings, CloudError> {
    let root = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .ok_or(CloudError::Configuration)?;
    let path = PathBuf::from(root).join("Listen/cloud-trial/ali.json");
    let mut settings = match fs::read(&path) {
        Ok(bytes) if bytes.len() < 16 * 1024 => {
            serde_json::from_slice(&bytes).map_err(|_| CloudError::Configuration)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AliSettings::beijing(String::new()),
        _ => return Err(CloudError::Configuration),
    };
    for (name, field) in [
        ("LISTEN_ALI_WORKSPACE_ID", &mut settings.workspace_id),
        ("LISTEN_ALI_MODEL", &mut settings.model),
        ("LISTEN_ALI_REGION", &mut settings.region),
    ] {
        if let Some(value) = std::env::var_os(name) {
            *field = value.into_string().map_err(|_| CloudError::Configuration)?;
        }
    }
    Ok(settings)
}
