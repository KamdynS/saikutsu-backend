use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{Deck, DeckResponse},
    processing::{dictionary, frequency, normalization::normalize_lemma, pdf, transcription, tokenizers::{EuropeanTokenizer, JapaneseTokenizer, Token}},
    services::known_words_service,
    AppState,
};

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeckType {
    WordDefinition,
    IPlusOne,
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeTextRequest {
    pub text: String,
    pub title: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeckFromTextRequest {
    pub text: String,
    pub name: String,
    #[serde(default)]
    pub deck_types: Vec<DeckType>,
    pub language: Option<String>,
    // Kept for backward compat, ignored
    #[allow(dead_code)]
    pub max_words: Option<usize>,
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

fn is_japanese(lang: &str) -> bool {
    lang == "ja"
}

/// Tokenize text using the appropriate tokenizer for the language.
/// Uses cached JapaneseTokenizer to avoid expensive re-initialization.
fn tokenize_text(text: &str, language: &str) -> Result<Vec<Token>, AppError> {
    if is_japanese(language) {
        Ok(JapaneseTokenizer::global().tokenize(text))
    } else {
        let tokenizer = EuropeanTokenizer::new(language)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Tokenizer init failed: {}", e)))?;
        Ok(tokenizer.tokenize(text))
    }
}

/// Split text into sentences, handling both Japanese and European punctuation
fn split_sentences(text: &str, language: &str) -> Vec<String> {
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
fn analyze_text_core(
    full_text: &str,
    language: &str,
) -> Result<(Vec<Token>, Vec<String>, HashMap<String, Vec<String>>, HashMap<String, String>, HashMap<String, Vec<String>>, HashMap<String, HashSet<String>>), AppError> {
    let step_start = Instant::now();
    let sentences = split_sentences(full_text, language);
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, sentences = sentences.len(), "analyze_text_core split_sentences");

    // Tokenize full text only ONCE (was previously tokenized again per-sentence)
    let step_start = Instant::now();
    let all_tokens = tokenize_text(full_text, language)?;
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, tokens = all_tokens.len(), "analyze_text_core tokenize full text");

    // Build surface form and POS mappings (normalized lemma keys)
    let step_start = Instant::now();
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
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, "analyze_text_core build surface/pos maps");

    // Build lemma → sentences and sentence → content lemmas mappings
    // using per-sentence tokenization for accurate matching (no false positives from substring matching)
    let step_start = Instant::now();
    let mut lemma_sentences: HashMap<String, Vec<String>> = HashMap::new();
    let mut sentence_content_lemmas: HashMap<String, HashSet<String>> = HashMap::new();

    for sentence in &sentences {
        let tokens = tokenize_text(sentence, language)?;
        let mut seen: HashSet<String> = HashSet::new();
        for token in &tokens {
            if token.is_content {
                let norm = normalize_lemma(&token.lemma, language);
                if seen.insert(norm.clone()) {
                    lemma_sentences.entry(norm).or_default().push(sentence.clone());
                }
            }
        }
        sentence_content_lemmas.insert(sentence.clone(), seen);
    }
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, sentences_tokenized = sentences.len(), "analyze_text_core per-sentence tokenization");

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

fn build_word_list(
    all_tokens: &[Token],
    surface_forms: &HashMap<String, Vec<String>>,
    pos_map: &HashMap<String, String>,
    lemma_sentences: &HashMap<String, Vec<String>>,
    language: &str,
) -> Vec<WordInfo> {
    let step_start = Instant::now();
    let freq_map = frequency::count_lemmas(all_tokens, language);
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, "build_word_list count_lemmas");

    let step_start = Instant::now();
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
    tracing::info!(duration_ms = step_start.elapsed().as_millis() as u64, unique_words = words.len(), "build_word_list dictionary lookups + sort");

    words
}

