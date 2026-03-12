use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::{
    error::AppError,
    processing::{
        dictionary, frequency,
        nlp_client,
        normalization::{clean_for_nlp, normalize_lemma, strip_brackets},
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

fn is_european(lang: &str) -> bool {
    matches!(lang, "es" | "fr" | "de" | "it" | "pt")
}

/// Tokenize text using the local (non-NLP) tokenizer. Used as fallback and for Japanese.
pub fn tokenize_text_local(text: &str, language: &str) -> Result<Vec<Token>, AppError> {
    if is_japanese(language) {
        Ok(JapaneseTokenizer::global().tokenize(text))
    } else {
        let tokenizer = EuropeanTokenizer::new(language)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Tokenizer init failed: {}", e)))?;
        Ok(tokenizer.tokenize(text))
    }
}

/// Split text into sentences using local heuristics. Used as fallback and for Japanese.
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

/// NLP service configuration for spaCy-based tokenization.
pub struct NlpConfig<'a> {
    pub client: &'a reqwest::Client,
    pub base_url: &'a str,
}

/// Core analysis logic shared by PDF and text analysis.
/// Tokenizes text only ONCE and maps tokens to sentences by substring matching,
/// avoiding expensive per-sentence re-tokenization.
///
/// When `nlp` is Some and the language is European, uses the spaCy NLP service
/// for tokenization (accurate POS + lemma). Falls back to local tokenizer on error.
#[allow(clippy::type_complexity)]
pub async fn analyze_text_core(
    full_text: &str,
    language: &str,
    nlp: Option<NlpConfig<'_>>,
) -> Result<(Vec<Token>, Vec<String>, HashMap<String, Vec<String>>, HashMap<String, String>, HashMap<String, Vec<String>>, HashMap<String, HashSet<String>>), AppError> {
    let text = if language == "ja" {
        std::borrow::Cow::Owned(crate::processing::furigana::strip_furigana(full_text))
    } else {
        std::borrow::Cow::Borrowed(full_text)
    };

    // Clean dialogue markers (subtitle dashes) before NLP processing
    let cleaned_text = if is_european(language) {
        std::borrow::Cow::Owned(clean_for_nlp(&text))
    } else {
        std::borrow::Cow::Borrowed(text.as_ref())
    };

    // Try spaCy NLP service for European languages, fall back to local tokenizer
    let (all_tokens, sentences) = if is_european(language) {
        if let Some(ref nlp_cfg) = nlp {
            match tokenize_with_nlp(nlp_cfg, &cleaned_text, language).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!("NLP service unavailable, falling back to local tokenizer: {}", e);
                    let sentences = split_sentences(&cleaned_text, language);
                    let tokens = tokenize_text_local(&cleaned_text, language)?;
                    (tokens, sentences)
                }
            }
        } else {
            let sentences = split_sentences(&cleaned_text, language);
            let tokens = tokenize_text_local(&cleaned_text, language)?;
            (tokens, sentences)
        }
    } else {
        let sentences = split_sentences(&text, language);
        let tokens = tokenize_text_local(&text, language)?;
        (tokens, sentences)
    };

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
        // For sentence→lemma mapping, use local tokenizer (fast, and we just need
        // to match surface forms to lemmas we already identified from the full-text pass)
        let tokens = tokenize_text_local(&clean, language)?;
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

/// Tokenize text using the spaCy NLP service. Returns (tokens, sentences).
async fn tokenize_with_nlp(
    nlp: &NlpConfig<'_>,
    text: &str,
    language: &str,
) -> Result<(Vec<Token>, Vec<String>), AppError> {
    let stopwords_set: HashSet<&str> = EuropeanTokenizer::new(language)
        .map(|t| t.get_stopwords().iter().copied().collect())
        .unwrap_or_default();

    let resp = nlp_client::tokenize(nlp.client, nlp.base_url, text, language)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("NLP service error: {}", e)))?;

    let tokens: Vec<Token> = resp.tokens.into_iter().filter_map(|t| {
        // Skip tokens with leading punctuation (dialogue dashes that survived cleaning)
        let first_char = t.surface.chars().next()?;
        if first_char == '-' || first_char == '–' || first_char == '—' {
            return None;
        }

        // Skip tokens that contain spaces (spaCy merged multiple words)
        if t.lemma.contains(' ') {
            return None;
        }

        // Skip tokens that are purely punctuation
        if t.surface.chars().all(|c| !c.is_alphanumeric()) {
            return None;
        }

        let lower = t.surface.to_lowercase();
        let is_content = !t.is_stop
            && !stopwords_set.contains(lower.as_str())
            && lower.chars().count() >= 2
            && !lower.chars().all(|c| c.is_numeric());

        Some(Token {
            surface: t.surface,
            lemma: t.lemma,
            reading: String::new(),
            pos: t.pos,
            is_content,
        })
    }).collect();

    Ok((tokens, resp.sentences))
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
            let pos = pos_map.get(&lemma).cloned().unwrap_or_default();

            // Use POS-aware definitions when available
            let definitions = dict_entry
                .map(|e| e.definitions_with_pos_preference(&pos))
                .unwrap_or_default();
            let reading = if is_japanese(language) {
                dict_entry
                    .map(|e| Some(e.reading.clone()))
                    .unwrap_or(wf.reading)
            } else {
                None
            };

            WordInfo {
                pos,
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
