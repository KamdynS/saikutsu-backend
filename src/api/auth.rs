use axum::{extract::State, Extension, Json};
use std::sync::Arc;
use std::time::Instant;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::UserResponse,
    AppState,
};

pub async fn me(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<UserResponse>> {
    let start = Instant::now();

    let db_start = Instant::now();
    let user = sqlx::query_as::<_, crate::models::User>(
        "SELECT * FROM users WHERE id = $1",
    )
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("User not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "auth::me db query");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "auth::me total");

    Ok(Json(user.into()))
}
