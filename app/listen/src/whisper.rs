//! Local whisper.cpp adapter. The model can stay resident in this process
//! (in-process `whisper.dll`, fresh decode state per request) with an explicit
//! fallback to the one-shot `whisper-cli.exe`. No networking or alternative
//! recognizer exists.
use crate::{
    AppResult, CapturedAudio, LocalTranscriber, TimedText,
    whisper_host::{LoadFailure, ModelHost},
};
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
    time::{SystemTime, UNIX_EPOCH},
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
        let mut guard = self.host.lock().unwrap_or_else(|e| e.into_inner());
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
        *self.note.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
            "The resident local model failed ({error}); remaining windows use the whisper.cpp CLI, which reloads the model. No audio was lost."
        ));
        self.host.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

impl LocalTranscriber for Whisper {
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        let (sender, receiver) = mpsc::channel();
        let host = self.host.clone();
        let note = self.note.clone();
        let disabled = self.host_disabled.clone();
        let executable = self.executable.clone();
        let model = self.model.clone();
        let vad_model = self.vad_model.clone();
        let language = self.language.clone();
        let threads = self.threads;
        let backend = self.backend;
        thread::spawn(move || {
            // The model files are verified by a real whisper.cpp load *before* the
            // in-process host touches them: whisper.cpp aborts the process on some
            // corrupt models (GGML_ASSERT), so the host must never be the first
            // reader of an unverified model. The CLI check runs in a child process,
            // so a corrupt model becomes a reported Failure instead of a crash.
            if let Err(error) = verify_models_with_cli(&executable, &model, &vad_model, &language) {
                disabled.store(true, Ordering::Release);
                let message = format!(
                    "The configured local model could not be loaded by the whisper.cpp CLI: {error}. Capture is unavailable until the local model and backend are repaired; this is not a claim of readiness."
                );
                *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                let _ = sender.send(Err(message));
                return;
            }
            if backend == Backend::Cli {
                // The CLI is the actual decoder in this mode; it just loaded both
                // model files above, so readiness is no longer inferred from magic.
                let message = "whisper.cpp CLI (one process per window; the model is reloaded each window; both local model files were load-verified).".to_string();
                *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                let _ = sender.send(Ok(message));
                return;
            }
            match ModelHost::load(&executable, &model, threads, &language) {
                Ok(loaded) => {
                    *host.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
                    let message = "Local model is loaded and resident in this process (CPU, no CUDA backend); the model files were also load-verified by the local CLI, every window decodes in a fresh state and no window waits to reload the model.".to_string();
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                    let _ = sender.send(Ok(message));
                }
                Err(LoadFailure::Model(error)) => {
                    // The CLI loaded the same model during verification, so the model
                    // itself is not the cause; do not pretend the backend is ready.
                    disabled.store(true, Ordering::Release);
                    let message = format!(
                        "The local model could not be loaded by the resident host ({error}); the CLI loaded it during verification, so the local backend is inconsistent. Capture is unavailable until the local backend is repaired."
                    );
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                    let _ = sender.send(Err(message));
                }
                Err(LoadFailure::Host(error)) => {
                    // A host-only problem (missing/unknown DLL or export) is
                    // recoverable through the CLI, which just loaded the model.
                    disabled.store(true, Ordering::Release);
                    let message = format!(
                        "The resident local model is unavailable ({error}); the whisper.cpp CLI was verified to load the same local model, so Listen will use the CLI for each window. That reloads the model and is slower. No audio is lost."
                    );
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                    let _ = sender.send(Ok(message));
                }
            }
        });
        Some(receiver)
    }

    fn backend_note(&self) -> Option<String> {
        self.note.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

/// Proves that the configured model files can actually be loaded by the local
/// executables that will decode with them. The pinned v1.8.2 CLI loads the model
/// and then fails to read the zero-frame WAV without encoding, which still exits 0;
/// a corrupt or truncated model aborts before that point. The VAD model is loaded
/// against a short valid WAV. The check runs in a private temporary directory and
/// never writes into the user's runtime directory.
fn verify_models_with_cli(
    executable: &Path,
    model: &Path,
    vad_model: &Path,
    language: &str,
) -> AppResult<()> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "listen-model-check-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory)
        .map_err(|e| format!("Cannot create a model verification directory: {e}"))?;
    let result = (|| -> AppResult<()> {
        let empty = directory.join("empty.wav");
        write_probe_wav(&empty, &[])?;
        let probe = directory.join("probe.wav");
        write_probe_wav(&probe, &[0i16; 4_000])?;
        let log_path = directory.join("model-check.log");
        let log = File::create(&log_path)
            .map_err(|e| format!("Cannot create the model verification log: {e}"))?;
        let status = Command::new(executable)
            .arg("-m")
            .arg(model)
            .arg("-f")
            .arg(&empty)
            .args(["-l", language, "-t", "1", "-nt", "-otxt", "-of"])
            .arg(directory.join("stem"))
            .arg("-ng")
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .status()
            .map_err(|e| {
                format!(
                    "Cannot run the local whisper.cpp executable for model verification ({}): {e}",
                    executable.display()
                )
            })?;
        let log_text = fs::read_to_string(&log_path).unwrap_or_default();
        // The timing line is printed only after the model was loaded; the zero-frame
        // audio failure then follows. Requiring it avoids treating an early CLI
        // argument failure as a successful model load.
        if !status.success() || !log_text.contains("load time") {
            return Err(format!(
                "the whisper.cpp executable did not load the model ({status}). Log: {}",
                log_path.display()
            ));
        }
        let vad_log_path = directory.join("vad-check.log");
        let vad_output_path = directory.join("vad-check.txt");
        let vad_log = File::create(&vad_log_path)
            .map_err(|e| format!("Cannot create the VAD verification log: {e}"))?;
        let vad_output = File::create(&vad_output_path)
            .map_err(|e| format!("Cannot create the VAD verification output: {e}"))?;
        let status = Command::new(executable.with_file_name("vad-speech-segments.exe"))
            .args(["-vm"])
            .arg(vad_model)
            .arg("-f")
            .arg(&probe)
            .arg("-np")
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::from(vad_output))
            .stderr(Stdio::from(vad_log))
            .status()
            .map_err(|e| {
                format!("Cannot run the local speech detector for model verification: {e}")
            })?;
        // The pinned VAD CLI writes its segment count to stdout (the same stream
        // the application reads during normal speech detection).
        let vad_text = fs::read_to_string(&vad_output_path).unwrap_or_default();
        if !status.success() || !vad_text.contains("Detected") {
            return Err(format!(
                "the local speech detector did not load its model ({status}). Log: {}",
                vad_log_path.display()
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&directory);
    result
}

fn write_probe_wav(path: &Path, samples: &[i16]) -> AppResult<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("Cannot create a probe WAV: {e}"))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .map_err(|e| format!("Cannot write a probe WAV: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("Cannot finalize a probe WAV: {e}"))
}
