//! Local 术语表 (glossary) correction for recognised text.
//!
//! The user maintains a UTF-8 text file with one term per line:
//!
//! ```text
//! # comment
//! DeepSeek Harness | shen du qiu suo, 迪普西克
//! WASAPI
//! ```
//!
//! `|` separates the canonical spelling from optional aliases. Matching is
//! approximate: both sides are lowercased and stripped of separators, and
//! multi-token spans of the recognized text are compared against the entry keys
//! by edit distance. A correction replaces only the matched span, so the rest of
//! the sentence, its spacing and its line structure are preserved.

use crate::AppResult;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Process-wide default glossary path. The executable sets it once from
/// `--glossary`; tests inject a per-application path instead.
static DEFAULT_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set the glossary file used by applications created from now on. Called once by
/// the executable after parsing `--glossary`; a second call is ignored.
pub fn set_default_path(path: impl Into<PathBuf>) {
    let _ = DEFAULT_PATH.set(path.into());
}

/// Candidate glossary paths for an application whose runtime directory is
/// `runtime`. An explicit `--glossary` path is used alone; otherwise the runtime
/// copy is preferred and the copy next to the executable is the fallback, so a
/// copied portable build still finds the glossary shipped beside it. When no
/// candidate exists, loading the first path is a silent no-op.
pub(crate) fn default_paths(runtime: &Path, executable: Option<&Path>) -> Vec<PathBuf> {
    if let Some(path) = DEFAULT_PATH.get() {
        return vec![path.clone()];
    }
    let runtime_path = runtime.join("glossary.txt");
    let mut paths = vec![runtime_path.clone()];
    if let Some(directory) = executable.and_then(Path::parent) {
        let sibling = directory.join("glossary.txt");
        if sibling != runtime_path {
            paths.push(sibling);
        }
    }
    paths
}

/// A loaded 术语表. The application reloads it at capture start.
#[derive(Clone, Debug, Default)]
pub struct Glossary {
    entries: Vec<Entry>,
    keys: Vec<CompiledKey>,
    exact: HashMap<String, usize>,
    /// Approximate candidates bucketed by the first and last normalized character,
    /// which a mis-recognition nearly always preserves.
    by_edges: HashMap<(char, char), Vec<usize>>,
    max_latin_tokens: usize,
    max_cjk_tokens: usize,
}

#[derive(Clone, Debug)]
struct Entry {
    canonical: String,
}

#[derive(Clone, Debug)]
struct Key {
    normalized: String,
    tokens: Vec<String>,
    script: Script,
    /// Canonical keys are trusted to identify the term; aliases are user guesses
    /// and get the extra short-span safety check.
    trusted: bool,
    /// An all-caps spelling such as `JSON` may be recognised through one dropped
    /// or inserted vowel, which is how speech engines spell it in practice.
    acronym: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    Latin,
    Cjk,
}

#[derive(Clone, Debug)]
struct CompiledKey {
    entry: usize,
    chars: Vec<char>,
    /// Normalized key tokens as characters. Latin keys hold words; CJK keys hold
    /// one character per token so a term can be located inside Chinese text.
    tokens: Vec<Vec<char>>,
    script: Script,
    trusted: bool,
    acronym: bool,
}

#[derive(Clone, Debug)]
struct Token {
    normalized: String,
    chars: Vec<char>,
    start: usize,
    end: usize,
    script: Script,
}

