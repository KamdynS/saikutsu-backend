use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::{error::AppError, AppState};

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
    let status = response.status();
    let body = response.text().await?;
    tracing::info!("JWKS response status={}, body_len={}, body={}", status, body.len(), &body[..body.len().min(500)]);

    let jwks: JwksResponse = serde_json::from_str(&body)?;
    tracing::info!("Parsed {} keys from JWKS", jwks.keys.len());

    let mut keys = Vec::new();
    for key in &jwks.keys {
        tracing::info!("JWKS key: kty={}, kid={:?}, has_x={}, has_y={}", key.kty, key.kid, key.x.is_some(), key.y.is_some());
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
            if state.jwks_keys.is_empty() {
                tracing::error!("No JWKS keys available for ES256 validation");
                return Err(AppError::Unauthorized);
            }

            let mut validation = Validation::new(Algorithm::ES256);
            validation.set_audience(&["authenticated"]);

            let kid = header.kid.as_deref();
            let key = kid
                .and_then(|kid| {
                    state
                        .jwks_keys
                        .iter()
                        .find(|(k, _)| k.as_deref() == Some(kid))
                })
                .or_else(|| state.jwks_keys.first())
                .map(|(_, dk)| dk)
                .ok_or(AppError::Unauthorized)?;

            decode::<Claims>(token, key, &validation)
                .map_err(|e| {
                    tracing::debug!("ES256 JWT validation failed: {}", e);
                    AppError::Unauthorized
                })?
                .claims
        }
        _ => {
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
