-- Add 'subtitle' source type to decks
ALTER TABLE decks DROP CONSTRAINT IF EXISTS decks_source_type_check;
ALTER TABLE decks ADD CONSTRAINT decks_source_type_check
  CHECK (source_type IN ('pdf', 'frequency', 'import', 'text', 'video', 'audio', 'subtitle'));

-- Table for encrypted user API keys (Jimaku BYOK, future providers)
CREATE TABLE user_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider VARCHAR(50) NOT NULL,
    encrypted_key TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(user_id, provider)
);

CREATE INDEX idx_user_api_keys_user_id ON user_api_keys(user_id);

-- RLS for user_api_keys
ALTER TABLE user_api_keys ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can manage their own API keys"
  ON user_api_keys
  FOR ALL
  USING (user_id = auth.uid())
  WITH CHECK (user_id = auth.uid());
