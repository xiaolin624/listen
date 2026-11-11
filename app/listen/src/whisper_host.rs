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
//! v1.8.2 `include/whisper.h`; `validate_layout` checks the loaded DLL's
//! defaults before the host is used. A mismatch makes the caller fall back to
//! the existing CLI path instead of guessing at an ABI.
#![cfg(windows)]

use crate::{AppResult, TimedText, whisper_timing::TimedToken};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    ptr,
};
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
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
type FullDefaultParams = unsafe extern "system" fn(i32) -> FullParams;
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
    full_default_params: FullDefaultParams,
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
                    s!("whisper_full_default_params"),
                    "whisper_full_default_params",
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

impl ModelHost {
    pub(crate) fn load(
        engine: &Path,
        model: &Path,
        threads: usize,
        language: &str,
    ) -> AppResult<Self> {
        let dll = engine.with_file_name("whisper.dll");
        if !dll.is_file() {
            return Err(format!(
                "The in-process model host library is missing: {}",
                dll.display()
            ));
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
            format!(
                "Cannot load the in-process model host {}: {e}",
                dll.display()
            )
        })?;
        let api = match unsafe { Api::load(module) } {
            Ok(api) => api,
            Err(error) => {
                unsafe {
                    let _ = FreeLibrary(module);
                }
                return Err(error);
            }
        };
        // Validate the struct layout against the DLL before passing anything by
        // value. A mismatched header would otherwise corrupt the stack.
        let defaults = unsafe { (api.full_default_params)(1) };
        let expected = defaults.strategy == 1
            && defaults.n_max_text_ctx == 16_384
            && defaults.beam_size == 5
            && (defaults.grammar_penalty - 100.0).abs() < 1e-6
            && (defaults.thold_pt - 0.01).abs() < 1e-6
            && (defaults.temperature_inc - 0.2).abs() < 1e-6;
        if !expected {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(
                "The local whisper.dll build does not match the expected v1.8.2 parameter layout; refusing the in-process model host".into(),
            );
        }
        let model_wide = model.to_string_lossy();
        let model_c = CString::new(model_wide.as_bytes())
            .map_err(|_| "The local model path contains an embedded NUL".to_string())?;
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
            return Err(format!(
                "The local model host could not load the model: {}",
                model.display()
            ));
        }
        Ok(Self {
            _module: module,
            api,
            context,
            threads: threads as i32,
            language: CString::new(language)
                .map_err(|_| "The configured language contains an embedded NUL".to_string())?,
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

    fn params(&self, token_timestamps: bool) -> FullParams {
        let mut params = unsafe { (self.api.full_default_params)(1) };
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
        params
    }

    fn run_plain(&mut self, pcm: &[f32]) -> AppResult<String> {
        let state = self.state()?;
        let params = self.params(false);
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
        let params = self.params(true);
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
