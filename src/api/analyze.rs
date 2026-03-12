use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::Deck,
    processing::{frequency, pdf, transcription},
    services::{
        analyze_service::{
            analyze_text_core, build_word_list, detect_language, parse_deck_types,
            AnalysisResult, DeckType, NlpConfig,
        },
        card_creation::{create_cards_from_analysis, update_deck_descriptions, CardFilters, CreateDeckResult},
    },
    AppState,
};

/// Build NLP config from app state if NLP_SERVICE_URL is set.
fn nlp_config(state: &AppState) -> Option<NlpConfig<'_>> {
    state.config.nlp_service_url.as_ref().map(|url| NlpConfig {
        client: &state.http_client,
        base_url: url.as_str(),
    })
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
    #[serde(default)]
    pub filters: Option<CardFilters>,
}

pub async fn analyze_pdf(mut multipart: Multipart) -> AppResult<Json<AnalysisResult>> {
    let start = Instant::now();

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
        analyze_text_core(&full_text, &language, None).await?;

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();

    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);

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

    let (all_tokens, sentences, surface_forms, pos_map, lemma_sentences, _sentence_content_lemmas) =
        analyze_text_core(&full_text, &language, None).await?;

    let total_tokens = all_tokens.len();
    let content_token_count = all_tokens.iter().filter(|t| t.is_content).count();

    let words = build_word_list(&all_tokens, &surface_forms, &pos_map, &lemma_sentences, &language);

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

/// Create a deck from raw text
pub async fn create_deck_from_text(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckFromTextRequest>,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let full_text = req.text;
    let deck_name = req.name;
    let filters = req.filters;
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
        analyze_text_core(&full_text, &language, nlp_config(&state)).await?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);

    // Create deck(s) based on selected types
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (i+1)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "i_plus_one"
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
        let name = if both_types { format!("{} (all words)", deck_name) } else { deck_name.clone() };
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

    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language, filters.as_ref(),
    ).await?;

    // If no cards were created, delete the empty decks and return error
    if result.cards_created == 0 {
        for deck in &created_decks {
            let _ = sqlx::query("DELETE FROM decks WHERE id = $1")
                .bind(deck.id)
                .execute(&state.db)
                .await;
        }
        let msg = if result.words_skipped_duplicate > 0 {
            format!("All {} words are already in your decks", result.words_skipped_duplicate)
        } else {
            "No new vocabulary found in this text".to_string()
        };
        return Err(AppError::BadRequest(msg));
    }

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
    let mut filters: Option<CardFilters> = None;

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
            Some("filters") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read filters: {}", e)))?;
                filters = serde_json::from_str(&text).ok();
            }
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
        analyze_text_core(&full_text, &language, nlp_config(&state)).await?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);

    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (i+1)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "i_plus_one"
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
        let name = if both_types { format!("{} (all words)", deck_name) } else { deck_name.clone() };
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

    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language, filters.as_ref(),
    ).await?;

    // If no cards were created, delete the empty decks and return error
    if result.cards_created == 0 {
        for deck in &created_decks {
            let _ = sqlx::query("DELETE FROM decks WHERE id = $1")
                .bind(deck.id)
                .execute(&state.db)
                .await;
        }
        let msg = if result.words_skipped_duplicate > 0 {
            format!("All {} words are already in your decks", result.words_skipped_duplicate)
        } else {
            "No new vocabulary found in this file".to_string()
        };
        return Err(AppError::BadRequest(msg));
    }

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
    let mut filters: Option<CardFilters> = None;

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
            Some("filters") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read filters: {}", e)))?;
                filters = serde_json::from_str(&text).ok();
            }
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

    if transcript.trim().is_empty() {
        return Err(AppError::BadRequest("Transcription produced no text. The file may contain no speech.".to_string()));
    }

    let language = detect_language(&transcript, language_hint.as_deref());

    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&transcript, &language, nlp_config(&state)).await?;

    let freq_map = frequency::count_lemmas(&all_tokens, &language);

    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);
    let placeholder_desc = format!("from {} ({:.0}s)", filename, duration);

    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (i+1)", deck_name) } else { deck_name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "i_plus_one"
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
        let name = if both_types { format!("{} (all words)", deck_name) } else { deck_name.clone() };
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

    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language, filters.as_ref(),
    ).await?;

    // If no cards were created, delete the empty decks and return error
    if result.cards_created == 0 {
        for deck in &created_decks {
            let _ = sqlx::query("DELETE FROM decks WHERE id = $1")
                .bind(deck.id)
                .execute(&state.db)
                .await;
        }
        let msg = if result.words_skipped_duplicate > 0 {
            format!("All {} words are already in your decks", result.words_skipped_duplicate)
        } else {
            "No new vocabulary found in this file".to_string()
        };
        return Err(AppError::BadRequest(msg));
    }

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
