use std::collections::HashSet;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

/// Batch-insert lemmas into known_words for a user+language.
/// Uses ON CONFLICT DO NOTHING so duplicates are silently skipped.
pub async fn add_known_words(
    pool: &PgPool,
    user_id: Uuid,
    language: &str,
    lemmas: &[&str],
) -> AppResult<()> {
    if lemmas.is_empty() {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO known_words (user_id, language, lemma)
        SELECT $1, $2, unnest($3::text[])
        ON CONFLICT (user_id, language, lemma) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(language)
    .bind(lemmas)
    .execute(pool)
    .await?;

    Ok(())
}

/// Remove lemmas from known_words, but only if they don't appear in any
/// other deck for this user+language.
pub async fn remove_orphaned_known_words(
    pool: &PgPool,
    user_id: Uuid,
    language: &str,
    lemmas: &[String],
) -> AppResult<()> {
    if lemmas.is_empty() {
        return Ok(());
    }

    // Delete from known_words where the lemma doesn't exist in any remaining card
    sqlx::query(
        r#"
        DELETE FROM known_words kw
        WHERE kw.user_id = $1
          AND kw.language = $2
          AND kw.lemma = ANY($3::text[])
          AND NOT EXISTS (
            SELECT 1 FROM cards c
            JOIN decks d ON c.deck_id = d.id
            WHERE d.user_id = $1 AND d.language = $2 AND c.lemma = kw.lemma
          )
        "#,
    )
    .bind(user_id)
    .bind(language)
    .bind(lemmas)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fetch all known lemmas for a user in a given language.
pub async fn get_known_lemmas(
    pool: &PgPool,
    user_id: Uuid,
    language: &str,
) -> AppResult<HashSet<String>> {
    let lemmas: Vec<String> = sqlx::query_scalar(
        "SELECT lemma FROM known_words WHERE user_id = $1 AND language = $2",
    )
    .bind(user_id)
    .bind(language)
    .fetch_all(pool)
    .await?;

    Ok(lemmas.into_iter().collect())
}
