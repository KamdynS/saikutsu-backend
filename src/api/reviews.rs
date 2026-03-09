use axum::{extract::{Query, State}, Extension, Json};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use crate::{
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    fsrs::{Rating, FSRS},
    models::{
        CardStateSnapshot, ReviewQueueItem, ReviewQueueResponse, ReviewResponse,
        SentenceForReview, SubmitReviewRequest,
    },
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct ReviewQueueQuery {
    pub deck_id: Option<Uuid>,
}

pub async fn get_queue(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ReviewQueueQuery>,
    Extension(auth_user): Extension<AuthUser>,
) -> AppResult<Json<ReviewQueueResponse>> {
    let start = Instant::now();
    let today = Utc::now().date_naive();
    tracing::info!(deck_id = ?query.deck_id, "reviews::get_queue called");

    // Run queue fetch and counts concurrently
    // Use separate queries for filtered vs unfiltered to avoid nullable param issues
    let db_start = Instant::now();
    let (items, counts) = match query.deck_id {
        Some(deck_id) => {
            // Filtered to a specific deck
            tokio::try_join!(
                sqlx::query_as::<_, ReviewQueueRow>(
                    r#"
                    SELECT * FROM (
                        SELECT DISTINCT ON (c.lemma)
                            cs.id as card_state_id,
                            c.id as card_id,
                            c.deck_id,
                            d.name as deck_name,
                            c.lemma,
                            c.reading,
                            c.definition,
                            cs.status,
                            cs.due_date,
                            s.text as sentence_text,
                            s.cloze_text,
                            s.cloze_answer,
                            s.surface_form
                        FROM card_states cs
                        JOIN cards c ON cs.card_id = c.id
                        JOIN decks d ON c.deck_id = d.id
                        LEFT JOIN sentences s ON s.card_id = c.id AND s.is_primary = true
                        WHERE cs.user_id = $1
                          AND cs.suspended = false
                          AND c.deck_id = $3
                          AND (
                            cs.status = 'new'
                            OR (cs.due_date <= $2 AND cs.status IN ('learning', 'review', 'relearning'))
                          )
                        ORDER BY
                          c.lemma,
                          CASE cs.status
                            WHEN 'relearning' THEN 0
                            WHEN 'learning' THEN 1
                            WHEN 'review' THEN 2
                            WHEN 'new' THEN 3
                          END,
                          cs.due_date ASC NULLS LAST,
                          cs.created_at ASC
                    ) deduped
                    ORDER BY
                      CASE status
                        WHEN 'relearning' THEN 0
                        WHEN 'learning' THEN 1
                        WHEN 'review' THEN 2
                        WHEN 'new' THEN 3
                      END,
                      due_date ASC NULLS LAST
                    LIMIT 200
                    "#,
                )
                .bind(auth_user.user_id)
                .bind(today)
                .bind(deck_id)
                .fetch_all(&state.db),
                sqlx::query_as::<_, ReviewCounts>(
                    r#"
                    SELECT
                        COUNT(*) FILTER (WHERE cs.status = 'new' AND NOT cs.suspended) as total_new,
                        COUNT(*) FILTER (WHERE cs.status = 'learning' AND NOT cs.suspended) as total_learning,
                        COUNT(*) FILTER (WHERE cs.due_date <= $2 AND cs.status IN ('review', 'relearning') AND NOT cs.suspended) as total_due
                    FROM card_states cs
                    JOIN cards c ON c.id = cs.card_id
                    WHERE cs.user_id = $1 AND c.deck_id = $3
                    "#,
                )
                .bind(auth_user.user_id)
                .bind(today)
                .bind(deck_id)
                .fetch_one(&state.db),
            )?
        }
        None => {
            // All decks
            tokio::try_join!(
                sqlx::query_as::<_, ReviewQueueRow>(
                    r#"
                    SELECT * FROM (
                        SELECT DISTINCT ON (c.lemma)
                            cs.id as card_state_id,
                            c.id as card_id,
                            c.deck_id,
                            d.name as deck_name,
                            c.lemma,
                            c.reading,
                            c.definition,
                            cs.status,
                            cs.due_date,
                            s.text as sentence_text,
                            s.cloze_text,
                            s.cloze_answer,
                            s.surface_form
                        FROM card_states cs
                        JOIN cards c ON cs.card_id = c.id
                        JOIN decks d ON c.deck_id = d.id
                        LEFT JOIN sentences s ON s.card_id = c.id AND s.is_primary = true
                        WHERE cs.user_id = $1
                          AND cs.suspended = false
                          AND (
                            cs.status = 'new'
                            OR (cs.due_date <= $2 AND cs.status IN ('learning', 'review', 'relearning'))
                          )
                        ORDER BY
                          c.lemma,
                          CASE cs.status
                            WHEN 'relearning' THEN 0
                            WHEN 'learning' THEN 1
                            WHEN 'review' THEN 2
                            WHEN 'new' THEN 3
                          END,
                          cs.due_date ASC NULLS LAST,
                          cs.created_at ASC
                    ) deduped
                    ORDER BY
                      CASE status
                        WHEN 'relearning' THEN 0
                        WHEN 'learning' THEN 1
                        WHEN 'review' THEN 2
                        WHEN 'new' THEN 3
                      END,
                      due_date ASC NULLS LAST
                    LIMIT 200
                    "#,
                )
                .bind(auth_user.user_id)
                .bind(today)
                .fetch_all(&state.db),
                sqlx::query_as::<_, ReviewCounts>(
                    r#"
                    SELECT
                        COUNT(*) FILTER (WHERE status = 'new' AND NOT suspended) as total_new,
                        COUNT(*) FILTER (WHERE status = 'learning' AND NOT suspended) as total_learning,
                        COUNT(*) FILTER (WHERE due_date <= $2 AND status IN ('review', 'relearning') AND NOT suspended) as total_due
                    FROM card_states
                    WHERE user_id = $1
                    "#,
                )
                .bind(auth_user.user_id)
                .bind(today)
                .fetch_one(&state.db),
            )?
        }
    };
    tracing::info!(
        duration_ms = db_start.elapsed().as_millis() as u64,
        queue_items = items.len(),
        total_new = counts.total_new,
        total_due = counts.total_due,
        "reviews::get_queue db queries"
    );

    let queue_items: Vec<ReviewQueueItem> = items
        .into_iter()
        .map(|row| ReviewQueueItem {
            card_id: row.card_id,
            deck_id: row.deck_id,
            deck_name: row.deck_name,
            lemma: row.lemma,
            reading: row.reading,
            definition: row.definition,
            status: row.status,
            due_date: row.due_date,
            sentence: row.sentence_text.map(|text| SentenceForReview {
                text,
                cloze_text: row.cloze_text.unwrap_or_default(),
                cloze_answer: row.cloze_answer.unwrap_or_default(),
                surface_form: row.surface_form.unwrap_or_default(),
            }),
        })
        .collect();

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "reviews::get_queue total");

    Ok(Json(ReviewQueueResponse {
        items: queue_items,
        total_due: counts.total_due,
        total_new: counts.total_new,
        total_learning: counts.total_learning,
    }))
}

