-- Add video and audio source types for decks
ALTER TABLE decks DROP CONSTRAINT IF EXISTS decks_source_type_check;
ALTER TABLE decks ADD CONSTRAINT decks_source_type_check
    CHECK (source_type IN ('pdf', 'frequency', 'import', 'text', 'video', 'audio'));

-- Add transcribing status for uploads
ALTER TABLE uploads DROP CONSTRAINT IF EXISTS uploads_status_check;
ALTER TABLE uploads ADD CONSTRAINT uploads_status_check
    CHECK (status IN ('pending', 'extracting', 'transcribing', 'tokenizing', 'analyzing', 'generating', 'completed', 'failed'));

-- Add media metadata columns to uploads
ALTER TABLE uploads ADD COLUMN IF NOT EXISTS media_type VARCHAR(10);
ALTER TABLE uploads ADD COLUMN IF NOT EXISTS duration_seconds INT;
