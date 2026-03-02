-- Enable UUID extension
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ============================================
-- USERS
-- ============================================
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    tier VARCHAR(20) DEFAULT 'free' CHECK (tier IN ('free', 'paid')),

    -- Stripe
    stripe_customer_id VARCHAR(255),

    -- Usage tracking (reset periodically)
    daily_reviews_used INT DEFAULT 0,
    daily_reviews_reset_at TIMESTAMPTZ DEFAULT NOW(),
    monthly_uploads_used INT DEFAULT 0,
    monthly_exports_used INT DEFAULT 0,
    monthly_reset_at TIMESTAMPTZ DEFAULT NOW(),

    -- Verification
    email_verified BOOLEAN DEFAULT FALSE,
    email_verified_at TIMESTAMPTZ,
    email_verification_token VARCHAR(255),
    password_reset_token VARCHAR(255),
    password_reset_expires_at TIMESTAMPTZ,

    -- Settings
    settings JSONB DEFAULT '{}',

    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users(email);
CREATE INDEX idx_users_stripe_customer_id ON users(stripe_customer_id) WHERE stripe_customer_id IS NOT NULL;

-- ============================================
-- SUBSCRIPTIONS
-- ============================================
CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    stripe_subscription_id VARCHAR(255) UNIQUE NOT NULL,
    stripe_price_id VARCHAR(255) NOT NULL,
    plan VARCHAR(20) NOT NULL CHECK (plan IN ('monthly', 'yearly')),
    status VARCHAR(30) NOT NULL,

    current_period_start TIMESTAMPTZ NOT NULL,
    current_period_end TIMESTAMPTZ NOT NULL,
    cancel_at_period_end BOOLEAN DEFAULT FALSE,

    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_subscriptions_user_id ON subscriptions(user_id);
CREATE INDEX idx_subscriptions_stripe_id ON subscriptions(stripe_subscription_id);
CREATE INDEX idx_subscriptions_status ON subscriptions(status) WHERE status = 'active';

-- ============================================
-- DECKS
-- ============================================
CREATE TABLE decks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    name VARCHAR(255) NOT NULL,
    description TEXT,
    language VARCHAR(10) NOT NULL,
    source_type VARCHAR(20) NOT NULL CHECK (source_type IN ('pdf', 'frequency', 'import')),

    -- Denormalized counts (updated via triggers or application code)
    card_count INT DEFAULT 0,
    new_count INT DEFAULT 0,
    learning_count INT DEFAULT 0,
    mature_count INT DEFAULT 0,

    -- Deck-specific settings
    settings JSONB DEFAULT '{"new_cards_per_day": 20, "study_mode": "cloze"}',

    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_decks_user_id ON decks(user_id);
CREATE INDEX idx_decks_language ON decks(language);
CREATE INDEX idx_decks_user_language ON decks(user_id, language);

-- ============================================
-- CARDS
-- ============================================
CREATE TABLE cards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deck_id UUID NOT NULL REFERENCES decks(id) ON DELETE CASCADE,

    lemma VARCHAR(100) NOT NULL,
    reading VARCHAR(100),
    definition TEXT NOT NULL,
    part_of_speech VARCHAR(50),

    frequency_rank INT,
    doc_frequency INT DEFAULT 1,

    audio_url VARCHAR(500),
    notes TEXT,
    tags VARCHAR(255)[] DEFAULT '{}',

    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_cards_deck_id ON cards(deck_id);
CREATE INDEX idx_cards_lemma ON cards(lemma);
CREATE INDEX idx_cards_frequency_rank ON cards(frequency_rank) WHERE frequency_rank IS NOT NULL;
CREATE INDEX idx_cards_deck_frequency ON cards(deck_id, frequency_rank);

-- GIN index for tag searching
CREATE INDEX idx_cards_tags ON cards USING GIN(tags);

-- ============================================
-- SENTENCES
-- ============================================
CREATE TABLE sentences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    card_id UUID NOT NULL REFERENCES cards(id) ON DELETE CASCADE,

    text TEXT NOT NULL,
    cloze_text TEXT NOT NULL,
    cloze_answer VARCHAR(100) NOT NULL,
    source_page INT,

    audio_url VARCHAR(500),
    is_primary BOOLEAN DEFAULT FALSE,

    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_sentences_card_id ON sentences(card_id);
CREATE INDEX idx_sentences_primary ON sentences(card_id) WHERE is_primary = TRUE;

-- ============================================
-- CARD STATES (FSRS)
-- ============================================
CREATE TABLE card_states (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    card_id UUID NOT NULL REFERENCES cards(id) ON DELETE CASCADE,

    -- FSRS state
    status VARCHAR(20) DEFAULT 'new' CHECK (status IN ('new', 'learning', 'review', 'relearning')),
    difficulty REAL DEFAULT 0 CHECK (difficulty >= 0 AND difficulty <= 10),
    stability REAL DEFAULT 0 CHECK (stability >= 0),
    due_date DATE,

    -- Tracking
    last_review TIMESTAMPTZ,
    reps INT DEFAULT 0,
    lapses INT DEFAULT 0,

    -- Suspension
    suspended BOOLEAN DEFAULT FALSE,
    suspended_at TIMESTAMPTZ,

    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE(user_id, card_id)
);

