//! In-process reuse of the local whisper.cpp model.
//!
//! The official `whisper-server` keeps one decode state for every request. In
//! v1.8.2 that state contaminates later requests even with `no_context=true`, so
//! the same WAV no longer matches a one-shot `whisper-cli` run. This host loads
//! the same `whisper.dll` and model once, then creates a fresh state for every
//! request. It uses no socket, no server process and no network: only the local
//! DLL and model paths configured at startup.
//!
//! Every function pointer and structure below is copied from whisper.cpp
//! v1.8.2 `include/whisper.h`. The build is pinned by SHA-256 *before* any DLL
//! function is called, then the loaded defaults are checked against the mirrored
//! layout with `whisper_full_default_params_by_ref` (no by-value return). A
//! mismatch makes the caller fall back to the existing CLI path instead of
//! guessing at an ABI.
#![cfg(windows)]

use crate::{AppResult, TimedText, whisper_timing::TimedToken};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs,
    path::Path,
    ptr,
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

type WhisperContext = c_void;
type WhisperState = c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct WhisperAhead {
    n_text_layer: i32,
    n_head: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WhisperAheads {
    n_heads: usize,
    heads: *const WhisperAhead,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ContextParams {
    use_gpu: bool,
    flash_attn: bool,
    gpu_device: i32,
    dtw_token_timestamps: bool,
    dtw_aheads_preset: i32,
    dtw_n_top: i32,
    dtw_aheads: WhisperAheads,
    dtw_mem_size: usize,
}

/// `whisper_full_params` from whisper.h v1.8.2. The nested `greedy`,
/// `beam_search` and `vad_params` structs are flattened: each is naturally
/// aligned and adds no padding. The compile-time size check below and the
/// runtime default-value check both guard this definition.
#[repr(C)]
#[derive(Clone, Copy)]
struct FullParams {
    strategy: i32,
    n_threads: i32,
    n_max_text_ctx: i32,
    offset_ms: i32,
    duration_ms: i32,
    translate: bool,
    no_context: bool,
    no_timestamps: bool,
    single_segment: bool,
    print_special: bool,
    print_progress: bool,
    print_realtime: bool,
    print_timestamps: bool,
    token_timestamps: bool,
    thold_pt: f32,
    thold_ptsum: f32,
    max_len: i32,
    split_on_word: bool,
    max_tokens: i32,
    debug_mode: bool,
    audio_ctx: i32,
    tdrz_enable: bool,
    suppress_regex: *const c_char,
    initial_prompt: *const c_char,
    carry_initial_prompt: bool,
    prompt_tokens: *const i32,
    prompt_n_tokens: i32,
    language: *const c_char,
    detect_language: bool,
    suppress_blank: bool,
    suppress_nst: bool,
    temperature: f32,
    max_initial_ts: f32,
    length_penalty: f32,
    temperature_inc: f32,
    entropy_thold: f32,
    logprob_thold: f32,
    no_speech_thold: f32,
    greedy_best_of: i32,
    beam_size: i32,
    beam_patience: f32,
    new_segment_callback: *mut c_void,
    new_segment_callback_user_data: *mut c_void,
    progress_callback: *mut c_void,
    progress_callback_user_data: *mut c_void,
    encoder_begin_callback: *mut c_void,
    encoder_begin_callback_user_data: *mut c_void,
    abort_callback: *mut c_void,
    abort_callback_user_data: *mut c_void,
    logits_filter_callback: *mut c_void,
    logits_filter_callback_user_data: *mut c_void,
    grammar_rules: *const *const c_void,
    n_grammar_rules: usize,
    i_start_rule: usize,
    grammar_penalty: f32,
    vad: bool,
    vad_model_path: *const c_char,
    vad_threshold: f32,
    vad_min_speech_duration_ms: i32,
    vad_min_silence_duration_ms: i32,
    vad_max_speech_duration_s: f32,
    vad_speech_pad_ms: i32,
    vad_samples_overlap: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TokenData {
    id: i32,
    tid: i32,
    p: f32,
    plog: f32,
    pt: f32,
    ptsum: f32,
    t0: i64,
    t1: i64,
    t_dtw: i64,
    vlen: f32,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<FullParams>() == 304);
const _: () = assert!(std::mem::size_of::<TokenData>() == 56);

type InitFromFileNoState =
    unsafe extern "system" fn(*const c_char, ContextParams) -> *mut WhisperContext;
type InitState = unsafe extern "system" fn(*mut WhisperContext) -> *mut WhisperState;
type FreeState = unsafe extern "system" fn(*mut WhisperState);
type FreeContext = unsafe extern "system" fn(*mut WhisperContext);
/// The pinned v1.8.2 build exports the by-reference accessor as well, so the host
/// never relies on the size of a by-value return before the layout is validated.
type FullDefaultParamsByRef = unsafe extern "system" fn(i32) -> *const FullParams;
type FullWithState = unsafe extern "system" fn(
    *mut WhisperContext,
    *mut WhisperState,
    FullParams,
    *const f32,
    i32,
) -> i32;
type NSegmentsFromState = unsafe extern "system" fn(*mut WhisperState) -> i32;
type NTokensFromState = unsafe extern "system" fn(*mut WhisperState, i32) -> i32;
type SegmentTextFromState = unsafe extern "system" fn(*mut WhisperState, i32) -> *const c_char;
type TokenText = unsafe extern "system" fn(*mut WhisperContext, i32) -> *const c_char;
type TokenDataFromState = unsafe extern "system" fn(*mut WhisperState, i32, i32) -> TokenData;
type IsMultilingual = unsafe extern "system" fn(*mut WhisperContext) -> i32;
type TokenEot = unsafe extern "system" fn(*mut WhisperContext) -> i32;

#[derive(Clone, Copy)]
struct Api {
    init_no_state: InitFromFileNoState,
    init_state: InitState,
    free_state: FreeState,
    free_context: FreeContext,
    full_default_params: FullDefaultParamsByRef,
    full_with_state: FullWithState,
    n_segments: NSegmentsFromState,
    n_tokens: NTokensFromState,
    segment_text: SegmentTextFromState,
    token_text: TokenText,
    token_data: TokenDataFromState,
    is_multilingual: IsMultilingual,
    token_eot: TokenEot,
}

impl Api {
    unsafe fn load(module: HMODULE) -> AppResult<Self> {
        unsafe fn symbol<T: Copy>(module: HMODULE, name: PCSTR, label: &str) -> AppResult<T> {
            let address = unsafe { GetProcAddress(module, name) }
                .ok_or_else(|| format!("Local whisper.dll does not export {label}"))?;
            Ok(unsafe { std::mem::transmute_copy(&address) })
        }
        Ok(Self {
            init_no_state: unsafe {
                symbol(
                    module,
                    s!("whisper_init_from_file_with_params_no_state"),
                    "whisper_init_from_file_with_params_no_state",
                )?
            },
            init_state: unsafe { symbol(module, s!("whisper_init_state"), "whisper_init_state")? },
            free_state: unsafe { symbol(module, s!("whisper_free_state"), "whisper_free_state")? },
            free_context: unsafe { symbol(module, s!("whisper_free"), "whisper_free")? },
            full_default_params: unsafe {
                symbol(
                    module,
                    s!("whisper_full_default_params_by_ref"),
                    "whisper_full_default_params_by_ref",
                )?
            },
            full_with_state: unsafe {
                symbol(
                    module,
                    s!("whisper_full_with_state"),
                    "whisper_full_with_state",
                )?
            },
            n_segments: unsafe {
                symbol(
                    module,
                    s!("whisper_full_n_segments_from_state"),
                    "whisper_full_n_segments_from_state",
                )?
            },
            n_tokens: unsafe {
                symbol(
                    module,
                    s!("whisper_full_n_tokens_from_state"),
                    "whisper_full_n_tokens_from_state",
                )?
            },
            segment_text: unsafe {
                symbol(
                    module,
                    s!("whisper_full_get_segment_text_from_state"),
                    "whisper_full_get_segment_text_from_state",
                )?
            },
            token_text: unsafe {
                symbol(module, s!("whisper_token_to_str"), "whisper_token_to_str")?
            },
            token_data: unsafe {
                symbol(
                    module,
                    s!("whisper_full_get_token_data_from_state"),
                    "whisper_full_get_token_data_from_state",
                )?
            },
            is_multilingual: unsafe {
                symbol(
                    module,
                    s!("whisper_is_multilingual"),
                    "whisper_is_multilingual",
                )?
            },
            token_eot: unsafe { symbol(module, s!("whisper_token_eot"), "whisper_token_eot")? },
        })
    }
}

/// A loaded model and DLL. The decode state is created per request, so no
/// request can condition on another request's audio or text.
pub(crate) struct ModelHost {
    _module: HMODULE,
    api: Api,
    context: *mut WhisperContext,
    threads: i32,
    language: CString,
}

// The host is only used behind a Mutex, and whisper.cpp never accesses a context
// concurrently. The loaded model data is read-only after initialization.
unsafe impl Send for ModelHost {}

/// SHA-256 of the whisper.cpp v1.8.2 CPU `whisper.dll` that the mirrored
/// structures and default values belong to. It is checked before the library is
/// loaded or any export is called. A different build makes the caller use the CLI
/// instead of handing it a structure whose layout is only assumed.
const PINNED_WHISPER_DLL_SHA256: [u8; 32] = [
    0x41, 0x55, 0xed, 0xb6, 0x86, 0x88, 0x10, 0xb5, 0x64, 0x23, 0xcb, 0x00, 0x89, 0xfe, 0xe8, 0x5f,
    0x4e, 0x87, 0x39, 0xfb, 0x86, 0x05, 0x3b, 0x26, 0x9f, 0x4e, 0x7c, 0x63, 0x50, 0xec, 0x61, 0xff,
];

/// Why the in-process host is unavailable. A host-level problem (missing or
/// unknown DLL, missing export) may still be recoverable through the CLI; a model
/// that `whisper_init_from_file_with_params_no_state` rejects cannot be loaded by
/// the CLI either, so it must not be reported as a working fallback.
pub(crate) enum LoadFailure {
    Host(String),
    Model(String),
}

impl ModelHost {
    pub(crate) fn load(
        engine: &Path,
        model: &Path,
        threads: usize,
        language: &str,
    ) -> Result<Self, LoadFailure> {
        let dll = engine.with_file_name("whisper.dll");
        if !dll.is_file() {
            return Err(LoadFailure::Host(format!(
                "The in-process model host library is missing: {}",
                dll.display()
            )));
        }
        // Fail closed: validate the exact build before calling into it. Without
        // this, `whisper_full_default_params_by_ref` or the by-value decode call
        // could be handed a structure the DLL does not share.
        let actual = sha256_file(&dll).map_err(LoadFailure::Host)?;
        if actual != PINNED_WHISPER_DLL_SHA256 {
            return Err(LoadFailure::Host(format!(
                "The local whisper.dll does not match the pinned whisper.cpp v1.8.2 CPU build (expected SHA-256 {}, found {}); refusing the in-process model host",
                hex(&PINNED_WHISPER_DLL_SHA256),
                hex(&actual)
            )));
        }
        let dll_wide = wide(&dll.to_string_lossy());
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(dll_wide.as_ptr()),
                None,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        }
        .map_err(|e| {
            LoadFailure::Host(format!(
                "Cannot load the in-process model host {}: {e}",
                dll.display()
            ))
        })?;
        let api = match unsafe { Api::load(module) } {
            Ok(api) => api,
            Err(error) => {
                unsafe {
                    let _ = FreeLibrary(module);
                }
                return Err(LoadFailure::Host(error));
            }
        };
        // The DLL is pinned, but still verify that the layout we mirror reports the
        // documented v1.8.2 defaults. This catches a build identity mistake before
        // a by-value decode call.
        let match_result = match defaults_for(&api, 1) {
            Ok(defaults) if layout_matches(&defaults) => Ok(()),
            Ok(_) | Err(_) => Err(()),
        };
        if let Err(()) = match_result {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(LoadFailure::Host(
                "The local whisper.dll build does not match the expected v1.8.2 parameter layout; refusing the in-process model host"
                    .into(),
            ));
        }
        let model_wide = model.to_string_lossy();
        let model_c = CString::new(model_wide.as_bytes()).map_err(|_| {
            LoadFailure::Host("The local model path contains an embedded NUL".into())
        })?;
        let context_params = ContextParams {
            use_gpu: false,
            flash_attn: true,
            gpu_device: 0,
            dtw_token_timestamps: false,
            dtw_aheads_preset: 0,
            dtw_n_top: 0,
            dtw_aheads: WhisperAheads {
                n_heads: 0,
                heads: ptr::null(),
            },
            dtw_mem_size: 0,
        };
        let context = unsafe { (api.init_no_state)(model_c.as_ptr(), context_params) };
        if context.is_null() {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(LoadFailure::Model(format!(
                "The local model host could not load the model: {}",
                model.display()
            )));
        }
        Ok(Self {
            _module: module,
            api,
            context,
            threads: threads as i32,
            language: CString::new(language).map_err(|_| {
                LoadFailure::Host("The configured language contains an embedded NUL".into())
            })?,
        })
    }

    pub(crate) fn transcribe(&mut self, wav: &Path) -> AppResult<String> {
        let pcm = read_pcm(wav)?;
        self.run_plain(&pcm)
    }

    pub(crate) fn transcribe_segment(&mut self, wav: &Path) -> AppResult<Vec<TimedText>> {
        let pcm = read_pcm(wav)?;
        self.run_timed(&pcm)
    }

    fn state(&self) -> AppResult<StateGuard<'_>> {
        let state = unsafe { (self.api.init_state)(self.context) };
        if state.is_null() {
            return Err("The local model host could not allocate a decode state".into());
        }
        Ok(StateGuard { host: self, state })
    }

    fn params(&self, token_timestamps: bool) -> AppResult<FullParams> {
        let mut params = defaults_for(&self.api, 1)?;
        params.n_threads = self.threads;
        params.language = self.language.as_ptr();
        params.no_context = true;
        params.no_timestamps = true;
        params.token_timestamps = token_timestamps;
        params.greedy_best_of = 5;
        params.beam_size = 5;
        params.temperature = 0.0;
        params.temperature_inc = 0.2;
        params.entropy_thold = 2.4;
        params.logprob_thold = -1.0;
        params.no_speech_thold = 0.6;
        params.suppress_nst = false;
        Ok(params)
    }

    fn run_plain(&mut self, pcm: &[f32]) -> AppResult<String> {
        let state = self.state()?;
        let params = self.params(false)?;
        let result = unsafe {
            (self.api.full_with_state)(
                self.context,
                state.state,
                params,
                pcm.as_ptr(),
                pcm.len() as i32,
            )
        };
        if result != 0 {
            return Err(format!(
                "The local model host could not recognize this audio ({result}); audio retained"
            ));
        }
        let (text, _) = self.plain_text(state.state)?;
        Ok(text)
    }

    fn run_timed(&mut self, pcm: &[f32]) -> AppResult<Vec<TimedText>> {
        let state = self.state()?;
        let params = self.params(true)?;
        let result = unsafe {
            (self.api.full_with_state)(
                self.context,
                state.state,
                params,
                pcm.as_ptr(),
                pcm.len() as i32,
            )
        };
        if result != 0 {
            return Err(format!(
                "The local model host could not recognize this audio ({result}); audio retained"
            ));
        }
        self.timed_text(state.state)
    }

    fn plain_text(&self, state: *mut WhisperState) -> AppResult<(String, usize)> {
        let count = unsafe { (self.api.n_segments)(state) };
        if count < 0 {
            return Err("The local model host returned an invalid segment count".into());
        }
        let mut pieces = Vec::with_capacity(count as usize);
        for index in 0..count {
            let pointer = unsafe { (self.api.segment_text)(state, index) };
            if pointer.is_null() {
                return Err("The local model host returned a missing segment".into());
            }
            let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
            pieces.push(String::from_utf8(bytes.to_vec()).map_err(|error| {
                format!("The local model host produced an incomplete UTF-8 result: {error}")
            })?);
        }
        Ok((pieces.join("\n").trim().to_string(), count as usize))
    }

    fn timed_text(&self, state: *mut WhisperState) -> AppResult<Vec<TimedText>> {
        let count = unsafe { (self.api.n_segments)(state) };
        if count < 0 {
            return Err("The local model host returned an invalid segment count".into());
        }
        let eot = unsafe { (self.api.token_eot)(self.context) } as u32;
        let multilingual = unsafe { (self.api.is_multilingual)(self.context) } != 0;
        let mut segments = Vec::with_capacity(count as usize);
        for index in 0..count {
            let pointer = unsafe { (self.api.segment_text)(state, index) };
            if pointer.is_null() {
                return Err("The local model host returned a missing segment".into());
            }
            let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
            let text = String::from_utf8(bytes.to_vec()).map_err(|error| {
                format!("The local model host produced an incomplete UTF-8 result: {error}")
            })?;
            let token_count = unsafe { (self.api.n_tokens)(state, index) };
            if token_count < 0 {
                return Err("The local model host returned an invalid token count".into());
            }
            let mut tokens = Vec::with_capacity(token_count as usize);
            for token_index in 0..token_count {
                let data = unsafe { (self.api.token_data)(state, index, token_index) };
                if data.id < 0 || data.id as u32 >= eot {
                    continue;
                }
                let token_pointer = unsafe { (self.api.token_text)(self.context, data.id) };
                if token_pointer.is_null() {
                    continue;
                }
                let text = unsafe { CStr::from_ptr(token_pointer) }.to_bytes().to_vec();
                let offsets = if data.t0 > -1 && data.t1 > -1 {
                    Some((data.t0 * 10, data.t1 * 10))
                } else {
                    None
                };
                tokens.push(TimedToken {
                    id: data.id as u32,
                    text,
                    offsets,
                });
            }
            segments.push((text, tokens));
        }
        crate::whisper_timing::build(multilingual, segments)
    }
}

/// Reads the DLL's defaults by reference. The pinned v1.8.2 build exports this
/// accessor; it avoids assuming the size of a by-value return before the layout
/// check has run.
fn defaults_for(api: &Api, strategy: i32) -> AppResult<FullParams> {
    let pointer = unsafe { (api.full_default_params)(strategy) };
    if pointer.is_null() {
        return Err("The local whisper.dll returned no default parameters".into());
    }
    // The library is pinned by SHA-256, so this pointer names a FullParams of the
    // mirrored layout. Copying it does not call into the unverified ABI.
    Ok(unsafe { ptr::read(pointer) })
}

fn close(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 1e-6
}

/// Checks the documented v1.8.2 defaults before any by-value decode call. This is
/// defense in depth after the DLL hash check: a wrongly identified build must be
/// rejected, not interpreted as a compatible ABI.
fn layout_matches(defaults: &FullParams) -> bool {
    defaults.strategy == 1
        && defaults.n_threads == 4
        && defaults.n_max_text_ctx == 16_384
        && defaults.offset_ms == 0
        && defaults.duration_ms == 0
        && !defaults.translate
        && defaults.no_context
        && !defaults.no_timestamps
        && !defaults.single_segment
        && !defaults.print_special
        && defaults.print_progress
        && !defaults.print_realtime
        && defaults.print_timestamps
        && !defaults.token_timestamps
        && close(defaults.thold_pt, 0.01)
        && close(defaults.thold_ptsum, 0.01)
        && defaults.max_len == 0
        && !defaults.split_on_word
        && defaults.max_tokens == 0
        && !defaults.debug_mode
        && defaults.audio_ctx == 0
        && !defaults.tdrz_enable
        && defaults.suppress_regex.is_null()
        && defaults.initial_prompt.is_null()
        && !defaults.carry_initial_prompt
        && defaults.prompt_tokens.is_null()
        && defaults.prompt_n_tokens == 0
        && !defaults.detect_language
        && defaults.suppress_blank
        && !defaults.suppress_nst
        && defaults.temperature == 0.0
        && close(defaults.max_initial_ts, 1.0)
        && close(defaults.length_penalty, -1.0)
        && close(defaults.temperature_inc, 0.2)
        && close(defaults.entropy_thold, 2.4)
        && close(defaults.logprob_thold, -1.0)
        && close(defaults.no_speech_thold, 0.6)
        && defaults.greedy_best_of == -1
        && defaults.beam_size == 5
        && close(defaults.beam_patience, -1.0)
        && defaults.new_segment_callback.is_null()
        && defaults.new_segment_callback_user_data.is_null()
        && defaults.progress_callback.is_null()
        && defaults.progress_callback_user_data.is_null()
        && defaults.encoder_begin_callback.is_null()
        && defaults.encoder_begin_callback_user_data.is_null()
        && defaults.abort_callback.is_null()
        && defaults.abort_callback_user_data.is_null()
        && defaults.logits_filter_callback.is_null()
        && defaults.logits_filter_callback_user_data.is_null()
        && defaults.grammar_rules.is_null()
        && defaults.n_grammar_rules == 0
        && defaults.i_start_rule == 0
        && close(defaults.grammar_penalty, 100.0)
        && !defaults.vad
        && defaults.vad_model_path.is_null()
        && close(defaults.vad_threshold, 0.5)
        && defaults.vad_min_speech_duration_ms == 250
        && defaults.vad_min_silence_duration_ms == 100
        && defaults.vad_max_speech_duration_s >= 3.4e38
        && defaults.vad_speech_pad_ms == 30
        && close(defaults.vad_samples_overlap, 0.1)
}

fn sha256_file(path: &Path) -> AppResult<[u8; 32]> {
    let bytes = fs::read(path).map_err(|e| {
        format!(
            "Cannot read the in-process model host for hash verification {}: {e}",
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
            "Cannot start SHA-256 verification of the local whisper.dll (NTSTATUS {:#x})",
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
            "Cannot hash the local whisper.dll (NTSTATUS {:#x})",
            status.0 as u32
        ));
    }
    Ok(digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

impl Drop for ModelHost {
    fn drop(&mut self) {
        unsafe { (self.api.free_context)(self.context) };
        // whisper.dll and its ggml dependencies are intentionally not unloaded.
        // FreeLibrary during process teardown has produced an access violation
        // after the model was freed; the OS reclaims this module at exit.
    }
}

struct StateGuard<'a> {
    host: &'a ModelHost,
    state: *mut WhisperState,
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        unsafe { (self.host.api.free_state)(self.state) }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn read_pcm(path: &Path) -> AppResult<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("The local model host cannot read the captured WAV: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != 16_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("The local model host expects the application's mono 16-bit 16 kHz WAV".into());
    }
    reader
        .samples::<i16>()
        .map(|sample| {
            sample
                .map(|value| value as f32 / 32768.0)
                .map_err(|e| format!("The local model host cannot decode the captured WAV: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "listen-host-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn sha256_of_a_known_payload_is_the_standard_digest() {
        let directory = temp_dir("sha");
        let path = directory.join("payload.bin");
        fs::write(&path, b"abc").unwrap();
        let digest = sha256_file(&path).unwrap();
        assert_eq!(
            hex(&digest),
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unknown_whisper_dll_is_rejected_before_any_call() {
        let directory = temp_dir("pin");
        let engine = directory.join("whisper-cli.exe");
        let dll = directory.join("whisper.dll");
        let model = directory.join("model.bin");
        fs::write(&dll, b"not the pinned whisper.cpp build").unwrap();
        fs::write(&model, b"ggml").unwrap();
        let error = match ModelHost::load(&engine, &model, 1, "zh") {
            Err(error) => error,
            Ok(_) => panic!("an unknown DLL must be rejected before any call"),
        };
        let detail = match &error {
            LoadFailure::Host(detail) => detail,
            LoadFailure::Model(detail) => panic!("expected a host failure, got: {detail}"),
        };
        assert!(detail.contains("does not match the pinned"), "{detail}");
        assert!(detail.contains("SHA-256"), "{detail}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_layout_check_is_not_vacuous() {
        let zeroed: FullParams = unsafe { std::mem::zeroed() };
        assert!(!layout_matches(&zeroed));
    }
}