impl Glossary {
    /// Read the glossary from a human-editable file. A missing file is an empty
    /// glossary; an unreadable or malformed file is an explicit error.
    pub fn load(path: impl AsRef<Path>) -> AppResult<Self> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Correct one recognized text. Only matched spans are replaced; every other
    /// byte, including spacing and line breaks, is preserved.
    pub fn correct(&self, text: &str) -> String {
        if self.entries.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let mut corrected = String::with_capacity(text.len());
        let mut scratch = Scratch::default();
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                corrected.push('\n');
            }
            self.correct_line(line, &mut scratch, &mut corrected);
        }
        corrected
    }

    fn parse(text: &str) -> AppResult<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut parsed: Vec<(String, Vec<Key>)> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (canonical, aliases) = match line.split_once('|') {
                Some((canonical, aliases)) => {
                    if aliases.contains('|') {
                        return Err(format!("line {line_number}: more than one '|' separator"));
                    }
                    (canonical.trim(), Some(aliases))
                }
                None => (line, None),
            };
            if canonical.is_empty() {
                return Err(format!("line {line_number}: the term before '|' is empty"));
            }
            let canonical_key = Key::from_text(canonical, line_number, true)?;
            let mut keys = vec![canonical_key.clone()];
            if let Some(aliases) = aliases {
                for alias in aliases.split(',') {
                    let alias = alias.trim();
                    if alias.is_empty() {
                        continue;
                    }
                    keys.push(Key::from_text(alias, line_number, false)?);
                }
            }
            // A multi-word term is also recognised by its distinctive leading
            // word, so "deep seek" or "deepsek" already maps to the full canonical
            // form without listing every mis-recognition as an alias.
            let leading = &canonical_key.tokens[0];
            if canonical_key.script == Script::Latin
                && canonical_key.tokens.len() >= 2
                && leading.chars().count() >= 6
                && !keys.iter().any(|key| key.normalized == *leading)
            {
                keys.push(Key {
                    normalized: leading.clone(),
                    tokens: vec![leading.clone()],
                    script: Script::Latin,
                    trusted: true,
                    acronym: false,
                });
            }
            parsed.push((canonical.to_string(), keys));
        }

        let mut glossary = Self::default();
        for (entry, keys) in parsed {
            let entry_index = glossary.entries.len();
            glossary.entries.push(Entry { canonical: entry });
            for key in keys {
                let normalized = key.normalized.clone();
                let compiled = CompiledKey {
                    entry: entry_index,
                    chars: normalized.chars().collect(),
                    tokens: key
                        .tokens
                        .iter()
                        .map(|token| token.chars().collect())
                        .collect(),
                    script: key.script,
                    trusted: key.trusted,
                    acronym: key.acronym,
                };
                let key_index = glossary.keys.len();
                glossary.exact.entry(normalized).or_insert(key_index);
                if let (Some(first), Some(last)) = (compiled.chars.first(), compiled.chars.last()) {
                    glossary
                        .by_edges
                        .entry((*first, *last))
                        .or_default()
                        .push(key_index);
                }
                match compiled.script {
                    Script::Latin => {
                        glossary.max_latin_tokens =
                            glossary.max_latin_tokens.max(compiled.tokens.len());
                    }
                    Script::Cjk => {
                        glossary.max_cjk_tokens =
                            glossary.max_cjk_tokens.max(compiled.tokens.len());
                    }
                }
                glossary.keys.push(compiled);
            }
        }
        Ok(glossary)
    }

    fn correct_line(&self, line: &str, scratch: &mut Scratch, corrected: &mut String) {
        let tokens = tokenize(line);
        let mut cursor = 0;
        let mut index = 0;
        while index < tokens.len() {
            let script = tokens[index].script;
            let longest = if script == Script::Latin {
                self.max_latin_tokens
            } else {
                self.max_cjk_tokens
            } + 1;
            let available = tokens[index..]
                .iter()
                .take_while(|token| token.script == script)
                .count();
            let mut matched = None;
            let mut window_size = longest.min(available);
            while window_size > 0 {
                let window = &tokens[index..index + window_size];
                let normalized: String = window
                    .iter()
                    .map(|token| token.normalized.as_str())
                    .collect();
                if let Some((entry, key_index)) = self.match_window(window, &normalized, scratch) {
                    let start = window[0].start;
                    let end = window[window_size - 1].end;
                    if normalized.chars().count() > 2
                        || self.keys[key_index].trusted
                        || short_span_next_to_cjk(line, start, end)
                    {
                        matched = Some((window_size, entry));
                        break;
                    }
                }
                window_size -= 1;
            }
            if let Some((window_size, entry)) = matched {
                let start = tokens[index].start;
                let end = tokens[index + window_size - 1].end;
                corrected.push_str(&line[cursor..start]);
                corrected.push_str(&self.entries[entry].canonical);
                cursor = end;
                index += window_size;
            } else {
                index += 1;
            }
        }
        corrected.push_str(&line[cursor..]);
    }

    fn match_window(
        &self,
        window: &[Token],
        normalized: &str,
        scratch: &mut Scratch,
    ) -> Option<(usize, usize)> {
        if let Some(&key_index) = self.exact.get(normalized) {
            return Some((self.keys[key_index].entry, key_index));
        }
        let span: Vec<char> = normalized.chars().collect();
        if span.len() < MIN_APPROXIMATE_LEN {
            return None;
        }
        let first = *span.first()?;
        let last = *span.last()?;
        let candidates = self.by_edges.get(&(first, last))?;
        let mut best: Option<(f64, usize)> = None;
        for &key_index in candidates {
            let key = &self.keys[key_index];
            if key.script != window[0].script {
                continue;
            }
            if let Some(score) = approximate_score(&span, window, key, scratch) {
                if best.is_none_or(|(best_score, _)| score > best_score) {
                    best = Some((score, key_index));
                }
            }
        }
        best.map(|(_, key_index)| (self.keys[key_index].entry, key_index))
    }
}

