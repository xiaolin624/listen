//! Resident SenseVoiceSmall recognition through the sherpa-onnx C API.
//!
//! The model is loaded once into this process by [`SenseVoice::prepare`] and
//! every captured window is decoded against it. Windows are gated by the same
//! Silero VAD build: a window with no detected speech never reaches the
//! recognizer, so silence, music and notification sounds cannot fabricate text.
//! Each detected speech segment is decoded on its own, which yields truthful
//! per-utterance [`TimedText`] spans for the overlap ownership rule in
//! `streaming.rs`.
//!
//! Fail-closed rules, mirroring `whisper_host.rs`:
//! - every native input is pinned by SHA-256 before it is loaded or decoded;
//! - a missing file, a hash mismatch, a null recognizer/VAD/stream/result and an
//!   invalid VAD span are explicit [`AppResult`] errors;
//! - this module never falls back to whisper, a CLI or a cloud service.
//!
//! All structures below are mirrored in declaration order from
//! `sherpa-onnx-v2.0.1-win-x64-shared-MT-Release/include/sherpa-onnx/c-api/c-api.h`;
//! the `size_of` assertions are checked against an independent parse of that
//! header (see `docs/issue-17-verification.md`).
#![cfg(windows)]

use crate::{AppResult, CapturedAudio, LocalTranscriber, TimedText};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        Security::Cryptography::{
            BCRYPT_ALG_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
            BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider,
        },
        System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW},
    },
    core::{PCSTR, PCWSTR, s},
};

/// SHA-256 of the pinned sherpa-onnx v2.0.1 Windows x64 shared (MT/Release)
/// `sherpa-onnx-c-api.dll`. Checked before the library is loaded.
const PINNED_SHERPA_DLL_SHA256: [u8; 32] = [
    0x09, 0x65, 0xA6, 0xE4, 0xC0, 0x25, 0x4D, 0xA2, 0xBE, 0xFF, 0x48, 0x9C, 0xF7, 0xA8, 0xAA, 0xFF,
    0xAF, 0x7D, 0xC9, 0x4F, 0x90, 0xB4, 0x3B, 0x3E, 0x66, 0x44, 0x8B, 0x8C, 0xAB, 0x41, 0x77, 0xEA,
];

/// SHA-256 of the `onnxruntime.dll` shipped beside the pinned C API DLL.
const PINNED_ONNXRUNTIME_SHA256: [u8; 32] = [
    0x7F, 0x66, 0xF9, 0x39, 0xA8, 0x81, 0xBA, 0xF4, 0xF4, 0x6A, 0x22, 0x16, 0x49, 0x67, 0x98, 0xED,
    0xF4, 0xA1, 0x42, 0x98, 0x78, 0xB6, 0x46, 0xD1, 0x26, 0x74, 0xAA, 0x62, 0xF2, 0x7D, 0x8A, 0x25,
];

/// SHA-256 of the SenseVoiceSmall q8 ONNX model.
const PINNED_MODEL_SHA256: [u8; 32] = [
    0xC7, 0x1F, 0x0C, 0xE0, 0x0B, 0xEC, 0x95, 0xB0, 0x77, 0x44, 0xE1, 0x16, 0x34, 0x5E, 0x33, 0xD8,
    0xCB, 0xBE, 0x08, 0xCE, 0xF8, 0x96, 0x38, 0x2C, 0xF9, 0x07, 0xBF, 0x4B, 0x51, 0xA2, 0xCD, 0x51,
];

/// SHA-256 of the SenseVoice tokens file.
const PINNED_TOKENS_SHA256: [u8; 32] = [
    0xF4, 0x49, 0xEB, 0x28, 0xDC, 0x56, 0x75, 0x33, 0xD7, 0xFA, 0x59, 0xBE, 0x34, 0xE2, 0xAB, 0xCA,
    0x87, 0x84, 0xF7, 0x71, 0x85, 0x0C, 0x78, 0xA4, 0x7F, 0xB7, 0x31, 0xA3, 0x14, 0x29, 0xA1, 0xDC,
];

/// SHA-256 of the pinned Silero VAD ONNX model.
const PINNED_VAD_SHA256: [u8; 32] = [
    0x9E, 0x24, 0x49, 0xE1, 0x08, 0x74, 0x96, 0xD8, 0xD4, 0xCA, 0xBA, 0x90, 0x7F, 0x23, 0xE0, 0xBD,
    0x3F, 0x78, 0xD9, 0x1F, 0xA5, 0x52, 0x47, 0x9B, 0xB9, 0xC2, 0x3A, 0xC0, 0x9C, 0xBB, 0x1F, 0xD6,
];

/// The application's captured windows are always mono 16 kHz.
const SAMPLE_RATE: u32 = 16_000;
/// Silero VAD processes 512-sample frames at 16 kHz.
const VAD_WINDOW_SAMPLES: usize = 512;
/// The VAD trims a segment when speech stops; decode a short tail so the final
/// consonant of a short utterance is not cut off. The reported span still ends
/// at the VAD boundary.
const VAD_DECODE_TAIL_SAMPLES: u64 = 320;
const MODEL_THREADS_DEFAULT: usize = 6;