pub async fn submit(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(req): Json<SubmitReviewRequest>,
) -> AppResult<Json<ReviewResponse>> {
    let start = Instant::now();

    // Validate rating
    if !(1..=4).contains(&req.rating) {
        return Err(AppError::Validation("Rating must be between 1 and 4".to_string()));
    }

    // Get current card state
    let db_start = Instant::now();
    let card_state = sqlx::query_as::<_, CardStateRow>(
        "SELECT * FROM card_states WHERE card_id = $1 AND user_id = $2",
    )
    .bind(req.card_id)
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Card state not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "reviews::submit fetch card_state");

    let today = Utc::now().date_naive();
    let fsrs = FSRS::new(None);

    let fsrs_start = Instant::now();
    let current_card = crate::fsrs::Card {
        state: card_state.status.parse().unwrap_or(crate::fsrs::State::New),
        difficulty: card_state.difficulty as f64,
        stability: card_state.stability as f64,
        due: card_state.due_date,
        last_review: card_state.last_review.map(|dt| dt.date_naive()),
        reps: card_state.reps,
        lapses: card_state.lapses,
    };

    let rating = match req.rating {
        1 => Rating::Again,
        2 => Rating::Hard,
        3 => Rating::Good,
        4 => Rating::Easy,
        _ => Rating::Good,
    };

    let new_card = fsrs.review(&current_card, rating, today);
    tracing::info!(duration_ms = fsrs_start.elapsed().as_millis() as u64, "reviews::submit FSRS computation");

    // Update card state
    let db_start = Instant::now();
    let new_status = format!("{:?}", new_card.state).to_lowercase();
    sqlx::query(
        r#"
        UPDATE card_states
        SET status = $1, difficulty = $2, stability = $3, due_date = $4,
            last_review = NOW(), reps = $5, lapses = $6, updated_at = NOW()
        WHERE id = $7
        "#,
    )
    .bind(&new_status)
    .bind(new_card.difficulty as f32)
    .bind(new_card.stability as f32)
    .bind(new_card.due)
    .bind(new_card.reps)
    .bind(new_card.lapses)
    .bind(card_state.id)
    .execute(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "reviews::submit update card_state");

    // Record review history
    let db_start = Instant::now();
    let review_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO reviews (id, card_state_id, rating, time_taken_ms, difficulty_before,
                            stability_before, difficulty_after, stability_after)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(review_id)
    .bind(card_state.id)
    .bind(req.rating)
    .bind(req.time_taken_ms)
    .bind(card_state.difficulty)
    .bind(card_state.stability)
    .bind(new_card.difficulty as f32)
    .bind(new_card.stability as f32)
    .execute(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "reviews::submit insert review");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "reviews::submit total");

    Ok(Json(ReviewResponse {
        id: review_id,
        card_id: req.card_id,
        rating: req.rating,
        reviewed_at: Utc::now(),
        new_state: CardStateSnapshot {
            status: new_status,
            difficulty: new_card.difficulty as f32,
            stability: new_card.stability as f32,
            due_date: new_card.due,
            reps: new_card.reps,
            lapses: new_card.lapses,
        },
    }))
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct ReviewQueueRow {
    card_state_id: Uuid,
    card_id: Uuid,
    deck_id: Uuid,
    deck_name: String,
    lemma: String,
    reading: Option<String>,
    definition: String,
    status: String,
    due_date: Option<NaiveDate>,
    sentence_text: Option<String>,
    cloze_text: Option<String>,
    cloze_answer: Option<String>,
    surface_form: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ReviewCounts {
    total_new: i64,
    total_learning: i64,
    total_due: i64,
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct CardStateRow {
    id: Uuid,
    user_id: Uuid,
    card_id: Uuid,
    status: String,
    difficulty: f32,
    stability: f32,
    due_date: Option<NaiveDate>,
    last_review: Option<chrono::DateTime<chrono::Utc>>,
    reps: i32,
    lapses: i32,
}