/// Approximate matching limits. They are deliberately separated: a one-word key
/// such as "Rust" is short enough to collide with ordinary words, so it needs a
/// tighter ratio than a multi-word term like "DeepSeek Harness".
const MIN_APPROXIMATE_LEN: usize = 5;
const SINGLE_KEY_THRESHOLD: f64 = 0.85;
const MULTI_KEY_THRESHOLD: f64 = 0.6;
const EDGE_THRESHOLD: f64 = 0.6;
const MAX_LENGTH_DIFFERENCE: f64 = 0.4;
/// Real speech also changes the number of words: "sens voice" for `SenseVoice`
/// and "waSAPpilook back" for `WASAPI loopback`. Comparing the whole normalized
/// span with the whole normalized key recovers those without loosening the
/// token-level alignment. The key must stay distinctive (six characters) and the
/// first/last character must still agree; 0.85 accepts the measured 0.87-0.90
/// deformations while "wasabi" (0.83) and "deep water harness" (0.75) stay below.
const WHOLE_SPAN_THRESHOLD: f64 = 0.85;
const WHOLE_SPAN_MIN_KEY_LEN: usize = 6;
/// The shortest all-caps acronym that may lose or gain one vowel ("jason" ->
/// "JSON"). Three-letter acronyms stay exact-only because "QOS" is one edit from
/// "QPS", and two-letter acronyms ("PR") are too ambiguous to approximate.
const ACRONYM_MIN_KEY_LEN: usize = 4;
const ACRONYM_MIN_SPAN_LEN: usize = 4;
/// A multi-word key may be recognised by a clearly truncated first word when
/// every remaining word still matches ("de harness" -> "DeepSeek Harness").
const ABBREVIATED_FIRST_WORD_MIN_LEN: usize = 6;

#[derive(Clone, Copy)]
enum Edge {
    Prefix,
    Suffix,
}

fn approximate_score(
    span: &[char],
    window: &[Token],
    key: &CompiledKey,
    scratch: &mut Scratch,
) -> Option<f64> {
    if window.len() == 1 && key.acronym {
        if let Some(score) = acronym_score(span, &key.chars) {
            return Some(score);
        }
    }
    if key.chars.len() < MIN_APPROXIMATE_LEN || span.len() < MIN_APPROXIMATE_LEN {
        return None;
    }
    if !length_difference_ok(span.len(), key.chars.len()) {
        return None;
    }
    // A mis-recognition almost always preserves the first and last character of
    // the term; requiring both is a cheap guard against comparing unrelated
    // words at the same length.
    if span.first() != key.chars.first() || span.last() != key.chars.last() {
        return None;
    }
    let key_tokens = key.tokens.len();
    let window_tokens = window.len();

    // Align key tokens to contiguous groups of span tokens. Every key token must
    // get at least one token and every span token must belong to a group, which
    // lets "deep sea harnessness" align as "deepsea" + "harnessness" while a
    // different phrase such as "deep water harness" stays below the threshold.
    let structured = match (key_tokens, window_tokens) {
        (1, 1) => {
            let score = scratch.ratio(span, &key.chars);
            (score >= SINGLE_KEY_THRESHOLD).then_some(score)
        }
        (1, _) => None,
        (key_tokens, window_tokens) if window_tokens >= key_tokens => {
            // When the span has more tokens than the key, every key token must
            // cover one or more span tokens. Anchor the outer tokens to the edges
            // of the key word so an adjacent ordinary word ("Harness and") is
            // never swallowed.
            if window_tokens > key_tokens && !outer_tokens_align(window, key, scratch) {
                None
            } else {
                aligned_token_score(window, key, scratch)
            }
        }
        _ => None,
    };
    if let Some(score) = structured {
        return Some(score);
    }

    // Word counts differ or the token grouping did not line up. Compare the whole
    // normalized span against the whole normalized key before giving up. This is
    // a Latin-word problem; Chinese terms keep the stricter per-character path.
    if key.script == Script::Latin && key.chars.len() >= WHOLE_SPAN_MIN_KEY_LEN {
        let score = scratch.ratio(span, &key.chars);
        if score >= WHOLE_SPAN_THRESHOLD {
            return Some(score);
        }
    }

    abbreviated_leading_word(window, key, scratch)
}

/// A speech engine may spell an acronym with one vowel dropped or inserted
/// ("jason" for `JSON`, "waspi" for `WASAPI`). Only a pure insertion/deletion is
/// accepted: a substitution ("wasabi" for `WASAPI`, "coda" for `CUDA`) and any
/// three-letter acronym stay untouched.
fn acronym_score(span: &[char], key: &[char]) -> Option<f64> {
    if key.len() < ACRONYM_MIN_KEY_LEN || span.len() < ACRONYM_MIN_SPAN_LEN {
        return None;
    }
    if span.first() != key.first() || span.last() != key.last() {
        return None;
    }
    let (longer, shorter) = if span.len() == key.len() + 1 {
        (span, key)
    } else if key.len() == span.len() + 1 {
        (key, span)
    } else {
        return None;
    };
    let mut index = 0;
    while index < shorter.len() && longer[index] == shorter[index] {
        index += 1;
    }
    if index == shorter.len() || longer[index + 1..] != shorter[index..] {
        return None;
    }
    if !matches!(longer[index], 'a' | 'e' | 'i' | 'o' | 'u') {
        return None;
    }
    Some(1.0 - 1.0 / longer.len() as f64)
}

