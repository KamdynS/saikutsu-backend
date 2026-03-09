use axum::{
    extract::State,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use crate::{
    api::middleware::AuthUser,
    api::analyze::{analyze_text_core, create_cards_from_analysis, detect_language, DeckType, CreateDeckResult},
    api::settings::get_decrypted_key,
    error::{AppError, AppResult},
    models::Deck,
    processing::{frequency, subtitles as sub_parser},
    services::subtitle_service,
    AppState,
};

// ============================================================================
// Search
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SubtitleSearchRequest {
    pub query: String,
    pub language: String,
}

#[derive(Debug, Serialize)]
pub struct SubtitleSearchResult {
    pub entries: Vec<SubtitleEntry>,
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct SubtitleEntry {
    pub id: String,
    pub name: String,
    pub english_name: Option<String>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub year: Option<u32>,
    pub file_id: Option<String>,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<SubtitleSearchRequest>,
) -> AppResult<Json<SubtitleSearchResult>> {
    if req.query.trim().is_empty() {
        return Err(AppError::BadRequest("Search query cannot be empty".to_string()));
    }

    if req.language == "ja" {
        // Jimaku: requires user's own API key
        let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
        })?;
        let api_key = get_decrypted_key(&state.db, auth_user.user_id, "jimaku", encryption_key).await?;

        let entries = subtitle_service::jimaku_search(&api_key, &req.query)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

        Ok(Json(SubtitleSearchResult {
            entries: entries
                .into_iter()
                .map(|e| SubtitleEntry {
                    id: e.id.to_string(),
                    name: e.name,
                    english_name: e.english_name,
                    season_number: None,
                    episode_number: None,
                    year: None,
                    file_id: None,
                })
                .collect(),
            provider: "jimaku".to_string(),
        }))
    } else {
        // OpenSubtitles: server-side API key
        let api_key = state.config.opensubtitles_api_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("OPENSUBTITLES_API_KEY is not configured"))
        })?;

        let entries = subtitle_service::opensub_search(api_key, &req.query, &req.language)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

        Ok(Json(SubtitleSearchResult {
            entries: entries
                .into_iter()
                .map(|e| SubtitleEntry {
                    id: e.id.clone(),
                    name: e.title,
                    english_name: None,
                    season_number: e.season_number,
                    episode_number: e.episode_number,
                    year: e.year,
                    file_id: Some(e.file_id.to_string()),
                })
                .collect(),
            provider: "opensubtitles".to_string(),
        }))
    }
}

// ============================================================================
// List files for an entry (Jimaku only — OpenSubtitles returns files in search)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SubtitleFilesRequest {
    pub entry_id: String,
    pub language: String,
    pub episode: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SubtitleFileInfo {
    pub name: String,
    pub url: Option<String>,
    pub file_id: Option<String>,
}

pub async fn list_files(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<SubtitleFilesRequest>,
) -> AppResult<Json<Vec<SubtitleFileInfo>>> {
    if req.language == "ja" {
        let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
        })?;
        let api_key = get_decrypted_key(&state.db, auth_user.user_id, "jimaku", encryption_key).await?;

        let entry_id: u64 = req.entry_id.parse()
            .map_err(|_| AppError::BadRequest("Invalid entry ID".to_string()))?;

        let files = subtitle_service::jimaku_get_files(&api_key, entry_id, req.episode)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

        Ok(Json(
            files
                .into_iter()
                .map(|f| SubtitleFileInfo {
                    name: f.name,
                    url: Some(f.url),
                    file_id: None,
                })
                .collect(),
        ))
    } else {
        // OpenSubtitles doesn't have a separate files endpoint — files come with search results
        Err(AppError::BadRequest("Use search results directly for OpenSubtitles file IDs".to_string()))
    }
}

// ============================================================================
// Create deck from subtitles
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateDeckFromSubtitlesRequest {
    pub entry_id: String,
    pub language: String,
    pub name: String,
    #[serde(default)]
    pub deck_types: Vec<DeckType>,
    /// None = whole show (all available files)
    pub episode: Option<u32>,
    /// For OpenSubtitles: specific file IDs to download
    pub file_ids: Option<Vec<String>>,
    /// For Jimaku: specific file URLs to download
    pub file_urls: Option<Vec<String>>,
}