pub async fn analyze_pdf(mut multipart: Multipart) -> AppResult<Json<AnalysisResult>> {
    let start = Instant::now();

    let mut filename: Option<String> = None;
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut language_hint: Option<String> = None;

    let upload_start = Instant::now();
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        let file_name = field.file_name().map(|s| s.to_string());

        match field_name.as_deref() {
            Some("file") => {
                filename = file_name;
                let bytes = field.bytes().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
                pdf_bytes = Some(bytes.to_vec());
            }
            Some("language") => {
                let text = field.text().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read language: {}", e)))?;
                language_hint = Some(text);
            }
            _ => {}
        }
    }
    tracing::info!(duration_ms = upload_start.elapsed().as_millis() as u64, "analyze::analyze_pdf read upload");

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;

    if !filename.to_lowercase().ends_with(".pdf") {
        return Err(AppError::BadRequest("Only PDF files are supported".to_string()));
    }

    let extract_start = Instant::now();
    let pages = pdf::extract_text_from_bytes(&pdf_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to extract text: {}", e)))?;
    tracing::info!(duration_ms = extract_start.elapsed().as_millis() as u64, pages = pages.len(), "analyze::analyze_pdf PDF extraction");

    let full_text: String = pages.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n");
    let language = detect_language(&full_text, language_hint.as_deref());
    let text_length = full_text.len();
    let sample_text: String = full_text.chars().take(500).collect();

    let core_start = Instant::now();
    let (all_tokens, sentences, surface_forms, pos_map, lemma_sentences, _sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "analyze::analyze_pdf analyze_text_core total");

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();

    let word_list_start = Instant::now();
    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);
    tracing::info!(duration_ms = word_list_start.elapsed().as_millis() as u64, "analyze::analyze_pdf build_word_list total");

    let unique_words = words.len();
    let sentence_count = sentences.len();

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        text_length = text_length,
        tokens = total_tokens,
        unique_words = unique_words,
        "analyze::analyze_pdf total"
    );

    Ok(Json(AnalysisResult {
        filename,
        text_length,
        total_tokens,
        content_tokens: content_token_count,
        unique_words,
        sentence_count,
        words,
        sample_text,
        language,
    }))
}

pub async fn analyze_text(
    Json(req): Json<AnalyzeTextRequest>,
) -> AppResult<Json<AnalysisResult>> {
    let start = Instant::now();

    let full_text = req.text;
    let title = req.title.unwrap_or_else(|| "Pasted Text".to_string());

    if full_text.trim().is_empty() {
        return Err(AppError::BadRequest("Text cannot be empty".to_string()));
    }

    let language = detect_language(&full_text, req.language.as_deref());
    let text_length = full_text.len();
    let sample_text: String = full_text.chars().take(500).collect();

    let core_start = Instant::now();
    let (all_tokens, sentences, surface_forms, pos_map, lemma_sentences, _sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "analyze::analyze_text analyze_text_core total");

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();

    let word_list_start = Instant::now();
    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);
    tracing::info!(duration_ms = word_list_start.elapsed().as_millis() as u64, "analyze::analyze_text build_word_list total");

    let unique_words = words.len();
    let sentence_count = sentences.len();

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        text_length = text_length,
        tokens = total_tokens,
        unique_words = unique_words,
        "analyze::analyze_text total"
    );

    Ok(Json(AnalysisResult {
        filename: title,
        text_length,
        total_tokens,
        content_tokens: content_token_count,
        unique_words,
        sentence_count,
        words,
        sample_text,
        language,
    }))
}

#[derive(Debug, Serialize)]
pub struct CreateDeckResult {
    pub decks: Vec<DeckResponse>,
    pub cards_created: usize,
    pub sentences_created: usize,
    pub i_plus_one_found: usize,
    pub words_skipped_duplicate: usize,
}

/// Parse deck_types from a comma-separated string (for multipart forms)
fn parse_deck_types(s: &str) -> Vec<DeckType> {
    s.split(',')
        .filter_map(|t| match t.trim() {
            "word_definition" => Some(DeckType::WordDefinition),
            "i_plus_one" => Some(DeckType::IPlusOne),
            _ => None,
        })
        .collect()
}

