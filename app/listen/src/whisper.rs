//! Local whisper.cpp CLI adapter. No networking or alternative recognizer exists.
use crate::{AppResult, CapturedAudio, LocalTranscriber, TimedText};
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
    pub vad_model: PathBuf,
    pub language: String,
    pub threads: usize,
}

impl Whisper {
    fn vad_executable(&self) -> PathBuf {
        self.executable.with_file_name("vad-speech-segments.exe")
    }

    fn has_speech(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<bool> {
        let wav = hound::WavReader::open(&audio.path)
            .map_err(|e| format!("Cannot read captured WAV for local speech detection: {e}"))?;
        // WASAPI can legitimately return no packets while the render device is idle.
        // The standalone VAD CLI rejects a zero-frame WAV, so finish it explicitly.
        if wav.len() == 0 {
            return Ok(false);
        }
        let output_path = directory.join("vad.txt");
        let log_path = directory.join("vad.log");
        let output =
            File::create(&output_path).map_err(|e| format!("Cannot create VAD result: {e}"))?;
        let log = File::create(&log_path).map_err(|e| format!("Cannot create VAD log: {e}"))?;
        let status = Command::new(self.vad_executable())
            .arg("-vm")
            .arg(&self.vad_model)
            .arg("-f")
            .arg(&audio.path)
            // v1.8.2's standalone CLI has a -vspd/-vsd parser bug. Keep its
            // documented defaults: speech >=250ms, silence >=100ms. VAD only
            // gates the whole window; decoding receives every original sample.
            .args([
                "-t",
                &self.threads.to_string(),
                "-vt",
                "0.5",
                "-vp",
                "30",
                "-vo",
                "0.1",
                "-np",
            ])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(log))
            .status()
            .map_err(|e| {
                format!(
                    "Cannot start local speech detector: {e}. Log: {}",
                    log_path.display()
                )
            })?;
        if !status.success() {
            return Err(format!(
                "Local speech detection failed ({status}). Log: {}",
                log_path.display()
            ));
        }
        let result = fs::read_to_string(&output_path)
            .map_err(|e| format!("Cannot read local speech detection result: {e}"))?;
        let mut lines = result.lines().filter(|line| !line.trim().is_empty());
        let count = lines.next()
            .and_then(|line| line.strip_prefix("Detected "))
            .and_then(|line| line.strip_suffix(" speech segments:"))
            .and_then(|count| count.parse::<usize>().ok())
            .ok_or_else(|| format!("Local speech detector returned an unrecognized result; audio was not treated as silence. Log: {}", log_path.display()))?;
        let segments: Vec<_> = lines.collect();
        if segments.len() != count
            || segments
                .iter()
                .enumerate()
                .any(|(i, line)| !line.starts_with(&format!("Speech segment {i}: start = ")))
        {
            return Err(format!(
                "Local speech detector returned incomplete segments; audio was not treated as silence. Log: {}",
                log_path.display()
            ));
        }
        Ok(count > 0)
    }

    /// Run the local decoder for one captured window and return its output stem.
    /// Full JSON adds the per-token byte fragments and timestamps that overlap
    /// ownership needs; plain text is enough for a single whole-file result.
    fn recognize(
        &self,
        audio: &CapturedAudio,
        directory: &Path,
        full_json: bool,
    ) -> AppResult<PathBuf> {
        let stem = directory.join("transcript");
        let log_path = directory.join("whisper.log");
        let log =
            File::create(&log_path).map_err(|e| format!("Cannot create local backend log: {e}"))?;
        let mut command = Command::new(&self.executable);
        command
            .args(["-m"])
            .arg(&self.model)
            .arg("-f")
            .arg(&audio.path)
            .args([
                "-l",
                &self.language,
                "-t",
                &self.threads.to_string(),
                "-bs",
                "5",
                "-bo",
                "5",
                "-tp",
                "0",
                "-tpi",
                "0.2",
                "-nth",
                "0.6",
                "-lpt",
                "-1",
                "-et",
                "2.4",
                "-nt",
            ]);
        command.arg(if full_json { "-ojf" } else { "-otxt" });
        let status = command
            .arg("-of")
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
        Ok(stem)
    }
}

impl LocalTranscriber for Whisper {
    fn check_ready(&self) -> AppResult<()> {
        if !self.executable.is_file() {
            return Err(format!(
                "Local whisper.cpp executable is missing: {}",
                self.executable.display()
            ));
        }
        for path in [&self.model, &self.vad_model] {
            let mut model = File::open(path)
                .map_err(|e| format!("Local model is unavailable ({}): {e}", path.display()))?;
            let mut header = [0; 4];
            model.read_exact(&mut header).map_err(|e| {
                format!(
                    "Local model is empty or unreadable ({}): {e}",
                    path.display()
                )
            })?;
            if u32::from_le_bytes(header) != 0x67676d6c {
                return Err(format!(
                    "Local model is not a whisper.cpp ggml model: {}",
                    path.display()
                ));
            }
        }
        for executable in [&self.executable, &self.vad_executable()] {
            let status = Command::new(executable)
                .arg("--help")
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| {
                    format!(
                        "Cannot start local whisper.cpp backend ({}): {e}",
                        executable.display()
                    )
                })?;
            if !status.success() {
                return Err(format!(
                    "Local whisper.cpp backend is unavailable ({status}); check its adjacent DLLs: {}",
                    executable.display()
                ));
            }
        }
        Ok(())
    }

    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String> {
        if !self.has_speech(audio, directory)? {
            return Ok(String::new());
        }
        let stem = self.recognize(audio, directory, false)?;
        let text = fs::read_to_string(stem.with_extension("txt")).map_err(|e| {
            format!(
                "Local backend did not produce a complete UTF-8 result: {e}. Log: {}",
                directory.join("whisper.log").display()
            )
        })?;
        Ok(text.trim().to_string())
    }

    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        // A speech-free window produces no timed spans. Its audio stays on disk
        // until the owning window commits, so silence never fabricates text and
        // never discards speech recognized in an adjacent window.
        if !self.has_speech(audio, directory)? {
            return Ok(Vec::new());
        }
        let stem = self.recognize(audio, directory, true)?;
        crate::whisper_timing::read(&stem.with_extension("json"))
    }
}
