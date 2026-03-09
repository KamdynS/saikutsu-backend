use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::{error::AppError, AppState, JwksCache};

/// JWKS cache TTL — keys are refreshed after this duration
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub email: Option<String>,
    pub role: Option<String>,
    pub aud: Option<String>,
    pub exp: i64,
    pub iat: i64,
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

#[derive(Debug, Deserialize)]
struct JwkKey {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    kid: Option<String>,
}

/// Fetch JWKS keys from Supabase. Uses our app's reqwest (with rustls).
pub async fn fetch_jwks_keys(
    jwks_url: &str,
) -> anyhow::Result<Vec<(Option<String>, DecodingKey)>> {
    let response = reqwest::get(jwks_url).await?;
    let jwks: JwksResponse = response.json().await?;

    let mut keys = Vec::new();
    for key in &jwks.keys {
        if key.kty == "EC" {
            if let (Some(x), Some(y)) = (&key.x, &key.y) {
                match DecodingKey::from_ec_components(x, y) {
                    Ok(dk) => {
                        tracing::info!("Loaded EC key kid={:?}", key.kid);
                        keys.push((key.kid.clone(), dk));
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load EC key: {}", e);
                    }
                }
            }
        }
    }

    Ok(keys)
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

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    let header = decode_header(token).map_err(|e| {
        tracing::debug!("JWT header decode failed: {}", e);
        AppError::Unauthorized
    })?;

    let claims = match header.alg {
        Algorithm::ES256 => {
            let supabase_url = state.config.supabase_url.as_deref().ok_or_else(|| {
                tracing::error!("SUPABASE_URL is required for ES256 JWT validation");
                AppError::Unauthorized
            })?;

            let jwks_url = format!("{}/auth/v1/.well-known/jwks.json", supabase_url.trim_end_matches('/'));

            // Read cached keys; if missing or expired, fetch new ones
            {
                let cache = state.jwks_cache.read().await;
                if cache.is_none() {
                    drop(cache);
                    // First fetch — must block
                    tracing::info!("Fetching JWKS from {}", jwks_url);
                    let new_keys = fetch_jwks_keys(&jwks_url).await.map_err(|e| {
                        tracing::error!("Failed to fetch JWKS: {}", e);
                        AppError::Unauthorized
                    })?;
                    let mut w = state.jwks_cache.write().await;
                    *w = Some(JwksCache { keys: new_keys, fetched_at: Instant::now() });
                } else if cache.as_ref().unwrap().fetched_at.elapsed() > JWKS_CACHE_TTL {
                    // Cache expired — spawn background refresh, use stale keys for this request
                    let url = jwks_url.clone();
                    let state_clone = state.clone();
                    tokio::spawn(async move {
                        tracing::info!("Refreshing JWKS in background from {}", url);
                        match fetch_jwks_keys(&url).await {
                            Ok(new_keys) => {
                                let mut w = state_clone.jwks_cache.write().await;
                                *w = Some(JwksCache { keys: new_keys, fetched_at: Instant::now() });
                                tracing::info!("JWKS background refresh succeeded");
                            }
                            Err(e) => {
                                tracing::warn!("JWKS background refresh failed: {}", e);
                            }
                        }
                    });
                }
            }

            let cache = state.jwks_cache.read().await;
            let keys = cache.as_ref().map(|c| &c.keys).ok_or_else(|| {
                tracing::error!("No JWKS keys available for ES256 validation");
                AppError::Unauthorized
            })?;

            if keys.is_empty() {
                tracing::error!("No JWKS keys available for ES256 validation");
                return Err(AppError::Unauthorized);
            }

            let mut validation = Validation::new(Algorithm::ES256);
            validation.set_audience(&["authenticated"]);

            let kid = header.kid.as_deref();
            let key = kid
                .and_then(|kid| {
                    keys.iter()
                        .find(|(k, _)| k.as_deref() == Some(kid))
                })
                .or_else(|| keys.first())
                .map(|(_, dk)| dk)
                .ok_or(AppError::Unauthorized)?;

            decode::<Claims>(token, key, &validation)
                .map_err(|e| {
                    tracing::debug!("ES256 JWT validation failed: {}", e);
                    AppError::Unauthorized
                })?
                .claims
        }
        Algorithm::HS256 => {
            let secret = &state.config.supabase_jwt_secret;
            if secret.is_empty() {
                tracing::error!("SUPABASE_JWT_SECRET is not configured for HS256");
                return Err(AppError::Unauthorized);
            }

            let mut validation = Validation::new(Algorithm::HS256);
            validation.set_audience(&["authenticated"]);

            decode::<Claims>(
                token,
                &DecodingKey::from_secret(secret.as_bytes()),
                &validation,
            )
            .map_err(|e| {
                tracing::debug!("HS256 JWT validation failed: {}", e);
                AppError::Unauthorized
            })?
            .claims
        }
        other => {
            tracing::debug!("Unsupported JWT algorithm: {:?}", other);
            return Err(AppError::Unauthorized);
        }
    };

    if claims.role.as_deref() != Some("authenticated") {
        return Err(AppError::Forbidden);
    }

    let auth_user = AuthUser {
        user_id: claims.sub,
    };
    request.extensions_mut().insert(auth_user);

    Ok(next.run(request).await)
}
