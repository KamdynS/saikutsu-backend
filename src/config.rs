use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub supabase_jwt_secret: String,
    pub supabase_url: Option<String>,
    pub allowed_origins: Vec<String>,
    pub openai_api_key: Option<String>,
    pub opensubtitles_api_key: Option<String>,
    pub encryption_key: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let supabase_jwt_secret = env::var("SUPABASE_JWT_SECRET").unwrap_or_default();
        let supabase_url = env::var("SUPABASE_URL").ok().filter(|s| !s.is_empty());

        if supabase_jwt_secret.is_empty() && supabase_url.is_none() {
            tracing::warn!(
                "Both SUPABASE_JWT_SECRET and SUPABASE_URL are empty — JWT auth will not work"
            );
        }

        Ok(Self {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgresql://localhost/saikutsu".to_string()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .unwrap_or(8080),
            supabase_jwt_secret,
            supabase_url,
            allowed_origins: env::var("ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
            opensubtitles_api_key: env::var("OPENSUBTITLES_API_KEY").ok().filter(|s| !s.is_empty()),
            encryption_key: env::var("ENCRYPTION_KEY").ok().filter(|s| !s.is_empty()),
        })
    }
}