/// One mirrored C structure. All fields are `i32`, `f32` or pointers, so the
/// C layout is fully described by declaration order plus pointer alignment.
#[repr(C)]
#[derive(Clone, Copy)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineParaformerModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineNemoEncDecCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineWhisperModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    task: *const c_char,
    tail_paddings: i32,
    enable_token_timestamps: i32,
    enable_segment_timestamps: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTdnnModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineSenseVoiceModelConfig {
    model: *const c_char,
    language: *const c_char,
    use_itn: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineMoonshineModelConfig {
    preprocessor: *const c_char,
    encoder: *const c_char,
    uncached_decoder: *const c_char,
    cached_decoder: *const c_char,
    merged_decoder: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineFireRedAsrModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineDolphinModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineZipformerCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineCanaryModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    src_lang: *const c_char,
    tgt_lang: *const c_char,
    use_pnc: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineWenetCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineOmnilingualAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineMedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineFunAsrNanoModelConfig {
    encoder_adaptor: *const c_char,
    llm: *const c_char,
    embedding: *const c_char,
    tokenizer: *const c_char,
    system_prompt: *const c_char,
    user_prompt: *const c_char,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    language: *const c_char,
    itn: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineFireRedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineQwen3AsrModelConfig {
    conv_frontend: *const c_char,
    encoder: *const c_char,
    decoder: *const c_char,
    tokenizer: *const c_char,
    max_total_len: i32,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineCohereTranscribeModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    use_punct: i32,
    use_itn: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineModelConfig {
    transducer: OfflineTransducerModelConfig,
    paraformer: OfflineParaformerModelConfig,
    nemo_ctc: OfflineNemoEncDecCtcModelConfig,
    whisper: OfflineWhisperModelConfig,
    tdnn: OfflineTdnnModelConfig,
    tokens: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    telespeech_ctc: *const c_char,
    sense_voice: OfflineSenseVoiceModelConfig,
    moonshine: OfflineMoonshineModelConfig,
    fire_red_asr: OfflineFireRedAsrModelConfig,
    dolphin: OfflineDolphinModelConfig,
    zipformer_ctc: OfflineZipformerCtcModelConfig,
    canary: OfflineCanaryModelConfig,
    wenet_ctc: OfflineWenetCtcModelConfig,
    omnilingual: OfflineOmnilingualAsrCtcModelConfig,
    medasr: OfflineMedAsrCtcModelConfig,
    funasr_nano: OfflineFunAsrNanoModelConfig,
    fire_red_asr_ctc: OfflineFireRedAsrCtcModelConfig,
    qwen3_asr: OfflineQwen3AsrModelConfig,
    cohere_transcribe: OfflineCohereTranscribeModelConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineLmConfig {
    model: *const c_char,
    scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineCtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OfflineModelConfig,
    lm_config: OfflineLmConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hr: HomophoneReplacerConfig,
    ctc_fst_decoder_config: OfflineCtcFstDecoderConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineRecognizerResult {
    text: *const c_char,
    timestamps: *mut f32,
    count: i32,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    json: *const c_char,
    lang: *const c_char,
    emotion: *const c_char,
    event: *const c_char,
    durations: *mut f32,
    ys_log_probs: *mut f32,
    segment_timestamps: *const f32,
    segment_durations: *const f32,
    segment_texts: *const c_char,
    segment_texts_arr: *const *const c_char,
    segment_count: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SileroVadModelConfig {
    model: *const c_char,
    threshold: f32,
    min_silence_duration: f32,
    min_speech_duration: f32,
    window_size: i32,
    max_speech_duration: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TenVadModelConfig {
    model: *const c_char,
    threshold: f32,
    min_silence_duration: f32,
    min_speech_duration: f32,
    window_size: i32,
    max_speech_duration: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VadModelConfig {
    silero_vad: SileroVadModelConfig,
    sample_rate: i32,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    ten_vad: TenVadModelConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpeechSegment {
    start: i64,
    samples: *mut f32,
    n: i32,
}

// Sizes derived from c-api.h with an independent layout parser. A wrong mirror
// would make the DLL read a different field than the one we filled.
const _: () = assert!(std::mem::size_of::<FeatureConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineTransducerModelConfig>() == 24);
const _: () = assert!(std::mem::size_of::<OfflineParaformerModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineNemoEncDecCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineWhisperModelConfig>() == 48);
const _: () = assert!(std::mem::size_of::<OfflineTdnnModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineSenseVoiceModelConfig>() == 24);
const _: () = assert!(std::mem::size_of::<OfflineMoonshineModelConfig>() == 40);
const _: () = assert!(std::mem::size_of::<OfflineFireRedAsrModelConfig>() == 16);
const _: () = assert!(std::mem::size_of::<OfflineDolphinModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineZipformerCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineCanaryModelConfig>() == 40);
const _: () = assert!(std::mem::size_of::<OfflineWenetCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineOmnilingualAsrCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineMedAsrCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineFunAsrNanoModelConfig>() == 88);
const _: () = assert!(std::mem::size_of::<OfflineFireRedAsrCtcModelConfig>() == 8);
const _: () = assert!(std::mem::size_of::<OfflineQwen3AsrModelConfig>() == 64);
const _: () = assert!(std::mem::size_of::<OfflineCohereTranscribeModelConfig>() == 32);
const _: () = assert!(std::mem::size_of::<OfflineModelConfig>() == 504);
const _: () = assert!(std::mem::size_of::<OfflineLmConfig>() == 16);
const _: () = assert!(std::mem::size_of::<HomophoneReplacerConfig>() == 24);
const _: () = assert!(std::mem::size_of::<OfflineCtcFstDecoderConfig>() == 16);
const _: () = assert!(std::mem::size_of::<OfflineRecognizerConfig>() == 624);
const _: () = assert!(std::mem::size_of::<OfflineRecognizerResult>() == 128);
const _: () = assert!(std::mem::size_of::<SileroVadModelConfig>() == 32);
const _: () = assert!(std::mem::size_of::<TenVadModelConfig>() == 32);
const _: () = assert!(std::mem::size_of::<VadModelConfig>() == 88);
const _: () = assert!(std::mem::size_of::<SpeechSegment>() == 24);

type CreateOfflineRecognizer =
    unsafe extern "system" fn(*const OfflineRecognizerConfig) -> *const c_void;
type DestroyOfflineRecognizer = unsafe extern "system" fn(*const c_void);
type CreateOfflineStream = unsafe extern "system" fn(*const c_void) -> *const c_void;
type DestroyOfflineStream = unsafe extern "system" fn(*const c_void);
type AcceptWaveformOffline = unsafe extern "system" fn(*const c_void, i32, *const f32, i32);
type DecodeOfflineStream = unsafe extern "system" fn(*const c_void, *const c_void);
type GetOfflineStreamResult =
    unsafe extern "system" fn(*const c_void) -> *const OfflineRecognizerResult;
type DestroyOfflineRecognizerResult = unsafe extern "system" fn(*const OfflineRecognizerResult);
type CreateVoiceActivityDetector =
    unsafe extern "system" fn(*const VadModelConfig, f32) -> *const c_void;
type DestroyVoiceActivityDetector = unsafe extern "system" fn(*const c_void);
type VadAcceptWaveform = unsafe extern "system" fn(*const c_void, *const f32, i32);
type VadEmpty = unsafe extern "system" fn(*const c_void) -> i32;
type VadFront = unsafe extern "system" fn(*const c_void) -> *const SpeechSegment;
type VadPop = unsafe extern "system" fn(*const c_void);
type VadReset = unsafe extern "system" fn(*const c_void);
type VadFlush = unsafe extern "system" fn(*const c_void);
type DestroySpeechSegment = unsafe extern "system" fn(*const SpeechSegment);

struct Api {
    create_recognizer: CreateOfflineRecognizer,
    destroy_recognizer: DestroyOfflineRecognizer,
    create_stream: CreateOfflineStream,
    destroy_stream: DestroyOfflineStream,
    accept_waveform: AcceptWaveformOffline,
    decode: DecodeOfflineStream,
    get_result: GetOfflineStreamResult,
    destroy_result: DestroyOfflineRecognizerResult,
    create_vad: CreateVoiceActivityDetector,
    destroy_vad: DestroyVoiceActivityDetector,
    vad_accept: VadAcceptWaveform,
    vad_empty: VadEmpty,
    vad_front: VadFront,
    vad_pop: VadPop,
    vad_reset: VadReset,
    vad_flush: VadFlush,
    destroy_segment: DestroySpeechSegment,
}

impl Api {
    unsafe fn load(module: HMODULE) -> AppResult<Self> {
        unsafe fn symbol<T: Copy>(module: HMODULE, name: PCSTR) -> AppResult<T> {
            let address = unsafe { GetProcAddress(module, name) }.ok_or_else(|| {
                let exported = unsafe { CStr::from_ptr(name.0 as *const c_char) }
                    .to_string_lossy()
                    .into_owned();
                format!(
                    "The local sherpa-onnx C API does not export {exported}; the pinned v2.0.1 build is required"
                )
            })?;
            Ok(unsafe { std::mem::transmute_copy(&address) })
        }
        Ok(Self {
            create_recognizer: unsafe { symbol(module, s!("SherpaOnnxCreateOfflineRecognizer"))? },
            destroy_recognizer: unsafe {
                symbol(module, s!("SherpaOnnxDestroyOfflineRecognizer"))?
            },
            create_stream: unsafe { symbol(module, s!("SherpaOnnxCreateOfflineStream"))? },
            destroy_stream: unsafe { symbol(module, s!("SherpaOnnxDestroyOfflineStream"))? },
            accept_waveform: unsafe { symbol(module, s!("SherpaOnnxAcceptWaveformOffline"))? },
            decode: unsafe { symbol(module, s!("SherpaOnnxDecodeOfflineStream"))? },
            get_result: unsafe { symbol(module, s!("SherpaOnnxGetOfflineStreamResult"))? },
            destroy_result: unsafe {
                symbol(module, s!("SherpaOnnxDestroyOfflineRecognizerResult"))?
            },
            create_vad: unsafe { symbol(module, s!("SherpaOnnxCreateVoiceActivityDetector"))? },
            destroy_vad: unsafe { symbol(module, s!("SherpaOnnxDestroyVoiceActivityDetector"))? },
            vad_accept: unsafe {
                symbol(module, s!("SherpaOnnxVoiceActivityDetectorAcceptWaveform"))?
            },
            vad_empty: unsafe { symbol(module, s!("SherpaOnnxVoiceActivityDetectorEmpty"))? },
            vad_front: unsafe { symbol(module, s!("SherpaOnnxVoiceActivityDetectorFront"))? },
            vad_pop: unsafe { symbol(module, s!("SherpaOnnxVoiceActivityDetectorPop"))? },
            vad_reset: unsafe { symbol(module, s!("SherpaOnnxVoiceActivityDetectorReset"))? },
            vad_flush: unsafe { symbol(module, s!("SherpaOnnxVoiceActivityDetectorFlush"))? },
            destroy_segment: unsafe { symbol(module, s!("SherpaOnnxDestroySpeechSegment"))? },
        })
    }
}

/// The loaded DLL and its resident recognizer/VAD handles. Only ever used
/// through the owning mutex; sherpa-onnx handles do not support concurrent use.
struct Resident {
    _module: HMODULE,
    api: Api,
    recognizer: *const c_void,
    vad: *const c_void,
}

// The handles are serialized behind `SenseVoice::resident` and the DLL stays
// loaded for the process lifetime, so moving this container between threads is
// sound. No reference to a handle escapes the mutex.
unsafe impl Send for Resident {}

impl Drop for Resident {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy_vad)(self.vad);
            (self.api.destroy_recognizer)(self.recognizer);
        }
        // The DLL and onnxruntime are intentionally not unloaded here. They are
        // loaded for the process lifetime; unloading at teardown has the same
        // lifetime hazard documented in whisper_host.rs, and the OS reclaims it.
    }
}

/// A decoded speech segment inside one captured window.
#[derive(Debug)]
struct SpeechChunk {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

/// The raw fields of one SenseVoice result that the gate needs. Kept separate
/// from the C result so tests can exercise the filter without the DLL.
struct Recognized {
    text: String,
    /// Captured for diagnostics; the current gate only needs `event`.
    _lang: String,
    /// Captured for diagnostics; the current gate only needs `event`.
    _emotion: String,
    event: String,
    /// Per-token text in order; the pinned build returns this together with
    /// `timestamps`, which lets one whole-window decode keep mid-window spans.
    tokens: Vec<String>,
    /// Per-token start time in seconds, relative to the decoded audio.
    timestamps: Vec<f32>,
}

/// A resident SenseVoiceSmall engine. `library` is the directory holding
/// `sherpa-onnx-c-api.dll` and `onnxruntime.dll`.
pub struct SenseVoice {
    library: PathBuf,
    model: PathBuf,
    tokens: PathBuf,
    vad_model: PathBuf,
    language: String,
    threads: usize,
    resident: Arc<Mutex<Option<Resident>>>,
    note: Arc<Mutex<Option<String>>>,
    failed: Arc<AtomicBool>,
}

impl SenseVoice {
    pub fn new(
        library: PathBuf,
        model: PathBuf,
        tokens: PathBuf,
        vad_model: PathBuf,
        language: String,
        threads: usize,
    ) -> Self {
        let note = "SenseVoice q8 model is not loaded yet; preparation starts at launch. No cloud service is used.".to_string();
        Self {
            library,
            model,
            tokens,
            vad_model,
            language: if language.is_empty() {
                "auto".into()
            } else {
                language
            },
            threads: threads.max(1),
            resident: Arc::new(Mutex::new(None)),
            note: Arc::new(Mutex::new(Some(note))),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_defaults(
        library: PathBuf,
        model: PathBuf,
        tokens: PathBuf,
        vad_model: PathBuf,
    ) -> Self {
        Self::new(
            library,
            model,
            tokens,
            vad_model,
            "auto".into(),
            MODEL_THREADS_DEFAULT,
        )
    }

    fn dll(&self) -> PathBuf {
        self.library.join("sherpa-onnx-c-api.dll")
    }

    fn onnxruntime(&self) -> PathBuf {
        self.library.join("onnxruntime.dll")
    }

    /// Reads the PCM of one captured WAV. The application always writes mono
    /// 16-bit 16 kHz windows; anything else is rejected instead of resampled.
    fn read_pcm(path: &Path) -> AppResult<Vec<f32>> {
        let mut reader = hound::WavReader::open(path).map_err(|e| {
            format!("The local SenseVoice engine cannot read the captured WAV: {e}")
        })?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.sample_rate != SAMPLE_RATE
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err(
                "The local SenseVoice engine expects the application's mono 16-bit 16 kHz WAV; audio was not decoded"
                    .into(),
            );
        }
        reader
            .samples::<i16>()
            .map(|sample| {
                sample.map(|value| value as f32 / 32768.0).map_err(|e| {
                    format!("The local SenseVoice engine cannot decode the captured WAV: {e}")
                })
            })
            .collect()
    }

    /// Decodes the whole captured window once, then attributes the decoded text
    /// to the VAD utterances through the engine's token timestamps. Decoding the
    /// whole window is what keeps sentence context at the start of a later
    /// utterance ("请保留…" was lost when that utterance was decoded alone); the
    /// token timestamps give `streaming.rs` the mid-window spans its overlap rule
    /// needs, so no fabricated whole-window midpoint is ever used.
    fn recognize_chunks(&self, pcm: &[f32]) -> AppResult<Vec<SpeechChunk>> {
        let mut guard = self.resident.lock().unwrap_or_else(|e| e.into_inner());
        let resident = guard.as_mut().ok_or_else(|| {
            "The local SenseVoice engine is not prepared; audio was not decoded".to_string()
        })?;
        let segments = resident.speech_segments(pcm)?;
        if segments.is_empty() {
            return Ok(Vec::new());
        }
        let recognized = resident.recognize(pcm)?;
        let Some(text) = accepted_text(&recognized) else {
            return Ok(Vec::new());
        };
        if !recognized.tokens.is_empty() && recognized.tokens.len() == recognized.timestamps.len() {
            let mut chunks =
                token_chunks(&text, &recognized.tokens, &recognized.timestamps, &segments)?;
            drop_implausible_tail(&mut chunks);
            return Ok(chunks);
        }
        // A build that does not expose token timestamps cannot attribute one
        // whole-window decode without inventing times; decode the VAD utterances
        // separately instead (same engine, same gate, explicit fallback). The
        // pinned v2.0.1 build always takes the token path above.
        let mut chunks = Vec::with_capacity(segments.len());
        for (start, samples) in segments {
            let decode_end = (start + samples + VAD_DECODE_TAIL_SAMPLES).min(pcm.len() as u64);
            let start_index = start as usize;
            let end_index = decode_end as usize;
            if start_index >= end_index {
                return Err(
                    "The local SenseVoice engine received an empty speech segment; audio retained"
                        .into(),
                );
            }
            let recognized = resident.recognize(&pcm[start_index..end_index])?;
            let Some(text) = accepted_text(&recognized) else {
                continue;
            };
            chunks.push(SpeechChunk {
                start_ms: start * 1000 / SAMPLE_RATE as u64,
                end_ms: (start + samples) * 1000 / SAMPLE_RATE as u64,
                text: prefixed(&text),
            });
        }
        drop_implausible_tail(&mut chunks);
        Ok(chunks)
    }
}

impl Resident {
    fn speech_segments(&mut self, pcm: &[f32]) -> AppResult<Vec<(u64, u64)>> {
        if pcm.is_empty() {
            return Ok(Vec::new());
        }
        unsafe { (self.api.vad_reset)(self.vad) };
        let mut frame = vec![0.0f32; VAD_WINDOW_SAMPLES];
        for chunk in pcm.chunks(VAD_WINDOW_SAMPLES) {
            frame.fill(0.0);
            frame[..chunk.len()].copy_from_slice(chunk);
            unsafe {
                (self.api.vad_accept)(self.vad, frame.as_ptr(), VAD_WINDOW_SAMPLES as i32);
            }
        }
        unsafe { (self.api.vad_flush)(self.vad) };
        let mut segments = Vec::new();
        while unsafe { (self.api.vad_empty)(self.vad) } == 0 {
            let segment = unsafe { (self.api.vad_front)(self.vad) };
            if segment.is_null() {
                return Err(
                    "The local Silero VAD returned a missing speech segment; audio retained".into(),
                );
            }
            let start = unsafe { (*segment).start };
            let samples = unsafe { (*segment).n };
            unsafe {
                (self.api.destroy_segment)(segment);
                (self.api.vad_pop)(self.vad);
            }
            if start < 0 || samples <= 0 {
                return Err(
                    "The local Silero VAD returned an invalid speech segment; audio retained"
                        .into(),
                );
            }
            let start = start as u64;
            let total = pcm.len() as u64;
            if start >= total {
                // The VAD timeline includes the zero padding of the final frame;
                // a segment made only of that padding is not speech.
                continue;
            }
            // The final accept call is zero-padded to a full 512-sample frame, so
            // an open segment can end a few dozen samples past the real audio.
            let samples = (samples as u64).min(total - start);
            if samples == 0 {
                continue;
            }
            segments.push((start, samples));
        }
        Ok(segments)
    }

    fn recognize(&mut self, pcm: &[f32]) -> AppResult<Recognized> {
        let stream = unsafe { (self.api.create_stream)(self.recognizer) };
        if stream.is_null() {
            return Err(
                "The local SenseVoice engine could not create a decode stream; audio retained"
                    .into(),
            );
        }
        let stream = StreamGuard {
            destroy: self.api.destroy_stream,
            stream,
        };
        unsafe {
            (self.api.accept_waveform)(
                stream.stream,
                SAMPLE_RATE as i32,
                pcm.as_ptr(),
                pcm.len() as i32,
            );
            (self.api.decode)(self.recognizer, stream.stream);
        }
        let result = unsafe { (self.api.get_result)(stream.stream) };
        if result.is_null() {
            return Err(
                "The local SenseVoice engine returned no recognition result; audio retained".into(),
            );
        }
        let result = ResultGuard {
            destroy: self.api.destroy_result,
            result,
        };
        let field = |pointer: *const c_char, label: &str| -> AppResult<String> {
            if pointer.is_null() {
                return Ok(String::new());
            }
            let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|error| {
                    format!(
                        "The local SenseVoice engine returned {label} that is not valid UTF-8 ({error}); audio retained"
                    )
                })
        };
        let count = unsafe { (*result.result).count };
        let mut tokens = Vec::new();
        let mut timestamps = Vec::new();
        // Guard against a corrupt count before indexing the arrays; the pinned
        // build returns one token per character or subword.
        if count > 0 && count <= 1_000_000 {
            let tokens_array = unsafe { (*result.result).tokens_arr };
            if !tokens_array.is_null() {
                for index in 0..count as usize {
                    let pointer = unsafe { *tokens_array.add(index) };
                    if pointer.is_null() {
                        return Err(
                            "The local SenseVoice engine returned a missing token; audio retained"
                                .into(),
                        );
                    }
                    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
                    tokens.push(std::str::from_utf8(bytes).map(str::to_owned).map_err(
                        |error| {
                            format!(
                                "The local SenseVoice engine returned a token that is not valid UTF-8 ({error}); audio retained"
                            )
                        },
                    )?);
                }
            }
            let stamps = unsafe { (*result.result).timestamps };
            if !stamps.is_null() {
                for index in 0..count as usize {
                    timestamps.push(unsafe { *stamps.add(index) });
                }
            }
        }
        Ok(Recognized {
            text: field(unsafe { (*result.result).text }, "text")?,
            _lang: field(unsafe { (*result.result).lang }, "a language label")?,
            _emotion: field(unsafe { (*result.result).emotion }, "an emotion label")?,
            event: field(unsafe { (*result.result).event }, "an event label")?,
            tokens,
            timestamps,
        })
    }
}

struct StreamGuard {
    destroy: DestroyOfflineStream,
    stream: *const c_void,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.stream) }
    }
}

struct ResultGuard {
    destroy: DestroyOfflineRecognizerResult,
    result: *const OfflineRecognizerResult,
}

impl Drop for ResultGuard {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.result) }
    }
}

impl LocalTranscriber for SenseVoice {
    fn check_ready(&self) -> AppResult<()> {
        if self
            .resident
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Ok(());
        }
        if self.failed.load(Ordering::Acquire) {
            let detail = self
                .note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| "the local engine did not load".into());
            return Err(format!(
                "The local SenseVoice engine is unavailable: {detail} Capture is unavailable until it is repaired; no cloud service is used."
            ));
        }
        if !std::arch::is_x86_feature_detected!("avx2") {
            return Err(
                "The local SenseVoice q8 engine requires an AVX2-capable CPU, and this CPU does not report AVX2. Capture is unavailable; no cloud service is used."
                    .into(),
            );
        }
        for (path, label) in [
            (self.dll(), "sherpa-onnx C API library"),
            (self.onnxruntime(), "onnxruntime library"),
            (self.model.clone(), "SenseVoice model"),
            (self.tokens.clone(), "SenseVoice tokens"),
            (self.vad_model.clone(), "Silero VAD model"),
        ] {
            if !path.is_file() {
                return Err(format!(
                    "The local SenseVoice {label} is missing: {}. Capture is unavailable until the engine files are restored; no cloud service is used.",
                    path.display()
                ));
            }
        }
        // Fail closed on the exact native build before it is loaded. The model
        // and VAD hashes are checked during preparation, where their cost is
        // already background work.
        let actual = sha256_file(&self.dll())?;
        if actual != PINNED_SHERPA_DLL_SHA256 {
            return Err(format!(
                "The local sherpa-onnx C API library does not match the pinned v2.0.1 build (expected SHA-256 {}, found {}); refusing to load it and no cloud fallback is used",
                hex(&PINNED_SHERPA_DLL_SHA256),
                hex(&actual)
            ));
        }
        Ok(())
    }

    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        if self
            .resident
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        if self.failed.load(Ordering::Acquire) {
            let detail = self
                .note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| "the local engine did not load".into());
            let _ = sender.send(Err(detail));
            return Some(receiver);
        }
        let library = self.library.clone();
        let model = self.model.clone();
        let tokens = self.tokens.clone();
        let vad_model = self.vad_model.clone();
        let language = self.language.clone();
        let threads = self.threads;
        let resident = self.resident.clone();
        let note = self.note.clone();
        let failed = self.failed.clone();
        thread::spawn(move || {
            let result = load_resident(LoadRequest {
                library: &library,
                model: &model,
                tokens: &tokens,
                vad_model: &vad_model,
                language: &language,
                threads,
            });
            match result {
                Ok(loaded) => {
                    *resident.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
                    let message = format!(
                        "SenseVoice q8, resident (sherpa-onnx), CPU, {} thread(s); Silero VAD gates every window and each detected utterance is decoded separately.",
                        threads
                    );
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                    let _ = sender.send(Ok(message));
                }
                Err(error) => {
                    failed.store(true, Ordering::Release);
                    let message = format!(
                        "The local SenseVoice engine could not be prepared: {error}. Capture is unavailable until the engine files are repaired; no cloud service or whisper fallback is used."
                    );
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                    let _ = sender.send(Err(message));
                }
            }
        });
        Some(receiver)
    }

    fn backend_note(&self) -> Option<String> {
        self.note.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let pcm = Self::read_pcm(&audio.path)?;
        let chunks = self.recognize_chunks(&pcm)?;
        Ok(join_chunk_texts(
            chunks.iter().map(|chunk| chunk.text.as_str()),
        ))
    }

    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        _directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        let pcm = Self::read_pcm(&audio.path)?;
        let chunks = self.recognize_chunks(&pcm)?;
        Ok(chunks
            .into_iter()
            .map(|chunk| TimedText {
                start_ms: chunk.start_ms,
                end_ms: chunk.end_ms,
                text: chunk.text,
            })
            .collect())
    }
}

