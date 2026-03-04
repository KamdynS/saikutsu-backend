use std::collections::HashMap;

use super::tokenizers::Token;

#[derive(Debug, Clone)]
pub struct WordFrequency {
    pub lemma: String,
    pub reading: Option<String>,
    pub doc_count: i32,
    pub corpus_rank: Option<i32>,
}

pub fn count_lemmas(tokens: &[Token]) -> HashMap<String, WordFrequency> {
    let mut counts: HashMap<String, WordFrequency> = HashMap::new();

    for token in tokens {
        if !token.is_content {
            continue;
        }

        counts
            .entry(token.lemma.clone())
            .and_modify(|wf| wf.doc_count += 1)
            .or_insert(WordFrequency {
                lemma: token.lemma.clone(),
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
