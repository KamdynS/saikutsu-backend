-- Extend the source_type CHECK constraint to allow 'video' and 'audio' media types
ALTER TABLE decks DROP CONSTRAINT IF EXISTS decks_source_type_check;
ALTER TABLE decks ADD CONSTRAINT decks_source_type_check
  CHECK (source_type IN ('pdf', 'frequency', 'import', 'text', 'video', 'audio'));