struct LoadRequest<'a> {
    library: &'a Path,
    model: &'a Path,
    tokens: &'a Path,
    vad_model: &'a Path,
    language: &'a str,
    threads: usize,
}

/// Verifies every input by hash, loads the DLL, then creates the resident
/// recognizer and VAD. Any failure destroys what was created and returns the
/// error; nothing is left half-configured.
fn load_resident(request: LoadRequest<'_>) -> AppResult<Resident> {
    if !std::arch::is_x86_feature_detected!("avx2") {
        return Err("this CPU does not report AVX2, which the SenseVoice q8 model requires".into());
    }
    let dll = request.library.join("sherpa-onnx-c-api.dll");
    let onnxruntime = request.library.join("onnxruntime.dll");
    verify_pinned(&dll, &PINNED_SHERPA_DLL_SHA256, "sherpa-onnx C API library")?;
    verify_pinned(
        &onnxruntime,
        &PINNED_ONNXRUNTIME_SHA256,
        "onnxruntime library",
    )?;
    verify_pinned(request.model, &PINNED_MODEL_SHA256, "SenseVoice q8 model")?;
    verify_pinned(request.tokens, &PINNED_TOKENS_SHA256, "SenseVoice tokens")?;
    verify_pinned(request.vad_model, &PINNED_VAD_SHA256, "Silero VAD model")?;

    let dll_wide = wide(&dll.to_string_lossy());
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(dll_wide.as_ptr()),
            None,
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
    }
    .map_err(|error| format!("cannot load {}: {error}", dll.display()))?;
    let api = match unsafe { Api::load(module) } {
        Ok(api) => api,
        Err(error) => {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(error);
        }
    };
    let provider = CString::new("cpu").unwrap();
    let language = CString::new(request.language)
        .map_err(|_| "the configured SenseVoice language contains an embedded NUL".to_string())?;
    let decoding = CString::new("greedy_search").unwrap();
    let model = path_c_string(request.model, "SenseVoice model")?;
    let tokens = path_c_string(request.tokens, "SenseVoice tokens")?;
    let vad_model = path_c_string(request.vad_model, "Silero VAD model")?;

    let mut config: OfflineRecognizerConfig = unsafe { std::mem::zeroed() };
    config.feat_config = FeatureConfig {
        sample_rate: SAMPLE_RATE as i32,
        feature_dim: 80,
    };
    config.model_config.tokens = tokens.as_ptr();
    config.model_config.num_threads = request.threads.max(1) as i32;
    config.model_config.debug = 0;
    config.model_config.provider = provider.as_ptr();
    config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
        model: model.as_ptr(),
        language: language.as_ptr(),
        use_itn: 1,
    };
    config.lm_config = OfflineLmConfig {
        model: ptr::null(),
        scale: 1.0,
    };
    config.decoding_method = decoding.as_ptr();
    config.max_active_paths = 4;
    let recognizer = unsafe { (api.create_recognizer)(&config) };
    if recognizer.is_null() {
        unsafe {
            let _ = FreeLibrary(module);
        }
        return Err(format!(
            "SherpaOnnxCreateOfflineRecognizer rejected the pinned SenseVoice q8 model ({})",
            request.model.display()
        ));
    }

    let mut vad_config: VadModelConfig = unsafe { std::mem::zeroed() };
    vad_config.silero_vad = SileroVadModelConfig {
        model: vad_model.as_ptr(),
        // A 0.6 s pause is a sentence boundary for this corpus; shorter closures
        // split mid-sentence and lose English technical words at the edges.
        threshold: 0.5,
        min_silence_duration: 0.6,
        min_speech_duration: 0.25,
        window_size: VAD_WINDOW_SAMPLES as i32,
        max_speech_duration: 10.0,
    };
    vad_config.sample_rate = SAMPLE_RATE as i32;
    vad_config.num_threads = 1;
    vad_config.provider = provider.as_ptr();
    let vad = unsafe { (api.create_vad)(&vad_config, 60.0) };
    if vad.is_null() {
        unsafe {
            (api.destroy_recognizer)(recognizer);
            let _ = FreeLibrary(module);
        }
        return Err(format!(
            "SherpaOnnxCreateVoiceActivityDetector rejected the pinned Silero VAD model ({})",
            request.vad_model.display()
        ));
    }
    Ok(Resident {
        _module: module,
        api,
        recognizer,
        vad,
    })
}

