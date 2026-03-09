use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use rusqlite::Connection;
use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::Instant;
use zip::ZipArchive;

use crate::{
    api::analyze::{self, CreateDeckResult},
    api::middleware::AuthUser,
    error::{AppError, AppResult},
    models::{Deck, DeckResponse},
    processing::normalization::normalize_lemma,
    services::known_words_service,
    AppState,
};

pub async fn import_apkg(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let mut apkg_bytes: Option<Vec<u8>> = None;

    let upload_start = Instant::now();
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
    tracing::info!(duration_ms = upload_start.elapsed().as_millis() as u64, "imports::import_apkg read upload");

    let apkg_bytes =
        apkg_bytes.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;

    // Extract SQLite DB from ZIP
    let extract_start = Instant::now();
    let sqlite_bytes = extract_sqlite_from_apkg(&apkg_bytes)
        .map_err(|e| AppError::BadRequest(format!("Invalid .apkg file: {}", e)))?;
    tracing::info!(duration_ms = extract_start.elapsed().as_millis() as u64, "imports::import_apkg extract sqlite");

    // Read notes from the SQLite DB
    let read_start = Instant::now();
    let (deck_name, notes) = read_anki_db(&sqlite_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to read Anki database: {}", e)))?;
    tracing::info!(duration_ms = read_start.elapsed().as_millis() as u64, notes = notes.len(), "imports::import_apkg read anki db");

    if notes.is_empty() {
        return Err(AppError::BadRequest(
            "No notes found in Anki deck".to_string(),
        ));
    }

    // Auto-detect if fields are swapped (English front, CJK back) and fix
    let notes = maybe_swap_fields(notes);

    // Auto-detect language from card content
    let sample_text: String = notes.iter()
        .take(20)
        .flat_map(|n| [n.front.as_str(), " ", n.back.as_str(), " "])
        .collect();
    let language = analyze::detect_language(&sample_text, None);

    // Create deck
    let db_start = Instant::now();
    let settings = serde_json::json!({});
    let deck = sqlx::query_as::<_, Deck>(
        r#"
        INSERT INTO decks (user_id, name, description, language, source_type, settings)
        VALUES ($1, $2, $3, $4, 'import', $5)
        RETURNING *
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&deck_name)
    .bind(format!("Imported from Anki - {} notes", notes.len()))
    .bind(&language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "imports::import_apkg create deck");

    // Batch insert all cards at once
    let db_start = Instant::now();
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
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards = card_ids.len(), "imports::import_apkg batch insert cards");

    let cards_created = card_ids.len();

    // Add imported lemmas to known_words
    let db_start = Instant::now();
    let normalized_lemmas: Vec<String> = notes.iter()
        .map(|n| normalize_lemma(&n.front, &language))
        .collect();
    let lemma_refs: Vec<&str> = normalized_lemmas.iter().map(|s| s.as_str()).collect();
    known_words_service::add_known_words(&state.db, auth_user.user_id, &language, &lemma_refs).await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "imports::import_apkg add known_words");

    // Update deck card count
    let db_start = Instant::now();
    sqlx::query("UPDATE decks SET card_count = $1 WHERE id = $2")
        .bind(cards_created as i32)
        .bind(deck.id)
        .execute(&state.db)
        .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "imports::import_apkg update deck counts");

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, cards = cards_created, "imports::import_apkg total");

    Ok(Json(CreateDeckResult {
        decks: vec![DeckResponse::from(deck)],
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

/// Count CJK and Latin characters in a string.
fn count_script_chars(text: &str) -> (usize, usize) {
    let mut cjk = 0;
    let mut latin = 0;
    for c in text.chars() {
        if analyze::is_cjk(c) {
            cjk += 1;
        } else if c.is_alphabetic() {
            latin += 1;
        }
    }
    (cjk, latin)
}

/// Detect if fields are swapped and fix so the foreign word is always the lemma (front)
/// and the English definition is always the back.
///
/// Handles two cases:
/// 1. CJK decks: If fronts are Latin and backs are CJK, swap.
/// 2. European decks: If fronts look like English (multi-word definitions) and backs look
///    like single foreign words, swap.
fn maybe_swap_fields(mut notes: Vec<AnkiNote>) -> Vec<AnkiNote> {
    let sample_size = notes.len().min(30);
    let mut front_cjk = 0usize;
    let mut front_latin = 0usize;
    let mut back_cjk = 0usize;
    let mut back_latin = 0usize;

    for note in notes.iter().take(sample_size) {
        let (fc, fl) = count_script_chars(&note.front);
        let (bc, bl) = count_script_chars(&note.back);
        front_cjk += fc;
        front_latin += fl;
        back_cjk += bc;
        back_latin += bl;
    }

    // Case 1: CJK swap — fronts are mostly Latin, backs are mostly CJK
    let cjk_swap = front_latin > front_cjk && back_cjk > back_latin && back_cjk > 5;

    // Case 2: European language swap — both sides are Latin, but front looks like
    // English definitions (longer, multi-word) and back looks like single foreign words.
    // Heuristic: if front fields are significantly longer on average (English definitions
    // tend to be multi-word) and contain common English words, swap them.
    let european_swap = if !cjk_swap && front_cjk < 5 && back_cjk < 5 {
        let mut front_english_score = 0usize;
        let mut back_english_score = 0usize;

        for note in notes.iter().take(sample_size) {
            front_english_score += english_score(&note.front);
            back_english_score += english_score(&note.back);
        }

        // Swap if fronts are significantly more English than backs
        front_english_score > back_english_score * 2 && front_english_score > sample_size
    } else {
        false
    };

    if cjk_swap || european_swap {
        for note in &mut notes {
            std::mem::swap(&mut note.front, &mut note.back);
        }
    }

    notes
}

/// Score how "English" a text looks based on common English words and patterns.
fn english_score(text: &str) -> usize {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut score = 0;

    // Multi-word text is more likely to be a definition than a vocabulary word
    if words.len() > 2 {
        score += 1;
    }

    // Common English function words that appear in definitions
    const ENGLISH_MARKERS: &[&str] = &[
        "the", "a", "an", "to", "of", "in", "is", "for", "and", "or", "with",
        "that", "this", "it", "be", "as", "on", "not", "by", "from", "at",
        "are", "was", "have", "has", "do", "does", "will", "would", "can",
        "could", "should", "may", "might", "but", "if", "when", "than",
    ];

    for word in &words {
        if ENGLISH_MARKERS.contains(word) {
            score += 1;
        }
    }

    score
}