/// Create a deck from raw text
pub async fn create_deck_from_text(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckFromTextRequest>,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let full_text = req.text;
    let deck_name = req.name;
    let deck_types = if req.deck_types.is_empty() {
        vec![DeckType::WordDefinition]
    } else {
        req.deck_types
    };

    if full_text.trim().is_empty() {
        return Err(AppError::BadRequest("Text cannot be empty".to_string()));
    }

    let language = detect_language(&full_text, req.language.as_deref());

    let core_start = Instant::now();
    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "analyze::create_deck_from_text analyze_text_core");

    let freq_start = Instant::now();
    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));
    tracing::info!(duration_ms = freq_start.elapsed().as_millis() as u64, words = words.len(), "analyze::create_deck_from_text frequency counting");

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);

    // Create deck(s) based on selected types
    let db_start = Instant::now();
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (cloze)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "cloze",
            "deck_type": "cloze"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'text', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind("from pasted text")
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        cloze_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    if deck_types.contains(&DeckType::WordDefinition) {
        let name = if both_types { format!("{} (definition)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "definition"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'text', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind("from pasted text")
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        def_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, decks = created_decks.len(), "analyze::create_deck_from_text create deck(s)");

    let cards_start = Instant::now();
    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;
    tracing::info!(duration_ms = cards_start.elapsed().as_millis() as u64, "analyze::create_deck_from_text create_cards_from_analysis");

    // Update descriptions with actual card counts (post-dedup)
    let skipped = result.words_skipped_duplicate;
    let new_words = result.cards_created;
    let desc = if skipped > 0 {
        format!("{} new words from pasted text ({} already known)", new_words, skipped)
    } else {
        format!("{} words from pasted text", new_words)
    };
    let updated_decks = update_deck_descriptions(&state.db, &created_decks, &desc).await?;

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        cards = result.cards_created,
        sentences = result.sentences_created,
        skipped = result.words_skipped_duplicate,
        "analyze::create_deck_from_text total"
    );

    Ok(Json(CreateDeckResult {
        decks: updated_decks.into_iter().map(|d| d.into()).collect(),
        cards_created: result.cards_created,
        sentences_created: result.sentences_created,
        i_plus_one_found: result.i_plus_one_found,
        words_skipped_duplicate: result.words_skipped_duplicate,
    }))
}

