use axum::{
    extract::{Path, State},
    Extension, Json,
};
use std::sync::Arc;
use std::time::Instant;
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
    let start = Instant::now();

    let db_start = Instant::now();
    let decks = sqlx::query_as::<_, Deck>(
        "SELECT * FROM decks WHERE user_id = $1 ORDER BY updated_at DESC",
    )
    .bind(auth_user.user_id)
    .fetch_all(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::list db query");

    let total = decks.len() as i64;
    let deck_responses: Vec<DeckResponse> = decks.into_iter().map(|d| d.into()).collect();

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, total = total, "decks::list total");

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
    let start = Instant::now();

    let db_start = Instant::now();
    let deck = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Deck not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::get db query");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "decks::get total");

    Ok(Json(deck.into()))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<CreateDeckRequest>,
) -> AppResult<Json<DeckResponse>> {
    let start = Instant::now();
    let deck_id = Uuid::new_v4();
    let settings = serde_json::json!({
        "new_cards_per_day": 20,
        "study_mode": "cloze"
    });

    let valid_languages = ["ja", "es", "fr", "de", "it", "pt"];
    let language = req
        .language
        .as_deref()
        .filter(|l| valid_languages.contains(l))
        .unwrap_or("ja");

    let db_start = Instant::now();
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
    .bind(language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::create db insert");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "decks::create total");

    Ok(Json(deck.into()))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<UpdateDeckRequest>,
) -> AppResult<Json<DeckResponse>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let existing = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Deck not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::update ownership check");

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.or(existing.description);
    let settings = req.settings.unwrap_or(existing.settings);

    let db_start = Instant::now();
    let deck = sqlx::query_as::<_, Deck>(
        r#"
        UPDATE decks
        SET name = $1, description = $2, settings = $3, updated_at = NOW()
        WHERE id = $4 AND user_id = $5
        RETURNING *
        "#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&settings)
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::update db update");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "decks::update total");

    Ok(Json(deck.into()))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<serde_json::Value>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let result = sqlx::query("DELETE FROM decks WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.user_id)
        .execute(&state.db)
        .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "decks::delete db query");

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Deck not found".to_string()));
    }

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "decks::delete total");

    Ok(Json(serde_json::json!({ "message": "Deck deleted" })))
}
