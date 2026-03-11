use axum::{
    extract::{Multipart, State},
    Extension, Json,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
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
    services::known_words_service,
    AppState,
};

// ─── Preview endpoint ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub deck_name: String,
    pub total_notes: usize,
    pub field_names: Vec<String>,
    pub sample_notes: Vec<Vec<String>>,
    pub suggested_mapping: FieldMappingRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMappingRequest {
    pub word: Option<usize>,
    pub reading: Option<usize>,
    pub definition: Option<usize>,
    pub sentence: Option<usize>,
    pub sentence_meaning: Option<usize>,
    pub frequency: Option<usize>,
    pub part_of_speech: Option<usize>,
}

pub async fn preview_apkg(
    mut multipart: Multipart,
) -> AppResult<Json<ImportPreview>> {
    let mut apkg_bytes: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        if let Some("file") = field_name.as_deref() {
            let bytes = field.bytes().await
                .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
            apkg_bytes = Some(bytes.to_vec());
        }
    }

    let apkg_bytes = apkg_bytes.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;

    let sqlite_bytes = extract_sqlite_from_apkg(&apkg_bytes)
        .map_err(|e| AppError::BadRequest(format!("Invalid .apkg file: {}", e)))?;

    let (deck_name, field_names, sample_notes, total_notes, suggested_mapping) =
        preview_anki_db(&sqlite_bytes)
            .map_err(|e| AppError::BadRequest(format!("Failed to read Anki database: {}", e)))?;

    Ok(Json(ImportPreview {
        deck_name,
        total_notes,
        field_names,
        sample_notes,
        suggested_mapping,
    }))
}