fn outer_tokens_align(window: &[Token], key: &CompiledKey, scratch: &mut Scratch) -> bool {
    edge_ratio(&window[0].chars, &key.tokens[0], Edge::Prefix, scratch) >= EDGE_THRESHOLD
        && edge_ratio(
            &window[window.len() - 1].chars,
            &key.tokens[key.tokens.len() - 1],
            Edge::Suffix,
            scratch,
        ) >= EDGE_THRESHOLD
}

fn aligned_token_score(window: &[Token], key: &CompiledKey, scratch: &mut Scratch) -> Option<f64> {
    let key_tokens = key.tokens.len();
    let window_tokens = window.len();
    let mut dp = vec![vec![-1.0f64; window_tokens + 1]; key_tokens + 1];
    dp[0][0] = 1.0;
    for i in 1..=key_tokens {
        for j in i..=window_tokens {
            if window_tokens - j < key_tokens - i {
                continue;
            }
            let mut best = -1.0;
            for start in (i - 1)..j {
                let previous = dp[i - 1][start];
                if previous < 0.0 {
                    continue;
                }
                let group_len: usize = window[start..j].iter().map(|token| token.chars.len()).sum();
                let key_token = &key.tokens[i - 1];
                if !length_difference_ok(group_len, key_token.len()) {
                    continue;
                }
                let mut group = Vec::with_capacity(group_len);
                for token in &window[start..j] {
                    group.extend_from_slice(&token.chars);
                }
                let group_score = previous.min(scratch.ratio(&group, key_token));
                if group_score > best {
                    best = group_score;
                }
            }
            dp[i][j] = best;
        }
    }
    let score = dp[key_tokens][window_tokens];
    (score >= MULTI_KEY_THRESHOLD).then_some(score)
}

/// Recover a term whose first word was shortened to a prefix ("de harness").
/// The rest of the words must still match nearly exactly, which is what keeps
/// ordinary phrases such as "the harness" or "deep water harness" untouched.
fn abbreviated_leading_word(
    window: &[Token],
    key: &CompiledKey,
    scratch: &mut Scratch,
) -> Option<f64> {
    if key.script != Script::Latin || key.tokens.len() < 2 || window.len() != key.tokens.len() {
        return None;
    }
    let first_key = &key.tokens[0];
    let first_span = &window[0].chars;
    if first_key.len() < ABBREVIATED_FIRST_WORD_MIN_LEN
        || first_span.len() < 2
        || first_span.len() >= first_key.len()
        || !first_key.starts_with(first_span)
    {
        return None;
    }
    let mut score = 1.0f64;
    for (span, key_token) in window.iter().skip(1).zip(key.tokens.iter().skip(1)) {
        if span.chars.first() != key_token.first() || span.chars.last() != key_token.last() {
            return None;
        }
        let token_score = scratch.ratio(&span.chars, key_token);
        if token_score < SINGLE_KEY_THRESHOLD {
            return None;
        }
        score = score.min(token_score);
    }
    Some(score)
}

fn edge_ratio(candidate: &[char], key_token: &[char], edge: Edge, scratch: &mut Scratch) -> f64 {
    let slice = match edge {
        Edge::Prefix => &key_token[..key_token.len().min(candidate.len())],
        Edge::Suffix => &key_token[key_token.len().saturating_sub(candidate.len())..],
    };
    scratch.ratio(candidate, slice)
}

fn length_difference_ok(left: usize, right: usize) -> bool {
    let longest = left.max(right);
    longest == 0 || left.abs_diff(right) as f64 / longest as f64 <= MAX_LENGTH_DIFFERENCE
}

/// Reusable Levenshtein rows. Correction is called once per capture, so one
/// allocation per call is enough; comparing hundreds of entries must not allocate
/// per candidate.
#[derive(Default)]
struct Scratch {
    previous: Vec<usize>,
    current: Vec<usize>,
}

