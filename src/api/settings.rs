use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    processing::encryption,
    AppState,
};

#[derive(Debug, Serialize)]
pub struct ApiKeyStatus {
    pub provider: String,
    pub configured: bool,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetApiKeyRequest {
    pub api_key: String,
}

/// List which API key providers the user has configured (without revealing the keys).
pub async fn list_api_keys(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<Vec<ApiKeyStatus>>> {
    let rows = sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>)>(
        "SELECT provider, updated_at FROM user_api_keys WHERE user_id = $1",
    )
    .bind(auth_user.user_id)
    .fetch_all(&state.db)
    .await?;

    // Always include jimaku in the list
    let mut statuses = vec![ApiKeyStatus {
        provider: "jimaku".to_string(),
        configured: false,
        updated_at: None,
    }];

    for (provider, updated_at) in rows {
        if let Some(status) = statuses.iter_mut().find(|s| s.provider == provider) {
            status.configured = true;
            status.updated_at = Some(updated_at.to_rfc3339());
        }
    }

    Ok(Json(statuses))
}

/// Store an encrypted API key for a provider.
pub async fn set_api_key(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(provider): Path<String>,
    Json(req): Json<SetApiKeyRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let valid_providers = ["jimaku"];
    if !valid_providers.contains(&provider.as_str()) {
        return Err(AppError::BadRequest(format!("Unknown provider: {}", provider)));
    }

    if req.api_key.trim().is_empty() {
        return Err(AppError::BadRequest("API key cannot be empty".to_string()));
    }

    let encryption_key = state.config.encryption_key.as_deref().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("ENCRYPTION_KEY is not configured on the server"))
    })?;

    let encrypted = encryption::encrypt_api_key(&req.api_key, encryption_key)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Encryption failed: {}", e)))?;

    sqlx::query(
        r#"
        INSERT INTO user_api_keys (user_id, provider, encrypted_key, updated_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (user_id, provider)
        DO UPDATE SET encrypted_key = $3, updated_at = NOW()
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&provider)
    .bind(&encrypted)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "status": "saved" })))
}

/// Delete a stored API key.
pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(provider): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM user_api_keys WHERE user_id = $1 AND provider = $2")
        .bind(auth_user.user_id)
        .bind(&provider)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "status": "deleted" })))
}

/// Helper: retrieve and decrypt a user's API key for a given provider.
pub async fn get_decrypted_key(
    db: &sqlx::PgPool,
    user_id: uuid::Uuid,
    provider: &str,
    encryption_key: &str,
) -> AppResult<String> {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT encrypted_key FROM user_api_keys WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        AppError::BadRequest(format!(
            "No {} API key configured. Please add your key in Settings.",
            provider
        ))
    })?;

    encryption::decrypt_api_key(&row.0, encryption_key)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to decrypt API key: {}", e)))
}
