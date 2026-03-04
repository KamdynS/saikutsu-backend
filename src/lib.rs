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

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Config,
    pub jwks_cache: Option<supabase_jwt::JwksCache>,
}

pub type SharedAppState = Arc<AppState>;
