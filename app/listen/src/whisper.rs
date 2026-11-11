//! Local whisper.cpp adapter. The model can stay resident in this process
//! (in-process `whisper.dll`, fresh decode state per request) with an explicit
//! fallback to the one-shot `whisper-cli.exe`. No networking or alternative
//! recognizer exists.
use crate::{AppResult, CapturedAudio, LocalTranscriber, TimedText, whisper_host::ModelHost};
use std::{
    fs::{self, File},
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    /// Load the model once into this process and decode each window in a fresh state.
    #[default]
    ModelHost,
    /// Keep the previous behavior: one `whisper-cli.exe` process per window.
    Cli,
}

pub struct Whisper {
    pub executable: PathBuf,
    pub model: PathBuf,
    pub vad_model: PathBuf,
    pub language: String,
    pub threads: usize,
    pub backend: Backend,
    host: Arc<Mutex<Option<ModelHost>>>,
    note: Arc<Mutex<Option<String>>>,
    host_disabled: Arc<AtomicBool>,
}

impl Whisper {
    pub fn with_backend(
        executable: PathBuf,
        model: PathBuf,
        vad_model: PathBuf,
        language: String,
        threads: usize,
        backend: Backend,
    ) -> Self {
        Self {
            executable,
            model,
            vad_model,
            language,
            threads,
            backend,
            host: Arc::new(Mutex::new(None)),
            note: Arc::new(Mutex::new(if backend == Backend::Cli {
                Some(
                    "whisper.cpp CLI (one process per window; the model is reloaded each window)."
                        .to_string(),
                )
            } else {
                None
            })),
            host_disabled: Arc::new(AtomicBool::new(backend == Backend::Cli)),
        }
    }

    pub fn new(
        executable: PathBuf,
        model: PathBuf,
        vad_model: PathBuf,
        language: String,
        threads: usize,
    ) -> Self {
        Self::with_backend(
            executable,
            model,
            vad_model,
            language,
            threads,
            Backend::ModelHost,
        )
    }

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

    /// Run the local CLI for one captured window and return its output stem.
    /// Full JSON adds the per-token byte fragments and timestamps that overlap
    /// ownership needs; plain text is enough for a single whole-file result.
    fn recognize_cli(
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
            // CPU is explicit for this integration. CUDA selection/performance is later work.
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

    fn host_result<T>(
        &self,
        run: impl FnOnce(&mut ModelHost) -> AppResult<T>,
    ) -> Option<AppResult<T>> {
        if self.backend == Backend::Cli || self.host_disabled.load(Ordering::Acquire) {
            return None;
        }
        let mut guard = self.host.lock().unwrap();
        let host = guard.as_mut()?;
        let result = run(host);
        drop(guard);
        if let Err(error) = &result {
            self.disable_host(error);
        }
        Some(result)
    }

    /// Stop using the resident model after a host error and say so. The current
    /// window is retried through the CLI path by the caller; no audio is lost.
    fn disable_host(&self, error: &str) {
        self.host_disabled.store(true, Ordering::Release);
        *self.note.lock().unwrap() = Some(format!(
            "The resident local model failed ({error}); remaining windows use the whisper.cpp CLI, which reloads the model. No audio was lost."
        ));
        self.host.lock().unwrap().take();
    }
}

impl LocalTranscriber for Whisper {
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        if self.backend == Backend::Cli {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        let host = self.host.clone();
        let note = self.note.clone();
        let disabled = self.host_disabled.clone();
        let executable = self.executable.clone();
        let model = self.model.clone();
        let language = self.language.clone();
        let threads = self.threads;
        thread::spawn(move || {
            match ModelHost::load(&executable, &model, threads, &language) {
                Ok(loaded) => {
                    *host.lock().unwrap() = Some(loaded);
                    let message = "Local model is loaded and resident in this process (CPU, no CUDA backend); every window decodes in a fresh state and no window waits to reload the model.".to_string();
                    *note.lock().unwrap() = Some(message.clone());
                    let _ = sender.send(Ok(message));
                }
                Err(error) => {
                    // The CLI path was validated by check_ready. Keep capture
                    // available, but state explicitly that every window will pay
                    // the model load again rather than claiming a false warm state.
                    disabled.store(true, Ordering::Release);
                    let message = format!(
                        "The resident local model is unavailable ({error}); Listen will use the whisper.cpp CLI for each window, which reloads the model and is slower. No audio is lost."
                    );
                    *note.lock().unwrap() = Some(message.clone());
                    let _ = sender.send(Ok(message));
                }
            }
        });
        Some(receiver)
    }

    fn backend_note(&self) -> Option<String> {
        self.note.lock().unwrap().clone()
    }

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
        if let Some(result) = self.host_result(|host| host.transcribe(&audio.path)) {
            match result {
                Ok(text) => return Ok(text),
                Err(error) => {
                    return self
                        .recognize_cli(audio, directory, false)
                        .and_then(|stem| read_cli_text(&stem, directory))
                        .map_err(|fallback| {
                            format!("{error}; the CLI fallback also failed: {fallback}")
                        });
                }
            }
        }
        let stem = self.recognize_cli(audio, directory, false)?;
        read_cli_text(&stem, directory)
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
        if let Some(result) = self.host_result(|host| host.transcribe_segment(&audio.path)) {
            match result {
                Ok(text) => return Ok(text),
                Err(error) => {
                    return self
                        .recognize_cli(audio, directory, true)
                        .and_then(|stem| crate::whisper_timing::read(&stem.with_extension("json")))
                        .map_err(|fallback| {
                            format!("{error}; the CLI fallback also failed: {fallback}")
                        });
                }
            }
        }
        let stem = self.recognize_cli(audio, directory, true)?;
        crate::whisper_timing::read(&stem.with_extension("json"))
    }
}

fn read_cli_text(stem: &Path, directory: &Path) -> AppResult<String> {
    let text = fs::read_to_string(stem.with_extension("txt")).map_err(|e| {
        format!(
            "Local backend did not produce a complete UTF-8 result: {e}. Log: {}",
            directory.join("whisper.log").display()
        )
    })?;
    Ok(text.trim().to_string())
}
