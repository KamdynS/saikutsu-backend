use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct Upload {
    pub id: Uuid,
    pub user_id: Uuid,
    pub deck_id: Option<Uuid>,
    pub filename: String,
    pub file_size_bytes: Option<i64>,
    pub storage_key: Option<String>,
    pub language: String,
    pub detected_language: Option<String>,
    pub page_count: Option<i32>,
    pub word_count: Option<i32>,
    pub unique_words: Option<i32>,
    pub status: String,
    pub progress: Option<i32>,
    pub error_message: Option<String>,
    pub processing_started_at: Option<DateTime<Utc>>,
    pub processing_completed_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResponse {
    pub id: Uuid,
    pub filename: String,
    pub language: String,
    pub status: String,
    pub progress: i32,
    pub error_message: Option<String>,
    pub deck_id: Option<Uuid>,
    pub page_count: Option<i32>,
    pub word_count: Option<i32>,
    pub unique_words: Option<i32>,
    pub created_at: DateTime<Utc>,
}

impl From<Upload> for UploadResponse {
    fn from(upload: Upload) -> Self {
        Self {
            id: upload.id,
            filename: upload.filename,
            language: upload.language,
            status: upload.status,
            progress: upload.progress.unwrap_or(0),
            error_message: upload.error_message,
            deck_id: upload.deck_id,
            page_count: upload.page_count,
            word_count: upload.word_count,
            unique_words: upload.unique_words,
            created_at: upload.created_at.unwrap_or_else(Utc::now),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadPreview {
    pub id: Uuid,
    pub filename: String,
    pub language: String,
    pub page_count: i32,
    pub word_count: i32,
    pub unique_words: i32,
    pub words: Vec<PreviewWord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewWord {
    pub lemma: String,
    pub reading: Option<String>,
    pub definition: Option<String>,
    pub frequency_rank: Option<i32>,
    pub doc_count: i32,
    pub selected: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateUploadRequest {
    pub filename: String,
    pub language: String,
}

#[derive(Debug, Deserialize)]
pub struct FinalizeUploadRequest {
    pub deck_name: String,
    pub selected_words: Vec<String>,
}
