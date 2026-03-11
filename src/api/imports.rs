use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use rusqlite::Connection;
use std::collections::HashMap;
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
    let (deck_name, cards) = read_anki_db(&sqlite_bytes)
        .map_err(|e| AppError::BadRequest(format!("Failed to read Anki database: {}", e)))?;
    tracing::info!(duration_ms = read_start.elapsed().as_millis() as u64, cards = cards.len(), "imports::import_apkg read anki db");

    if cards.is_empty() {
        return Err(AppError::BadRequest(
            "No notes found in Anki deck".to_string(),
        ));
    }

    // Auto-detect language from card content
    let sample_text: String = cards.iter()
        .take(20)
        .flat_map(|c| [c.lemma.as_str(), " ", c.definition.as_str(), " "])
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
    .bind(format!("Imported from Anki - {} notes", cards.len()))
    .bind(&language)
    .bind(&settings)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "imports::import_apkg create deck");

    // Insert cards one by one to capture IDs for sentence insertion
    let db_start = Instant::now();
    let mut cards_created = 0usize;
    let mut sentences_created = 0usize;
    let mut normalized_lemmas: Vec<String> = Vec::with_capacity(cards.len());

    for card in &cards {
        let freq_rank: Option<i32> = card.frequency_rank;

        let card_id = sqlx::query_scalar::<_, uuid::Uuid>(
            r#"
            INSERT INTO cards (deck_id, lemma, reading, definition, part_of_speech, frequency_rank)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
        )
        .bind(deck.id)
        .bind(&card.lemma)
        .bind(&card.reading)
        .bind(&card.definition)
        .bind(&card.part_of_speech)
        .bind(freq_rank)
        .fetch_one(&state.db)
        .await?;
        cards_created += 1;

        // Insert sentence if present
        if let Some(ref sentence) = card.sentence_text {
            if !sentence.is_empty() {
                let cloze_text = sentence.replace(&card.lemma, "[...]");
                let surface_form = card.lemma.clone();

                sqlx::query(
                    r#"
                    INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, surface_form, is_primary)
                    VALUES ($1, $2, $3, $4, $5, true)
                    "#,
                )
                .bind(card_id)
                .bind(sentence)
                .bind(&cloze_text)
                .bind(&card.lemma)
                .bind(&surface_form)
                .execute(&state.db)
                .await?;
                sentences_created += 1;
            }
        }

        normalized_lemmas.push(normalize_lemma(&card.lemma, &language));
    }
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards = cards_created, sentences = sentences_created, "imports::import_apkg insert cards+sentences");

    // Add imported lemmas to known_words
    let db_start = Instant::now();
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

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, cards = cards_created, sentences = sentences_created, "imports::import_apkg total");

    Ok(Json(CreateDeckResult {
        decks: vec![DeckResponse::from(deck)],
        cards_created,
        sentences_created,
        i_plus_one_found: 0,
        words_skipped_duplicate: 0,
    }))
}

