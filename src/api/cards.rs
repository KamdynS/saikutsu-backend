use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{
        Card, CardListResponse, CardResponse, CreateCardRequest, CreateSentenceRequest, Sentence,
        SentenceResponse, UpdateCardRequest, UpdateSentenceRequest,
    },
    processing::normalization::normalize_lemma,
    services::known_words_service,
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct ListCardsQuery {
    pub page: Option<i32>,
    pub per_page: Option<i32>,
    pub status: Option<String>,
    pub sort: Option<String>,
    pub search: Option<String>,
}

fn card_to_response(card: Card, sentences: Vec<SentenceResponse>) -> CardResponse {
    CardResponse {
        id: card.id,
        deck_id: card.deck_id,
        lemma: card.lemma,
        reading: card.reading,
        definition: card.definition,
        part_of_speech: card.part_of_speech,
        frequency_rank: card.frequency_rank,
        doc_frequency: card.doc_frequency,
        audio_url: card.audio_url,
        notes: card.notes,
        tags: card.tags,
        sentences,
    }
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Path(deck_id): Path<Uuid>,
    Query(query): Query<ListCardsQuery>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<CardListResponse>> {
    let start = Instant::now();

    // Verify deck ownership
    let db_start = Instant::now();
    let deck_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::list ownership check");

    if deck_exists == 0 {
        return Err(AppError::NotFound("Deck not found".to_string()));
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(50).clamp(1, 100);
    let offset = (page - 1) * per_page;

    // Build ORDER BY clause based on sort param
    let order_by = match query.sort.as_deref() {
        Some("alpha") => "lemma ASC",
        Some("alpha_desc") => "lemma DESC",
        Some("date") => "created_at ASC",
        Some("date_desc") => "created_at DESC",
        Some("frequency_desc") => "frequency_rank DESC NULLS LAST, created_at ASC",
        _ => "frequency_rank ASC NULLS LAST, created_at ASC", // default: frequency
    };

    // Get cards and total count
    let db_start = Instant::now();
    let (cards, total) = if let Some(ref search) = query.search {
        let search_pattern = format!("%{}%", search.to_lowercase());
        let query_str = format!(
            "SELECT * FROM cards WHERE deck_id = $1 AND (LOWER(lemma) LIKE $2 OR LOWER(definition) LIKE $2) ORDER BY {} LIMIT $3 OFFSET $4",
            order_by
        );
        let count_query = "SELECT COUNT(*) FROM cards WHERE deck_id = $1 AND (LOWER(lemma) LIKE $2 OR LOWER(definition) LIKE $2)";
        tokio::try_join!(
            sqlx::query_as::<_, Card>(&query_str)
                .bind(deck_id)
                .bind(&search_pattern)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&state.db),
            sqlx::query_scalar::<_, i64>(count_query)
                .bind(deck_id)
                .bind(&search_pattern)
                .fetch_one(&state.db),
        )?
    } else {
        let query_str = format!(
            "SELECT * FROM cards WHERE deck_id = $1 ORDER BY {} LIMIT $2 OFFSET $3",
            order_by
        );
        tokio::try_join!(
            sqlx::query_as::<_, Card>(&query_str)
                .bind(deck_id)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&state.db),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cards WHERE deck_id = $1")
                .bind(deck_id)
                .fetch_one(&state.db),
        )?
    };
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards_fetched = cards.len(), "cards::list fetch cards+count");

    // Batch fetch sentences for all cards at once (avoids N+1)
    let card_ids: Vec<Uuid> = cards.iter().map(|c| c.id).collect();

    let db_start = Instant::now();
    let sentences = sqlx::query_as::<_, Sentence>(
        "SELECT * FROM sentences WHERE card_id = ANY($1) ORDER BY is_primary DESC, created_at ASC",
    )
    .bind(&card_ids)
    .fetch_all(&state.db)
    .await?;
    tracing::info!(
        duration_ms = db_start.elapsed().as_millis() as u64,
        sentences = sentences.len(),
        "cards::list fetch sentences"
    );

    let mut sentences_by_card: HashMap<Uuid, Vec<Sentence>> = HashMap::with_capacity(cards.len());
    for s in sentences {
        sentences_by_card.entry(s.card_id).or_default().push(s);
    }

    let card_responses: Vec<CardResponse> = cards
        .into_iter()
        .map(|card| {
            let card_id = card.id;
            let sents = sentences_by_card
                .remove(&card_id)
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.into())
                .collect();
            card_to_response(card, sents)
        })
        .collect();

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, total = total, page = page, "cards::list total");

    Ok(Json(CardListResponse {
        cards: card_responses,
        total,
        page,
        per_page,
    }))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Path(deck_id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateCardRequest>,
) -> AppResult<Json<CardResponse>> {
    let start = Instant::now();

    // Verify deck ownership and get language
    let db_start = Instant::now();
    let deck_language = sqlx::query_scalar::<_, String>(
        "SELECT language FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(deck_id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Deck not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create ownership check");

    // Create the card
    let db_start = Instant::now();
    let card = sqlx::query_as::<_, Card>(
        r#"
        INSERT INTO cards (deck_id, lemma, definition, reading, part_of_speech)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING *
        "#,
    )
    .bind(deck_id)
    .bind(&req.lemma)
    .bind(&req.definition)
    .bind(&req.reading)
    .bind(&req.part_of_speech)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create insert card");

    // Add lemma to known_words
    let db_start = Instant::now();
    let norm = normalize_lemma(&req.lemma, &deck_language);
    known_words_service::add_known_words(&state.db, auth_user.user_id, &deck_language, &[norm.as_str()]).await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create add known_words");

    // If a sentence was provided, create it
    let sentences = if let Some(sentence_text) = &req.sentence {
        let cloze_text = sentence_text.replace(&req.lemma, "[...]");
        let db_start = Instant::now();
        let sentence = sqlx::query_as::<_, Sentence>(
            r#"
            INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, surface_form, is_primary)
            VALUES ($1, $2, $3, $4, $5, true)
            RETURNING *
            "#,
        )
        .bind(card.id)
        .bind(sentence_text)
        .bind(&cloze_text)
        .bind(&req.lemma)
        .bind(&req.lemma)
        .fetch_one(&state.db)
        .await?;
        tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create insert sentence");
        vec![sentence.into()]
    } else {
        vec![]
    };

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::create total");

    Ok(Json(card_to_response(card, sentences)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<CardResponse>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let card = sqlx::query_as::<_, Card>(
        "SELECT c.* FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2",
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Card not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::get fetch card");

    let db_start = Instant::now();
    let sentences = sqlx::query_as::<_, Sentence>(
        "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC, created_at ASC",
    )
    .bind(card.id)
    .fetch_all(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::get fetch sentences");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::get total");

    Ok(Json(card_to_response(
        card,
        sentences.into_iter().map(|s| s.into()).collect(),
    )))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<UpdateCardRequest>,
) -> AppResult<Json<CardResponse>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let card = sqlx::query_as::<_, Card>(
        "SELECT c.* FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2",
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Card not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::update ownership check");

    // Frontend always sends all fields — present values are kept, cleared fields are null.
    // For required fields (lemma, definition), fall back to existing if absent.
    let lemma = req.lemma.unwrap_or(card.lemma);
    let definition = req.definition.unwrap_or(card.definition);
    // For optional fields, always overwrite (null = clear the field).
    let reading = req.reading;
    let part_of_speech = req.part_of_speech;
    let notes = req.notes;
    let tags = req.tags.unwrap_or(card.tags);

    let db_start = Instant::now();
    let updated_card = sqlx::query_as::<_, Card>(
        r#"UPDATE cards SET lemma = $1, definition = $2, reading = $3, part_of_speech = $4, notes = $5, tags = $6, updated_at = NOW()
        WHERE id = $7 RETURNING *"#,
    )
    .bind(&lemma)
    .bind(&definition)
    .bind(&reading)
    .bind(&part_of_speech)
    .bind(&notes)
    .bind(&tags)
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::update db update");

    let db_start = Instant::now();
    let sentences = sqlx::query_as::<_, Sentence>(
        "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC, created_at ASC",
    )
    .bind(updated_card.id)
    .fetch_all(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::update fetch sentences");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::update total");

    Ok(Json(card_to_response(
        updated_card,
        sentences.into_iter().map(|s| s.into()).collect(),
    )))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    // Verify ownership
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2",
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if exists == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    sqlx::query("DELETE FROM cards WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
pub struct BulkDeleteRequest {
    pub card_ids: Vec<Uuid>,
}

pub async fn bulk_delete(
    State(state): State<Arc<AppState>>,
    Path(deck_id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<BulkDeleteRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if req.card_ids.is_empty() {
        return Err(AppError::Validation("No card IDs provided".to_string()));
    }

    // Verify deck ownership
    let deck_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if deck_exists == 0 {
        return Err(AppError::NotFound("Deck not found".to_string()));
    }

    let result = sqlx::query(
        "DELETE FROM cards WHERE id = ANY($1) AND deck_id = $2",
    )
    .bind(&req.card_ids)
    .bind(deck_id)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "deleted": result.rows_affected() })))
}

// --- Sentence CRUD ---

pub async fn create_sentence(
    State(state): State<Arc<AppState>>,
    Path(card_id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateSentenceRequest>,
) -> AppResult<Json<SentenceResponse>> {
    // Verify card ownership
    let card_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2",
    )
    .bind(card_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if card_exists == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    let is_primary = req.is_primary.unwrap_or(false);
    let surface_form = req.surface_form.unwrap_or_else(|| req.cloze_answer.clone());

    // If setting as primary, unset existing primary
    if is_primary {
        sqlx::query("UPDATE sentences SET is_primary = false WHERE card_id = $1 AND is_primary = true")
            .bind(card_id)
            .execute(&state.db)
            .await?;
    }

    let sentence = sqlx::query_as::<_, Sentence>(
        r#"INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, surface_form, source_page, is_primary)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING *"#,
    )
    .bind(card_id)
    .bind(&req.text)
    .bind(&req.cloze_text)
    .bind(&req.cloze_answer)
    .bind(&surface_form)
    .bind(req.source_page)
    .bind(is_primary)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(sentence.into()))
}

pub async fn update_sentence(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<UpdateSentenceRequest>,
) -> AppResult<Json<SentenceResponse>> {
    // Verify ownership via card -> deck -> user
    let sentence = sqlx::query_as::<_, Sentence>(
        r#"SELECT s.* FROM sentences s
        JOIN cards c ON s.card_id = c.id
        JOIN decks d ON c.deck_id = d.id
        WHERE s.id = $1 AND d.user_id = $2"#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Sentence not found".to_string()))?;

    let text = req.text.unwrap_or(sentence.text);
    let cloze_text = req.cloze_text.unwrap_or(sentence.cloze_text);
    let cloze_answer = req.cloze_answer.unwrap_or(sentence.cloze_answer);
    let surface_form = req.surface_form.unwrap_or(sentence.surface_form);
    let is_primary = req.is_primary.unwrap_or(sentence.is_primary);

    // If setting as primary, unset existing primary on same card
    if is_primary && !sentence.is_primary {
        sqlx::query("UPDATE sentences SET is_primary = false WHERE card_id = $1 AND is_primary = true")
            .bind(sentence.card_id)
            .execute(&state.db)
            .await?;
    }

    let updated = sqlx::query_as::<_, Sentence>(
        r#"UPDATE sentences SET text = $1, cloze_text = $2, cloze_answer = $3, surface_form = $4, is_primary = $5
        WHERE id = $6 RETURNING *"#,
    )
    .bind(&text)
    .bind(&cloze_text)
    .bind(&cloze_answer)
    .bind(&surface_form)
    .bind(is_primary)
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(updated.into()))
}

pub async fn delete_sentence(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    // Verify ownership
    let exists = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM sentences s
        JOIN cards c ON s.card_id = c.id
        JOIN decks d ON c.deck_id = d.id
        WHERE s.id = $1 AND d.user_id = $2"#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if exists == 0 {
        return Err(AppError::NotFound("Sentence not found".to_string()));
    }

    sqlx::query("DELETE FROM sentences WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}
