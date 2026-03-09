use std::collections::HashMap;
use sqlx::PgPool;
use uuid::Uuid;
use chrono::Utc;

use crate::error::{AppError, AppResult};
use crate::models::{Upload, UploadPreview, PreviewWord};
use crate::processing::{
    pdf::extract_text_from_bytes,
    tokenizers::{JapaneseTokenizer, EuropeanTokenizer, Token},
    frequency::count_lemmas,
    normalization::normalize_lemma,
};
use crate::services::known_words_service;

// Processed data stored temporarily in memory (in production, use Redis)
use std::sync::Mutex;
use once_cell::sync::Lazy;

#[derive(Debug, Clone)]
pub struct ProcessedUpload {
    pub words: Vec<ProcessedWord>,
    pub sentences: HashMap<String, Vec<ExtractedSentence>>,
}

#[derive(Debug, Clone)]
pub struct ProcessedWord {
    pub lemma: String,
    pub reading: Option<String>,
    pub definition: Option<String>,
    pub frequency_rank: Option<i32>,
    pub doc_count: i32,
}

#[derive(Debug, Clone)]
pub struct ExtractedSentence {
    pub text: String,
    pub page: i32,
    pub surface_form: String,
}

static PROCESSED_UPLOADS: Lazy<Mutex<HashMap<Uuid, ProcessedUpload>>> = 
    Lazy::new(|| Mutex::new(HashMap::new()));

pub async fn create_upload(
    pool: &PgPool,
    user_id: Uuid,
    filename: &str,
    language: &str,
) -> AppResult<Upload> {
    let upload = sqlx::query_as::<_, Upload>(
        r#"
        INSERT INTO uploads (user_id, filename, language, status, progress)
        VALUES ($1, $2, $3, 'pending', 0)
        RETURNING *
        "#
    )
    .bind(user_id)
    .bind(filename)
    .bind(language)
    .fetch_one(pool)
    .await?;

    Ok(upload)
}

