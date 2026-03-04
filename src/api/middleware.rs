use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;
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

static JWKS_KEYS: OnceCell<Vec<(Option<String>, DecodingKey)>> = OnceCell::const_new();

async fn get_jwks_keys(supabase_url: &str) -> Result<&'static Vec<(Option<String>, DecodingKey)>, AppError> {
    JWKS_KEYS
        .get_or_try_init(|| async {
            // Supabase JWKS endpoint is under /auth/v1/
            let jwks_url = format!(
                "{}/auth/v1/.well-known/jwks.json",
                supabase_url.trim_end_matches('/')
            );
            tracing::info!("Fetching JWKS from {}", jwks_url);

            let response = reqwest::get(&jwks_url).await.map_err(|e| {
                tracing::error!("Failed to fetch JWKS: {}", e);
                AppError::Internal(anyhow::anyhow!("Failed to fetch JWKS: {}", e))
            })?;

            let jwks: JwksResponse = response.json().await.map_err(|e| {
                tracing::error!("Failed to parse JWKS: {}", e);
                AppError::Internal(anyhow::anyhow!("Failed to parse JWKS: {}", e))
            })?;

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

            if keys.is_empty() {
                tracing::warn!("No usable keys found in JWKS response");
            }

            Ok(keys)
        })
        .await
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

            let keys = get_jwks_keys(supabase_url).await?;
            if keys.is_empty() {
                tracing::error!("No JWKS keys available");
                return Err(AppError::Unauthorized);
            }

            let mut validation = Validation::new(Algorithm::ES256);
            validation.set_audience(&["authenticated"]);

            // Match by kid if present, otherwise use first key
            let kid = header.kid.as_deref();
            let key = kid
                .and_then(|kid| keys.iter().find(|(k, _)| k.as_deref() == Some(kid)))
                .or_else(|| keys.first())
                .map(|(_, dk)| dk)
                .unwrap();

            decode::<Claims>(token, key, &validation)
                .map_err(|e| {
                    tracing::debug!("ES256 JWT validation failed: {}", e);
                    AppError::Unauthorized
                })?
                .claims
        }
        _ => {
            // HS256 fallback with shared secret
            let secret = &state.config.supabase_jwt_secret;
            if secret.is_empty() {
                tracing::error!("SUPABASE_JWT_SECRET is not configured");
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
