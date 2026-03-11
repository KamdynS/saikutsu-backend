use chrono::{DateTime, Utc};
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
    pub surface_form: String,
    pub source_page: Option<i32>,
    pub audio_url: Option<String>,
    pub is_primary: bool,
    pub created_at: DateTime<Utc>,
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
    pub doc_frequency: Option<i32>,
    pub audio_url: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub sentences: Vec<SentenceResponse>,
}

#[derive(Debug, Serialize)]
pub struct SentenceResponse {
    pub id: Uuid,
    pub text: String,
    pub cloze_text: String,
    pub cloze_answer: String,
    pub surface_form: String,
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
            surface_form: sentence.surface_form,
            source_page: sentence.source_page,
            audio_url: sentence.audio_url,
            is_primary: sentence.is_primary,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateCardRequest {
    pub lemma: Option<String>,
    pub definition: Option<String>,
    pub reading: Option<Option<String>>,
    pub part_of_speech: Option<Option<String>>,
    pub notes: Option<Option<String>>,
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

#[derive(Debug, Deserialize)]
pub struct CreateSentenceRequest {
    pub text: String,
    pub cloze_text: String,
    pub cloze_answer: String,
    pub surface_form: Option<String>,
    pub source_page: Option<i32>,
    pub is_primary: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSentenceRequest {
    pub text: Option<String>,
    pub cloze_text: Option<String>,
    pub cloze_answer: Option<String>,
    pub surface_form: Option<String>,
    pub is_primary: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CardListResponse {
    pub cards: Vec<CardResponse>,
    pub total: i64,
    pub page: i32,
    pub per_page: i32,
}
