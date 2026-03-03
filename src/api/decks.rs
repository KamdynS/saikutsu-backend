use axum::{
    extract::{Path, State},
    Extension, Json,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{CreateDeckRequest, Deck, DeckListResponse, DeckResponse, UpdateDeckRequest},
    AppState,
};

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