impl Scratch {
    fn ratio(&mut self, left: &[char], right: &[char]) -> f64 {
        let longest = left.len().max(right.len());
        if longest == 0 {
            return 1.0;
        }
        let width = right.len();
        if self.previous.len() <= width {
            self.previous.resize(width + 1, 0);
            self.current.resize(width + 1, 0);
        }
        for (index, slot) in self.previous[..=width].iter_mut().enumerate() {
            *slot = index;
        }
        for (i, left_char) in left.iter().enumerate() {
            self.current[0] = i + 1;
            for j in 0..width {
                self.current[j + 1] = (self.previous[j + 1] + 1)
                    .min(self.current[j] + 1)
                    .min(self.previous[j] + usize::from(left_char != &right[j]));
            }
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        let distance = self.previous[width];
        1.0 - distance as f64 / longest as f64
    }
}

impl Key {
    fn from_text(text: &str, line_number: usize, trusted: bool) -> AppResult<Self> {
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return Err(format!(
                "line {line_number}: '{text}' contains no letters or digits"
            ));
        }
        let script = tokens[0].script;
        if tokens.iter().any(|token| token.script != script) {
            return Err(format!(
                "line {line_number}: '{text}' mixes Latin and CJK; write one script per term"
            ));
        }
        let normalized = tokens
            .iter()
            .map(|token| token.normalized.as_str())
            .collect();
        Ok(Self {
            normalized,
            tokens: tokens.into_iter().map(|token| token.normalized).collect(),
            script,
            trusted,
            acronym: is_acronym(text),
        })
    }
}

/// An all-caps spelling is an acronym, not an ordinary word, so the safe
/// one-vowel shortcut in [`acronym_score`] may consider it.
fn is_acronym(text: &str) -> bool {
    let length = text.chars().count();
    length >= 2 && text.chars().all(|ch| ch.is_ascii_uppercase())
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut latin_start: Option<usize> = None;
    let mut latin_end = 0;
    for (index, ch) in text.char_indices() {
        let end = index + ch.len_utf8();
        if is_cjk(ch) {
            flush_latin(text, &mut latin_start, latin_end, &mut tokens);
            let normalized: String = ch.to_lowercase().collect();
            tokens.push(Token {
                chars: normalized.chars().collect(),
                normalized,
                start: index,
                end,
                script: Script::Cjk,
            });
        } else if ch.is_alphanumeric() {
            if latin_start.is_none() {
                latin_start = Some(index);
            }
            latin_end = end;
        } else {
            flush_latin(text, &mut latin_start, latin_end, &mut tokens);
        }
    }
    flush_latin(text, &mut latin_start, latin_end, &mut tokens);
    tokens
}

fn flush_latin(text: &str, start: &mut Option<usize>, end: usize, tokens: &mut Vec<Token>) {
    if let Some(start) = start.take() {
        let normalized = text[start..end].to_lowercase();
        tokens.push(Token {
            chars: normalized.chars().collect(),
            normalized,
            start,
            end,
            script: Script::Latin,
        });
    }
}

/// Chinese ideographs, including the compatibility and extension blocks. Each CJK
/// character is its own matching token so a term can be found inside Chinese text.
fn is_cjk(ch: char) -> bool {
    let code = ch as u32;
    (0x3400..=0x4DBF).contains(&code)
        || (0x4E00..=0x9FFF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0x20000..=0x2FA1F).contains(&code)
}

