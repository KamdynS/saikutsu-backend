pub mod api;
pub mod config;
pub mod db;
pub mod error;
pub mod fsrs;
pub mod models;
pub mod processing;
pub mod services;

pub use config::Config;
pub use error::{AppError, AppResult};

use std::sync::Arc;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Config,
    /// Cached JWKS decoding keys fetched at startup
    pub jwks_keys: Vec<(Option<String>, jsonwebtoken::DecodingKey)>,
}

pub type SharedAppState = Arc<AppState>;