fn verify_pinned(path: &Path, expected: &[u8; 32], label: &str) -> AppResult<()> {
    let actual =
        sha256_file(path).map_err(|error| format!("cannot verify the {label}: {error}"))?;
    if &actual != expected {
        return Err(format!(
            "the {label} does not match the pinned SHA-256 (expected {}, found {}); refusing to load it",
            hex(expected),
            hex(&actual)
        ));
    }
    Ok(())
}

fn path_c_string(path: &Path, label: &str) -> AppResult<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("the {label} path contains an embedded NUL"))
}

/// Removes `<|...|>` control tokens. SenseVoice returns language, emotion and
/// event labels in that wrapper, and a malformed build can prefix the text with
/// them; none of that may reach the user.
fn strip_control_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find("<|") {
        output.push_str(&rest[..open]);
        if let Some(close) = rest[open..].find("|>") {
            rest = &rest[open + close + 2..];
        } else {
            // An unterminated tag is not text; drop it rather than leaking it.
            rest = "";
            break;
        }
    }
    output.push_str(rest);
    output
}

/// Unwraps one `<|...|>` label (the DLL returns e.g. `<|Speech|>`) and trims it.
fn normalized_label(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<|")
        .and_then(|rest| rest.strip_suffix("|>"))
    {
        return inner.trim().to_string();
    }
    trimmed.to_string()
}