/// A parsed card ready for insertion, extracted from an Anki note.
struct ParsedCard {
    lemma: String,
    reading: Option<String>,
    definition: String,
    part_of_speech: Option<String>,
    frequency_rank: Option<i32>,
    sentence_text: Option<String>,
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

/// Known field name patterns mapped to our card model.
/// Each variant lists names we recognize (case-insensitive).
struct FieldMapping {
    word_idx: Option<usize>,
    reading_idx: Option<usize>,
    meaning_idx: Option<usize>,
    sentence_idx: Option<usize>,
    frequency_idx: Option<usize>,
    pos_idx: Option<usize>,
}

/// Try to build a smart field mapping from Anki model field names.
fn detect_field_mapping(field_names: &[String]) -> Option<FieldMapping> {
    if field_names.len() < 3 {
        return None; // Too few fields, use fallback
    }

    let lower_names: Vec<String> = field_names.iter().map(|n| n.to_lowercase()).collect();

    let word_idx = lower_names.iter().position(|n|
        n == "word" || n == "expression" || n == "vocab" || n == "vocabulary"
        || n == "kanji" || n == "term" || n == "front"
    );
    let reading_idx = lower_names.iter().position(|n|
        n == "word reading" || n == "reading" || n == "kana" || n == "pronunciation"
        || n == "furigana"
    );
    let meaning_idx = lower_names.iter().position(|n|
        n == "word meaning" || n == "meaning" || n == "definition" || n == "english"
        || n == "translation" || n == "back" || n == "glossary" || n == "gloss"
    );
    let sentence_idx = lower_names.iter().position(|n|
        n == "sentence" || n == "example" || n == "example sentence" || n == "context"
    );
    let frequency_idx = lower_names.iter().position(|n|
        n == "frequency" || n == "freq" || n == "frequency rank"
    );
    let pos_idx = lower_names.iter().position(|n|
        n == "pos" || n == "part of speech" || n == "word type" || n == "type"
    );

    // Must at least find the word field
    if word_idx.is_some() {
        Some(FieldMapping {
            word_idx,
            reading_idx,
            meaning_idx,
            sentence_idx,
            frequency_idx,
            pos_idx,
        })
    } else {
        None
    }
}

fn read_anki_db(sqlite_bytes: &[u8]) -> anyhow::Result<(String, Vec<ParsedCard>)> {
    // Write to temp file since rusqlite can't open from bytes directly
    let tmp = tempfile::NamedTempFile::new()?;
    let tmp_path = tmp.path().to_path_buf();
    std::fs::write(&tmp_path, sqlite_bytes)?;

    let conn = Connection::open_with_flags(
        &tmp_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    // Get deck name
    let deck_name = extract_deck_name(&conn).unwrap_or_else(|_| "Anki Import".to_string());

    // Parse models from col table to get field names per model
    let model_fields = parse_model_fields(&conn).unwrap_or_default();
    tracing::info!(models = model_fields.len(), "imports: parsed model field names");

    // Read notes with their model IDs
    let raw_notes: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT mid, flds FROM notes")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let mut cards: Vec<ParsedCard> = Vec::with_capacity(raw_notes.len());

    for (mid, flds) in &raw_notes {
        let fields: Vec<&str> = flds.split('\x1f').collect();
        if fields.is_empty() {
            continue;
        }

        // Try smart mapping using model field names
        let parsed = if let Some(field_names) = model_fields.get(mid) {
            if let Some(mapping) = detect_field_mapping(field_names) {
                parse_with_mapping(&fields, &mapping)
            } else {
                parse_two_field(&fields)
            }
        } else {
            parse_two_field(&fields)
        };

        if let Some(card) = parsed {
            cards.push(card);
        }
    }

    // If smart mapping produced cards where definition == lemma (bad mapping),
    // it means the field names didn't match our patterns. Try the swap heuristic.
    let bad_mapping_count = cards.iter()
        .take(20)
        .filter(|c| c.lemma == c.definition)
        .count();

    if bad_mapping_count > cards.len().min(20) / 2 {
        // Re-parse with fallback two-field + swap
        cards.clear();
        for (_, flds) in &raw_notes {
            let fields: Vec<&str> = flds.split('\x1f').collect();
            if let Some(card) = parse_two_field(&fields) {
                cards.push(card);
            }
        }
        // Apply swap heuristic
        maybe_swap_cards(&mut cards);
    }

    drop(conn);
    let _ = std::fs::remove_file(&tmp_path);

    Ok((deck_name, cards))
}

/// Parse using detected field name mapping.
fn parse_with_mapping(fields: &[&str], mapping: &FieldMapping) -> Option<ParsedCard> {
    let get = |idx: Option<usize>| -> Option<String> {
        idx.and_then(|i| fields.get(i)).map(|s| strip_html(s)).filter(|s| !s.is_empty())
    };

    let lemma = get(mapping.word_idx)?;

    // For definition, try meaning field first; if not found, fall back to field after word
    let definition = get(mapping.meaning_idx)
        .or_else(|| {
            // If no meaning field matched, try field index 1 or 2 as fallback
            let word_i = mapping.word_idx.unwrap_or(0);
            // Skip reading field if it's right after word
            let next = if mapping.reading_idx == Some(word_i + 1) { word_i + 2 } else { word_i + 1 };
            get(Some(next))
        })
        .unwrap_or_default();

    if definition.is_empty() {
        return None;
    }

    let reading = get(mapping.reading_idx);
    let sentence_text = get(mapping.sentence_idx);
    let pos = get(mapping.pos_idx);

    let frequency_rank = get(mapping.frequency_idx).and_then(|f| {
        // Parse frequency — might be just a number, or have text around it
        f.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse::<i32>().ok()
    });

    Some(ParsedCard {
        lemma,
        reading,
        definition,
        part_of_speech: pos,
        frequency_rank,
        sentence_text,
    })
}

/// Fallback: treat as simple 2-field note (front/back).
fn parse_two_field(fields: &[&str]) -> Option<ParsedCard> {
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
    Some(ParsedCard {
        lemma: front,
        reading: None,
        definition: back,
        part_of_speech: None,
        frequency_rank: None,
        sentence_text: None,
    })
}

/// Parse model field names from the col.models JSON.
fn parse_model_fields(conn: &Connection) -> anyhow::Result<HashMap<i64, Vec<String>>> {
    let models_json: String = conn.query_row("SELECT models FROM col", [], |row| row.get(0))?;
    let models: serde_json::Value = serde_json::from_str(&models_json)?;

    let mut result = HashMap::new();

    if let Some(obj) = models.as_object() {
        for (model_id_str, model) in obj {
            let mid: i64 = model_id_str.parse().unwrap_or(0);
            if mid == 0 { continue; }

            if let Some(flds) = model.get("flds").and_then(|f| f.as_array()) {
                let names: Vec<String> = flds.iter()
                    .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                    .collect();
                if !names.is_empty() {
                    tracing::info!(model_id = mid, fields = ?names, "imports: detected model fields");
                    result.insert(mid, names);
                }
            }
        }
    }

    Ok(result)
}

fn extract_deck_name(conn: &Connection) -> anyhow::Result<String> {
    let decks_json: String = conn.query_row("SELECT decks FROM col", [], |row| row.get(0))?;
    let decks: serde_json::Value = serde_json::from_str(&decks_json)?;

    if let Some(obj) = decks.as_object() {
        for (id, deck) in obj {
            if id == "1" {
                continue;
            }
            if let Some(name) = deck.get("name").and_then(|n| n.as_str()) {
                return Ok(name.to_string());
            }
        }
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

/// Apply swap heuristic to parsed cards (for simple 2-field notes).
fn maybe_swap_cards(cards: &mut [ParsedCard]) {
    let sample_size = cards.len().min(30);
    let mut front_cjk = 0usize;
    let mut front_latin = 0usize;
    let mut back_cjk = 0usize;
    let mut back_latin = 0usize;

    for card in cards.iter().take(sample_size) {
        let (fc, fl) = count_script_chars(&card.lemma);
        let (bc, bl) = count_script_chars(&card.definition);
        front_cjk += fc;
        front_latin += fl;
        back_cjk += bc;
        back_latin += bl;
    }

    let cjk_swap = front_latin > front_cjk && back_cjk > back_latin && back_cjk > 5;

    let european_swap = if !cjk_swap && front_cjk < 5 && back_cjk < 5 {
        let mut front_english_score = 0usize;
        let mut back_english_score = 0usize;
        for card in cards.iter().take(sample_size) {
            front_english_score += english_score(&card.lemma);
            back_english_score += english_score(&card.definition);
        }
        front_english_score > back_english_score * 2 && front_english_score > sample_size
    } else {
        false
    };

    if cjk_swap || european_swap {
        for card in cards.iter_mut() {
            std::mem::swap(&mut card.lemma, &mut card.definition);
        }
    }
}

/// Score how "English" a text looks based on common English words and patterns.
fn english_score(text: &str) -> usize {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut score = 0;

    if words.len() > 2 {
        score += 1;
    }

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