/// Create a deck directly from a PDF upload
pub async fn create_deck_from_pdf(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let mut filename: Option<String> = None;
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut deck_name: Option<String> = None;
    let mut deck_types: Vec<DeckType> = Vec::new();
    let mut language_hint: Option<String> = None;

    let upload_start = Instant::now();
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        let file_name = field.file_name().map(|s| s.to_string());

        match field_name.as_deref() {
            Some("file") => {
                filename = file_name;
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
                pdf_bytes = Some(bytes.to_vec());
            }
            Some("name") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read name: {}", e)))?;
                deck_name = Some(text);
            }
            Some("deck_types") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read deck_types: {}", e)))?;
                deck_types = parse_deck_types(&text);
            }
            Some("language") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read language: {}", e)))?;
                language_hint = Some(text);
            }
            // Ignore max_words for backward compat
            _ => {}
        }
    }
    tracing::info!(duration_ms = upload_start.elapsed().as_millis() as u64, "analyze::create_deck_from_pdf read upload");

    if deck_types.is_empty() {
        deck_types = vec![DeckType::WordDefinition];
    }

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;
    let deck_name = deck_name.unwrap_or_else(|| filename.replace(".pdf", "").replace(".PDF", ""));

    if !filename.to_lowercase().ends_with(".pdf") {
        return Err(AppError::BadRequest("Only PDF files are supported".to_string()));
    }

    let extract_start = Instant::now();
    let pages = pdf::extract_text_from_bytes(&pdf_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to extract text: {}", e)))?;
    tracing::info!(duration_ms = extract_start.elapsed().as_millis() as u64, pages = pages.len(), "analyze::create_deck_from_pdf PDF extraction");

    let full_text: String = pages.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n");
    let language = detect_language(&full_text, language_hint.as_deref());

    let core_start = Instant::now();
    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "analyze::create_deck_from_pdf analyze_text_core");

    let freq_start = Instant::now();
    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));
    tracing::info!(duration_ms = freq_start.elapsed().as_millis() as u64, words = words.len(), "analyze::create_deck_from_pdf frequency counting");

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);

    let db_start = Instant::now();
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (cloze)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "cloze",
            "deck_type": "cloze"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'pdf', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind(format!("from {}", filename))
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        cloze_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    if deck_types.contains(&DeckType::WordDefinition) {
        let name = if both_types { format!("{} (definition)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "definition"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'pdf', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind(format!("from {}", filename))
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        def_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, decks = created_decks.len(), "analyze::create_deck_from_pdf create deck(s)");

    let cards_start = Instant::now();
    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;
    tracing::info!(duration_ms = cards_start.elapsed().as_millis() as u64, "analyze::create_deck_from_pdf create_cards_from_analysis");

    // Update descriptions with actual card counts (post-dedup)
    let skipped = result.words_skipped_duplicate;
    let new_words = result.cards_created;
    let desc = if skipped > 0 {
        format!("{} new words from {} ({} already known)", new_words, filename, skipped)
    } else {
        format!("{} words from {}", new_words, filename)
    };
    let updated_decks = update_deck_descriptions(&state.db, &created_decks, &desc).await?;

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        cards = result.cards_created,
        sentences = result.sentences_created,
        skipped = result.words_skipped_duplicate,
        "analyze::create_deck_from_pdf total"
    );

    Ok(Json(CreateDeckResult {
        decks: updated_decks.into_iter().map(|d| d.into()).collect(),
        cards_created: result.cards_created,
        sentences_created: result.sentences_created,
        i_plus_one_found: result.i_plus_one_found,
        words_skipped_duplicate: result.words_skipped_duplicate,
    }))
}

