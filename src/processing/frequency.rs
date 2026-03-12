use std::collections::HashMap;
use std::sync::OnceLock;

use super::normalization::normalize_lemma;
use super::tokenizers::Token;

#[derive(Debug, Clone)]
pub struct WordFrequency {
    pub lemma: String,
    pub reading: Option<String>,
    pub doc_count: i32,
    pub corpus_rank: Option<i32>,
}

pub fn count_lemmas(tokens: &[Token], language: &str) -> HashMap<String, WordFrequency> {
    let mut counts: HashMap<String, WordFrequency> = HashMap::new();

    for token in tokens {
        if !token.is_content {
            continue;
        }

        let norm = normalize_lemma(&token.lemma, language);

        counts
            .entry(norm.clone())
            .and_modify(|wf| wf.doc_count += 1)
            .or_insert(WordFrequency {
                lemma: norm,
                reading: if token.reading.is_empty() {
                    None
                } else {
                    Some(token.reading.clone())
                },
                doc_count: 1,
                corpus_rank: None,
            });
    }

    counts
}

pub fn rank_by_frequency(
    word_counts: HashMap<String, WordFrequency>,
    corpus_freq: &HashMap<String, i32>,
) -> Vec<WordFrequency> {
    let mut words: Vec<WordFrequency> = word_counts
        .into_values()
        .map(|mut wf| {
            wf.corpus_rank = corpus_freq.get(&wf.lemma).copied();
            wf
        })
        .collect();

    // Sort by corpus rank (unknown words last), then by document frequency
    words.sort_by(|a, b| {
        match (a.corpus_rank, b.corpus_rank) {
            (Some(ra), Some(rb)) => ra.cmp(&rb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.doc_count.cmp(&a.doc_count),
        }
    });

    words
}

// ── Corpus frequency lists (loaded once from data/frequency/*.json) ──

type FreqMap = HashMap<String, i32>;

static FREQ_JA: OnceLock<FreqMap> = OnceLock::new();
static FREQ_ES: OnceLock<FreqMap> = OnceLock::new();
static FREQ_FR: OnceLock<FreqMap> = OnceLock::new();
static FREQ_DE: OnceLock<FreqMap> = OnceLock::new();
static FREQ_IT: OnceLock<FreqMap> = OnceLock::new();
static FREQ_PT: OnceLock<FreqMap> = OnceLock::new();

fn load_freq_file(path: &str) -> FreqMap {
    match std::fs::read_to_string(path) {
        Ok(data) => {
            let map: HashMap<String, i32> = serde_json::from_str(&data).unwrap_or_default();
            tracing::info!(path = path, entries = map.len(), "loaded corpus frequency list");
            map
        }
        Err(e) => {
            tracing::warn!(path = path, error = %e, "corpus frequency file not found");
            HashMap::new()
        }
    }
}

/// Get the corpus frequency map for a language. Loaded lazily on first access.
pub fn corpus_freq(language: &str) -> &'static FreqMap {
    match language {
        "ja" => FREQ_JA.get_or_init(|| load_freq_file("data/frequency/ja.json")),
        "es" => FREQ_ES.get_or_init(|| load_freq_file("data/frequency/es.json")),
        "fr" => FREQ_FR.get_or_init(|| load_freq_file("data/frequency/fr.json")),
        "de" => FREQ_DE.get_or_init(|| load_freq_file("data/frequency/de.json")),
        "it" => FREQ_IT.get_or_init(|| load_freq_file("data/frequency/it.json")),
        "pt" => FREQ_PT.get_or_init(|| load_freq_file("data/frequency/pt.json")),
        _ => FREQ_JA.get_or_init(|| load_freq_file("data/frequency/ja.json")),
    }
}

/// Look up the corpus rank for a word. Returns None if not in the frequency list.
pub fn corpus_rank(word: &str, language: &str) -> Option<i32> {
    corpus_freq(language).get(word).copied()
}
