use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{Card, CreateDeckRequest, Deck, DeckListResponse, DeckResponse, Sentence, UpdateDeckRequest},
    AppState,
};

#[derive(Debug, Deserialize)]
struct SeedWord {
    lemma: String,
    definition: String,
    sentence: String,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<DeckListResponse>> {
    let decks = sqlx::query_as::<_, Deck>(
        "SELECT * FROM decks WHERE user_id = $1 ORDER BY updated_at DESC",
    )
    .bind(auth_user.user_id)
    .fetch_all(&state.db)
    .await?;

    let total = decks.len() as i64;
    let deck_responses: Vec<DeckResponse> = decks.into_iter().map(|d| d.into()).collect();

    Ok(Json(DeckListResponse {
        decks: deck_responses,
        total,
    }))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<DeckResponse>> {
    let deck = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Deck not found".to_string()))?;

    Ok(Json(deck.into()))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckRequest>,
) -> AppResult<Json<DeckResponse>> {
    // Validate language
    let valid_languages = ["ja", "es", "fr", "de", "it", "pt"];
    if !valid_languages.contains(&req.language.as_str()) {
        return Err(AppError::Validation(format!(
            "Invalid language. Supported: {}",
            valid_languages.join(", ")
        )));
    }

    let deck_id = Uuid::new_v4();
    let settings = serde_json::json!({
        "new_cards_per_day": 20,
        "study_mode": "cloze"
    });

    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (id, user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, $4, $5, 'import', $6)
        RETURNING *
        "#,
    )
    .bind(deck_id)
    .bind(auth_user.user_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(deck.into()))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<UpdateDeckRequest>,
) -> AppResult<Json<DeckResponse>> {
    // First check ownership
    let existing = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Deck not found".to_string()))?;

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.or(existing.description);
    let settings = req.settings.unwrap_or(existing.settings);

    let deck = sqlx::query_as::<_, Deck>(
        r#"
        UPDATE decks
        SET name = $1, description = $2, settings = $3, updated_at = NOW()
        WHERE id = $4
        RETURNING *
        "#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&settings)
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(deck.into()))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    let result = sqlx::query("DELETE FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Deck not found".to_string()));
    }

    Ok(Json(serde_json::json!({ "message": "Deck deleted" })))
}

const SPANISH_500_DATA: &str = include_str!("../../data/spanish_500.json");
const JAPANESE_500_DATA: &str = include_str!("../../data/japanese_500.json");
const FRENCH_500_DATA: &str = include_str!("../../data/french_500.json");
const GERMAN_500_DATA: &str = include_str!("../../data/german_500.json");
const ITALIAN_500_DATA: &str = include_str!("../../data/italian_500.json");
const PORTUGUESE_500_DATA: &str = include_str!("../../data/portuguese_500.json");

#[derive(Debug, Deserialize)]
pub struct FrequencyDeckRequest {
    pub language: String,
    pub word_count: Option<i32>,
}

pub async fn start_frequency(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<FrequencyDeckRequest>,
) -> AppResult<Json<DeckResponse>> {
    let (seed_data, lang_name) = match req.language.as_str() {
        "es" => (SPANISH_500_DATA, "Spanish"),
        "ja" => (JAPANESE_500_DATA, "Japanese"),
        "fr" => (FRENCH_500_DATA, "French"),
        "de" => (GERMAN_500_DATA, "German"),
        "it" => (ITALIAN_500_DATA, "Italian"),
        "pt" => (PORTUGUESE_500_DATA, "Portuguese"),
        _ => {
            return Err(AppError::BadRequest(
                "Frequency decks are available for: ja, es, fr, de, it, pt".to_string(),
            ));
        }
    };

    let word_count = req.word_count.unwrap_or(500).min(500) as usize;

    // Parse the seed data
    let words: Vec<SeedWord> = serde_json::from_str(seed_data)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to parse seed data: {}", e)))?;

    // Create the deck
    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type)
        VALUES ($1, $2, $3, $4, 'frequency')
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(format!("{} Top {}", lang_name, word_count))
    .bind(format!("The {} most common {} words", word_count, lang_name.to_lowercase()))
    .bind(&req.language)
    .fetch_one(&state.db)
    .await?;

    // Create cards for each word
    for (i, word) in words.iter().take(word_count).enumerate() {
        let card = sqlx::query_as::<_, Card>(
            r#"
            INSERT INTO cards (deck_id, lemma, definition, frequency_rank)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(deck.id)
        .bind(&word.lemma)
        .bind(&word.definition)
        .bind((i + 1) as i32)
        .fetch_one(&state.db)
        .await?;

        // Create card state
        sqlx::query(
            "INSERT INTO card_states (user_id, card_id, status) VALUES ($1, $2, 'new')",
        )
        .bind(auth_user.user_id)
        .bind(card.id)
        .execute(&state.db)
        .await?;

        // Create sentence with cloze
        let cloze_text = word.sentence.replace(&word.lemma, "[...]");
        sqlx::query_as::<_, Sentence>(
            r#"
            INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, is_primary)
            VALUES ($1, $2, $3, $4, true)
            RETURNING *
            "#,
        )
        .bind(card.id)
        .bind(&word.sentence)
        .bind(&cloze_text)
        .bind(&word.lemma)
        .fetch_one(&state.db)
        .await?;
    }

    Ok(Json(deck.into()))
}