pub async fn create_deck_from_subtitles(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckFromSubtitlesRequest>,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();
    let deck_types = if req.deck_types.is_empty() {
        vec![DeckType::WordDefinition]
    } else {
        req.deck_types.clone()
    };

    // Step 1: Fetch subtitle content
    let subtitle_text = if req.language == "ja" {
        fetch_jimaku_subtitles(&state, auth_user.user_id, &req).await?
    } else {
        fetch_opensub_subtitles(&state, &req).await?
    };

    if subtitle_text.trim().is_empty() {
        return Err(AppError::BadRequest("No subtitle text could be extracted. The files may be empty or in an unsupported format.".to_string()));
    }

    tracing::info!(
        chars = subtitle_text.len(),
        "subtitles::create_deck subtitle text extracted"
    );

    // Step 2: Run through the analysis pipeline
    let language = detect_language(&subtitle_text, Some(&req.language));

    let core_start = Instant::now();
    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        analyze_text_core(&subtitle_text, &language)?;
    tracing::info!(duration_ms = core_start.elapsed().as_millis() as u64, "subtitles::create_deck analyze_text_core");

    let freq_map = frequency::count_lemmas(&all_tokens, &language);
    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    // Step 3: Create deck(s)
    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (cloze)", req.name) } else { req.name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "cloze",
            "deck_type": "cloze"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'subtitle', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind("from subtitles")
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        cloze_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    if deck_types.contains(&DeckType::WordDefinition) {
        let name = if both_types { format!("{} (definition)", req.name) } else { req.name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "definition"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'subtitle', $5)
            RETURNING *
            "#,
        )
        .bind(auth_user.user_id)
        .bind(&name)
        .bind("from subtitles")
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        def_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    // Step 4: Create cards
    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await?;

    // Update descriptions
    let skipped = result.words_skipped_duplicate;
    let new_words = result.cards_created;
    let desc = if skipped > 0 {
        format!("{} new words from subtitles ({} already known)", new_words, skipped)
    } else {
        format!("{} words from subtitles", new_words)
    };

    // Update deck descriptions
    for deck in &created_decks {
        let _ = sqlx::query("UPDATE decks SET description = $1 WHERE id = $2")
            .bind(&desc)
            .bind(deck.id)
            .execute(&state.db)
            .await;
    }

    // Re-fetch decks for response
    let mut updated_decks = Vec::new();
    for deck in &created_decks {
        let d = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1")
            .bind(deck.id)
            .fetch_one(&state.db)
            .await?;
        updated_decks.push(d);
    }

    tracing::info!(
        duration_ms = start.elapsed().as_millis() as u64,
        cards = result.cards_created,
        sentences = result.sentences_created,
        skipped = result.words_skipped_duplicate,
        "subtitles::create_deck total"
    );

    Ok(Json(CreateDeckResult {
        decks: updated_decks.into_iter().map(|d| d.into()).collect(),
        cards_created: result.cards_created,
        sentences_created: result.sentences_created,
        i_plus_one_found: result.i_plus_one_found,
        words_skipped_duplicate: result.words_skipped_duplicate,
    }))
}

// ============================================================================
// Internal helpers
// ============================================================================

async fn fetch_jimaku_subtitles(
    state: &Arc<AppState>,
    user_id: uuid::Uuid,
    req: &CreateDeckFromSubtitlesRequest,
) -> AppResult<String> {
    let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
    })?;
    let api_key = get_decrypted_key(&state.db, user_id, "jimaku", encryption_key).await?;

    let entry_id: u64 = req.entry_id.parse()
        .map_err(|_| AppError::BadRequest("Invalid entry ID".to_string()))?;

    // If specific file URLs provided, download those
    if let Some(urls) = &req.file_urls {
        let mut all_text = String::new();
        for url in urls {
            let content = subtitle_service::jimaku_download_file(&api_key, url)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

            let filename = url.rsplit('/').next().unwrap_or("subtitle.srt");
            let parsed = sub_parser::parse_subtitle_file(filename, &content)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Parse error: {}", e)))?;

            if !all_text.is_empty() {
                all_text.push(' ');
            }
            all_text.push_str(&parsed);
        }
        return Ok(all_text);
    }

    // Otherwise, fetch all files for the entry/episode
    let files = subtitle_service::jimaku_get_files(&api_key, entry_id, req.episode)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

    if files.is_empty() {
        return Err(AppError::BadRequest("No subtitle files found for this entry/episode.".to_string()));
    }

    // Download and parse all files, concatenate
    let mut all_text = String::new();
    for file in &files {
        let content = subtitle_service::jimaku_download_file(&api_key, &file.url)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

        let parsed = sub_parser::parse_subtitle_file(&file.name, &content)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Parse error: {}", e)))?;

        if !all_text.is_empty() {
            all_text.push(' ');
        }
        all_text.push_str(&parsed);
    }

    Ok(all_text)
}

async fn fetch_opensub_subtitles(
    state: &Arc<AppState>,
    req: &CreateDeckFromSubtitlesRequest,
) -> AppResult<String> {
    let api_key = state.config.opensubtitles_api_key.as_deref().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("OPENSUBTITLES_API_KEY is not configured"))
    })?;

    // If specific file IDs provided, download those
    if let Some(ids) = &req.file_ids {
        let mut all_text = String::new();
        for id_str in ids {
            let file_id: u64 = id_str.parse()
                .map_err(|_| AppError::BadRequest(format!("Invalid file ID: {}", id_str)))?;

            let content = subtitle_service::opensub_download(api_key, file_id)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

            let parsed = sub_parser::parse_subtitle_file("subtitle.srt", &content)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Parse error: {}", e)))?;

            if !all_text.is_empty() {
                all_text.push(' ');
            }
            all_text.push_str(&parsed);
        }
        return Ok(all_text);
    }

    Err(AppError::BadRequest("file_ids is required for OpenSubtitles".to_string()))
}
