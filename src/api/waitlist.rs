use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{error::{AppError, AppResult}, AppState};

#[derive(Debug, Deserialize)]
pub struct WaitlistRequest {
    pub email: String,
    pub referral_source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WaitlistResponse {
    pub message: String,
}

pub async fn join_waitlist(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WaitlistRequest>,
) -> AppResult<Json<WaitlistResponse>> {
    let email = body.email.trim().to_lowercase();

    // Basic email validation: must contain @ with a dot after it
    if let Some(at_pos) = email.find('@') {
        let domain = &email[at_pos + 1..];
        if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
            return Err(AppError::BadRequest("Invalid email address".to_string()));
        }
    } else {
        return Err(AppError::BadRequest("Invalid email address".to_string()));
    }

    // Upsert so duplicate submissions don't error
    sqlx::query(
        "INSERT INTO waitlist (email, referral_source) VALUES ($1, $2) ON CONFLICT (email) DO NOTHING",
    )
    .bind(&email)
    .bind(&body.referral_source)
    .execute(&state.db)
    .await?;

    Ok(Json(WaitlistResponse {
        message: "You're on the list! We'll email you when spots open up.".to_string(),
    }))
}
