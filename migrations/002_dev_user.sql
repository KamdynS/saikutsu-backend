-- Dev user for local development (no auth)
INSERT INTO users (id, email, password_hash, tier)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'dev@localhost',
    'not-used-in-dev-mode',
    'paid'
) ON CONFLICT (id) DO NOTHING;