fn clean_transcript(value: &str) -> String {
    strip_control_tags(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Known hallucination words that the q8 model produces on non-speech input.
/// Comparisons are case-insensitive and ignore punctuation; only the whole
/// remaining string is compared, so a sentence containing "the" is unaffected.
const FABRICATED_RESULTS: &[&str] = &[
    "the",
    "a",
    "an",
    "i",
    "you",
    "thankyou",
    "thanks",
    "bye",
    "okay",
    "oh",
    "uh",
    "um",
    "hmm",
    "mmm",
    "yeah",
    "music",
    "applause",
    "laughter",
    "bgm",
    "subtitle",
    "subtitles",
    "字幕",
    "谢谢观看",
    "请不吝点赞",
];

/// True for results that carry no spoken content: punctuation-only text, lone
/// control tags, and the short hallucination words the model fabricates on
/// silence or music. This is a second gate behind the VAD; it never trims real
/// sentences.
fn is_fabricated_short_result(value: &str) -> bool {
    let core: String = strip_control_tags(value)
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if core.is_empty() {
        return true;
    }
    FABRICATED_RESULTS.contains(&core.as_str())
}

/// The recognizer plus VAD decision for one speech segment. Music/BGM events and
/// fabricated short results are dropped; otherwise the cleaned text is kept.
fn accepted_text(recognized: &Recognized) -> Option<String> {
    let event = normalized_label(&recognized.event);
    if !event.is_empty() && !event.eq_ignore_ascii_case("speech") {
        return None;
    }
    let text = clean_transcript(&recognized.text);
    if text.is_empty() || is_fabricated_short_result(&text) {
        return None;
    }
    Some(text)
}

/// Partitions one whole-window decode back onto the VAD utterances using the
/// engine's token start timestamps. Tokens are consumed in order; the boundary
/// between two utterances is the middle of their silence gap, and the last
/// utterance receives any trailing punctuation. No timestamp is fabricated, so
/// the existing overlap-midpoint rule keeps working on real spans.
fn token_chunks(
    expected_text: &str,
    tokens: &[String],
    timestamps: &[f32],
    segments: &[(u64, u64)],
) -> AppResult<Vec<SpeechChunk>> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    let mut chunks = Vec::with_capacity(segments.len());
    let mut token_index = 0;
    for (index, &(start, samples)) in segments.iter().enumerate() {
        let end = start + samples;
        let boundary = match segments.get(index + 1) {
            Some(&(next_start, _)) if next_start > end => (end + next_start) / 2,
            Some(_) => end,
            None => u64::MAX,
        };
        let mut piece = String::new();
        while token_index < tokens.len() {
            let seconds = timestamps[token_index];
            if !seconds.is_finite() {
                token_index += 1;
                continue;
            }
            let sample = (f64::from(seconds.max(0.0)) * SAMPLE_RATE as f64) as u64;
            if sample >= boundary {
                break;
            }
            piece.push_str(&tokens[token_index]);
            token_index += 1;
        }
        let text = clean_transcript(&piece);
        if !text.is_empty() {
            chunks.push(SpeechChunk {
                start_ms: start * 1000 / SAMPLE_RATE as u64,
                end_ms: end * 1000 / SAMPLE_RATE as u64,
                text: prefixed(&text),
            });
        }
    }
    let joined = join_chunk_texts(chunks.iter().map(|chunk| chunk.text.as_str()));
    if normalized_characters(&joined) != normalized_characters(expected_text) {
        return Err(
            "The local SenseVoice engine returned token timestamps that do not cover its decoded text; audio retained"
                .into(),
        );
    }
    Ok(chunks)
}

fn normalized_characters(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// A final VAD utterance can be a noise burst that the detector called speech.
/// If its decoded text is far denser than any human speaking rate, the text is a
/// hallucination for that audio. Chinese characters count as one speech unit and
/// Latin letters as a quarter (five letters per syllable), so the same threshold
/// works for both scripts. It is deliberately generous (seven units per second
/// plus one) and only applies to the last utterance.
fn implausible_tail_text(text: &str, start_ms: u64, end_ms: u64) -> bool {
    let units: f64 = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                0.25
            } else if character.is_alphanumeric() {
                1.0
            } else {
                0.0
            }
        })
        .sum();
    if units < 1.0 {
        return false;
    }
    let seconds = end_ms.saturating_sub(start_ms) as f64 / 1000.0;
    units > seconds * 7.0 + 1.0
}

