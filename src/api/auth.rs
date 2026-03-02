use axum::{extract::State, Extension, Json};
use std::sync::Arc;

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
    let user = sqlx::query_as::<_, crate::models::User>(
        "SELECT * FROM users WHERE id = $1",
    )
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("User not found".to_string()))?;

    Ok(Json(user.into()))
}
