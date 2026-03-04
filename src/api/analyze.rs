use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    let sentences = split_sentences(full_text, language);

    // Tokenize full text only ONCE (was previously tokenized again per-sentence)
    let all_tokens = tokenize_text(full_text, language)?;

    // Build surface form and POS mappings (normalized lemma keys)
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

    // Build lemma → sentences and sentence → content lemmas mappings
    // using per-sentence tokenization for accurate matching (no false positives from substring matching)
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

pub async fn analyze_pdf(mut multipart: Multipart) -> AppResult<Json<AnalysisResult>> {
    let mut filename: Option<String> = None;
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut language_hint: Option<String> = None;

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

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;

    if !filename.to_lowercase().ends_with(".pdf") {
        return Err(AppError::BadRequest("Only PDF files are supported".to_string()));
    }

    let pages = pdf::extract_text_from_bytes(&pdf_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to extract text: {}", e)))?;

    let full_text: String = pages.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n");
    let language = detect_language(&full_text, language_hint.as_deref());
    let text_length = full_text.len();
    let sample_text: String = full_text.chars().take(500).collect();

    let (all_tokens, sentences, surface_forms, pos_map, lemma_sentences, _sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();
    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);
    let unique_words = words.len();
    let sentence_count = sentences.len();

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
    let full_text = req.text;
    let title = req.title.unwrap_or_else(|| "Pasted Text".to_string());

    if full_text.trim().is_empty() {
        return Err(AppError::BadRequest("Text cannot be empty".to_string()));
    }

    let language = detect_language(&full_text, req.language.as_deref());
    let text_length = full_text.len();
    let sample_text: String = full_text.chars().take(500).collect();

    let (all_tokens, sentences, surface_forms, pos_map, lemma_sentences, _sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();
    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);
    let unique_words = words.len();
    let sentence_count = sentences.len();

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
    pub deck: DeckResponse,
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

    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let study_mode = if deck_types.contains(&DeckType::IPlusOne) || deck_types.contains(&DeckType::WordDefinition) && deck_types.contains(&DeckType::IPlusOne) {
        "cloze"
    } else {
        "flashcard"
    };

    let settings = serde_json::json!({
        "new_cards_per_day": 20,
        "study_mode": study_mode
    });

    // Create the deck with detected language
    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, $4, 'text', $5)
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&deck_name)
    .bind(format!("{} words from pasted text", words.len()))
    .bind(&language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;

    let result = create_cards_from_analysis(
        &state, &auth_user, &deck, &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;

    tracing::info!(
        "Created deck '{}' ({}) with {} cards and {} sentences ({} i+1, {} skipped)",
        deck_name, language, result.cards_created, result.sentences_created,
        result.i_plus_one_found, result.words_skipped_duplicate
    );

    Ok(Json(CreateDeckResult {
        deck: deck.into(),
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
    let mut filename: Option<String> = None;
    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut deck_name: Option<String> = None;
    let mut deck_types: Vec<DeckType> = Vec::new();
    let mut language_hint: Option<String> = None;

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

    if deck_types.is_empty() {
        deck_types = vec![DeckType::WordDefinition];
    }

    let filename = filename.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;
    let pdf_bytes = pdf_bytes.ok_or_else(|| AppError::BadRequest("No file data".to_string()))?;
    let deck_name = deck_name.unwrap_or_else(|| filename.replace(".pdf", "").replace(".PDF", ""));

    if !filename.to_lowercase().ends_with(".pdf") {
        return Err(AppError::BadRequest("Only PDF files are supported".to_string()));
    }

    let pages = pdf::extract_text_from_bytes(&pdf_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to extract text: {}", e)))?;

    let full_text: String = pages.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n");
    let language = detect_language(&full_text, language_hint.as_deref());

    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&full_text, &language)?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let study_mode = if deck_types.contains(&DeckType::IPlusOne) {
        "cloze"
    } else {
        "flashcard"
    };

    let settings = serde_json::json!({
        "new_cards_per_day": 20,
        "study_mode": study_mode
    });

    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, $4, 'pdf', $5)
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&deck_name)
    .bind(format!("Generated from {} - {} words", filename, words.len()))
    .bind(&language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;

    let result = create_cards_from_analysis(
        &state, &auth_user, &deck, &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;

    tracing::info!(
        "Created deck '{}' ({}) with {} cards and {} sentences ({} i+1, {} skipped)",
        deck_name, language, result.cards_created, result.sentences_created,
        result.i_plus_one_found, result.words_skipped_duplicate
    );

    Ok(Json(CreateDeckResult {
        deck: deck.into(),
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
    let api_key = state.config.openai_api_key.as_deref().ok_or_else(|| {
        AppError::BadRequest("OPENAI_API_KEY is not configured. Media transcription requires an OpenAI API key.".to_string())
    })?;

    let mut filename: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut deck_name: Option<String> = None;
    let mut deck_types: Vec<DeckType> = Vec::new();
    let mut language_hint: Option<String> = None;

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

    let (transcript, duration) = transcription::transcribe_media(
        &file_bytes,
        &filename,
        language_hint.as_deref(),
        api_key,
    )
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("Transcription failed: {}", e)))?;

    tracing::info!(
        "Transcription complete: {:.0}s duration, {} chars",
        duration,
        transcript.len()
    );

    if transcript.trim().is_empty() {
        return Err(AppError::BadRequest("Transcription produced no text. The file may contain no speech.".to_string()));
    }

    let language = detect_language(&transcript, language_hint.as_deref());

    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&transcript, &language)?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let study_mode = if deck_types.contains(&DeckType::IPlusOne) {
        "cloze"
    } else {
        "flashcard"
    };

    let settings = serde_json::json!({
        "new_cards_per_day": 20,
        "study_mode": study_mode
    });

    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&deck_name)
    .bind(format!(
        "Transcribed from {} ({:.0}s) - {} words",
        filename,
        duration,
        words.len()
    ))
    .bind(&language)
    .bind(source_type)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;

    let result = create_cards_from_analysis(
        &state, &auth_user, &deck, &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;

    tracing::info!(
        "Created deck '{}' ({}) from {} with {} cards and {} sentences ({} i+1, {} skipped)",
        deck_name, language, source_type, result.cards_created, result.sentences_created,
        result.i_plus_one_found, result.words_skipped_duplicate
    );

    Ok(Json(CreateDeckResult {
        deck: deck.into(),
        cards_created: result.cards_created,
        sentences_created: result.sentences_created,
        i_plus_one_found: result.i_plus_one_found,
        words_skipped_duplicate: result.words_skipped_duplicate,
    }))
}

struct CardCreationResult {
    cards_created: usize,
    sentences_created: usize,
    i_plus_one_found: usize,
    words_skipped_duplicate: usize,
}

/// Shared logic for creating cards from analyzed text.
/// Uses batch INSERTs instead of individual queries per card.
#[allow(clippy::too_many_arguments)]
async fn create_cards_from_analysis(
    state: &Arc<AppState>,
    auth_user: &AuthUser,
    deck: &Deck,
    words: &[(String, i32)],
    deck_types: &[DeckType],
    surface_forms: &HashMap<String, Vec<String>>,
    pos_map: &HashMap<String, String>,
    lemma_sentences: &HashMap<String, Vec<String>>,
    sentence_content_lemmas: &HashMap<String, HashSet<String>>,
    language: &str,
) -> AppResult<CardCreationResult> {
    // Query existing known lemmas for this user+language (language-scoped dedup)
    let existing_lemmas = known_words_service::get_known_lemmas(&state.db, auth_user.user_id, language).await?;

    let want_word_def = deck_types.contains(&DeckType::WordDefinition);
    let want_i_plus_one = deck_types.contains(&DeckType::IPlusOne);

    // Build i+1 map: lemma → Vec<(sentence, cloze_text, cloze_answer)>
    let mut i_plus_one_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let mut i_plus_one_found: usize = 0;

    if want_i_plus_one {
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

    let mut card_data_list: Vec<CardData> = Vec::new();
    let mut words_skipped_duplicate = 0;

    for (rank, (lemma, count)) in words.iter().enumerate() {
        if existing_lemmas.contains(lemma) {
            words_skipped_duplicate += 1;
            continue;
        }

        if want_i_plus_one && !want_word_def && !i_plus_one_map.contains_key(lemma) {
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

        card_data_list.push(CardData {
            lemma: lemma.clone(),
            reading,
            definition: definitions,
            pos: pos_map.get(lemma).cloned(),
            frequency_rank: (rank + 1) as i32,
            doc_frequency: *count,
        });
    }

    if card_data_list.is_empty() {
        return Ok(CardCreationResult {
            cards_created: 0,
            sentences_created: 0,
            i_plus_one_found,
            words_skipped_duplicate,
        });
    }

    // Batch INSERT all cards using unnest
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

    // Add newly created lemmas to known_words
    let new_lemmas: Vec<&str> = card_data_list.iter().map(|c| c.lemma.as_str()).collect();
    known_words_service::add_known_words(&state.db, auth_user.user_id, language, &new_lemmas).await?;

    // Batch INSERT all card_states
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

    // Build lemma → card_id map for sentence attachment
    let lemma_to_card_id: HashMap<&str, uuid::Uuid> = card_data_list.iter()
        .zip(card_ids.iter())
        .map(|(data, id)| (data.lemma.as_str(), *id))
        .collect();

    // Collect all sentences for batch insert
    let mut sent_card_ids: Vec<uuid::Uuid> = Vec::new();
    let mut sent_texts: Vec<String> = Vec::new();
    let mut sent_cloze_texts: Vec<String> = Vec::new();
    let mut sent_cloze_answers: Vec<String> = Vec::new();
    let mut sent_is_primary: Vec<bool> = Vec::new();

    for data in &card_data_list {
        let card_id = match lemma_to_card_id.get(data.lemma.as_str()) {
            Some(id) => *id,
            None => continue,
        };

        // Track which sentences have been attached (to avoid duplicates when both types selected)
        let mut attached_sentences: HashSet<String> = HashSet::new();

        // Attach i+1 sentences if requested
        if want_i_plus_one {
            if let Some(i1_sentences) = i_plus_one_map.get(&data.lemma) {
                for (i, (sentence, cloze_text, cloze_answer)) in i1_sentences.iter().enumerate() {
                    attached_sentences.insert(sentence.clone());
                    sent_card_ids.push(card_id);
                    sent_texts.push(sentence.clone());
                    sent_cloze_texts.push(cloze_text.clone());
                    sent_cloze_answers.push(cloze_answer.clone());
                    // When both types selected, word_def sentence is primary (for flashcard mode)
                    sent_is_primary.push(!want_word_def && i == 0);
                }
            }
        }

        // Attach word_def sentences if requested (deduped against i+1 sentences)
        if want_word_def {
            let word_sentences = lemma_sentences.get(&data.lemma);
            let word_surfaces = surface_forms.get(&data.lemma);

            if let Some(sents) = word_sentences {
                let mut word_def_count = 0;
                for sentence in sents.iter() {
                    if word_def_count >= 3 {
                        break;
                    }
                    if attached_sentences.contains(sentence) {
                        continue;
                    }

                    let mut cloze_text = sentence.clone();
                    let mut cloze_answer = data.lemma.clone();

                    if let Some(surfaces) = word_surfaces {
                        for surface in surfaces {
                            if sentence.contains(surface) {
                                cloze_text = sentence.replace(surface, "＿＿＿");
                                cloze_answer = surface.clone();
                                break;
                            }
                        }
                    }

                    sent_card_ids.push(card_id);
                    sent_texts.push(sentence.clone());
                    sent_cloze_texts.push(cloze_text);
                    sent_cloze_answers.push(cloze_answer);
                    sent_is_primary.push(word_def_count == 0);
                    word_def_count += 1;
                }
            }
        }
    }

    let sentences_created = sent_card_ids.len();

    // Batch INSERT all sentences
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

    Ok(CardCreationResult {
        cards_created,
        sentences_created,
        i_plus_one_found,
        words_skipped_duplicate,
    })
}
