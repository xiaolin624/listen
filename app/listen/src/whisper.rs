//! Local whisper.cpp CLI adapter. No networking or alternative recognizer exists.
use crate::{AppResult, CapturedAudio, LocalTranscriber};
use std::{
    fs::{self, File},
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct Whisper {
    pub executable: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub threads: usize,
}

impl LocalTranscriber for Whisper {
    fn check_ready(&self) -> AppResult<()> {
        if !self.executable.is_file() {
            return Err(format!(
                "Local whisper.cpp executable is missing: {}",
                self.executable.display()
            ));
        }
        let mut model = File::open(&self.model)
            .map_err(|e| format!("Local model is unavailable ({}): {e}", self.model.display()))?;
        let mut header = [0; 4];
        model
            .read_exact(&mut header)
            .map_err(|e| format!("Local model is empty or unreadable: {e}"))?;
        if u32::from_le_bytes(header) != 0x67676d6c {
            return Err(format!(
                "Local model is not a whisper.cpp ggml model: {}",
                self.model.display()
            ));
        }
        let status = Command::new(&self.executable)
            .arg("--help")
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Cannot start local whisper.cpp backend: {e}"))?;
        if !status.success() {
            return Err(format!(
                "Local whisper.cpp backend is unavailable ({status}); check its adjacent DLLs."
            ));
        }
        Ok(())
    }

    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String> {
        let stem = directory.join("transcript");
        let log_path = directory.join("whisper.log");
        let log =
            File::create(&log_path).map_err(|e| format!("Cannot create local backend log: {e}"))?;
        let status = Command::new(&self.executable)
            .args(["-m"])
            .arg(&self.model)
            .arg("-f")
            .arg(&audio.path)
            .args([
                "-l",
                &self.language,
                "-t",
                &self.threads.to_string(),
                "-nt",
                "-otxt",
                "-of",
            ])
            .arg(&stem)
            // CPU is explicit for this first integration. CUDA selection/performance is later work.
            .arg("-ng")
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .status()
            .map_err(|e| {
                format!(
                    "Cannot start local whisper.cpp backend: {e}. Log: {}",
                    log_path.display()
                )
            })?;
        if !status.success() {
            return Err(format!(
                "Local whisper.cpp recognition failed ({status}). Log: {}",
                log_path.display()
            ));
        }
        let text = fs::read_to_string(stem.with_extension("txt")).map_err(|e| {
            format!(
                "Local backend did not produce a complete UTF-8 result: {e}. Log: {}",
                log_path.display()
            )
        })?;
        Ok(text.trim().to_string())
    }
}
