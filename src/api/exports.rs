use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
    Extension,
};
use rusqlite::Connection;
use std::io::{Cursor, Write};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::{
    api::middleware::AuthUser,
    error::AppError,
    models::{Card, Deck, Sentence},
    processing::normalization::strip_brackets,
    AppState,
};

pub async fn export_apkg(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Path(deck_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let start = Instant::now();

    // Verify deck ownership
    let db_start = Instant::now();
    let deck = sqlx::query_as::<_, Deck>("SELECT * FROM decks WHERE id = $1 AND user_id = $2")
        .bind(deck_id)
        .bind(auth_user.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Deck not found".to_string()))?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, "exports::export_apkg fetch deck");

    // Fetch all cards for this deck
    let db_start = Instant::now();
    let cards = sqlx::query_as::<_, Card>("SELECT * FROM cards WHERE deck_id = $1 ORDER BY frequency_rank ASC NULLS LAST, created_at ASC")
        .bind(deck_id)
        .fetch_all(&state.db)
        .await?;
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, cards = cards.len(), "exports::export_apkg fetch cards");

    // Fetch all sentences for these cards
    let card_ids: Vec<Uuid> = cards.iter().map(|c| c.id).collect();
    let db_start = Instant::now();
    let sentences = if card_ids.is_empty() {
        vec![]
    } else {
        sqlx::query_as::<_, Sentence>(
            "SELECT * FROM sentences WHERE card_id = ANY($1) ORDER BY is_primary DESC, created_at ASC",
        )
        .bind(&card_ids)
        .fetch_all(&state.db)
        .await?
    };
    tracing::info!(duration_ms = db_start.elapsed().as_millis() as u64, sentences = sentences.len(), "exports::export_apkg fetch sentences");

    // Build the .apkg in memory
    let build_start = Instant::now();
    let apkg_bytes = build_apkg(&deck, &cards, &sentences)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to build .apkg: {}", e)))?;
    tracing::info!(duration_ms = build_start.elapsed().as_millis() as u64, bytes = apkg_bytes.len(), "exports::export_apkg build apkg");

    let filename = format!("{}.apkg", sanitize_filename(&deck.name));

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "exports::export_apkg total");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(Body::from(apkg_bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to build response: {}", e)))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            // Strip double quotes explicitly to prevent Content-Disposition header injection,
            // and replace other non-safe characters with underscore
            if c == '"' {
                '_'
            } else if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn build_apkg(
    deck: &Deck,
    cards: &[Card],
    sentences: &[Sentence],
) -> anyhow::Result<Vec<u8>> {
    // Create SQLite database in memory
    let conn = Connection::open_in_memory()?;

    // Anki uses a deck ID that's a timestamp-based integer
    let deck_id_int: i64 = chrono::Utc::now().timestamp_millis();
    let model_id: i64 = deck_id_int + 1;

    create_anki_schema(&conn)?;
    populate_col(&conn, deck, deck_id_int, model_id)?;
    populate_cards_and_notes(&conn, cards, sentences, deck_id_int, model_id)?;

    // Export SQLite to bytes via temp file
    let sqlite_bytes: Vec<u8>;
    {
        let tmp = tempfile::NamedTempFile::new()?;
        let tmp_path = tmp.path().to_path_buf();
        drop(tmp);

        // Back up in-memory DB to temp file
        let mut dest = Connection::open(&tmp_path)?;
        let backup = rusqlite::backup::Backup::new(&conn, &mut dest)?;
        backup.run_to_completion(100, std::time::Duration::from_millis(0), None)?;
        drop(backup);
        drop(dest);

        sqlite_bytes = std::fs::read(&tmp_path)?;
        let _ = std::fs::remove_file(&tmp_path);
    }

    // Create ZIP (.apkg)
    let mut zip_buffer = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut zip_buffer);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("collection.anki21", options)?;
        zip.write_all(&sqlite_bytes)?;

        zip.start_file("media", options)?;
        zip.write_all(b"{}")?;

        zip.finish()?;
    }

    Ok(zip_buffer.into_inner())
}

fn create_anki_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE col (
            id integer PRIMARY KEY,
            crt integer NOT NULL,
            mod integer NOT NULL,
            scm integer NOT NULL,
            ver integer NOT NULL,
            dty integer NOT NULL,
            usn integer NOT NULL,
            ls integer NOT NULL,
            conf text NOT NULL,
            models text NOT NULL,
            decks text NOT NULL,
            dconf text NOT NULL,
            tags text NOT NULL
        );

        CREATE TABLE notes (
            id integer PRIMARY KEY,
            guid text NOT NULL,
            mid integer NOT NULL,
            mod integer NOT NULL,
            usn integer NOT NULL,
            tags text NOT NULL,
            flds text NOT NULL,
            sfld text NOT NULL,
            csum integer NOT NULL,
            flags integer NOT NULL,
            data text NOT NULL
        );

        CREATE TABLE cards (
            id integer PRIMARY KEY,
            nid integer NOT NULL,
            did integer NOT NULL,
            ord integer NOT NULL,
            mod integer NOT NULL,
            usn integer NOT NULL,
            type integer NOT NULL,
            queue integer NOT NULL,
            due integer NOT NULL,
            ivl integer NOT NULL,
            factor integer NOT NULL,
            reps integer NOT NULL,
            lapses integer NOT NULL,
            left integer NOT NULL,
            odue integer NOT NULL,
            odid integer NOT NULL,
            flags integer NOT NULL,
            data text NOT NULL
        );

        CREATE TABLE revlog (
            id integer PRIMARY KEY,
            cid integer NOT NULL,
            usn integer NOT NULL,
            ease integer NOT NULL,
            ivl integer NOT NULL,
            lastIvl integer NOT NULL,
            factor integer NOT NULL,
            time integer NOT NULL,
            type integer NOT NULL
        );

        CREATE TABLE graves (
            usn integer NOT NULL,
            oid integer NOT NULL,
            type integer NOT NULL
        );
        ",
    )?;
    Ok(())
}

