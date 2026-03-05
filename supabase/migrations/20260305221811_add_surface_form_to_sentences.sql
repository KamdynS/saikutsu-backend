-- Add surface_form column to track the word as it appeared in the sentence
-- (distinct from the lemma/dictionary form on the card)
ALTER TABLE sentences ADD COLUMN surface_form VARCHAR(200);

-- Backfill: use cloze_answer as the surface form for existing rows
UPDATE sentences SET surface_form = cloze_answer WHERE surface_form IS NULL;

-- Make it NOT NULL after backfill
ALTER TABLE sentences ALTER COLUMN surface_form SET NOT NULL;