fn preview_anki_db(sqlite_bytes: &[u8]) -> anyhow::Result<(String, Vec<String>, Vec<Vec<String>>, usize, FieldMappingRequest)> {
    let tmp = tempfile::NamedTempFile::new()?;
    let tmp_path = tmp.path().to_path_buf();
    std::fs::write(&tmp_path, sqlite_bytes)?;

    let conn = Connection::open_with_flags(&tmp_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let deck_name = extract_deck_name(&conn).unwrap_or_else(|_| "Anki Import".to_string());
    let model_fields = parse_model_fields(&conn).unwrap_or_default();

    // Get the first model's field names (most decks have one model)
    let (primary_mid, field_names) = model_fields.iter().next()
        .map(|(mid, names)| (*mid, names.clone()))
        .unwrap_or((0, vec!["Front".to_string(), "Back".to_string()]));

    // Count total notes
    let total_notes: usize = conn.query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;

    // Get sample notes from the middle of the deck (first cards are often intros)
    let offset = if total_notes > 6 { total_notes / 2 - 1 } else { 0 };
    let sample_notes: Vec<Vec<String>> = {
        let mut stmt = conn.prepare("SELECT flds FROM notes WHERE mid = ?1 LIMIT 3 OFFSET ?2")?;
        let rows: Vec<String> = stmt.query_map(rusqlite::params![primary_mid, offset], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        // If no notes matched the primary model, try without filter
        let rows = if rows.is_empty() {
            let mut stmt2 = conn.prepare("SELECT flds FROM notes LIMIT 3 OFFSET ?1")?;
            let fallback: Vec<String> = stmt2.query_map([offset], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            fallback
        } else {
            rows
        };

        rows.iter()
            .map(|flds| {
                flds.split('\x1f')
                    .map(|f| strip_html(f))
                    .collect()
            })
            .collect()
    };

    let suggested_mapping = suggest_mapping(&field_names);

    drop(conn);
    let _ = std::fs::remove_file(&tmp_path);

    Ok((deck_name, field_names, sample_notes, total_notes, suggested_mapping))
}

fn suggest_mapping(field_names: &[String]) -> FieldMappingRequest {
    let lower: Vec<String> = field_names.iter().map(|n| n.to_lowercase()).collect();

    let word = lower.iter().position(|n|
        n == "word" || n == "expression" || n == "vocab" || n == "vocabulary"
        || n == "kanji" || n == "term" || n == "front"
    );
    let reading = lower.iter().position(|n|
        n == "word reading" || n == "reading" || n == "kana" || n == "pronunciation"
    );
    let definition = lower.iter().position(|n|
        n == "word meaning" || n == "meaning" || n == "definition" || n == "english"
        || n == "translation" || n == "back" || n == "glossary" || n == "gloss"
    );
    let sentence = lower.iter().position(|n|
        n == "sentence" || n == "example" || n == "example sentence" || n == "context"
    );
    let sentence_meaning = lower.iter().position(|n|
        n == "sentence meaning" || n == "sentence translation" || n == "sentence english"
    );
    let frequency = lower.iter().position(|n|
        n == "frequency" || n == "freq" || n == "frequency rank"
    );
    let pos = lower.iter().position(|n|
        n == "pos" || n == "part of speech" || n == "word type" || n == "type"
    );

    // Fallback for 2-field decks: field 0 = word, field 1 = definition
    let word = word.or(if field_names.len() <= 2 { Some(0) } else { None });
    let definition = definition.or(if field_names.len() <= 2 && field_names.len() > 1 { Some(1) } else { None });

    FieldMappingRequest {
        word,
        reading,
        definition,
        sentence,
        sentence_meaning,
        frequency,
        part_of_speech: pos,
    }
}

// ─── Import endpoint (with user-provided mapping) ───────────────────

pub async fn import_apkg(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    mut multipart: Multipart,
) -> AppResult<Json<CreateDeckResult>> {
    let start = Instant::now();

    let mut apkg_bytes: Option<Vec<u8>> = None;
    let mut mapping_json: Option<String> = None;
    let mut user_language: Option<String> = None;

    let upload_start = Instant::now();
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        match field_name.as_deref() {
            Some("file") => {
                let bytes = field.bytes().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read file: {}", e)))?;
                apkg_bytes = Some(bytes.to_vec());
            }
            Some("mapping") => {
                let text = field.text().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read mapping: {}", e)))?;
                mapping_json = Some(text);
            }
            Some("language") => {
                let text = field.text().await
                    .map_err(|e| AppError::BadRequest(format!("Failed to read language: {}", e)))?;
                user_language = Some(text);
            }
            _ => {}
        }
    }
    tracing::info!(duration_ms = upload_start.elapsed().as_millis() as u64, "imports::import_apkg read upload");

    let apkg_bytes = apkg_bytes.ok_or_else(|| AppError::BadRequest("No file provided".to_string()))?;

    // Parse mapping if provided, otherwise auto-detect
    let user_mapping: Option<FieldMappingRequest> = mapping_json
        .as_deref()
        .map(|json| serde_json::from_str(json))
        .transpose()
        .map_err(|e| AppError::BadRequest(format!("Invalid mapping: {}", e)))?;

    // Extract SQLite DB from ZIP
    let extract_start = Instant::now();
    let sqlite_bytes = extract_sqlite_from_apkg(&apkg_bytes)
        .map_err(|e| AppError::BadRequest(format!("Invalid .apkg file: {}", e)))?;
    tracing::info!(duration_ms = extract_start.elapsed().as_millis() as u64, "imports::import_apkg extract sqlite");

    // Read notes from the SQLite DB
    let read_start = Instant::now();
    let (deck_name, cards) = read_anki_db(&sqlite_bytes, user_mapping.as_ref())
        .map_err(|e| AppError::BadRequest(format!("Failed to read Anki database: {}", e)))?;
    tracing::info!(duration_ms = read_start.elapsed().as_millis() as u64, cards = cards.len(), "imports::import_apkg read anki db");

    if cards.is_empty() {
        return Err(AppError::BadRequest("No notes found in Anki deck".to_string()));
    }

    // Auto-detect language from lemmas only (definitions are usually English, which skews detection)
    let sample_text: String = cards.iter()
        .take(50)
        .flat_map(|c| [c.lemma.as_str(), " "])
        .collect();
    let language = analyze::detect_language(&sample_text, user_language.as_deref());

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

    // Insert cards + sentences
    let db_start = Instant::now();
    let mut cards_created = 0usize;
    let mut sentences_created = 0usize;
    let mut raw_lemmas: Vec<&str> = Vec::with_capacity(cards.len());

    for card in &cards {
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
        .bind(card.frequency_rank)
        .fetch_one(&state.db)
        .await?;
        cards_created += 1;

        if let Some(ref sentence) = card.sentence_text {
            if !sentence.is_empty() {
                let cloze_text = sentence.replace(&card.lemma, "[...]");
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
                .bind(&card.lemma)
                .execute(&state.db)
                .await?;
                sentences_created += 1;
            }
        }

        raw_lemmas.push(card.lemma.as_str());
    }
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards = cards_created, sentences = sentences_created, "imports::import_apkg insert cards+sentences");

    // Add imported lemmas to known_words (service handles normalization)
    let db_start = Instant::now();
    known_words_service::add_known_words(&state.db, auth_user.user_id, &language, &raw_lemmas).await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "imports::import_apkg add known_words");

    // Update deck card count
    sqlx::query("UPDATE decks SET card_count = $1 WHERE id = $2")
        .bind(cards_created as i32)
        .bind(deck.id)
        .execute(&state.db)
        .await?;

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, cards = cards_created, sentences = sentences_created, "imports::import_apkg total");

    Ok(Json(CreateDeckResult {
        decks: vec![DeckResponse::from(deck)],
        cards_created,
        sentences_created,
        i_plus_one_found: 0,
        words_skipped_duplicate: 0,
    }))
}

// ─── Shared parsing logic ───────────────────────────────────────────

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