/// A one- or two-letter Latin alias is too ambiguous to apply everywhere ("Re:",
/// "re-do", "use re as a variable"). It is only trusted next to Chinese text,
/// which is the measured failure mode for "re" and "ku".
fn short_span_next_to_cjk(line: &str, start: usize, end: usize) -> bool {
    let before = line[..start].chars().rev().find(|ch| !ch.is_whitespace());
    let after = line[end..].chars().find(|ch| !ch.is_whitespace());
    before.is_some_and(is_cjk) || after.is_some_and(is_cjk)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# Local terms; one entry per line.
DeepSeek Harness | shen du qiu suo, 迪普西克

WASAPI
CUDA | ku da
";

    #[test]
    fn file_format_ignores_comments_and_blank_lines_and_restores_canonical_spelling() {
        let glossary = Glossary::parse(FILE).unwrap();
        assert_eq!(
            glossary.correct("我们使用 deepseek harness 和 WASAPI，再用 cuda。"),
            "我们使用 DeepSeek Harness 和 WASAPI，再用 CUDA。"
        );
    }

    #[test]
    fn a_missing_glossary_file_is_empty_and_leaves_the_text_unchanged() {
        let path = std::env::temp_dir().join(format!(
            "listen-glossary-missing-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let glossary = Glossary::load(&path).unwrap();
        let text = "We use the DeepSek harnessness API.";
        assert_eq!(glossary.correct(text), text);
    }

    /// The canonical form alone, without listing every wrong spelling, must catch
    /// the variants measured from the local engine.
    const CANONICAL_ONLY: &str = "\
DeepSeek Harness
WASAPI loopback
";

    #[test]
    fn one_canonical_term_catches_its_measured_misrecognitions() {
        let glossary = Glossary::parse(CANONICAL_ONLY).unwrap();
        let cases = [
            ("Deep sea harnessness.", "DeepSeek Harness."),
            (
                "We use the DeepSek harnessness API.",
                "We use the DeepSeek Harness API.",
            ),
            (
                "我们使用deep seek harness来做测试。",
                "我们使用DeepSeek Harness来做测试。",
            ),
            (
                "We use deep seek in the lab.",
                "We use DeepSeek Harness in the lab.",
            ),
            ("They said deepsek.", "They said DeepSeek Harness."),
            (
                "We use the deepse harnessness API.",
                "We use the DeepSeek Harness API.",
            ),
            (
                "Through waapp loop back we hear it.",
                "Through WASAPI loopback we hear it.",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn ordinary_sentences_are_not_rewritten_into_terms() {
        let glossary = Glossary::parse(CANONICAL_ONLY).unwrap();
        let cases = [
            "The deep sea is calm, and we seek consensus.",
            "The safety harness is loose.",
            "Wasabi is a plant, not an audio API.",
            "The deep water harness is not our product.",
            "Deep, deep down we seek the sea.",
            "这段语音只在本机处理，不会上传到云端。",
            "我们用 CPU 跑模型，本季的项目已完成。",
            "请确认本机的音频设置，不要把本机写成本季。",
        ];
        for input in cases {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    const ALIASES: &str = "\
DeepSeek Harness | shen du qiu suo, 迪普西克
WASAPI | waapp, wa-sa-pi
WASAPI loopback | waapp loop back
CUDA | ku da, ku
Rust | re
";

    #[test]
    fn aliases_cover_pinyin_hanzi_and_stubborn_variants() {
        let glossary = Glossary::parse(ALIASES).unwrap();
        let cases = [
            (
                "我们请 shen du qiu suo 做测试。",
                "我们请 DeepSeek Harness 做测试。",
            ),
            ("他们使用迪普西克。", "他们使用DeepSeek Harness。"),
            ("通过 wa-sa-pi 采集。", "通过 WASAPI 采集。"),
            (
                "通过waapp loop back获取声音。",
                "通过WASAPI loopback获取声音。",
            ),
            ("使用CPU和ku进行对照。", "使用CPU和CUDA进行对照。"),
            ("我们用re实现音频采集。", "我们用Rust实现音频采集。"),
            ("ku da is the old name.", "CUDA is the old name."),
        ];
        for (input, expected) in cases {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn short_aliases_do_not_match_inside_english_words_or_re_prefixes() {
        let glossary = Glossary::parse(ALIASES).unwrap();
        let cases = [
            "The result, there, and are ready.",
            "Please re-do the review after you re-read it.",
            "Re: the meeting notes",
            "The variable re is unused.",
            "They are there with results.",
        ];
        for input in cases {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    const SHIPPED: &str = include_str!("../../../packaging/glossary.txt");

    #[test]
    fn the_shipped_glossary_corrects_the_measured_engine_transcripts() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        let cases = [
            (
                "我们使用deep seek harness来做测试。",
                "我们使用DeepSeek Harness来做测试。",
            ),
            (
                "We use the DeepSek harnessness API for transcription.",
                "We use the DeepSeek Harness API for transcription.",
            ),
            ("Deep sea harnessness.", "DeepSeek Harness."),
            (
                "We use the Deepsek harnessness API.",
                "We use the DeepSeek Harness API.",
            ),
            (
                "We use the deepse harnessness API.",
                "We use the DeepSeek Harness API.",
            ),
            (
                "我们用re实现音频采集通过waapp loop back获取声音模型在本机运行使用CPU和进行对照。",
                "我们用Rust实现音频采集通过WASAPI loopback获取声音模型在本机运行使用CPU和进行对照。",
            ),
            ("使用CPU和ku进行对照。", "使用CPU和CUDA进行对照。"),
            (
                "We use the DeepSeek Harness API for transcription.",
                "We use the DeepSeek Harness API for transcription.",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn the_shipped_canonical_forms_alone_catch_the_removed_variants() {
        // The matcher normalizes spaces, hyphens and case, and a multi-word canonical
        // term is also matched by its distinctive leading word. Prove that two
        // aliases are derivable before keeping the shipped list one-entry-per-term:
        // `wa-sa-pi` normalizes to `wasapi` and `waapp loop back` is caught by
        // `WASAPI loopback`, so neither variant needs its own alias.
        let canonical_only: String = SHIPPED
            .lines()
            .map(|line| line.split('|').next().unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let glossary = Glossary::parse(&canonical_only).unwrap();
        for (input, expected) in [
            ("通过 wa-sa-pi 采集。", "通过 WASAPI 采集。"),
            (
                "通过waapp loop back获取声音。",
                "通过WASAPI loopback获取声音。",
            ),
        ] {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn the_shipped_glossary_leaves_clean_chinese_and_ordinary_latin_text_alone() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        let cases = [
            "这段语音只在本机处理，不会上传到云端，请保留完整的发言内容，由我检查以后手动发送。",
            "我们用 CPU 跑模型，本季的项目已经完成。",
            "请确认本机的音频设置，不要把本机写成本季。",
            "The deep sea is calm, and we seek consensus.",
            "The safety harness is loose.",
            "Wasabi is a plant, not an audio API.",
            "The deep water harness is not our product.",
            "The result, there, and are ready; please re-do the review.",
            "Re: the meeting notes",
        ];
        for input in cases {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn corrections_preserve_line_structure_spacing_and_punctuation() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        let input =
            "第一行：deep seek harness。\r\nSecond line, deep sea harnessness!\n\n  deepsek  ";
        assert_eq!(
            glossary.correct(input),
            "第一行：DeepSeek Harness。\r\nSecond line, DeepSeek Harness!\n\n  DeepSeek Harness  "
        );
    }

    #[test]
    fn term_characters_inside_a_longer_chinese_sentence_are_not_rewritten() {
        let glossary = Glossary::parse("DeepSeek | 深度求索\n").unwrap();
        assert_eq!(glossary.correct("我们使用深度求索。"), "我们使用DeepSeek。");
        let ordinary = "他深入研究了这个深度问题，并探索了求解思路。";
        assert_eq!(glossary.correct(ordinary), ordinary);
    }

    #[test]
    fn a_malformed_file_is_rejected_with_its_line_and_the_file_path() {
        let directory = std::env::temp_dir().join(format!(
            "listen-glossary-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("glossary.txt");
        fs::write(&path, "DeepSeek Harness | deepsek\n| missing canonical\n").unwrap();
        let error = Glossary::load(&path).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("empty"), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");

        assert!(Glossary::parse("DeepSeek Harness | deep|sek\n").is_err());
        assert!(Glossary::parse("!!!\n").is_err());
        assert!(
            Glossary::parse("DeepSeek 深度求索\n")
                .unwrap_err()
                .contains("mixes")
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn correcting_hundreds_of_terms_stays_in_the_millisecond_range() {
        let mut file = String::new();
        for index in 0..300 {
            file.push_str(&format!("Term{index:03}\n"));
        }
        let glossary = Glossary::parse(&file).unwrap();
        // 300 terms, each mis-recognized once with one wrong letter. The text is
        // 300 sentences so the assertion covers a full transcript, not one lookup.
        let mut transcript = String::new();
        for index in 0..300 {
            transcript.push_str(&format!(
                "we use tern{index:03} in the pipeline and then continue.\n"
            ));
        }
        let started = std::time::Instant::now();
        let corrected = glossary.correct(&transcript);
        let elapsed = started.elapsed();
        assert!(!corrected.contains("tern"), "not every term was corrected");
        assert_eq!(corrected.matches("Term").count(), 300);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "correcting 300 terms took {elapsed:?}"
        );
        println!("300-term correction took {elapsed:?}");
    }

    /// The ten sentences from the user's first real-world meeting run, paired
    /// with the exact recognised output the engine produced and the result the
    /// shipped glossary can honestly deliver. Lines whose recognised text is the
    /// reference are included so the diagnostic covers the whole run; the parts
    /// the matcher cannot recover are left untouched and documented in
    /// `docs/issue-glossary-real-speech.md`.
    const REAL_MEETING_LINES: [(&str, &str); 10] = [
        (
            "我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。",
            "我先同步一下今天的进展，后端接口已经联调完成，前端还在补最后的错误提示，明天下午可以走一遍验收流程。",
        ),
        (
            "我们把语音识别成w换成sens voice，现在用 ON X在本地跑推理。",
            "我们把语音识别成w换成SenseVoice，现在用 ON X在本地跑推理。",
        ),
        (
            "de harness的接口已经调通了ku打加速这一版先不开，选用CPU把流程抛稳。",
            "DeepSeek Harness的接口已经调通了CUDA打加速这一版先不开，选用CPU把流程抛稳。",
        ),
        (
            "采集走的是waSAPpilook back音频直接送进模接口偶尔返回HTTP500日志里能看到jason解析失败。",
            "采集走的是WASAPI loopback音频直接送进模接口偶尔返回HTTP500日志里能看到JSON解析失败。",
        ),
        (
            "这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。",
            "这次发布版本是 1.8.2，服务端口改成8080失败重试3次，间隔2秒，超时实验设30秒。",
        ),
        (
            "本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。",
            "本机环境已经配好了，本季度的目标不变，请确认是要在本季部署还是下个季度再部署一次。",
        ),
        (
            "这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.",
            "这个 PRR我 review过了，主要改了retryed的逻辑 merge之前还需要再补一个d stay unit test.",
        ),
        (
            "结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。",
            "结论有3点，第一，回度比例悬空制在5%。第二，回滚窗考留30分钟。第三，明天10点半之前给我确认。",
        ),
        (
            "地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。",
            "地方在腾讯会议里说话，我这边把耳机里的声音直接录下来，不做任何识别，先保证音频完整。",
        ),
        (
            "今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。",
            "今天的de先MVPAPI文档，我下午发到群里SL和QPS这两个指标下周再对齐。",
        ),
    ];

    #[test]
    fn the_users_real_meeting_transcripts_are_corrected_where_the_glossary_can() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for (index, (recognized, expected)) in REAL_MEETING_LINES.iter().enumerate() {
            assert_eq!(
                glossary.correct(recognized),
                *expected,
                "meeting line {}",
                index + 1
            );
        }
    }

    #[test]
    #[ignore = "diagnostic: prints the ten real meeting lines through Glossary::correct()"]
    fn diagnostic_real_meeting_lines() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for (index, (recognized, expected)) in REAL_MEETING_LINES.iter().enumerate() {
            let corrected = glossary.correct(recognized);
            let result = if corrected == *expected {
                if corrected == *recognized {
                    "unchanged"
                } else {
                    "fixed"
                }
            } else {
                "MISMATCH"
            };
            println!(
                "line {:2} [{result}]\n  recognized: {recognized}\n  corrected : {corrected}",
                index + 1
            );
        }
    }

    #[test]
    fn the_shipped_glossary_restores_the_corpus_acronyms() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for (input, expected) in [
            ("jason", "JSON"),
            ("日志里能看到jason解析失败。", "日志里能看到JSON解析失败。"),
            ("http 500", "HTTP 500"),
            ("the api and mvp", "the API and MVP"),
            ("sla and qps", "SLA and QPS"),
            ("pr review", "PR review"),
            ("cuda", "CUDA"),
        ] {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn short_acronym_deformations_are_left_alone_instead_of_guessed() {
        // "SL" -> "SLA" and "PRR" -> "PR" would need a 2/3 or 1/2 edit ratio on a
        // two- or three-letter key, where ordinary words and other acronyms are
        // too close. They stay in the documented gap rather than risk a wrong term.
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for input in ["发到群里SL和QPS", "这个 PRR我 review过了", "SLO 和 QPS"] {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn the_looser_whole_span_rule_leaves_ordinary_words_and_phrases_alone() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for input in [
            "we seek consensus.",
            "The deep sea is calm.",
            "The safety harness is loose.",
            "The deep water harness is not our product.",
            "Wasabi is a plant, not an audio API.",
            "Take a rest before the review.",
            "The deck is on the left.",
            "I sense that the voice is clear.",
            "The device works, and the service is down.",
        ] {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn a_truncated_leading_word_is_recovered_only_with_the_rest_of_the_term() {
        let glossary = Glossary::parse(SHIPPED).unwrap();
        for (input, expected) in [
            ("de harness的接口", "DeepSeek Harness的接口"),
            ("de harness", "DeepSeek Harness"),
            ("wa loopback", "WASAPI loopback"),
        ] {
            assert_eq!(glossary.correct(input), expected, "input: {input:?}");
        }
        for input in [
            "do harness",
            "the harness",
            "deployment harness",
            "deep water harness is not our product.",
        ] {
            assert_eq!(glossary.correct(input), input, "input: {input:?}");
        }
    }

    #[test]
    fn the_default_glossary_falls_back_to_the_copy_next_to_the_executable() {
        let directory = std::env::temp_dir().join(format!(
            "listen-glossary-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runtime = directory.join("runtime");
        let build = directory.join("build");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&build).unwrap();
        let executable = build.join("listen.exe");
        let runtime_file = runtime.join("glossary.txt");
        let sibling = build.join("glossary.txt");

        assert_eq!(
            default_paths(&runtime, Some(&executable)),
            vec![runtime_file.clone(), sibling.clone()]
        );

        fs::write(&sibling, "DeepSeek Harness\n").unwrap();
        let paths = default_paths(&runtime, Some(&executable));
        assert_eq!(
            paths.iter().find(|path| path.is_file()),
            Some(&sibling),
            "a copied build must find the glossary next to listen.exe"
        );

        fs::write(&runtime_file, "WASAPI\n").unwrap();
        let paths = default_paths(&runtime, Some(&executable));
        assert_eq!(
            paths.iter().find(|path| path.is_file()),
            Some(&runtime_file),
            "an existing runtime glossary must win over the copy next to the exe"
        );

        fs::remove_dir_all(&directory).unwrap();
    }
}
