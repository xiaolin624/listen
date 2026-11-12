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

/// The glossary path for an application whose runtime directory is `runtime`.
pub(crate) fn default_path(runtime: &Path) -> PathBuf {
    DEFAULT_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| runtime.join("glossary.txt"))
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
    if key_tokens == 1 {
        // A one-word key is only compared with one token. Multi-token spellings
        // ("deep seek", "wa-sa-pi") are caught by the exact normalized lookup.
        if window_tokens != 1 {
            return None;
        }
        let score = scratch.ratio(span, &key.chars);
        return (score >= SINGLE_KEY_THRESHOLD).then_some(score);
    }
    if window_tokens < key_tokens {
        return None;
    }
    // When the span has more tokens than the key, every key token must cover one
    // or more span tokens. Anchor the outer tokens to the edges of the key word so
    // an adjacent ordinary word ("Harness and") is never swallowed.
    if window_tokens > key_tokens {
        if edge_ratio(&window[0].chars, &key.tokens[0], Edge::Prefix, scratch) < EDGE_THRESHOLD {
            return None;
        }
        if edge_ratio(
            &window[window_tokens - 1].chars,
            &key.tokens[key_tokens - 1],
            Edge::Suffix,
            scratch,
        ) < EDGE_THRESHOLD
        {
            return None;
        }
    }
    // Align key tokens to contiguous groups of span tokens. Every key token must
    // get at least one token and every span token must belong to a group, which
    // lets "deep sea harnessness" align as "deepsea" + "harnessness" while a
    // different phrase such as "deep water harness" stays below the threshold.
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
        })
    }
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
}