fn populate_col(
    conn: &Connection,
    deck: &Deck,
    deck_id: i64,
    model_id: i64,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();

    let models = serde_json::json!({
        model_id.to_string(): {
            "id": model_id,
            "name": "Saikutsu",
            "type": 0,
            "mod": now,
            "usn": -1,
            "sortf": 0,
            "did": deck_id,
            "tmpls": [{
                "name": "Card 1",
                "ord": 0,
                "qfmt": "{{Front}}",
                "afmt": "{{FrontSide}}<hr id=answer>{{Back}}",
                "bqfmt": "",
                "bafmt": "",
                "did": null,
                "bfont": "",
                "bsize": 0
            }],
            "flds": [
                {
                    "name": "Front",
                    "ord": 0,
                    "sticky": false,
                    "rtl": false,
                    "font": "Arial",
                    "size": 20,
                    "media": []
                },
                {
                    "name": "Back",
                    "ord": 1,
                    "sticky": false,
                    "rtl": false,
                    "font": "Arial",
                    "size": 20,
                    "media": []
                }
            ],
            "css": ".card {\n font-family: arial;\n font-size: 20px;\n text-align: center;\n color: black;\n background-color: white;\n}\n",
            "latexPre": "",
            "latexPost": "",
            "latexsvg": false,
            "req": [[0, "any", [0]]]
        }
    });

    let decks = serde_json::json!({
        deck_id.to_string(): {
            "id": deck_id,
            "name": deck.name,
            "mod": now,
            "usn": -1,
            "lrnToday": [0, 0],
            "revToday": [0, 0],
            "newToday": [0, 0],
            "timeToday": [0, 0],
            "collapsed": false,
            "browserCollapsed": false,
            "desc": deck.description.as_deref().unwrap_or(""),
            "dyn": 0,
            "conf": 1,
            "extendNew": 0,
            "extendRev": 0
        }
    });

    let conf = serde_json::json!({
        "activeDecks": [1],
        "curDeck": 1,
        "newSpread": 0,
        "collapseTime": 1200,
        "timeLim": 0,
        "estTimes": true,
        "dueCounts": true,
        "curModel": model_id,
        "nextPos": 1,
        "sortType": "noteFld",
        "sortBackwards": false,
        "addToCur": true
    });

    let dconf = serde_json::json!({
        "1": {
            "id": 1,
            "name": "Default",
            "mod": 0,
            "usn": 0,
            "maxTaken": 60,
            "autoplay": true,
            "timer": 0,
            "replayq": true,
            "new": {
                "bury": true,
                "delays": [1.0, 10.0],
                "initialFactor": 2500,
                "ints": [1, 4, 0],
                "order": 1,
                "perDay": 20
            },
            "rev": {
                "bury": true,
                "ease4": 1.3,
                "ivlFct": 1.0,
                "maxIvl": 36500,
                "perDay": 200,
                "hardFactor": 1.2
            },
            "lapse": {
                "delays": [10.0],
                "leechAction": 0,
                "leechFails": 8,
                "minInt": 1,
                "mult": 0.0
            }
        }
    });

    conn.execute(
        "INSERT INTO col VALUES (1, ?1, ?2, ?3, 11, 0, 0, 0, ?4, ?5, ?6, ?7, '{}')",
        rusqlite::params![
            now,
            now,
            now * 1000,
            conf.to_string(),
            models.to_string(),
            decks.to_string(),
            dconf.to_string(),
        ],
    )?;

    Ok(())
}

