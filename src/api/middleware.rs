use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::{error::AppError, AppState};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let jwks_cache = state.jwks_cache.as_ref().ok_or_else(|| {
        tracing::error!("SUPABASE_URL is not configured — cannot validate JWTs");
        AppError::Unauthorized
    })?;

    let claims = supabase_jwt::Claims::from_bearer_token(auth_header, jwks_cache)
        .await
        .map_err(|e| {
            tracing::debug!("JWT validation failed: {}", e);
            AppError::Unauthorized
        })?;

    let user_id: Uuid = claims.sub.parse().map_err(|_| {
        tracing::debug!("JWT sub is not a valid UUID: {}", claims.sub);
        AppError::Unauthorized
    })?;

    if claims.role() != "authenticated" {
        return Err(AppError::Forbidden);
    }

    let auth_user = AuthUser { user_id };
    request.extensions_mut().insert(auth_user);

    Ok(next.run(request).await)
}
