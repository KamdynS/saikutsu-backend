use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use rusqlite::Connection;
use std::io::{Cursor, Read};
use std::sync::Arc;
use zip::ZipArchive;

use crate::{
    api::analyze::CreateDeckResult,
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{Deck, DeckResponse},
    AppState,
};

pub async fn import_apkg(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<CreateDeckResult>> {
    let mut apkg_bytes: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        if let Some("file") = field_name.as_deref() {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
            apkg_bytes = Some(bytes.to_vec());
        }
    }

    let apkg_bytes =
        apkg_bytes.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;

    // Extract SQLite DB from ZIP
    let sqlite_bytes = extract_sqlite_from_apkg(&apkg_bytes)
        .map_err(|e| AppError::BadRequest(format!("Invalid .apkg file: {}", e)))?;

    // Read notes from the SQLite DB
    let (deck_name, notes) = read_anki_db(&sqlite_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to read Anki database: {}", e)))?;

    if notes.is_empty() {
        return Err(AppError::BadRequest(
            "No notes found in Anki deck".to_string(),
        ));
    }

    // Create deck
    let settings = serde_json::json!({});
    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, 'ja', 'import', $4)
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&deck_name)
    .bind(format!("Imported from Anki - {} notes", notes.len()))
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;

    // Batch insert all cards at once
    let lemmas: Vec<&str> = notes.iter().map(|n| n.front.as_str()).collect();
    let definitions: Vec<&str> = notes.iter().map(|n| n.back.as_str()).collect();

    let card_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO cards (deck_id, lemma, definition)
        SELECT $1, unnest($2::text[]), unnest($3::text[])
        RETURNING id
        "#,
    )
    .bind(deck.id)
    .bind(&lemmas)
    .bind(&definitions)
    .fetch_all(&state.db)
    .await?;

    let cards_created = card_ids.len();

    // Batch insert all card_states
    sqlx::query(
        r#"
        INSERT INTO card_states (user_id, card_id, status)
        SELECT $1, unnest($2::uuid[]), 'new'
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&card_ids)
    .execute(&state.db)
    .await?;

    // Update deck card count
    sqlx::query("UPDATE decks SET card_count = $1, new_count = $1 WHERE id = $2")
        .bind(cards_created as i32)
        .bind(deck.id)
        .execute(&state.db)
        .await?;

    Ok(Json(CreateDeckResult {
        deck: DeckResponse::from(deck),
        cards_created,
        sentences_created: 0,
        i_plus_one_found: 0,
        words_skipped_duplicate: 0,
    }))
}

struct AnkiNote {
    front: String,
    back: String,
}

fn extract_sqlite_from_apkg(apkg_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cursor = Cursor::new(apkg_bytes);
    let mut archive = ZipArchive::new(cursor)?;

    // Look for collection.anki21 first, then collection.anki2
    for name in &["collection.anki21", "collection.anki2"] {
        if let Ok(mut file) = archive.by_name(name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    anyhow::bail!("No collection.anki21 or collection.anki2 found in archive")
}

fn strip_html(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

fn read_anki_db(sqlite_bytes: &[u8]) -> anyhow::Result<(String, Vec<AnkiNote>)> {
    // Write to temp file since rusqlite can't open from bytes directly
    let tmp = tempfile::NamedTempFile::new()?;
    let tmp_path = tmp.path().to_path_buf();
    std::fs::write(&tmp_path, sqlite_bytes)?;

    let conn = Connection::open_with_flags(
        &tmp_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    // Get deck name from col table
    let deck_name = extract_deck_name(&conn).unwrap_or_else(|_| "Anki Import".to_string());

    // Read notes — collect raw field strings first, then process
    let raw_flds: Vec<String> = {
        let mut stmt = conn.prepare("SELECT flds FROM notes")?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let notes: Vec<AnkiNote> = raw_flds
        .iter()
        .filter_map(|flds| {
            let fields: Vec<&str> = flds.split('\x1f').collect();
            if fields.is_empty() {
                return None;
            }
            let front = strip_html(fields[0]);
            let back = if fields.len() > 1 {
                strip_html(fields[1])
            } else {
                String::new()
            };
            if front.is_empty() {
                return None;
            }
            Some(AnkiNote { front, back })
        })
        .collect();

    drop(conn);
    let _ = std::fs::remove_file(&tmp_path);

    Ok((deck_name, notes))
}

fn extract_deck_name(conn: &Connection) -> anyhow::Result<String> {
    let decks_json: String = conn.query_row("SELECT decks FROM col", [], |row| row.get(0))?;
    let decks: serde_json::Value = serde_json::from_str(&decks_json)?;

    // decks is an object keyed by deck ID; find the first non-default deck
    if let Some(obj) = decks.as_object() {
        for (id, deck) in obj {
            // Skip the default deck (id "1")
            if id == "1" {
                continue;
            }
            if let Some(name) = deck.get("name").and_then(|n| n.as_str()) {
                return Ok(name.to_string());
            }
        }
        // Fall back to any deck name
        for deck in obj.values() {
            if let Some(name) = deck.get("name").and_then(|n| n.as_str()) {
                if name != "Default" {
                    return Ok(name.to_string());
                }
            }
        }
    }

    Ok("Anki Import".to_string())
}
