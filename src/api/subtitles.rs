use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    api::analyze::{analyze_text_core, create_cards_from_analysis, detect_language, DeckType},
    api::settings::get_decrypted_key,
    error::{AppError, AppResult},
    models::Deck,
    processing::{frequency, subtitles as sub_parser},
    services::subtitle_service,
    AppState,
};

/// Delay between individual file downloads for OpenSubtitles (ms).
const OPENSUB_DOWNLOAD_DELAY_MS: u64 = 250;

// ============================================================================
// Search — returns grouped show + per-episode entries
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

#[derive(Debug, Serialize, Clone)]
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

    tracing::info!(query = %req.query, language = %req.language, "subtitle search");

    if req.language == "ja" {
        let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
        })?;
        let api_key = get_decrypted_key(&state.db, auth_user.user_id, "jimaku", encryption_key).await?;

        let entries = subtitle_service::jimaku_search(&api_key, &req.query)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "jimaku search failed");
                AppError::Internal(anyhow::anyhow!("{}", e))
            })?;

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
        let api_key = state.config.opensubtitles_api_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("OPENSUBTITLES_API_KEY is not configured"))
        })?;

        let entries = subtitle_service::opensub_search(api_key, &req.query, &req.language)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "opensub search failed");
                AppError::Internal(anyhow::anyhow!("{}", e))
            })?;

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
// List files for a Jimaku entry (used to show available episodes)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct SubtitleFilesRequest {
    pub entry_id: String,
    pub language: String,
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
    tracing::info!(entry_id = %req.entry_id, "listing subtitle files");

    if req.language == "ja" {
        let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
        })?;
        let api_key = get_decrypted_key(&state.db, auth_user.user_id, "jimaku", encryption_key).await?;

        let entry_id: u64 = req.entry_id.parse()
            .map_err(|_| AppError::BadRequest("Invalid entry ID".to_string()))?;

        let files = subtitle_service::jimaku_get_files(&api_key, entry_id, None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "jimaku get_files failed");
                AppError::Internal(anyhow::anyhow!("{}", e))
            })?;

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
        Err(AppError::BadRequest("Use search results directly for OpenSubtitles file IDs".to_string()))
    }
}

// ============================================================================
// Job-based deck creation
// ============================================================================

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CreateDeckFromSubtitlesRequest {
    pub entry_id: String,
    pub language: String,
    pub name: String,
    #[serde(default)]
    pub deck_types: Vec<DeckType>,
    /// For OpenSubtitles: specific file_ids to download
    pub file_ids: Option<Vec<String>>,
    /// For Jimaku: specific file URLs to download (if empty, downloads all for the entry)
    pub file_urls: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct SubtitleJobResponse {
    pub job_id: Uuid,
    pub status: String,
}

/// Create a subtitle deck — returns a job ID immediately, work happens in background.
pub async fn create_deck_from_subtitles(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckFromSubtitlesRequest>,
) -> AppResult<Json<SubtitleJobResponse>> {
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("Deck name cannot be empty".to_string()));
    }

    tracing::info!(name = %req.name, language = %req.language, "creating subtitle deck");

    // Validate we have the right keys before creating the job
    if req.language == "ja" {
        let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured"))
        })?;
        get_decrypted_key(&state.db, auth_user.user_id, "jimaku", encryption_key).await?;
    } else {
        state.config.opensubtitles_api_key.as_deref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("OPENSUBTITLES_API_KEY is not configured"))
        })?;
    }

    // Insert job row
    let request_data = serde_json::to_value(&req)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to serialize request: {}", e)))?;

    let job_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO subtitle_jobs (user_id, status, progress_stage, request_data)
        VALUES ($1, 'queued', 'queued', $2)
        RETURNING id
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&request_data)
    .fetch_one(&state.db)
    .await?;

    tracing::info!(job_id = %job_id, "subtitle job created");

    // Spawn background task
    let task_state = state.clone();
    let task_user_id = auth_user.user_id;
    tokio::spawn(async move {
        if let Err(e) = run_subtitle_job(task_state, job_id, task_user_id).await {
            tracing::error!(job_id = %job_id, error = %e, "subtitle job failed");
        }
    });

    Ok(Json(SubtitleJobResponse {
        job_id,
        status: "queued".to_string(),
    }))
}

// ============================================================================
// Job status polling
// ============================================================================