pub async fn process_pdf(
    pool: &PgPool,
    upload_id: Uuid,
    pdf_bytes: &[u8],
    language: &str,
) -> AppResult<()> {
    // Update status to extracting
    update_upload_status(pool, upload_id, "extracting", 10).await?;

    // Extract text from PDF
    let pages = extract_text_from_bytes(pdf_bytes)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("PDF extraction failed: {}", e)))?;

    let page_count = pages.len() as i32;
    let full_text: String = pages.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n");
    let word_count = full_text.split_whitespace().count() as i32;

    // Update with page info
    sqlx::query(
        "UPDATE uploads SET page_count = $1, word_count = $2 WHERE id = $3"
    )
    .bind(page_count)
    .bind(word_count)
    .bind(upload_id)
    .execute(pool)
    .await?;

    // Update status to tokenizing
    update_upload_status(pool, upload_id, "tokenizing", 30).await?;

    // Tokenize based on language
    let tokens = tokenize_text(&full_text, language)?;

    // Update status to analyzing
    update_upload_status(pool, upload_id, "analyzing", 50).await?;

    // Count lemma frequencies
    let word_counts = count_lemmas(&tokens, language);
    let unique_words = word_counts.len() as i32;

    sqlx::query("UPDATE uploads SET unique_words = $1 WHERE id = $2")
        .bind(unique_words)
        .bind(upload_id)
        .execute(pool)
        .await?;

    // Update status to generating
    update_upload_status(pool, upload_id, "generating", 70).await?;

    // Build processed words with dummy definitions for now
    // In production, load actual dictionary data
    let mut processed_words: Vec<ProcessedWord> = word_counts
        .into_iter()
        .map(|(lemma, wf)| ProcessedWord {
            lemma: lemma.clone(),
            reading: wf.reading,
            definition: Some(format!("[Definition for {}]", lemma)),
            frequency_rank: wf.corpus_rank,
            doc_count: wf.doc_count,
        })
        .collect();

    // Sort by document frequency (most common first)
    processed_words.sort_by(|a, b| b.doc_count.cmp(&a.doc_count));

    // Extract sentences for each word using per-sentence tokenization (avoids substring false positives)
    let mut sentences: HashMap<String, Vec<ExtractedSentence>> = HashMap::new();
    for page in &pages {
        for sentence in extract_sentences(&page.text) {
            let sent_tokens = tokenize_text(&sentence, language)?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for token in &sent_tokens {
                if token.is_content {
                    let norm = normalize_lemma(&token.lemma, language);
                    if seen.insert(norm.clone()) {
                        sentences
                            .entry(norm)
                            .or_default()
                            .push(ExtractedSentence {
                                text: sentence.clone(),
                                page: page.page_num as i32,
                                surface_form: token.surface.clone(),
                            });
                    }
                }
            }
        }
    }

    // Store processed data
    PROCESSED_UPLOADS.lock().unwrap_or_else(|e| e.into_inner()).insert(upload_id, ProcessedUpload {
        words: processed_words,
        sentences,
    });

    // Update status to completed
    update_upload_status(pool, upload_id, "completed", 100).await?;

    sqlx::query("UPDATE uploads SET processing_completed_at = $1 WHERE id = $2")
        .bind(Utc::now())
        .bind(upload_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn get_upload(pool: &PgPool, upload_id: Uuid, user_id: Uuid) -> AppResult<Upload> {
    let upload = sqlx::query_as::<_, Upload>(
        "SELECT * FROM uploads WHERE id = $1 AND user_id = $2"
    )
    .bind(upload_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Upload not found".to_string()))?;

    Ok(upload)
}

pub async fn get_upload_preview(pool: &PgPool, upload_id: Uuid, user_id: Uuid) -> AppResult<UploadPreview> {
    let upload = get_upload(pool, upload_id, user_id).await?;

    if upload.status != "completed" {
        return Err(AppError::BadRequest("Upload not yet processed".to_string()));
    }

    let processed = PROCESSED_UPLOADS.lock().unwrap_or_else(|e| e.into_inner())
        .get(&upload_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound("Processed data not found".to_string()))?;

    let words: Vec<PreviewWord> = processed.words.iter()
        .take(100) // Limit preview to top 100 words
        .map(|w| PreviewWord {
            lemma: w.lemma.clone(),
            reading: w.reading.clone(),
            definition: w.definition.clone(),
            frequency_rank: w.frequency_rank,
            doc_count: w.doc_count,
            selected: true, // Default all words selected
        })
        .collect();

    Ok(UploadPreview {
        id: upload.id,
        filename: upload.filename,
        language: upload.language,
        page_count: upload.page_count.unwrap_or(0),
        word_count: upload.word_count.unwrap_or(0),
        unique_words: upload.unique_words.unwrap_or(0),
        words,
    })
}

pub async fn finalize_upload(
    pool: &PgPool,
    upload_id: Uuid,
    user_id: Uuid,
    deck_name: &str,
    selected_words: &[String],
) -> AppResult<Uuid> {
    let upload = get_upload(pool, upload_id, user_id).await?;

    if upload.status != "completed" {
        return Err(AppError::BadRequest("Upload not yet processed".to_string()));
    }

    let processed = PROCESSED_UPLOADS.lock().unwrap_or_else(|e| e.into_inner())
        .get(&upload_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound("Processed data not found".to_string()))?;

    // Create deck
    let deck = sqlx::query_as::<_, crate::models::Deck>(
        r#"
        INSERT INTO decks (user_id, name, language, source_type)
        VALUES ($1, $2, $3, 'pdf')
        RETURNING *
        "#
    )
    .bind(user_id)
    .bind(deck_name)
    .bind(&upload.language)
    .fetch_one(pool)
    .await?;

    // Filter words based on selection
    let selected_set: std::collections::HashSet<&str> = selected_words.iter().map(|s| s.as_str()).collect();
    let words_to_add: Vec<&ProcessedWord> = processed.words.iter()
        .filter(|w| selected_set.contains(w.lemma.as_str()))
        .collect();

    // Create cards for each selected word
    for word in &words_to_add {
        let card = sqlx::query_as::<_, crate::models::Card>(
            r#"
            INSERT INTO cards (deck_id, lemma, reading, definition, frequency_rank, doc_frequency)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#
        )
        .bind(deck.id)
        .bind(&word.lemma)
        .bind(&word.reading)
        .bind(word.definition.as_deref().unwrap_or(""))
        .bind(word.frequency_rank)
        .bind(word.doc_count)
        .fetch_one(pool)
        .await?;

        // Add sentences (up to 3 per word)
        if let Some(sentences) = processed.sentences.get(&word.lemma) {
            for (i, sentence) in sentences.iter().take(3).enumerate() {
                // Use the surface form (word as it appeared) for cloze, falling back to lemma
                let cloze_word = if sentence.text.contains(&sentence.surface_form) {
                    &sentence.surface_form
                } else {
                    &word.lemma
                };
                let cloze_text = sentence.text.replace(cloze_word, "[...]");
                sqlx::query(
                    r#"
                    INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, surface_form, source_page, is_primary)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#
                )
                .bind(card.id)
                .bind(&sentence.text)
                .bind(&cloze_text)
                .bind(cloze_word)
                .bind(&sentence.surface_form)
                .bind(sentence.page)
                .bind(i == 0) // First sentence is primary
                .execute(pool)
                .await?;
            }
        }
    }

    // Add created lemmas to known_words
    let normalized_lemmas: Vec<String> = words_to_add.iter()
        .map(|w| normalize_lemma(&w.lemma, &upload.language))
        .collect();
    let lemma_refs: Vec<&str> = normalized_lemmas.iter().map(|s| s.as_str()).collect();
    known_words_service::add_known_words(pool, user_id, &upload.language, &lemma_refs).await?;

    // Update upload with deck_id
    sqlx::query("UPDATE uploads SET deck_id = $1 WHERE id = $2")
        .bind(deck.id)
        .bind(upload_id)
        .execute(pool)
        .await?;

    // Clean up processed data
    PROCESSED_UPLOADS.lock().unwrap_or_else(|e| e.into_inner()).remove(&upload_id);

    Ok(deck.id)
}

async fn update_upload_status(pool: &PgPool, upload_id: Uuid, status: &str, progress: i32) -> AppResult<()> {
    sqlx::query("UPDATE uploads SET status = $1, progress = $2 WHERE id = $3")
        .bind(status)
        .bind(progress)
        .bind(upload_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn tokenize_text(text: &str, language: &str) -> AppResult<Vec<Token>> {
    match language {
        "ja" => {
            let tokenizer = JapaneseTokenizer::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Tokenizer init failed: {}", e)))?;
            Ok(tokenizer.get_content_words(text))
        }
        "es" | "fr" | "de" | "it" | "pt" => {
            let tokenizer = EuropeanTokenizer::new(language)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Tokenizer init failed: {}", e)))?;
            Ok(tokenizer.get_content_words(text))
        }
        _ => Err(AppError::BadRequest(format!("Unsupported language: {}", language))),
    }
}

fn extract_sentences(text: &str) -> Vec<String> {
    // Simple sentence extraction - split on sentence-ending punctuation
    let mut sentences = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        current.push(c);
        if c == '。' || c == '.' || c == '!' || c == '?' || c == '？' || c == '！' {
            let trimmed = current.trim();
            if trimmed.len() > 10 && trimmed.len() < 500 {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    sentences
}