fn populate_cards_and_notes(
    conn: &Connection,
    cards: &[Card],
    sentences: &[Sentence],
    deck_id: i64,
    model_id: i64,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();

    // Group sentences by card_id
    let mut sentences_by_card: std::collections::HashMap<Uuid, Vec<&Sentence>> =
        std::collections::HashMap::new();
    for s in sentences {
        sentences_by_card.entry(s.card_id).or_default().push(s);
    }

    for (i, card) in cards.iter().enumerate() {
        let note_id = now * 1000 + i as i64;
        let card_id_int = note_id + 1;

        // Build front: sentence with target word bolded + parenthesized, or just the word
        let card_sentences = sentences_by_card.get(&card.id);
        let front = if let Some(sents) = card_sentences {
            let sentence = sents.iter().find(|s| s.is_primary).or(sents.first());
            if let Some(s) = sentence {
                let clean = strip_brackets(&s.text);
                let word = &s.surface_form;
                if let Some(idx) = clean.find(word.as_str()) {
                    format!("{}<b>({})</b>{}", &clean[..idx], word, &clean[idx + word.len()..])
                } else {
                    clean
                }
            } else {
                card.lemma.clone()
            }
        } else {
            card.lemma.clone()
        };

        // Build back: word + reading + definition
        let mut back = card.lemma.clone();
        if let Some(ref reading) = card.reading {
            back = format!("{} [{}]", back, reading);
        }
        back = format!("{}<br>{}", back, card.definition);

        // flds uses \x1f (unit separator) between fields
        let flds = format!("{}\x1f{}", front, back);

        // Simple checksum of sort field
        let csum = simple_csum(&front);

        // Generate a short guid
        let guid = format!("{:x}", note_id);

        conn.execute(
            "INSERT INTO notes VALUES (?1, ?2, ?3, ?4, -1, '', ?5, ?6, ?7, 0, '')",
            rusqlite::params![note_id, guid, model_id, now, flds, front, csum],
        )?;

        conn.execute(
            "INSERT INTO cards VALUES (?1, ?2, ?3, 0, ?4, -1, 0, 0, ?5, 0, 0, 0, 0, 0, 0, 0, 0, '')",
            rusqlite::params![card_id_int, note_id, deck_id, now, i as i64],
        )?;
    }

    Ok(())
}

fn simple_csum(s: &str) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    (hasher.finish() % 1_000_000_000) as i64
}
