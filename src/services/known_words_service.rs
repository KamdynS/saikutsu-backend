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