#[derive(Debug, Serialize)]
pub struct SubtitleJobStatus {
    pub id: Uuid,
    pub status: String,
    pub progress_current: i32,
    pub progress_total: i32,
    pub progress_stage: String,
    pub error_message: Option<String>,
    /// Populated on completion
    pub result: Option<SubtitleJobResult>,
}

#[derive(Debug, Serialize)]
pub struct SubtitleJobResult {
    pub deck_ids: Vec<serde_json::Value>,
    pub cards_created: i32,
    pub sentences_created: i32,
    pub i_plus_one_found: i32,
    pub words_skipped_duplicate: i32,
}

pub async fn get_job_status(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<SubtitleJobStatus>> {
    let row = sqlx::query_as::<_, (
        Uuid, String, i32, i32, String, Option<String>,
        serde_json::Value, i32, i32, i32, i32,
    )>(
        r#"
        SELECT id, status, progress_current, progress_total, progress_stage,
               error_message, deck_ids, cards_created, sentences_created,
               i_plus_one_found, words_skipped_duplicate
        FROM subtitle_jobs
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(job_id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Job not found".to_string()))?;

    let result = if row.1 == "complete" {
        Some(SubtitleJobResult {
            deck_ids: row.6.as_array().cloned().unwrap_or_default(),
            cards_created: row.7,
            sentences_created: row.8,
            i_plus_one_found: row.9,
            words_skipped_duplicate: row.10,
        })
    } else {
        None
    };

    Ok(Json(SubtitleJobStatus {
        id: row.0,
        status: row.1,
        progress_current: row.2,
        progress_total: row.3,
        progress_stage: row.4,
        error_message: row.5,
        result,
    }))
}

// ============================================================================
// Background job execution
// ============================================================================

async fn run_subtitle_job(
    state: Arc<AppState>,
    job_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<()> {
    let start = Instant::now();
    tracing::info!(job_id = %job_id, "subtitle job: starting");

    // Load the request from the job row
    let request_data: serde_json::Value = sqlx::query_scalar(
        "SELECT request_data FROM subtitle_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_one(&state.db)
    .await?;

    let req: CreateDeckFromSubtitlesRequest = serde_json::from_value(request_data)?;
    let deck_types = if req.deck_types.is_empty() {
        vec![DeckType::WordDefinition]
    } else {
        req.deck_types.clone()
    };

    // ── Step 1: Download subtitles ──
    let subtitle_text = match download_with_rate_limiting(&state, job_id, user_id, &req).await {
        Ok(text) => text,
        Err(e) => {
            tracing::error!(job_id = %job_id, error = %e, "subtitle job: download failed");
            update_job_failed(&state.db, job_id, &e.to_string()).await;
            return Err(e);
        }
    };

    if subtitle_text.trim().is_empty() {
        let msg = "No subtitle text could be extracted. The files may be empty or in an unsupported format.";
        update_job_failed(&state.db, job_id, msg).await;
        return Err(anyhow::anyhow!("{}", msg));
    }

    // ── Step 2: Analyze text ──
    update_job_stage(&state.db, job_id, "processing", "analyzing text").await;
    tracing::info!(job_id = %job_id, "subtitle job: analyzing text");

    let language = detect_language(&subtitle_text, Some(&req.language));

    let (all_tokens, _sentences, surface_forms, pos_map, lemma_sentences, sentence_content_lemmas) =
        match analyze_text_core(&subtitle_text, &language) {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(job_id = %job_id, error = %e, "subtitle job: analysis failed");
                update_job_failed(&state.db, job_id, &format!("Analysis failed: {}", e)).await;
                return Err(anyhow::anyhow!("Analysis failed: {}", e));
            }
        };

    let freq_map = frequency::count_lemmas(&all_tokens, &language);
    let mut words: Vec<(String, i32)> = freq_map
        .iter()
        .map(|(lemma, wf)| (lemma.clone(), wf.doc_count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1));

    // ── Step 3: Create decks ──
    update_job_stage(&state.db, job_id, "processing", "creating deck").await;
    tracing::info!(job_id = %job_id, "subtitle job: creating decks");

    let both_types = deck_types.contains(&DeckType::IPlusOne) && deck_types.contains(&DeckType::WordDefinition);
    let mut created_decks: Vec<Deck> = Vec::new();
    let mut cloze_deck_ref: Option<usize> = None;
    let mut def_deck_ref: Option<usize> = None;

    if deck_types.contains(&DeckType::IPlusOne) {
        let name = if both_types { format!("{} (i+1)", req.name) } else { req.name.clone() };
        let settings = serde_json::json!({
            "new_cards_per_day": 20,
            "study_mode": "flashcard",
            "deck_type": "i_plus_one"
        });
        let deck = sqlx::query_as::<_, Deck>(
            r#"
            INSERT INTO decks (user_id, name, description, language, source_type, settings)
            VALUES ($1, $2, $3, $4, 'subtitle', $5)
            RETURNING *
            "#,
        )
        .bind(user_id)
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
        let name = if both_types { format!("{} (all words)", req.name) } else { req.name.clone() };
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
        .bind(user_id)
        .bind(&name)
        .bind("from subtitles")
        .bind(&language)
        .bind(&settings)
        .fetch_one(&state.db)
        .await?;
        def_deck_ref = Some(created_decks.len());
        created_decks.push(deck);
    }

    // ── Step 4: Create cards ──
    update_job_stage(&state.db, job_id, "processing", "generating cards").await;
    tracing::info!(job_id = %job_id, "subtitle job: generating cards");

    let auth_user = crate::api::middleware::AuthUser { user_id };

    let result = create_cards_from_analysis(
        &state, &auth_user,
        cloze_deck_ref.map(|i| &created_decks[i]),
        def_deck_ref.map(|i| &created_decks[i]),
        &words,
        &deck_types, &surface_forms, &pos_map, &lemma_sentences,
        &sentence_content_lemmas, &language,
    ).await
    .map_err(|e| anyhow::anyhow!("Card creation failed: {}", e))?;

    // If no cards were created, delete the empty decks and report as error
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
            "No new vocabulary found in these subtitles".to_string()
        };
        update_job_failed(&state.db, job_id, &msg).await;
        anyhow::bail!(msg);
    }

    // Update deck descriptions with actual per-deck card counts
    let skipped = result.words_skipped_duplicate;
    for deck in &created_decks {
        let card_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE deck_id = $1")
            .bind(deck.id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
        let desc = if skipped > 0 {
            format!("{} new words from subtitles ({} already known)", card_count, skipped)
        } else {
            format!("{} words from subtitles", card_count)
        };
        let _ = sqlx::query("UPDATE decks SET description = $1 WHERE id = $2")
            .bind(&desc)
            .bind(deck.id)
            .execute(&state.db)
            .await;
    }

    // ── Step 5: Mark complete ──
    let deck_ids: Vec<serde_json::Value> = created_decks
        .iter()
        .map(|d| serde_json::json!({ "id": d.id, "name": d.name }))
        .collect();

    sqlx::query(
        r#"
        UPDATE subtitle_jobs
        SET status = 'complete', progress_stage = 'complete',
            deck_ids = $2, cards_created = $3, sentences_created = $4,
            i_plus_one_found = $5, words_skipped_duplicate = $6,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(serde_json::json!(deck_ids))
    .bind(result.cards_created as i32)
    .bind(result.sentences_created as i32)
    .bind(result.i_plus_one_found as i32)
    .bind(result.words_skipped_duplicate as i32)
    .execute(&state.db)
    .await?;

    tracing::info!(
        job_id = %job_id,
        duration_ms = start.elapsed().as_millis() as u64,
        cards = result.cards_created,
        "subtitle job complete"
    );

    Ok(())
}

// ============================================================================
// Rate-limited downloading
// ============================================================================

async fn download_with_rate_limiting(
    state: &Arc<AppState>,
    job_id: Uuid,
    user_id: Uuid,
    req: &CreateDeckFromSubtitlesRequest,
) -> anyhow::Result<String> {
    if req.language == "ja" {
        download_jimaku_rate_limited(state, job_id, user_id, req).await
    } else {
        download_opensub_rate_limited(state, job_id, req).await
    }
}

async fn download_jimaku_rate_limited(
    state: &Arc<AppState>,
    job_id: Uuid,
    user_id: Uuid,
    req: &CreateDeckFromSubtitlesRequest,
) -> anyhow::Result<String> {
    let encryption_key = state.config.encryption_key.as_deref()
        .ok_or_else(|| anyhow::anyhow!("ENCRYPTION_KEY is not configured"))?;
    let api_key = get_decrypted_key(&state.db, user_id, "jimaku", encryption_key)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Determine what files to download
    let file_urls: Vec<String> = if let Some(urls) = &req.file_urls {
        if urls.is_empty() {
            let entry_id: u64 = req.entry_id.parse()?;
            let files = subtitle_service::jimaku_get_files(&api_key, entry_id, None).await?;
            if files.is_empty() {
                return Err(anyhow::anyhow!("No subtitle files found for this entry."));
            }
            files.into_iter().map(|f| f.url).collect()
        } else {
            urls.clone()
        }
    } else {
        let entry_id: u64 = req.entry_id.parse()?;
        let files = subtitle_service::jimaku_get_files(&api_key, entry_id, None).await?;
        if files.is_empty() {
            return Err(anyhow::anyhow!("No subtitle files found for this entry."));
        }
        files.into_iter().map(|f| f.url).collect()
    };

    let total = file_urls.len();
    tracing::info!(job_id = %job_id, files = total, "downloading jimaku subtitles");
    update_job_downloading(&state.db, job_id, 0, total as i32).await;

    let mut all_text = String::new();
    for (i, url) in file_urls.iter().enumerate() {
        update_job_downloading(&state.db, job_id, (i + 1) as i32, total as i32).await;

        let (content, rate_limit) = subtitle_service::jimaku_download_file(&api_key, url).await?;
        let filename = url.rsplit('/').next().unwrap_or("subtitle.srt");

        let parsed = sub_parser::parse_subtitle_file(filename, &content)?;

        if !all_text.is_empty() {
            all_text.push(' ');
        }
        all_text.push_str(&parsed);

        // Respect Jimaku rate limits before next download
        subtitle_service::jimaku_respect_rate_limit(rate_limit.as_ref()).await;
    }

    Ok(all_text)
}

async fn download_opensub_rate_limited(
    state: &Arc<AppState>,
    job_id: Uuid,
    req: &CreateDeckFromSubtitlesRequest,
) -> anyhow::Result<String> {
    let api_key = state.config.opensubtitles_api_key.as_deref()
        .ok_or_else(|| anyhow::anyhow!("OPENSUBTITLES_API_KEY is not configured"))?;

    let file_ids: Vec<u64> = req.file_ids.as_ref()
        .ok_or_else(|| anyhow::anyhow!("file_ids is required for OpenSubtitles"))?
        .iter()
        .map(|s| s.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;

    if file_ids.is_empty() {
        return Err(anyhow::anyhow!("No file IDs provided"));
    }

    let total = file_ids.len();
    tracing::info!(job_id = %job_id, files = total, "downloading opensub subtitles");
    update_job_downloading(&state.db, job_id, 0, total as i32).await;

    // Acquire the OpenSubtitles semaphore — only one download stream at a time
    let _permit = state.opensub_semaphore.acquire().await
        .map_err(|_| anyhow::anyhow!("OpenSubtitles rate limiter closed"))?;

    let mut all_text = String::new();
    for (i, file_id) in file_ids.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(OPENSUB_DOWNLOAD_DELAY_MS)).await;
        }

        update_job_downloading(&state.db, job_id, (i + 1) as i32, total as i32).await;

        let content = subtitle_service::opensub_download(api_key, *file_id).await?;
        let parsed = sub_parser::parse_subtitle_file("subtitle.srt", &content)?;

        if !all_text.is_empty() {
            all_text.push(' ');
        }
        all_text.push_str(&parsed);
    }

    Ok(all_text)
}

// ============================================================================
// DB update helpers
// ============================================================================

async fn update_job_downloading(db: &sqlx::PgPool, job_id: Uuid, current: i32, total: i32) {
    let _ = sqlx::query(
        r#"
        UPDATE subtitle_jobs
        SET status = 'downloading', progress_stage = 'downloading',
            progress_current = $2, progress_total = $3, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(current)
    .bind(total)
    .execute(db)
    .await;
}

async fn update_job_stage(db: &sqlx::PgPool, job_id: Uuid, status: &str, stage: &str) {
    let _ = sqlx::query(
        r#"
        UPDATE subtitle_jobs
        SET status = $2, progress_stage = $3, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(status)
    .bind(stage)
    .execute(db)
    .await;
}

async fn update_job_failed(db: &sqlx::PgPool, job_id: Uuid, error: &str) {
    let _ = sqlx::query(
        r#"
        UPDATE subtitle_jobs
        SET status = 'failed', progress_stage = 'failed',
            error_message = $2, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(error)
    .execute(db)
    .await;
}