fn drop_implausible_tail(chunks: &mut Vec<SpeechChunk>) {
    let Some(last) = chunks.last() else {
        return;
    };
    if implausible_tail_text(&last.text, last.start_ms, last.end_ms) {
        chunks.pop();
    }
}

/// A chunk that starts with a Latin word needs a leading space so concatenating
/// independent spans does not glue it to the previous Chinese sentence.
fn prefixed(text: &str) -> String {
    match text.chars().next() {
        Some(character) if character.is_ascii_alphanumeric() => format!(" {text}"),
        _ => text.to_string(),
    }
}

/// Joins independently decoded utterances. A space is inserted only where an
/// English word would otherwise be glued to the previous text; Chinese
/// punctuation needs no separator.
fn join_chunk_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    let mut output = String::new();
    for text in texts {
        if text.is_empty() {
            continue;
        }
        let first = text.chars().next().unwrap();
        let needs_space = !output.is_empty()
            && !output.ends_with(' ')
            && (first.is_ascii_alphanumeric()
                || output
                    .chars()
                    .last()
                    .is_some_and(|character| character.is_ascii_alphanumeric()));
        if needs_space {
            output.push(' ');
        }
        output.push_str(text);
    }
    output.trim().to_string()
}

fn sha256_file(path: &Path) -> AppResult<[u8; 32]> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "Cannot read the local SenseVoice engine file for hash verification {}: {error}",
            path.display()
        )
    })?;
    let mut algorithm = BCRYPT_ALG_HANDLE(ptr::null_mut());
    let status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    };
    if status.0 < 0 {
        return Err(format!(
            "Cannot start SHA-256 verification of the local SenseVoice engine (NTSTATUS {:#x})",
            status.0 as u32
        ));
    }
    let mut digest = [0u8; 32];
    let status = unsafe { BCryptHash(algorithm, None, &bytes, &mut digest) };
    unsafe {
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    if status.0 < 0 {
        return Err(format!(
            "Cannot hash the local SenseVoice engine (NTSTATUS {:#x})",
            status.0 as u32
        ));
    }
    Ok(digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "listen-sensevoice-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn engine(directory: &Path) -> SenseVoice {
        SenseVoice::new(
            directory.into(),
            directory.join("model.int8.onnx"),
            directory.join("tokens.txt"),
            directory.join("silero_vad.onnx"),
            "auto".into(),
            2,
        )
    }

    #[test]
    fn fabricated_short_results_are_filtered_but_sentences_are_kept() {
        for fabricated in [
            "",
            " ",
            "The.",
            ".",
            "，。！",
            "<|zh|>",
            "<|Speech|>",
            "thank you.",
            "Thanks.",
            "字幕",
            "Music",
            "BGM。",
        ] {
            assert!(
                is_fabricated_short_result(fabricated),
                "{fabricated:?} must be filtered"
            );
        }
        for real in [
            "我们使用 Rust 来做测试。",
            "The API uses Rust.",
            "好",
            "CPU。",
            "第二段，端口是 8080。",
        ] {
            assert!(!is_fabricated_short_result(real), "{real:?} must be kept");
        }
    }

    #[test]
    fn control_tags_are_removed_from_text_and_labels() {
        assert_eq!(normalized_label("<|zh|>"), "zh");
        assert_eq!(normalized_label(" <|Speech|> "), "Speech");
        assert_eq!(
            clean_transcript("<|zh|><|NEUTRAL|><|Speech|>我们来做测试。"),
            "我们来做测试。"
        );
        assert_eq!(clean_transcript("two   spaces\there"), "two spaces here");
    }

    #[test]
    fn non_speech_events_drop_the_whole_recognition() {
        let speech = Recognized {
            text: "我们来做测试。".into(),
            _lang: "<|zh|>".into(),
            _emotion: "<|NEUTRAL|>".into(),
            event: "<|Speech|>".into(),
            tokens: Vec::new(),
            timestamps: Vec::new(),
        };
        assert_eq!(accepted_text(&speech).as_deref(), Some("我们来做测试。"));

        let music = Recognized {
            text: "The.".into(),
            event: "<|BGM|>".into(),
            ..speech
        };
        assert!(accepted_text(&music).is_none());

        let no_event = Recognized {
            text: "我们来做测试。".into(),
            event: String::new(),
            ..music
        };
        assert_eq!(
            accepted_text(&no_event).as_deref(),
            Some("我们来做测试。"),
            "an empty event should defer to the VAD plus short-result gate"
        );

        let fabricated = Recognized {
            text: "The.".into(),
            event: "<|Speech|>".into(),
            ..no_event
        };
        assert!(accepted_text(&fabricated).is_none());
    }

    #[test]
    fn joining_utterances_spaces_only_latin_words() {
        assert_eq!(
            join_chunk_texts(["我们用。", "Rust.", "实现音频采集。"]),
            "我们用。 Rust.实现音频采集。"
        );
        assert_eq!(
            join_chunk_texts(["The API uses Rust.", "Version 1.8.2."]),
            "The API uses Rust. Version 1.8.2."
        );
        assert_eq!(
            join_chunk_texts(["模型在本机运行。", "CPU。"]),
            "模型在本机运行。 CPU。"
        );
        assert_eq!(join_chunk_texts(Vec::<&str>::new()), "");
    }

    #[test]
    fn whole_window_text_is_attributed_to_vad_spans_through_token_timestamps() {
        let tokens: Vec<String> = ["这", "段", "语", "音"]
            .iter()
            .map(|token| token.to_string())
            .collect();
        let timestamps = [0.10f32, 0.30, 2.10, 2.30];
        let segments = [(0u64, 16_000u64), (32_000, 16_000)];
        let chunks = token_chunks("这段语音", &tokens, &timestamps, &segments).unwrap();
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(
            (
                chunks[0].start_ms,
                chunks[0].end_ms,
                chunks[0].text.as_str()
            ),
            (0, 1_000, "这段")
        );
        assert_eq!(
            (
                chunks[1].start_ms,
                chunks[1].end_ms,
                chunks[1].text.as_str()
            ),
            (2_000, 3_000, "语音")
        );
        assert_eq!(
            join_chunk_texts(chunks.iter().map(|chunk| chunk.text.as_str())),
            "这段语音"
        );
    }

    #[test]
    fn token_timestamps_that_do_not_cover_the_decoded_text_are_rejected() {
        let tokens = vec!["这".to_string()];
        let error = token_chunks("这段", &tokens, &[0.0], &[(0, 16_000)]).unwrap_err();
        assert!(error.contains("do not cover"), "{error}");
    }

    #[test]
    fn an_implausibly_dense_final_utterance_is_dropped() {
        // 5 CJK characters + one Latin letter in 400 ms is 13 units/s.
        assert!(implausible_tail_text("登录用户的I", 500, 900));
        assert!(!implausible_tail_text("谢谢大家", 500, 1_000));
        assert!(!implausible_tail_text("好", 0, 100));
        assert!(!implausible_tail_text("The API uses Rust.", 0, 1_500));
        assert!(!implausible_tail_text("这段语音只在本机处理。", 0, 2_200));

        let mut chunks = vec![
            SpeechChunk {
                start_ms: 0,
                end_ms: 2_200,
                text: "这段语音只在本机处理。".into(),
            },
            SpeechChunk {
                start_ms: 2_200,
                end_ms: 2_600,
                text: "登录用户的I".into(),
            },
        ];
        drop_implausible_tail(&mut chunks);
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert_eq!(chunks[0].text, "这段语音只在本机处理。");
    }

    #[test]
    fn a_missing_library_is_reported_as_an_explicit_error() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let directory = temp_dir("missing");
        let sense_voice = engine(&directory);
        let error = sense_voice.check_ready().unwrap_err();
        assert!(
            error.contains("sherpa-onnx C API library is missing"),
            "{error}"
        );
        assert!(error.contains("no cloud service"), "{error}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unknown_dll_is_rejected_before_any_call() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let directory = temp_dir("pin");
        for name in ["model.int8.onnx", "tokens.txt", "silero_vad.onnx"] {
            fs::write(directory.join(name), b"placeholder").unwrap();
        }
        fs::write(
            directory.join("sherpa-onnx-c-api.dll"),
            b"not the pinned build",
        )
        .unwrap();
        fs::write(directory.join("onnxruntime.dll"), b"not the pinned build").unwrap();
        let sense_voice = engine(&directory);
        let error = sense_voice.check_ready().unwrap_err();
        assert!(error.contains("does not match the pinned"), "{error}");
        assert!(error.contains("SHA-256"), "{error}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_failed_preparation_blocks_a_later_ready_claim() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let directory = temp_dir("failed");
        // All files exist and the DLL hash is wrong, so prepare fails into an
        // explicit state instead of loading anything.
        for name in [
            "sherpa-onnx-c-api.dll",
            "onnxruntime.dll",
            "model.int8.onnx",
            "tokens.txt",
            "silero_vad.onnx",
        ] {
            fs::write(directory.join(name), b"placeholder").unwrap();
        }
        let sense_voice = engine(&directory);
        let receiver = sense_voice.prepare().expect("preparation must be reported");
        let error = receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("preparation must finish")
            .unwrap_err();
        assert!(error.contains("could not be prepared"), "{error}");
        let error = sense_voice.check_ready().unwrap_err();
        assert!(
            error.contains("is unavailable") && error.contains("no cloud service"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&directory);
    }
}
