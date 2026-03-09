use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Deck {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub language: String,
    pub source_type: String,
    pub card_count: i32,
    pub settings: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeckRequest {
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDeckRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct DeckResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub language: String,
    pub source_type: String,
    pub card_count: i32,
    pub settings: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Deck> for DeckResponse {
    fn from(deck: Deck) -> Self {
        Self {
            id: deck.id,
            name: deck.name,
            description: deck.description,
            language: deck.language,
            source_type: deck.source_type,
            card_count: deck.card_count,
            settings: deck.settings,
            created_at: deck.created_at,
            updated_at: deck.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeckListResponse {
    pub decks: Vec<DeckResponse>,
    pub total: i64,
}
