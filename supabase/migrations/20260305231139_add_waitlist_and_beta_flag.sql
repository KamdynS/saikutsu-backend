-- Waitlist table for pre-launch email collection
CREATE TABLE waitlist (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT UNIQUE NOT NULL,
    referral_source TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_waitlist_email ON waitlist(email);

ALTER TABLE waitlist ENABLE ROW LEVEL SECURITY;

-- Beta tester flag on users
ALTER TABLE users ADD COLUMN is_beta_tester BOOLEAN DEFAULT FALSE;
