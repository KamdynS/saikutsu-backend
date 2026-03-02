use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{Card, CardListResponse, CardResponse, CardState, CreateCardRequest, Sentence, UpdateCardRequest},
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

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(50).clamp(1, 100);
    let offset = (page - 1) * per_page;

    // Get cards
    let cards = sqlx::query_as::<_, Card>(
        "SELECT * FROM cards WHERE deck_id = $1 ORDER BY frequency_rank ASC NULLS LAST, created_at ASC LIMIT $2 OFFSET $3",
    )
    .bind(deck_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    // Get total count
    let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cards WHERE deck_id = $1")
        .bind(deck_id)
        .fetch_one(&state.db)
        .await?;

    // Get card states and sentences for each card
    let mut card_responses = Vec::new();
    for card in cards {
        let state_result = sqlx::query_as::<_, CardState>(
            "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
        )
        .bind(card.id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?;

        let sentences = sqlx::query_as::<_, Sentence>(
            "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC, created_at ASC",
        )
        .bind(card.id)
        .fetch_all(&state.db)
        .await?;

        card_responses.push(CardResponse {
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
        });
    }

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

    // Create the card
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

    // Create card state for the user
    sqlx::query(
        "INSERT INTO card_states (user_id, card_id, status) VALUES ($1, $2, 'new')",
    )
    .bind(auth_user.user_id)
    .bind(card.id)
    .execute(&state.db)
    .await?;

    // If a sentence was provided, create it
    let sentences = if let Some(sentence_text) = &req.sentence {
        let cloze_text = sentence_text.replace(&req.lemma, "[...]");
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
        vec![sentence.into()]
    } else {
        vec![]
    };

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
    let card = sqlx::query_as::<_, Card>("SELECT * FROM cards WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Card not found".to_string()))?;

    // Verify ownership through deck
    let deck_owned = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(card.deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if deck_owned == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    let state_result = sqlx::query_as::<_, CardState>(
        "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
    )
    .bind(card.id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?;

    let sentences = sqlx::query_as::<_, Sentence>(
        "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC, created_at ASC",
    )
    .bind(card.id)
    .fetch_all(&state.db)
    .await?;

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
    // Get card and verify ownership
    let card = sqlx::query_as::<_, Card>("SELECT * FROM cards WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Card not found".to_string()))?;

    let deck_owned = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(card.deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if deck_owned == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    let notes = req.notes.or(card.notes.clone());
    let tags = req.tags.unwrap_or(card.tags.clone());

    let updated_card = sqlx::query_as::<_, Card>(
        "UPDATE cards SET notes = $1, tags = $2, updated_at = NOW() WHERE id = $3 RETURNING *",
    )
    .bind(&notes)
    .bind(&tags)
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    let state_result = sqlx::query_as::<_, CardState>(
        "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
    )
    .bind(updated_card.id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?;

    let sentences = sqlx::query_as::<_, Sentence>(
        "SELECT * FROM sentences WHERE card_id = $1 ORDER BY is_primary DESC",
    )
    .bind(updated_card.id)
    .fetch_all(&state.db)
    .await?;

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
    // Verify ownership
    let card = sqlx::query_as::<_, Card>("SELECT * FROM cards WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Card not found".to_string()))?;

    let deck_owned = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(card.deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if deck_owned == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    sqlx::query(
        "UPDATE card_states SET suspended = true, suspended_at = NOW() WHERE card_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(auth_user.user_id)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "message": "Card suspended" })))
}

pub async fn reset(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    // Verify ownership
    let card = sqlx::query_as::<_, Card>("SELECT * FROM cards WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Card not found".to_string()))?;

    let deck_owned = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM decks WHERE id = $1 AND user_id = $2",
    )
    .bind(card.deck_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    if deck_owned == 0 {
        return Err(AppError::NotFound("Card not found".to_string()));
    }

    sqlx::query(
        r#"
        UPDATE card_states
        SET status = 'new', difficulty = 0, stability = 0, due_date = NULL,
            last_review = NULL, reps = 0, lapses = 0, suspended = false, suspended_at = NULL
        WHERE card_id = $1 AND user_id = $2
        "#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "message": "Card reset" })))
}
