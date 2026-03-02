-- Add 'text' as a valid source_type for pasted text content
ALTER TABLE decks DROP CONSTRAINT IF EXISTS decks_source_type_check;
ALTER TABLE decks ADD CONSTRAINT decks_source_type_check CHECK (source_type IN ('pdf', 'frequency', 'import', 'text'));
