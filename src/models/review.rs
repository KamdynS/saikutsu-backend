use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Review {
    pub id: Uuid,
    pub card_state_id: Uuid,
    pub rating: i16,
    pub reviewed_at: DateTime<Utc>,
    pub time_taken_ms: Option<i32>,
    pub scheduled_days: Option<f32>,
    pub elapsed_days: Option<f32>,
    pub difficulty_before: Option<f32>,
    pub stability_before: Option<f32>,
    pub difficulty_after: Option<f32>,
    pub stability_after: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitReviewRequest {
    pub card_id: Uuid,
    pub rating: i16,
    pub time_taken_ms: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct ReviewResponse {
    pub id: Uuid,
    pub card_id: Uuid,
    pub rating: i16,
    pub reviewed_at: DateTime<Utc>,
    pub new_state: CardStateSnapshot,
}

#[derive(Debug, Serialize)]
pub struct CardStateSnapshot {
    pub status: String,
    pub difficulty: f32,
    pub stability: f32,
    pub due_date: Option<NaiveDate>,
    pub reps: i32,
    pub lapses: i32,
}

#[derive(Debug, Serialize)]
pub struct ReviewQueueItem {
    pub card_id: Uuid,
    pub deck_id: Uuid,
    pub deck_name: String,
    pub lemma: String,
    pub reading: Option<String>,
    pub definition: String,
    pub status: String,
    pub due_date: Option<NaiveDate>,
    pub sentence: Option<SentenceForReview>,
}

#[derive(Debug, Serialize)]
pub struct SentenceForReview {
    pub text: String,
    pub cloze_text: String,
    pub cloze_answer: String,
    pub surface_form: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewQueueResponse {
    pub items: Vec<ReviewQueueItem>,
    pub total_due: i64,
    pub total_new: i64,
    pub total_learning: i64,
}