/// Create a deck from an uploaded video or audio file
pub async fn create_deck_from_media(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let api_key = state.config.openai_api_key.as_deref().ok_or_else(|| {
        AppError::BadRequest("OPENAI_API_KEY is not configured. Media transcription requires an OpenAI API key.".to_string())
    })?;

    let mut filename: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut deck_name: Option<String> = None;
    let mut deck_types: Vec<DeckType> = Vec::new();
    let mut language_hint: Option<String> = None;

    let upload_start = Instant::now();
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        let file_name = field.file_name().map(|s| s.to_string());

        match field_name.as_deref() {
            Some("file") => {
                filename = file_name;
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
                file_bytes = Some(bytes.to_vec());
            }
            Some("name") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read name: {}", e)))?;
                deck_name = Some(text);
            }
            Some("deck_types") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read deck_types: {}", e)))?;
                deck_types = parse_deck_types(&text);
            }
            Some("language") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read language: {}", e)))?;
                language_hint = Some(text);
            }
            // Ignore max_words for backward compat
            _ => {}
        }
    }
    tracing::info!(duration_ms = upload_start.elapsed().as_millis() as u64, "analyze::create_deck_from_media read upload");

    if deck_types.is_empty() {
        deck_types = vec![DeckType::WordDefinition];
    }

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let file_bytes = file_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;

    if !transcription::is_media(&filename) {
        return Err(AppError::BadRequest(
            "Unsupported file type. Supported: mp4, mkv, webm, mov, mp3, m4a, wav, ogg, flac".to_string(),
        ));
    }

    let source_type = if transcription::is_video(&filename) { "video" } else { "audio" };
    let deck_name = deck_name.unwrap_or_else(|| {
        let stem = std::path::Path::new(&filename)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| filename.clone());
        stem
    });

    tracing::info!("Transcribing {} file: {}", source_type, filename);

    let transcribe_start = Instant::now();
    let (transcript, duration) = transcription::transcribe_media(
        &file_bytes,
        &filename,
        language_hint.as_deref(),
        api_key,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("Transcription failed: {}", e)))?;
    tracing::info!(
        duration_ms = transcribe_start.elapsed().as_millis() as u64,
        media_duration_s = duration as u64,
        transcript_chars = transcript.len(),
        "analyze::create_deck_from_media transcription"
    );

    if transcript.trim().is_empty() {
        return Err(AppError::BadRequest("Transcription produced no text. The file may contain no speech.".to_string()));
    }

    let language = detect_language(&transcript, language_hint.as_deref());

    let core_start = Instant::now();
    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&transcript, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "analyze::create_deck_from_media analyze_text_core");

    let freq_start = Instant::now();
    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));
    tracing::info!(duration_ms = freq_start.elapsed().as_millis() as u64, words = words.len(), "analyze::create_deck_from_media frequency counting");

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);
    let placeholder_desc = format!("from {} ({:.0}s)", filename, duration);

    let db_start = Instant::now();
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (cloze)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "cloze",
            "deck_type": "cloze"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind(&placeholder_desc)
        .bind(&language)
        .bind(source_type)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        cloze_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    if deck_types.contains(&DeckType::WordDefinition) {
        let name = if both_types { format!("{} (definition)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "definition"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind(&placeholder_desc)
        .bind(&language)
        .bind(source_type)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        def_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, decks = created_decks.len(), "analyze::create_deck_from_media create deck(s)");

    let cards_start = Instant::now();
    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;
    tracing::info!(duration_ms = cards_start.elapsed().as_millis() as u64, "analyze::create_deck_from_media create_cards_from_analysis");

    // Update descriptions with actual card counts (post-dedup)
    let skipped = result.words_skipped_duplicate;
    let new_words = result.cards_created;
    let desc = if skipped > 0 {
        format!("{} new words from {} ({} already known)", new_words, filename, skipped)
    } else {
        format!("{} words from {}", new_words, filename)
    };
    let updated_decks = update_deck_descriptions(&state.db, &created_decks, &desc).await?;

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        cards = result.cards_created,
        sentences = result.sentences_created,
        skipped = result.words_skipped_duplicate,
        "analyze::create_deck_from_media total"
    );

    Ok(Json(CreateDeckResult {
        decks: updated_decks.into_iter().map(|d| d.into()).collect(),
        cards_created: result.cards_created,
        sentences_created: result.sentences_created,
        i_plus_one_found: result.i_plus_one_found,
        words_skipped_duplicate: result.words_skipped_duplicate,
    }))
}

/// Update deck descriptions and return refreshed deck data (with correct card_count from triggers).
async fn update_deck_descriptions(
    db: &sqlx::PgPool,
    decks: &[Deck],
    description: &str,
) -> AppResult<Vec<Deck>> {
    let mut updated = Vec::with_capacity(decks.len());
    for deck in decks {
        let d = sqlx::query_as::<_, Deck>(
            "UPDATE decks SET description = $1 WHERE id = $2 RETURNING *",
        )
        .bind(description)
        .bind(deck.id)
        .fetch_one(db)
        .await?;
        updated.push(d);
    }
    Ok(updated)
}

struct CardCreationResult {
    cards_created: usize,
    sentences_created: usize,
    i_plus_one_found: usize,
    words_skipped_duplicate: usize,
}

