-- Migration: Enable Row Level Security on all public tables
-- The Rust backend connects with service_role (bypasses RLS).
-- This locks down the PostgREST API exposed via the anon/authenticated keys.

-- ============================================
-- ENABLE RLS ON ALL TABLES
-- ============================================
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE decks ENABLE ROW LEVEL SECURITY;
ALTER TABLE cards ENABLE ROW LEVEL SECURITY;
ALTER TABLE sentences ENABLE ROW LEVEL SECURITY;
ALTER TABLE card_states ENABLE ROW LEVEL SECURITY;
ALTER TABLE reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE uploads ENABLE ROW LEVEL SECURITY;
ALTER TABLE exports ENABLE ROW LEVEL SECURITY;
ALTER TABLE known_words ENABLE ROW LEVEL SECURITY;

-- ============================================
-- USERS: users can only read/update their own row
-- ============================================
CREATE POLICY "users_select_own" ON users
    FOR SELECT TO authenticated
    USING (auth.uid() = id);

CREATE POLICY "users_update_own" ON users
    FOR UPDATE TO authenticated
    USING (auth.uid() = id)
    WITH CHECK (auth.uid() = id);

-- ============================================
-- SUBSCRIPTIONS: users can only read their own
-- ============================================
CREATE POLICY "subscriptions_select_own" ON subscriptions
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

-- ============================================
-- DECKS: full CRUD on own decks
-- ============================================
CREATE POLICY "decks_select_own" ON decks
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

CREATE POLICY "decks_insert_own" ON decks
    FOR INSERT TO authenticated
    WITH CHECK (auth.uid() = user_id);

CREATE POLICY "decks_update_own" ON decks
    FOR UPDATE TO authenticated
    USING (auth.uid() = user_id)
    WITH CHECK (auth.uid() = user_id);

CREATE POLICY "decks_delete_own" ON decks
    FOR DELETE TO authenticated
    USING (auth.uid() = user_id);

-- ============================================
-- CARDS: access via deck ownership
-- ============================================
CREATE POLICY "cards_select_own" ON cards
    FOR SELECT TO authenticated
    USING (EXISTS (
        SELECT 1 FROM decks WHERE decks.id = cards.deck_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "cards_insert_own" ON cards
    FOR INSERT TO authenticated
    WITH CHECK (EXISTS (
        SELECT 1 FROM decks WHERE decks.id = cards.deck_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "cards_update_own" ON cards
    FOR UPDATE TO authenticated
    USING (EXISTS (
        SELECT 1 FROM decks WHERE decks.id = cards.deck_id AND decks.user_id = auth.uid()
    ))
    WITH CHECK (EXISTS (
        SELECT 1 FROM decks WHERE decks.id = cards.deck_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "cards_delete_own" ON cards
    FOR DELETE TO authenticated
    USING (EXISTS (
        SELECT 1 FROM decks WHERE decks.id = cards.deck_id AND decks.user_id = auth.uid()
    ));

-- ============================================
-- SENTENCES: access via card → deck ownership
-- ============================================
CREATE POLICY "sentences_select_own" ON sentences
    FOR SELECT TO authenticated
    USING (EXISTS (
        SELECT 1 FROM cards
        JOIN decks ON decks.id = cards.deck_id
        WHERE cards.id = sentences.card_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "sentences_insert_own" ON sentences
    FOR INSERT TO authenticated
    WITH CHECK (EXISTS (
        SELECT 1 FROM cards
        JOIN decks ON decks.id = cards.deck_id
        WHERE cards.id = sentences.card_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "sentences_update_own" ON sentences
    FOR UPDATE TO authenticated
    USING (EXISTS (
        SELECT 1 FROM cards
        JOIN decks ON decks.id = cards.deck_id
        WHERE cards.id = sentences.card_id AND decks.user_id = auth.uid()
    ))
    WITH CHECK (EXISTS (
        SELECT 1 FROM cards
        JOIN decks ON decks.id = cards.deck_id
        WHERE cards.id = sentences.card_id AND decks.user_id = auth.uid()
    ));

CREATE POLICY "sentences_delete_own" ON sentences
    FOR DELETE TO authenticated
    USING (EXISTS (
        SELECT 1 FROM cards
        JOIN decks ON decks.id = cards.deck_id
        WHERE cards.id = sentences.card_id AND decks.user_id = auth.uid()
    ));

-- ============================================
-- CARD STATES: users can only access their own
-- ============================================
CREATE POLICY "card_states_select_own" ON card_states
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

CREATE POLICY "card_states_insert_own" ON card_states
    FOR INSERT TO authenticated
    WITH CHECK (auth.uid() = user_id);

CREATE POLICY "card_states_update_own" ON card_states
    FOR UPDATE TO authenticated
    USING (auth.uid() = user_id)
    WITH CHECK (auth.uid() = user_id);

CREATE POLICY "card_states_delete_own" ON card_states
    FOR DELETE TO authenticated
    USING (auth.uid() = user_id);

-- ============================================
-- REVIEWS: access via card_state ownership
-- ============================================
CREATE POLICY "reviews_select_own" ON reviews
    FOR SELECT TO authenticated
    USING (EXISTS (
        SELECT 1 FROM card_states
        WHERE card_states.id = reviews.card_state_id AND card_states.user_id = auth.uid()
    ));

CREATE POLICY "reviews_insert_own" ON reviews
    FOR INSERT TO authenticated
    WITH CHECK (EXISTS (
        SELECT 1 FROM card_states
        WHERE card_states.id = reviews.card_state_id AND card_states.user_id = auth.uid()
    ));

-- ============================================
-- UPLOADS: users can only access their own
-- ============================================
CREATE POLICY "uploads_select_own" ON uploads
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

CREATE POLICY "uploads_insert_own" ON uploads
    FOR INSERT TO authenticated
    WITH CHECK (auth.uid() = user_id);

-- ============================================
-- EXPORTS: users can only access their own
-- ============================================
CREATE POLICY "exports_select_own" ON exports
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

CREATE POLICY "exports_insert_own" ON exports
    FOR INSERT TO authenticated
    WITH CHECK (auth.uid() = user_id);

-- ============================================
-- KNOWN WORDS: full CRUD on own words
-- ============================================
CREATE POLICY "known_words_select_own" ON known_words
    FOR SELECT TO authenticated
    USING (auth.uid() = user_id);

CREATE POLICY "known_words_insert_own" ON known_words
    FOR INSERT TO authenticated
    WITH CHECK (auth.uid() = user_id);

CREATE POLICY "known_words_delete_own" ON known_words
    FOR DELETE TO authenticated
    USING (auth.uid() = user_id);