CREATE INDEX idx_card_states_user_id ON card_states(user_id);
CREATE INDEX idx_card_states_card_id ON card_states(card_id);
CREATE INDEX idx_card_states_status ON card_states(status);

-- Critical index for daily review query
CREATE INDEX idx_card_states_user_due ON card_states(user_id, due_date)
    WHERE NOT suspended AND status != 'new';

-- Index for finding new cards
CREATE INDEX idx_card_states_user_new ON card_states(user_id, created_at)
    WHERE NOT suspended AND status = 'new';

-- ============================================
-- REVIEWS
-- ============================================
CREATE TABLE reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    card_state_id UUID NOT NULL REFERENCES card_states(id) ON DELETE CASCADE,

    rating SMALLINT NOT NULL CHECK (rating BETWEEN 1 AND 4),
    reviewed_at TIMESTAMPTZ DEFAULT NOW(),
    time_taken_ms INT,

    -- State snapshot for analytics
    scheduled_days REAL,
    elapsed_days REAL,
    difficulty_before REAL,
    stability_before REAL,
    difficulty_after REAL,
    stability_after REAL
);

CREATE INDEX idx_reviews_card_state_id ON reviews(card_state_id);
CREATE INDEX idx_reviews_reviewed_at ON reviews(reviewed_at);

-- For analytics queries
CREATE INDEX idx_reviews_state_date ON reviews(card_state_id, reviewed_at DESC);

-- ============================================
-- UPLOADS
-- ============================================
CREATE TABLE uploads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    deck_id UUID REFERENCES decks(id) ON DELETE SET NULL,

    filename VARCHAR(255) NOT NULL,
    file_size_bytes BIGINT,
    storage_key VARCHAR(500),

    language VARCHAR(10) NOT NULL,
    detected_language VARCHAR(10),
    page_count INT,
    word_count INT,
    unique_words INT,

    status VARCHAR(30) DEFAULT 'pending' CHECK (status IN (
        'pending', 'extracting', 'tokenizing', 'analyzing',
        'generating', 'completed', 'failed'
    )),
    progress INT DEFAULT 0 CHECK (progress BETWEEN 0 AND 100),
    error_message TEXT,

    processing_started_at TIMESTAMPTZ,
    processing_completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_uploads_user_id ON uploads(user_id);
CREATE INDEX idx_uploads_status ON uploads(status) WHERE status NOT IN ('completed', 'failed');
CREATE INDEX idx_uploads_user_created ON uploads(user_id, created_at DESC);

-- ============================================
-- EXPORTS
-- ============================================
CREATE TABLE exports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    deck_id UUID NOT NULL REFERENCES decks(id) ON DELETE CASCADE,

    status VARCHAR(20) DEFAULT 'pending' CHECK (status IN ('pending', 'generating', 'completed', 'failed')),
    storage_key VARCHAR(500),
    download_url VARCHAR(1000),
    expires_at TIMESTAMPTZ,

    card_count INT,
    file_size_bytes BIGINT,
    options JSONB DEFAULT '{}',

    created_at TIMESTAMPTZ DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX idx_exports_user_id ON exports(user_id);
CREATE INDEX idx_exports_deck_id ON exports(deck_id);

-- ============================================
-- KNOWN WORDS
-- ============================================
CREATE TABLE known_words (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    language VARCHAR(10) NOT NULL,
    lemma VARCHAR(100) NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE(user_id, language, lemma)
);

CREATE INDEX idx_known_words_user_language ON known_words(user_id, language);

-- For fast lookup during deck generation
CREATE INDEX idx_known_words_lookup ON known_words(user_id, language, lemma);

-- ============================================
-- REFRESH TOKENS (for JWT auth)
-- ============================================
CREATE TABLE refresh_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash VARCHAR(255) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    revoked_at TIMESTAMPTZ
);

CREATE INDEX idx_refresh_tokens_user_id ON refresh_tokens(user_id);
CREATE INDEX idx_refresh_tokens_hash ON refresh_tokens(token_hash) WHERE revoked_at IS NULL;

-- ============================================
-- HELPER FUNCTIONS
-- ============================================

-- Function to update deck counts
CREATE OR REPLACE FUNCTION update_deck_counts()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' OR TG_OP = 'UPDATE' THEN
        UPDATE decks
        SET
            card_count = (SELECT COUNT(*) FROM cards WHERE deck_id = NEW.deck_id),
            updated_at = NOW()
        WHERE id = NEW.deck_id;
    END IF;

    IF TG_OP = 'DELETE' THEN
        UPDATE decks
        SET
            card_count = (SELECT COUNT(*) FROM cards WHERE deck_id = OLD.deck_id),
            updated_at = NOW()
        WHERE id = OLD.deck_id;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Trigger for card count updates
CREATE TRIGGER trigger_update_deck_counts
AFTER INSERT OR DELETE ON cards
FOR EACH ROW EXECUTE FUNCTION update_deck_counts();

-- Function to update user's updated_at
CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Apply updated_at trigger to relevant tables
CREATE TRIGGER trigger_users_updated_at
BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trigger_decks_updated_at
BEFORE UPDATE ON decks FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trigger_cards_updated_at
BEFORE UPDATE ON cards FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trigger_card_states_updated_at
BEFORE UPDATE ON card_states FOR EACH ROW EXECUTE FUNCTION update_updated_at();
