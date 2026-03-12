pub mod api;
pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod processing;
pub mod services;

pub use config::Config;
pub use error::{AppError, AppResult};

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, Semaphore};

pub struct JwksCache {
    pub keys: Vec<(Option<String>, jsonwebtoken::DecodingKey)>,
    pub fetched_at: Instant,
}

pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Config,
    pub http_client: reqwest::Client,
    /// Cached JWKS decoding keys with TTL (refreshed after 1 hour)
    pub jwks_cache: RwLock<Option<JwksCache>>,
    /// Rate limiter for Jimaku API — 1 concurrent download stream at a time
    pub jimaku_semaphore: Semaphore,
    /// Rate limiter for OpenSubtitles API — 1 concurrent download stream at a time
    pub opensub_semaphore: Semaphore,
}

pub type SharedAppState = Arc<AppState>;
