use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::{
    error::AppError,
    processing::{
        dictionary, frequency,
        normalization::{normalize_lemma, strip_brackets},
        tokenizers::{EuropeanTokenizer, JapaneseTokenizer, Token},
    },
};

/// Detect language from text content. If CJK characters dominate, it's Japanese.
/// Otherwise fall back to the provided hint or default to "ja".
pub fn detect_language(text: &str, hint: Option<&str>) -> String {
    if let Some(h) = hint {
        let valid = ["ja", "es", "fr", "de", "it", "pt"];
        if valid.contains(&h) {
            return h.to_string();
        }
    }

    // Count CJK vs Latin characters
    let mut cjk = 0;
    let mut latin = 0;
    for c in text.chars().take(2000) {
        if is_cjk(c) {
            cjk += 1;
        } else if c.is_alphabetic() {
            latin += 1;
        }
    }

    if cjk > latin / 2 && cjk > 10 {
        "ja".to_string()
    } else {
        // Default to Spanish if no hint and text is Latin script
        hint.unwrap_or("es").to_string()
    }
}

pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{309F}' | // Hiragana
        '\u{30A0}'..='\u{30FF}' | // Katakana
        '\u{4E00}'..='\u{9FFF}' | // CJK Unified
        '\u{3400}'..='\u{4DBF}'   // CJK Extension A
    )
}

pub fn is_japanese(lang: &str) -> bool {
    lang == "ja"
}

/// Tokenize text using the appropriate tokenizer for the language.
/// Uses cached JapaneseTokenizer to avoid expensive re-initialization.
pub fn tokenize_text(text: &str, language: &str) -> Result<Vec<Token>, AppError> {
    if is_japanese(language) {
        Ok(JapaneseTokenizer::global().tokenize(text))
    } else {
        let tokenizer = EuropeanTokenizer::new(language)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Tokenizer init failed: {}", e)))?;
        Ok(tokenizer.tokenize(text))
    }
}

/// Split text into sentences, handling both Japanese and European punctuation
pub fn split_sentences(text: &str, language: &str) -> Vec<String> {
    if is_japanese(language) {
        split_sentences_japanese(text)
    } else {
        split_sentences_european(text)
    }
}

fn split_sentences_japanese(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        current.push(c);
        if c == '。' || c == '！' || c == '？' || c == '!' || c == '?' {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() && trimmed.chars().count() > 5 {
                sentences.push(trimmed);
            }
            current = String::new();
        }
    }

    sentences
}

fn split_sentences_european(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        current.push(c);
        if c == '.' || c == '!' || c == '?' {
            let trimmed = current.trim().to_string();
            // Require minimum length to avoid splitting on abbreviations like "Mr."
            if !trimmed.is_empty() && trimmed.split_whitespace().count() >= 3 {
                sentences.push(trimmed);
            }
            current = String::new();
        }
    }

    sentences
}

