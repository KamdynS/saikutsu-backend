use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::{
    api::middleware::AuthUser,
    error::AppResult,
    models::Deck,
    processing::{dictionary, normalization::normalize_lemma},
    services::{analyze_service::{is_japanese, DeckType}, known_words_service},
    AppState,
};

pub struct CardCreationResult {
    pub cards_created: usize,
    pub sentences_created: usize,
    pub i_plus_one_found: usize,
    pub words_skipped_duplicate: usize,
}

#[derive(Debug, Serialize)]
pub struct CreateDeckResult {
    pub decks: Vec<crate::models::DeckResponse>,
    pub cards_created: usize,
    pub sentences_created: usize,
    pub i_plus_one_found: usize,
    pub words_skipped_duplicate: usize,
}

/// Update deck descriptions and return refreshed deck data (with correct card_count from triggers).
pub async fn update_deck_descriptions(
    db: &sqlx::PgPool,
    decks: &[Deck],
    description_template: &str,
) -> AppResult<Vec<Deck>> {
    let mut updated = Vec::with_capacity(decks.len());
    for deck in decks {
        // Use actual per-deck card count instead of combined total
        let card_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE deck_id = $1")
            .bind(deck.id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
        // Replace the leading number in the template with the actual per-deck count
        let rest = description_template.trim_start_matches(|c: char| c.is_ascii_digit());
        let desc = if rest.len() < description_template.len() {
            format!("{}{}", card_count, rest)
        } else {
            description_template.to_string()
        };
        let d = sqlx::query_as::<_, Deck>(
            "UPDATE decks SET description = $1 WHERE id = $2 RETURNING *",
        )
        .bind(&desc)
        .bind(deck.id)
        .fetch_one(db)
        .await?;
        updated.push(d);
    }
    Ok(updated)
}

/// Shared logic for creating cards from analyzed text.
/// Uses batch INSERTs instead of individual queries per card.
/// When both deck types are selected, `cloze_deck` and `def_deck` are separate decks.
/// When only one type is selected, only the relevant deck is Some.
#[allow(clippy::too_many_arguments)]
pub async fn create_cards_from_analysis(
    state: &Arc<AppState>,
    auth_user: &AuthUser,
    cloze_deck: Option<&Deck>,
    def_deck: Option<&Deck>,
    words: &[(String, i32)],
    deck_types: &[DeckType],
    surface_forms: &HashMap<String, Vec<String>>,
    pos_map: &HashMap<String, String>,
    lemma_sentences: &HashMap<String, Vec<String>>,
    sentence_content_lemmas: &HashMap<String, HashSet<String>>,
    language: &str,
) -> AppResult<CardCreationResult> {
    let start = Instant::now();

    // Query existing known lemmas for this user+language (language-scoped dedup)
    let existing_lemmas = known_words_service::get_known_lemmas(&state.db, auth_user.user_id, language).await?;

    let want_word_def = deck_types.contains(&DeckType::WordDefinition);
    let want_i_plus_one = deck_types.contains(&DeckType::IPlusOne);

    // Build i+1 map: lemma → Vec<(sentence, cloze_text, cloze_answer)>
    let mut i_plus_one_map: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
    let mut i_plus_one_found: usize = 0;

    if want_i_plus_one {
        let new_lemmas: HashSet<&str> = words.iter()
            .map(|(lemma, _)| lemma.as_str())
            .filter(|l| !existing_lemmas.contains(*l))
            .collect();

        for (sentence, content_lemmas) in sentence_content_lemmas {
            let unknown_lemmas: Vec<&String> = content_lemmas.iter()
                .filter(|l| !existing_lemmas.contains(l.as_str()) && new_lemmas.contains(l.as_str()))
                .collect();

            if unknown_lemmas.len() == 1 {
                i_plus_one_found += 1;
                let unknown_lemma = unknown_lemmas[0].clone();
                let word_surfaces = surface_forms.get(&unknown_lemma);

                let mut cloze_text = sentence.clone();
                let mut cloze_answer = unknown_lemma.clone();

                if let Some(surfaces) = word_surfaces {
                    for surface in surfaces {
                        if sentence.contains(surface) {
                            cloze_text = sentence.replace(surface, "＿＿＿");
                            cloze_answer = surface.clone();
                            break;
                        }
                    }
                }

                let entry = i_plus_one_map.entry(unknown_lemma).or_default();
                if entry.len() < 3 {
                    entry.push((sentence.clone(), cloze_text, cloze_answer));
                }
            }
        }
    }

    // Pre-compute all card data before batch insert
    struct CardData {
        lemma: String,
        reading: Option<String>,
        definition: String,
        pos: Option<String>,
        frequency_rank: i32,
        doc_frequency: i32,
    }

    // Helper to batch-insert cards into a specific deck and attach sentences
    async fn insert_cards_for_deck(
        state: &Arc<AppState>,
        auth_user: &AuthUser,
        deck: &Deck,
        card_data_list: &[CardData],
        sentences: &[(usize, String, String, String, String, bool)], // (card_data_idx, text, cloze_text, cloze_answer, surface_form, is_primary)
        language: &str,
    ) -> AppResult<(usize, usize)> {
        if card_data_list.is_empty() {
            return Ok((0, 0));
        }

        let lemmas: Vec<&str> = card_data_list.iter().map(|c| c.lemma.as_str()).collect();
        let readings: Vec<Option<&str>> = card_data_list.iter().map(|c| c.reading.as_deref()).collect();
        let definitions: Vec<&str> = card_data_list.iter().map(|c| c.definition.as_str()).collect();
        let pos_list: Vec<Option<&str>> = card_data_list.iter().map(|c| c.pos.as_deref()).collect();
        let freq_ranks: Vec<i32> = card_data_list.iter().map(|c| c.frequency_rank).collect();
        let doc_freqs: Vec<i32> = card_data_list.iter().map(|c| c.doc_frequency).collect();

        let card_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO cards (deck_id, lemma, reading, definition, part_of_speech, frequency_rank, doc_frequency)
            SELECT $1, unnest($2::text[]), unnest($3::text[]), unnest($4::text[]), unnest($5::text[]), unnest($6::int[]), unnest($7::int[])
            RETURNING id
            "#,
        )
        .bind(deck.id)
        .bind(&lemmas)
        .bind(&readings)
        .bind(&definitions)
        .bind(&pos_list)
        .bind(&freq_ranks)
        .bind(&doc_freqs)
        .fetch_all(&state.db)
        .await?;

        let cards_created = card_ids.len();

        // Add known words
        let new_lemmas: Vec<&str> = card_data_list.iter().map(|c| c.lemma.as_str()).collect();
        known_words_service::add_known_words(&state.db, auth_user.user_id, language, &new_lemmas).await?;

        // Build idx → card_id map
        let idx_to_card_id: HashMap<usize, uuid::Uuid> = (0..card_data_list.len())
            .zip(card_ids.iter())
            .map(|(idx, id)| (idx, *id))
            .collect();

        // Batch insert sentences
        let mut sent_card_ids: Vec<uuid::Uuid> = Vec::new();
        let mut sent_texts: Vec<String> = Vec::new();
        let mut sent_cloze_texts: Vec<String> = Vec::new();
        let mut sent_cloze_answers: Vec<String> = Vec::new();
        let mut sent_surface_forms: Vec<String> = Vec::new();
        let mut sent_is_primary: Vec<bool> = Vec::new();

        for (card_data_idx, text, cloze_text, cloze_answer, surface_form, is_primary) in sentences {
            if let Some(card_id) = idx_to_card_id.get(card_data_idx) {
                sent_card_ids.push(*card_id);
                sent_texts.push(text.clone());
                sent_cloze_texts.push(cloze_text.clone());
                sent_cloze_answers.push(cloze_answer.clone());
                sent_surface_forms.push(surface_form.clone());
                sent_is_primary.push(*is_primary);
            }
        }

        let sentences_created = sent_card_ids.len();
        if !sent_card_ids.is_empty() {
            sqlx::query(
                r#"
                INSERT INTO sentences (card_id, text, cloze_text, cloze_answer, surface_form, is_primary)
                SELECT unnest($1::uuid[]), unnest($2::text[]), unnest($3::text[]), unnest($4::text[]), unnest($5::text[]), unnest($6::bool[])
                "#,
            )
            .bind(&sent_card_ids)
            .bind(&sent_texts)
            .bind(&sent_cloze_texts)
            .bind(&sent_cloze_answers)
            .bind(&sent_surface_forms)
            .bind(&sent_is_primary)
            .execute(&state.db)
            .await?;
        }

        Ok((cards_created, sentences_created))
    }

    let mut words_skipped_duplicate = 0;

    // Build per-word data: dictionary info + which decks each word belongs to
    struct WordData {
        lemma: String,
        reading: Option<String>,
        definition: String,
        pos: Option<String>,
        frequency_rank: i32,
        doc_frequency: i32,
        has_i_plus_one: bool,
    }

    let mut word_data_list: Vec<WordData> = Vec::new();

    for (rank, (lemma, count)) in words.iter().enumerate() {
        let normalized = normalize_lemma(lemma, language);
        if existing_lemmas.contains(&normalized) {
            words_skipped_duplicate += 1;
            continue;
        }

        let has_i1 = i_plus_one_map.contains_key(lemma);

        // If only i+1 requested and this word has no i+1 sentences, skip
        if want_i_plus_one && !want_word_def && !has_i1 {
            continue;
        }

        let dict_entry = dictionary::lookup(lemma, language);
        let pos_tag = pos_map.get(lemma).map(|s| s.as_str()).unwrap_or("");
        let definitions = dict_entry
            .map(|e| e.definitions_with_pos_preference(pos_tag).join("; "))
            .unwrap_or_default();
        let reading = if is_japanese(language) {
            dict_entry.map(|e| e.reading.clone())
        } else {
            None
        };

        // Skip Japanese words with no definition AND no i+1 sentence
        if is_japanese(language) && definitions.is_empty() && !has_i1 {
            continue;
        }

        word_data_list.push(WordData {
            lemma: lemma.clone(),
            reading,
            definition: definitions,
            pos: pos_map.get(lemma).cloned(),
            frequency_rank: (rank + 1) as i32,
            doc_frequency: *count,
            has_i_plus_one: has_i1,
        });
    }

    if word_data_list.is_empty() {
        return Ok(CardCreationResult {
            cards_created: 0,
            sentences_created: 0,
            i_plus_one_found,
            words_skipped_duplicate,
        });
    }

    let mut total_cards_created = 0;
    let mut total_sentences_created = 0;

    // --- Cloze deck cards (i+1) ---
    if want_i_plus_one {
        if let Some(c_deck) = cloze_deck {
            let mut cloze_card_data: Vec<CardData> = Vec::new();
            let mut cloze_sentences: Vec<(usize, String, String, String, String, bool)> = Vec::new();

            for wd in &word_data_list {
                if !wd.has_i_plus_one {
                    continue;
                }

                let i1_sentences = match i_plus_one_map.get(&wd.lemma) {
                    Some(s) => s,
                    None => continue,
                };

                // Use dictionary lemma as card lemma; surface form is stored in the sentence
                let idx = cloze_card_data.len();
                cloze_card_data.push(CardData {
                    lemma: wd.lemma.clone(),
                    reading: wd.reading.clone(),
                    definition: wd.definition.clone(),
                    pos: wd.pos.clone(),
                    frequency_rank: wd.frequency_rank,
                    doc_frequency: wd.doc_frequency,
                });

                for (i, (sentence, cloze_text, cloze_answer)) in i1_sentences.iter().enumerate() {
                    cloze_sentences.push((idx, sentence.clone(), cloze_text.clone(), cloze_answer.clone(), cloze_answer.clone(), i == 0));
                }
            }

            let (cards, sents) = insert_cards_for_deck(
                state, auth_user, c_deck, &cloze_card_data, &cloze_sentences, language,
            ).await?;
            total_cards_created += cards;
            total_sentences_created += sents;
        }
    }

    // --- Definition deck cards ---
    if want_word_def {
        if let Some(d_deck) = def_deck {
            let mut def_card_data: Vec<CardData> = Vec::new();
            let mut def_sentences: Vec<(usize, String, String, String, String, bool)> = Vec::new();

            for wd in &word_data_list {
                let idx = def_card_data.len();
                // For definition cards, use the dictionary lemma
                def_card_data.push(CardData {
                    lemma: wd.lemma.clone(),
                    reading: wd.reading.clone(),
                    definition: wd.definition.clone(),
                    pos: wd.pos.clone(),
                    frequency_rank: wd.frequency_rank,
                    doc_frequency: wd.doc_frequency,
                });

                let word_sentences = lemma_sentences.get(&wd.lemma);
                let word_surfaces = surface_forms.get(&wd.lemma);

                if let Some(sents) = word_sentences {
                    for (i, sentence) in sents.iter().take(3).enumerate() {
                        let mut cloze_text = sentence.clone();
                        let mut cloze_answer = wd.lemma.clone();

                        if let Some(surfaces) = word_surfaces {
                            for surface in surfaces {
                                if sentence.contains(surface) {
                                    cloze_text = sentence.replace(surface, "＿＿＿");
                                    cloze_answer = surface.clone();
                                    break;
                                }
                            }
                        }

                        let surface = cloze_answer.clone();
                        def_sentences.push((idx, sentence.clone(), cloze_text, cloze_answer, surface, i == 0));
                    }
                }
            }

            let (cards, sents) = insert_cards_for_deck(
                state, auth_user, d_deck, &def_card_data, &def_sentences, language,
            ).await?;
            total_cards_created += cards;
            total_sentences_created += sents;
        }
    }

    tracing::info!(duration_ms = start.elapsed().as_millis() as u64, "create_cards_from_analysis total");

    Ok(CardCreationResult {
        cards_created: total_cards_created,
        sentences_created: total_sentences_created,
        i_plus_one_found,
        words_skipped_duplicate,
    })
}
