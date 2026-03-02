use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Card {
    pub id: Uuid,
    pub deck_id: Uuid,
    pub lemma: String,
    pub reading: Option<String>,
    pub definition: String,
    pub part_of_speech: Option<String>,
    pub frequency_rank: Option<i32>,
    pub doc_frequency: Option<i32>,
    pub audio_url: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Sentence {
    pub id: Uuid,
    pub card_id: Uuid,
    pub text: String,
    pub cloze_text: String,
    pub cloze_answer: String,
    pub source_page: Option<i32>,
    pub audio_url: Option<String>,
    pub is_primary: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CardState {
    pub id: Uuid,
    pub user_id: Uuid,
    pub card_id: Uuid,
    pub status: String,
    pub difficulty: f32,
    pub stability: f32,
    pub due_date: Option<NaiveDate>,
    pub last_review: Option<DateTime<Utc>>,
    pub reps: i32,
    pub lapses: i32,
    pub suspended: bool,
    pub suspended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CardResponse {
    pub id: Uuid,
    pub deck_id: Uuid,
    pub lemma: String,
    pub reading: Option<String>,
    pub definition: String,
    pub part_of_speech: Option<String>,
    pub frequency_rank: Option<i32>,
    pub audio_url: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub sentences: Vec<SentenceResponse>,
    pub state: Option<CardStateResponse>,
}

#[derive(Debug, Serialize)]
pub struct SentenceResponse {
    pub id: Uuid,
    pub text: String,
    pub cloze_text: String,
    pub cloze_answer: String,
    pub source_page: Option<i32>,
    pub audio_url: Option<String>,
    pub is_primary: bool,
}

impl From<Sentence> for SentenceResponse {
    fn from(sentence: Sentence) -> Self {
        Self {
            id: sentence.id,
            text: sentence.text,
            cloze_text: sentence.cloze_text,
            cloze_answer: sentence.cloze_answer,
            source_page: sentence.source_page,
            audio_url: sentence.audio_url,
            is_primary: sentence.is_primary,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CardStateResponse {
    pub status: String,
    pub difficulty: f32,
    pub stability: f32,
    pub due_date: Option<NaiveDate>,
    pub reps: i32,
    pub lapses: i32,
    pub suspended: bool,
}

impl From<CardState> for CardStateResponse {
    fn from(state: CardState) -> Self {
        Self {
            status: state.status,
            difficulty: state.difficulty,
            stability: state.stability,
            due_date: state.due_date,
            reps: state.reps,
            lapses: state.lapses,
            suspended: state.suspended,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateCardRequest {
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCardRequest {
    pub lemma: String,
    pub definition: String,
    pub reading: Option<String>,
    pub part_of_speech: Option<String>,
    pub sentence: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CardListResponse {
    pub cards: Vec<CardResponse>,
    pub total: i64,
    pub page: i32,
    pub per_page: i32,
}