fn read_anki_db(sqlite_bytes: &[u8], user_mapping: Option<&FieldMappingRequest>) -> anyhow::Result<(String, Vec<ParsedCard>)> {
    let tmp = tempfile::NamedTempFile::new()?;
    let tmp_path = tmp.path().to_path_buf();
    std::fs::write(&tmp_path, sqlite_bytes)?;

    let conn = Connection::open_with_flags(&tmp_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let deck_name = extract_deck_name(&conn).unwrap_or_else(|_| "Anki Import".to_string());

    // Determine the mapping to use
    let mapping = if let Some(m) = user_mapping {
        m.clone()
    } else {
        let model_fields = parse_model_fields(&conn).unwrap_or_default();
        let field_names = model_fields.values().next()
            .cloned()
            .unwrap_or_else(|| vec!["Front".to_string(), "Back".to_string()]);
        suggest_mapping(&field_names)
    };

    let raw_notes: Vec<String> = {
        let mut stmt = conn.prepare("SELECT flds FROM notes")?;
        let notes: Vec<String> = stmt.query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        notes
    };

    let mut cards: Vec<ParsedCard> = Vec::with_capacity(raw_notes.len());

    for flds in &raw_notes {
        let fields: Vec<&str> = flds.split('\x1f').collect();
        if fields.is_empty() { continue; }

        if let Some(card) = parse_with_user_mapping(&fields, &mapping) {
            cards.push(card);
        }
    }

    // If most cards have lemma == definition, the mapping is bad — apply swap heuristic
    let bad_count = cards.iter().take(20).filter(|c| c.lemma == c.definition || c.definition.is_empty()).count();
    if user_mapping.is_none() && bad_count > cards.len().min(20) / 2 {
        cards.clear();
        for flds in &raw_notes {
            let fields: Vec<&str> = flds.split('\x1f').collect();
            if let Some(card) = parse_two_field(&fields) {
                cards.push(card);
            }
        }
        maybe_swap_cards(&mut cards);
    }

    drop(conn);
    let _ = std::fs::remove_file(&tmp_path);

    Ok((deck_name, cards))
}

fn parse_with_user_mapping(fields: &[&str], mapping: &FieldMappingRequest) -> Option<ParsedCard> {
    let get = |idx: Option<usize>| -> Option<String> {
        idx.and_then(|i| fields.get(i)).map(|s| strip_html(s)).filter(|s| !s.is_empty())
    };

    let lemma = get(mapping.word)?;
    let definition = get(mapping.definition).unwrap_or_default();

    if definition.is_empty() && mapping.definition.is_none() {
        // No definition field mapped at all — skip
        return None;
    }

    Some(ParsedCard {
        lemma,
        reading: get(mapping.reading),
        definition,
        part_of_speech: get(mapping.part_of_speech),
        frequency_rank: get(mapping.frequency).and_then(|f|
            f.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse::<i32>().ok()
        ),
        sentence_text: get(mapping.sentence),
    })
}

fn parse_two_field(fields: &[&str]) -> Option<ParsedCard> {
    if fields.is_empty() { return None; }
    let front = strip_html(fields[0]);
    let back = if fields.len() > 1 { strip_html(fields[1]) } else { String::new() };
    if front.is_empty() { return None; }
    Some(ParsedCard {
        lemma: front,
        reading: None,
        definition: back,
        part_of_speech: None,
        frequency_rank: None,
        sentence_text: None,
    })
}

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
            if id == "1" { continue; }
            if let Some(name) = deck.get("name").and_then(|n| n.as_str()) {
                return Ok(name.to_string());
            }
        }
        for deck in obj.values() {
            if let Some(name) = deck.get("name").and_then(|n| n.as_str()) {
                if name != "Default" { return Ok(name.to_string()); }
            }
        }
    }
    Ok("Anki Import".to_string())
}

fn count_script_chars(text: &str) -> (usize, usize) {
    let mut cjk = 0;
    let mut latin = 0;
    for c in text.chars() {
        if analyze::is_cjk(c) { cjk += 1; }
        else if c.is_alphabetic() { latin += 1; }
    }
    (cjk, latin)
}

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
        let mut fe = 0usize;
        let mut be = 0usize;
        for card in cards.iter().take(sample_size) {
            fe += english_score(&card.lemma);
            be += english_score(&card.definition);
        }
        fe > be * 2 && fe > sample_size
    } else { false };

    if cjk_swap || european_swap {
        for card in cards.iter_mut() {
            std::mem::swap(&mut card.lemma, &mut card.definition);
        }
    }
}

fn english_score(text: &str) -> usize {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut score = 0;
    if words.len() > 2 { score += 1; }
    const ENGLISH_MARKERS: &[&str] = &[
        "the", "a", "an", "to", "of", "in", "is", "for", "and", "or", "with",
        "that", "this", "it", "be", "as", "on", "not", "by", "from", "at",
        "are", "was", "have", "has", "do", "does", "will", "would", "can",
        "could", "should", "may", "might", "but", "if", "when", "than",
    ];
    for word in &words {
        if ENGLISH_MARKERS.contains(word) { score += 1; }
    }
    score
}
