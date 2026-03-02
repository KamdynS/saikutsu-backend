use axum::{extract::State, Extension, Json};
use std::sync::Arc;

use crate::{
    api::AuthUser,
    error::{AppError, AppResult},
    models::{User, UserResponse},
    AppState,
};

pub async fn get_me(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<UserResponse>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("User not found".to_string()))?;

    Ok(Json(user.into()))
}

pub async fn update_me(
    State(_state): State<Arc<AppState>>,
    Extension(_auth_user): Extension<AuthUser>,
    Json(_req): Json<serde_json::Value>,
) -> AppResult<Json<UserResponse>> {
    // TODO: Implement user profile update
    Err(AppError::Internal(anyhow::anyhow!("Not implemented")))
}