/// Core analysis logic shared by PDF and text analysis.
/// Tokenizes text only ONCE and maps tokens to sentences by substring matching,
/// avoiding expensive per-sentence re-tokenization.
#[allow(clippy::type_complexity)]
pub fn analyze_text_core(
    full_text: &str,
    language: &str,
) -> Result<(Vec<Token>, Vec<String>, HashMap<String, Vec<String>>, HashMap<String, String>, HashMap<String, Vec<String>>, HashMap<String, HashSet<String>>), AppError> {
    let text = if language == "ja" {
        std::borrow::Cow::Owned(crate::processing::furigana::strip_furigana(full_text))
    } else {
        std::borrow::Cow::Borrowed(full_text)
    };

    let sentences = split_sentences(&text, language);
    let all_tokens = tokenize_text(&text, language)?;

    let mut surface_forms: HashMap<String, Vec<String>> = HashMap::new();
    let mut pos_map: HashMap<String, String> = HashMap::new();

    for token in &all_tokens {
        if token.is_content {
            let norm = normalize_lemma(&token.lemma, language);
            surface_forms
                .entry(norm.clone())
                .or_default()
                .push(token.surface.clone());
            pos_map.entry(norm).or_insert(token.pos.clone());
        }
    }

    for forms in surface_forms.values_mut() {
        forms.sort();
        forms.dedup();
    }
    let mut lemma_sentences: HashMap<String, Vec<String>> = HashMap::new();
    let mut sentence_content_lemmas: HashMap<String, HashSet<String>> = HashMap::new();

    for sentence in &sentences {
        // Strip bracketed content (speaker tags, SFX labels) before tokenizing
        // so words inside brackets like （禰豆子のうなり声） don't get matched
        let clean = strip_brackets(sentence);
        if clean.is_empty() || clean.chars().count() < 3 {
            continue;
        }
        let tokens = tokenize_text(&clean, language)?;
        let mut seen: HashSet<String> = HashSet::new();
        for token in &tokens {
            if token.is_content {
                let norm = normalize_lemma(&token.lemma, language);
                if seen.insert(norm.clone()) {
                    lemma_sentences.entry(norm).or_default().push(clean.clone());
                }
            }
        }
        sentence_content_lemmas.insert(clean, seen);
    }
    // Limit sentences per word
    for sents in lemma_sentences.values_mut() {
        sents.sort_by_key(|s| {
            let len = s.chars().count();
            if len < 20 { 100 + (20 - len) }
            else if len > 80 { 100 + (len - 80) }
            else { len }
        });
        sents.truncate(3);
    }

    Ok((all_tokens, sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas))
}

#[derive(Debug, Serialize)]
pub struct AnalysisResult {
    pub filename: String,
    pub text_length: usize,
    pub total_tokens: usize,
    pub content_tokens: usize,
    pub unique_words: usize,
    pub sentence_count: usize,
    pub words: Vec<WordInfo>,
    pub sample_text: String,
    pub language: String,
}

#[derive(Debug, Serialize)]
pub struct WordInfo {
    pub lemma: String,
    pub reading: Option<String>,
    pub definitions: Vec<String>,
    pub count: i32,
    pub pos: String,
    pub surface_forms: Vec<String>,
    pub sentences: Vec<String>,
}

pub fn build_word_list(
    all_tokens: &[Token],
    surface_forms: &HashMap<String, Vec<String>>,
    pos_map: &HashMap<String, String>,
    lemma_sentences: &HashMap<String, Vec<String>>,
    language: &str,
) -> Vec<WordInfo> {
    let freq_map = frequency::count_lemmas(all_tokens, language);
    let mut words: Vec<WordInfo> = freq_map
        .into_iter()
        .map(|(lemma, wf)| {
            let dict_entry = dictionary::lookup(&lemma, language);
            let definitions = dict_entry
                .map(|e| e.definitions.clone())
                .unwrap_or_default();
            let reading = if is_japanese(language) {
                dict_entry
                    .map(|e| Some(e.reading.clone()))
                    .unwrap_or(wf.reading)
            } else {
                None
            };

            WordInfo {
                pos: pos_map.get(&lemma).cloned().unwrap_or_default(),
                surface_forms: surface_forms.get(&lemma).cloned().unwrap_or_default(),
                sentences: lemma_sentences.get(&lemma).cloned().unwrap_or_default(),
                definitions,
                lemma,
                reading,
                count: wf.doc_count,
            }
        })
        .collect();

    words.sort_by(|a, b| b.count.cmp(&a.count));
    words
}

/// Parse deck_types from a comma-separated string (for multipart forms)
pub fn parse_deck_types(s: &str) -> Vec<DeckType> {
    s.split(',')
        .filter_map(|t| match t.trim() {
            "word_definition" => Some(DeckType::WordDefinition),
            "i_plus_one" => Some(DeckType::IPlusOne),
            _ => None,
        })
        .collect()
}

#[derive(Debug, serde::Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeckType {
    WordDefinition,
    IPlusOne,
}
