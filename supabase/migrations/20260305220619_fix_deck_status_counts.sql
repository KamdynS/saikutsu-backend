-- Fix deck status counts (new_count, learning_count, mature_count)
-- These were never being updated because the trigger only tracked card_count on the cards table.

-- New function to update status counts when card_states change
CREATE OR REPLACE FUNCTION update_deck_status_counts()
RETURNS TRIGGER AS $$
DECLARE
    v_deck_id UUID;
BEGIN
    -- Get the deck_id from the card
    IF TG_OP = 'DELETE' THEN
        SELECT deck_id INTO v_deck_id FROM cards WHERE id = OLD.card_id;
    ELSE
        SELECT deck_id INTO v_deck_id FROM cards WHERE id = NEW.card_id;
    END IF;

    IF v_deck_id IS NULL THEN
        RETURN NEW;
    END IF;

    UPDATE decks
    SET
        new_count = (
            SELECT COUNT(*) FROM card_states cs
            JOIN cards c ON c.id = cs.card_id
            WHERE c.deck_id = v_deck_id AND cs.status = 'new' AND NOT cs.suspended
        ),
        learning_count = (
            SELECT COUNT(*) FROM card_states cs
            JOIN cards c ON c.id = cs.card_id
            WHERE c.deck_id = v_deck_id AND cs.status IN ('learning', 'relearning') AND NOT cs.suspended
        ),
        mature_count = (
            SELECT COUNT(*) FROM card_states cs
            JOIN cards c ON c.id = cs.card_id
            WHERE c.deck_id = v_deck_id AND cs.status = 'review' AND NOT cs.suspended
        ),
        updated_at = NOW()
    WHERE id = v_deck_id;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_deck_status_counts
AFTER INSERT OR UPDATE OR DELETE ON card_states
FOR EACH ROW EXECUTE FUNCTION update_deck_status_counts();

-- Backfill existing decks with correct counts
UPDATE decks d
SET
    new_count = (
        SELECT COUNT(*) FROM card_states cs
        JOIN cards c ON c.id = cs.card_id
        WHERE c.deck_id = d.id AND cs.status = 'new' AND NOT cs.suspended
    ),
    learning_count = (
        SELECT COUNT(*) FROM card_states cs
        JOIN cards c ON c.id = cs.card_id
        WHERE c.deck_id = d.id AND cs.status IN ('learning', 'relearning') AND NOT cs.suspended
    ),
    mature_count = (
        SELECT COUNT(*) FROM card_states cs
        JOIN cards c ON c.id = cs.card_id
        WHERE c.deck_id = d.id AND cs.status = 'review' AND NOT cs.suspended
    );