/// Shared logic for creating cards from analyzed text.
/// Uses batch INSERTs instead of individual queries per card.
/// When both deck types are selected, `cloze_deck` and `def_deck` are separate decks.
/// When only one type is selected, only the relevant deck is Some.
#[allow(clippy::too_many_arguments)]
async fn create_cards_from_analysis(
    state: &Arc<AppState>,
    auth_user: &AuthUser,
    cloze_deck: Option<&Deck>,
    def_deck: Option<&Deck>,
    words: &[(String, i32)],
    deck_types: &[DeckType],
    surface_forms: &HashMap<String, Vec<String>>,
    pos_map: &HashMap<String, String>,
    lemma_sentences: &HashMap<String, Vec<String>>,
    sentence_content_lemmas: &HashMap<String, HashSet<String>>,
    language: &str,
) -> AppResult<CardCreationResult> {
    let start = Instant::now();

    // Query existing known lemmas for this user+language (language-scoped dedup)
    let db_start = Instant::now();
    let existing_lemmas = known_words_service::get_known_lemmas(&state.db, auth_user.user_id, language).await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, known = existing_lemmas.len(), "create_cards_from_analysis get_known_lemmas");

    let want_word_def = deck_types.contains(&DeckType::WordDefinition);
    let want_i_plus_one = deck_types.contains(&DeckType::IPlusOne);

    // Build i+1 map: lemma → Vec<(sentence, cloze_text, cloze_answer)>
    let mut i_plus_one_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let mut i_plus_one_found: usize = 0;

    if want_i_plus_one {
        let i1_start = Instant::now();
        let new_lemmas: HashSet<&str> = words.iter()
            .map(|(lemma, _)| lemma.as_str())
            .filter(|l| !existing_lemmas.contains(*l))
            .collect();

        for (sentence, content_lemmas) in sentence_content_lemmas {
            let unknown_lemmas: Vec<&String> = content_lemmas.iter()
                .filter(|l| !existing_lemmas.contains(l.as_str()) && new_lemmas.contains(l.as_str()))
                .collect();

            if unknown_lemmas.len() == 1 {
                i_plus_one_found += 1;
                let unknown_lemma = unknown_lemmas[0].clone();
                let word_surfaces = surface_forms.get(&unknown_lemma);

                let mut cloze_text = sentence.clone();
                let mut cloze_answer = unknown_lemma.clone();

                if let Some(surfaces) = word_surfaces {
                    for surface in surfaces {
                        if sentence.contains(surface) {
                            cloze_text = sentence.replace(surface, "＿＿＿");
                            cloze_answer = surface.clone();
                            break;
                        }
                    }
                }

                let entry = i_plus_one_map.entry(unknown_lemma).or_default();
                if entry.len() < 3 {
                    entry.push((sentence.clone(), cloze_text, cloze_answer));
                }
            }
        }
        tracing::info!(duration_ms = i1_start.elapsed().as_millis() as u64, i_plus_one_found = i_plus_one_found, "create_cards_from_analysis i+1 analysis");
    }

    // Pre-compute all card data before batch insert
    struct CardData {
        lemma: String,
        reading: Option<String>,
        definition: String,
        pos: Option<String>,
        frequency_rank: i32,
        doc_frequency: i32,
    }

    // Helper to batch-insert cards into a specific deck and attach sentences
    async fn insert_cards_for_deck(
        state: &Arc<AppState>,
        auth_user: &AuthUser,
        deck: &Deck,
        card_data_list: &[CardData],
        sentences: &[(usize, String, String, String, bool)], // (card_data_idx, text, cloze_text, cloze_answer, is_primary)
        language: &str,
    ) -> AppResult<(usize, usize)> {
        if card_data_list.is_empty() {
            return Ok((0, 0));
        }

        let lemmas: Vec<&str> = card_data_list.iter().map(|c| c.lemma.as_str()).collect();
        let readings: Vec<Option<&str>> = card_data_list.iter().map(|c| c.reading.as_deref()).collect();
        let definitions: Vec<&str> = card_data_list.iter().map(|c| c.definition.as_str()).collect();
        let pos_list: Vec<Option<&str>> = card_data_list.iter().map(|c| c.pos.as_deref()).collect();
        let freq_ranks: Vec<i32> = card_data_list.iter().map(|c| c.frequency_rank).collect();
        let doc_freqs: Vec<i32> = card_data_list.iter().map(|c| c.doc_frequency).collect();

        let card_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO cards (deck_id, lemma, reading, definition, part_of_speech, frequency_rank, doc_frequency)
            SELECT $1, unnest($2::text[]), unnest($3::text[]), unnest($4::text[]), unnest($5::text[]), unnest($6::int[]), unnest($7::int[])
            RETURNING id
            "#,
        )
        .bind(deck.id)
        .bind(&lemmas)
        .bind(&readings)
        .bind(&definitions)
        .bind(&pos_list)
        .bind(&freq_ranks)
        .bind(&doc_freqs)
        .fetch_all(&state.db)
        .await?;

        let cards_created = card_ids.len();

        // Add known words
        let new_lemmas: Vec<&str> = card_data_list.iter().map(|c| c.lemma.as_str()).collect();
        known_words_service::add_known_words(&state.db, auth_user.user_id, language, &new_lemmas).await?;

        // Insert card_states
        sqlx::query(
            r#"
            INSERT INTO card_states (user_id, card_id, status)
            SELECT $1, unnest($2::uuid[]), 'new'
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&card_ids)
        .execute(&state.db)
        .await?;

        // Build idx → card_id map
        let idx_to_card_id: HashMap<usize, uuid::Uuid> = (0..card_data_list.len())
            .zip(card_ids.iter())
            .map(|(idx, id)| (idx, *id))
            .collect();

        // Batch insert sentences
        let mut sent_card_ids: Vec<uuid::Uuid> = Vec::new();
        let mut sent_texts: Vec<String> = Vec::new();
        let mut sent_cloze_texts: Vec<String> = Vec::new();
        let mut sent_cloze_answers: Vec<String> = Vec::new();
        let mut sent_is_primary: Vec<bool> = Vec::new();

        for (card_data_idx, text, cloze_text, cloze_answer, is_primary) in sentences {
            if let Some(card_id) = idx_to_card_id.get(card_data_idx) {
                sent_card_ids.push(*card_id);
                sent_texts.push(text.clone());
                sent_cloze_texts.push(cloze_text.clone());
                sent_cloze_answers.push(cloze_answer.clone());
                sent_is_primary.push(*is_primary);
            }
        }

        let sentences_created = sent_card_ids.len();
        if !sent_card_ids.is_empty() {
            sqlx::query(
                r#"
                INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, is_primary)
                SELECT unnest($1::uuid[]), unnest($2::text[]), unnest($3::text[]), unnest($4::text[]), unnest($5::bool[])
                "#,
            )
            .bind(&sent_card_ids)
            .bind(&sent_texts)
            .bind(&sent_cloze_texts)
            .bind(&sent_cloze_answers)
            .bind(&sent_is_primary)
            .execute(&state.db)
            .await?;
        }

        Ok((cards_created, sentences_created))
    }

    let dict_start = Instant::now();
    let mut words_skipped_duplicate = 0;

    // Build per-word data: dictionary info + which decks each word belongs to
    struct WordData {
        lemma: String,
        reading: Option<String>,
        definition: String,
        pos: Option<String>,
        frequency_rank: i32,
        doc_frequency: i32,
        has_i_plus_one: bool,
    }

    let mut word_data_list: Vec<WordData> = Vec::new();

    for (rank, (lemma, count)) in words.iter().enumerate() {
        if existing_lemmas.contains(lemma) {
            words_skipped_duplicate += 1;
            continue;
        }

        let has_i1 = i_plus_one_map.contains_key(lemma);

        // If only i+1 requested and this word has no i+1 sentences, skip
        if want_i_plus_one && !want_word_def && !has_i1 {
            continue;
        }

        let dict_entry = dictionary::lookup(lemma, language);
        let definitions = dict_entry
            .map(|e| e.definitions.join("; "))
            .unwrap_or_default();
        let reading = if is_japanese(language) {
            dict_entry.map(|e| e.reading.clone())
        } else {
            None
        };

        if is_japanese(language) && definitions.is_empty() {
            continue;
        }

        word_data_list.push(WordData {
            lemma: lemma.clone(),
            reading,
            definition: definitions,
            pos: pos_map.get(lemma).cloned(),
            frequency_rank: (rank + 1) as i32,
            doc_frequency: *count,
            has_i_plus_one: has_i1,
        });
    }
    tracing::info!(
        duration_ms = dict_start.elapsed().as_millis() as u64,
        words_to_process = word_data_list.len(),
        skipped = words_skipped_duplicate,
        "create_cards_from_analysis dictionary lookups + card data prep"
    );

    if word_data_list.is_empty() {
        return Ok(CardCreationResult {
            cards_created: 0,
            sentences_created: 0,
            i_plus_one_found,
            words_skipped_duplicate,
        });
    }

    let mut total_cards_created = 0;
    let mut total_sentences_created = 0;

    // --- Cloze deck cards (i+1) ---
    if want_i_plus_one {
        if let Some(c_deck) = cloze_deck {
            let mut cloze_card_data: Vec<CardData> = Vec::new();
            let mut cloze_sentences: Vec<(usize, String, String, String, bool)> = Vec::new();

            for wd in &word_data_list {
                if !wd.has_i_plus_one {
                    continue;
                }

                let i1_sentences = match i_plus_one_map.get(&wd.lemma) {
                    Some(s) => s,
                    None => continue,
                };

                // For cloze cards, use the surface form (conjugated word) as the card lemma
                // Use the first i+1 sentence's cloze_answer as the surface form
                let surface_lemma = i1_sentences.first()
                    .map(|(_, _, answer)| answer.clone())
                    .unwrap_or_else(|| wd.lemma.clone());

                let idx = cloze_card_data.len();
                cloze_card_data.push(CardData {
                    lemma: surface_lemma,
                    reading: wd.reading.clone(),
                    definition: wd.definition.clone(),
                    pos: wd.pos.clone(),
                    frequency_rank: wd.frequency_rank,
                    doc_frequency: wd.doc_frequency,
                });

                for (i, (sentence, cloze_text, cloze_answer)) in i1_sentences.iter().enumerate() {
                    cloze_sentences.push((idx, sentence.clone(), cloze_text.clone(), cloze_answer.clone(), i == 0));
                }
            }

            let db_start = Instant::now();
            let (cards, sents) = insert_cards_for_deck(
                state, auth_user, c_deck, &cloze_card_data, &cloze_sentences, language,
            ).await?;
            tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards, sents, "create_cards_from_analysis cloze deck insert");
            total_cards_created += cards;
            total_sentences_created += sents;
        }
    }

    // --- Definition deck cards ---
    if want_word_def {
        if let Some(d_deck) = def_deck {
            let mut def_card_data: Vec<CardData> = Vec::new();
            let mut def_sentences: Vec<(usize, String, String, String, bool)> = Vec::new();

            for wd in &word_data_list {
                let idx = def_card_data.len();
                // For definition cards, use the dictionary lemma
                def_card_data.push(CardData {
                    lemma: wd.lemma.clone(),
                    reading: wd.reading.clone(),
                    definition: wd.definition.clone(),
                    pos: wd.pos.clone(),
                    frequency_rank: wd.frequency_rank,
                    doc_frequency: wd.doc_frequency,
                });

                let word_sentences = lemma_sentences.get(&wd.lemma);
                let word_surfaces = surface_forms.get(&wd.lemma);

                if let Some(sents) = word_sentences {
                    for (i, sentence) in sents.iter().take(3).enumerate() {
                        let mut cloze_text = sentence.clone();
                        let mut cloze_answer = wd.lemma.clone();

                        if let Some(surfaces) = word_surfaces {
                            for surface in surfaces {
                                if sentence.contains(surface) {
                                    cloze_text = sentence.replace(surface, "＿＿＿");
                                    cloze_answer = surface.clone();
                                    break;
                                }
                            }
                        }

                        def_sentences.push((idx, sentence.clone(), cloze_text, cloze_answer, i == 0));
                    }
                }
            }

            let db_start = Instant::now();
            let (cards, sents) = insert_cards_for_deck(
                state, auth_user, d_deck, &def_card_data, &def_sentences, language,
            ).await?;
            tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards, sents, "create_cards_from_analysis definition deck insert");
            total_cards_created += cards;
            total_sentences_created += sents;
        }
    }

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "create_cards_from_analysis total");

    Ok(CardCreationResult {
        cards_created: total_cards_created,
        sentences_created: total_sentences_created,
        i_plus_one_found,
        words_skipped_duplicate,
    })
}
