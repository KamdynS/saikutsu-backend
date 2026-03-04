-- Backfill known_words from existing cards so dedup works for previously created decks
INSERT INTO known_words (user_id, language, lemma)
SELECT DISTINCT d.user_id, d.language, LOWER(c.lemma)
FROM cards c
JOIN decks d ON c.deck_id = d.id
WHERE c.lemma IS NOT NULL AND c.lemma != ''
ON CONFLICT (user_id, language, lemma) DO NOTHING;
