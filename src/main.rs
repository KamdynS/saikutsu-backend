use axum::{
    extract::DefaultBodyLimit,
    http::{header, Method},
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use saikutsu::{api, config::Config, db, processing::{dictionary, lemma_dict}, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("debug,sqlx=warn,hyper=info,tower_http=info,reqwest=info")
        .init();

    // Load config
    let config = Config::from_env()?;
    tracing::info!("Configuration loaded");

    // Set up database pool
    let pool = db::create_pool(&config.database_url).await?;
    tracing::info!("Database connection established");

    tracing::info!("Database ready");

    // Load all dictionaries (JMdict for Japanese + Wiktionary for European languages)
    if let Err(e) = dictionary::load_dictionaries() {
        tracing::warn!("Failed to load dictionaries: {}", e);
    }

    // Load lemmatization dictionaries (European languages)
    if let Err(e) = lemma_dict::load_lemma_dictionaries() {
        tracing::warn!("Failed to load lemma dictionaries: {}. European lemmatization will fall back to lowercase forms.", e);
    }

    let state = Arc::new(AppState {
        db: pool,
        config: config.clone(),
        jwks_cache: tokio::sync::RwLock::new(None),
        jimaku_semaphore: tokio::sync::Semaphore::new(1),
        opensub_semaphore: tokio::sync::Semaphore::new(1),
    });

    // Mark any stale subtitle jobs (from previous crashes) as failed
    let _ = sqlx::query(
        "UPDATE subtitle_jobs SET status = 'failed', error_message = 'Server restarted during processing', updated_at = NOW() WHERE status IN ('queued', 'downloading', 'processing')"
    )
    .execute(&state.db)
    .await;

    // Build CORS layer
    let allowed_methods = vec![
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let allowed_headers = vec![
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
    ];

    let cors = if config.allowed_origins.is_empty() {
        CorsLayer::new()
            .allow_origin(AllowOrigin::any())
            .allow_methods(allowed_methods)
            .allow_headers(allowed_headers)
    } else {
        let origins: Vec<_> = config
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(allowed_methods)
            .allow_headers(allowed_headers)
    };

    // Protected routes (require auth)
    let protected = Router::new()
        // User
        .route("/v1/me", get(api::auth::me))
        // Decks
        .route("/v1/decks", get(api::decks::list))
        .route("/v1/decks", post(api::decks::create))
        .route("/v1/decks/{id}", get(api::decks::get))
        .route("/v1/decks/{id}", patch(api::decks::update))
        .route("/v1/decks/{id}", delete(api::decks::delete))
        .route("/v1/decks/{id}/cards", get(api::cards::list))
        .route("/v1/decks/{id}/cards", post(api::cards::create))
        // Cards
        .route("/v1/cards/{id}", get(api::cards::get))
        .route("/v1/cards/{id}", patch(api::cards::update))
        .route("/v1/cards/{id}", delete(api::cards::delete))
        .route("/v1/decks/{id}/cards/bulk-delete", post(api::cards::bulk_delete))
        // Sentences
        .route("/v1/cards/{id}/sentences", post(api::cards::create_sentence))
        .route("/v1/sentences/{id}", patch(api::cards::update_sentence))
        .route("/v1/sentences/{id}", delete(api::cards::delete_sentence))
        // Export
        .route("/v1/decks/{id}/export", post(api::exports::export_apkg))
        // Analysis (endpoints that create decks need auth)
        .route("/v1/decks/from-anki", post(api::imports::import_apkg))
        .route("/v1/imports/preview", post(api::imports::preview_apkg))
        .route("/v1/decks/from-pdf", post(api::analyze::create_deck_from_pdf))
        .route("/v1/decks/from-text", post(api::analyze::create_deck_from_text))
        .route("/v1/decks/from-media", post(api::analyze::create_deck_from_media))
        // Subtitle routes
        .route("/v1/subtitles/search", post(api::subtitles::search))
        .route("/v1/subtitles/files", post(api::subtitles::list_files))
        .route("/v1/decks/from-subtitles", post(api::subtitles::create_deck_from_subtitles))
        .route("/v1/subtitle-jobs/{id}", get(api::subtitles::get_job_status))
        // Settings routes
        .route("/v1/settings/api-keys", get(api::settings::list_api_keys))
        .route("/v1/settings/api-keys/{provider}", put(api::settings::set_api_key))
        .route("/v1/settings/api-keys/{provider}", delete(api::settings::delete_api_key))
        .layer(DefaultBodyLimit::max(500 * 1024 * 1024)) // 500MB for video uploads
        .layer(middleware::from_fn_with_state(
            state.clone(),
            api::middleware::auth_middleware,
        ));

    // Public routes
    // TODO: Add rate limiting to public analyze endpoints to prevent abuse
    let public = Router::new()
        .route("/health", get(health_check))
        // Analysis endpoints (stateless, no user data)
        .route("/v1/analyze", post(api::analyze::analyze_pdf))
        .route("/v1/analyze/text", post(api::analyze::analyze_text))
        // Waitlist (public, no auth)
        .route("/v1/waitlist", post(api::waitlist::join_waitlist))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024)); // 2MB limit for public endpoints

    let app = Router::new()
        .merge(public)
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state);

    // Start server
    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> &'static str {
    "OK"
}
