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
    models::{Card, CardListResponse, CardResponse, CardState, CreateCardRequest, Sentence, UpdateCardRequest},
    processing::normalization::normalize_lemma,
    services::known_words_service,
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct ListCardsQuery {
    pub page: Option<i32>,
    pub per_page: Option<i32>,
    pub status: Option<String>,
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

    // Get cards and total count concurrently
    let db_start = Instant::now();
    let (cards, total) = tokio::try_join!(
        sqlx::query_as::<_, Card>(
            "SELECT * FROM cards WHERE deck_id = $1 ORDER BY frequency_rank ASC NULLS LAST, created_at ASC LIMIT $2 OFFSET $3",
        )
        .bind(deck_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db),
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cards WHERE deck_id = $1")
            .bind(deck_id)
            .fetch_one(&state.db),
    )?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards_fetched = cards.len(), "cards::list fetch cards+count");

    // Batch fetch card_states and sentences for all cards at once (avoids N+1)
    let card_ids: Vec<Uuid> = cards.iter().map(|c| c.id).collect();

    let db_start = Instant::now();
    let (card_states, sentences) = tokio::try_join!(
        sqlx::query_as::<_, CardState>(
            "SELECT * FROM card_states WHERE card_id = ANY($1) AND user_id = $2",
        )
        .bind(&card_ids)
        .bind(auth_user.user_id)
        .fetch_all(&state.db),
        sqlx::query_as::<_, Sentence>(
            "SELECT * FROM sentences WHERE card_id = ANY($1) ORDER BY is_primary DESC, created_at ASC",
        )
        .bind(&card_ids)
        .fetch_all(&state.db),
    )?;
    tracing::info!(
        duration_ms = db_start.elapsed().as_millis() as u64,
        states = card_states.len(),
        sentences = sentences.len(),
        "cards::list fetch states+sentences"
    );

    // Index by card_id for O(1) lookups
    let mut states_by_card: HashMap<Uuid, CardState> = HashMap::with_capacity(card_states.len());
    for cs in card_states {
        states_by_card.insert(cs.card_id, cs);
    }

    let mut sentences_by_card: HashMap<Uuid, Vec<Sentence>> = HashMap::with_capacity(cards.len());
    for s in sentences {
        sentences_by_card.entry(s.card_id).or_default().push(s);
    }

    let card_responses: Vec<CardResponse> = cards
        .into_iter()
        .map(|card| {
            let card_id = card.id;
            CardResponse {
                id: card.id,
                deck_id: card.deck_id,
                lemma: card.lemma,
                reading: card.reading,
                definition: card.definition,
                part_of_speech: card.part_of_speech,
                frequency_rank: card.frequency_rank,
                audio_url: card.audio_url,
                notes: card.notes,
                tags: card.tags,
                sentences: sentences_by_card
                    .remove(&card_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.into())
                    .collect(),
                state: states_by_card.remove(&card_id).map(|s| s.into()),
            }
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

    // Create card state for the user
    let db_start = Instant::now();
    sqlx::query(
        "INSERT INTO card_states (user_id, card_id, status) VALUES ($1, $2, 'new')",
    )
    .bind(auth_user.user_id)
    .bind(card.id)
    .execute(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create insert card_state");

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
            INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, is_primary)
            VALUES ($1, $2, $3, $4, true)
            RETURNING *
            "#,
        )
        .bind(card.id)
        .bind(sentence_text)
        .bind(&cloze_text)
        .bind(&req.lemma)
        .fetch_one(&state.db)
        .await?;
        tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::create insert sentence");
        vec![sentence.into()]
    } else {
        vec![]
    };

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::create total");

    Ok(Json(CardResponse {
        id: card.id,
        deck_id: card.deck_id,
        lemma: card.lemma,
        reading: card.reading,
        definition: card.definition,
        part_of_speech: card.part_of_speech,
        frequency_rank: card.frequency_rank,
        audio_url: card.audio_url,
        notes: card.notes,
        tags: card.tags,
        sentences,
        state: None,
    }))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<CardResponse>> {
    let start = Instant::now();

    // Single query with JOIN to verify ownership
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

    // Fetch state and sentences concurrently
    let db_start = Instant::now();
    let (state_result, sentences) = tokio::try_join!(
        sqlx::query_as::<_, CardState>(
            "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
        )
        .bind(card.id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db),
        sqlx::query_as::<_, Sentence>(
            "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC, created_at ASC",
        )
        .bind(card.id)
        .fetch_all(&state.db),
    )?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::get fetch state+sentences");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::get total");

    Ok(Json(CardResponse {
        id: card.id,
        deck_id: card.deck_id,
        lemma: card.lemma,
        reading: card.reading,
        definition: card.definition,
        part_of_speech: card.part_of_speech,
        frequency_rank: card.frequency_rank,
        audio_url: card.audio_url,
        notes: card.notes,
        tags: card.tags,
        sentences: sentences.into_iter().map(|s| s.into()).collect(),
        state: state_result.map(|s| s.into()),
    }))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<UpdateCardRequest>,
) -> AppResult<Json<CardResponse>> {
    let start = Instant::now();

    // Get card with ownership check in one query
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

    let notes = req.notes.or(card.notes);
    let tags = req.tags.unwrap_or(card.tags);

    // Update and fetch related data concurrently
    let db_start = Instant::now();
    let updated_card = sqlx::query_as::<_, Card>(
        "UPDATE cards SET notes = $1, tags = $2, updated_at = NOW() WHERE id = $3 RETURNING *",
    )
    .bind(&notes)
    .bind(&tags)
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::update db update");

    let db_start = Instant::now();
    let (state_result, sentences) = tokio::try_join!(
        sqlx::query_as::<_, CardState>(
            "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
        )
        .bind(updated_card.id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db),
        sqlx::query_as::<_, Sentence>(
            "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC",
        )
        .bind(updated_card.id)
        .fetch_all(&state.db),
    )?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::update fetch state+sentences");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::update total");

    Ok(Json(CardResponse {
        id: updated_card.id,
        deck_id: updated_card.deck_id,
        lemma: updated_card.lemma,
        reading: updated_card.reading,
        definition: updated_card.definition,
        part_of_speech: updated_card.part_of_speech,
        frequency_rank: updated_card.frequency_rank,
        audio_url: updated_card.audio_url,
        notes: updated_card.notes,
        tags: updated_card.tags,
        sentences: sentences.into_iter().map(|s| s.into()).collect(),
        state: state_result.map(|s| s.into()),
    }))
}

pub async fn suspend(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let result = sqlx::query(
        r#"
        UPDATE card_states SET suspended = true, suspended_at = NOW()
        WHERE card_id = $1 AND user_id = $2
          AND EXISTS (SELECT 1 FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2)
        "#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .execute(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::suspend db query");

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::suspend total");

    Ok(Json(serde_json::json!({ "message": "Card suspended" })))
}

pub async fn reset(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let result = sqlx::query(
        r#"
        UPDATE card_states
        SET status = 'new', difficulty = 0, stability = 0, due_date = NULL,
            last_review = NULL, reps = 0, lapses = 0, suspended = false, suspended_at = NULL
        WHERE card_id = $1 AND user_id = $2
          AND EXISTS (SELECT 1 FROM cards c JOIN decks d ON c.deck_id = d.id WHERE c.id = $1 AND d.user_id = $2)
        "#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .execute(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "cards::reset db query");

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "cards::reset total");

    Ok(Json(serde_json::json!({ "message": "Card reset" })))
}
