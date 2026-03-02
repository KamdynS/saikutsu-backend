-- Migration: Transition from custom auth to Supabase Auth
-- Supabase manages passwords, email verification, and refresh tokens.

-- Make password_hash optional (Supabase manages passwords)
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
ALTER TABLE users ALTER COLUMN password_hash SET DEFAULT '';

-- Drop auth-related columns that Supabase handles
ALTER TABLE users DROP COLUMN IF EXISTS email_verification_token;
ALTER TABLE users DROP COLUMN IF EXISTS password_reset_token;
ALTER TABLE users DROP COLUMN IF EXISTS password_reset_expires_at;

-- Drop refresh_tokens table (Supabase manages sessions)
DROP TABLE IF EXISTS refresh_tokens;

-- Create trigger function to sync Supabase auth.users → public.users
-- When a user signs up via Supabase Auth, this creates their public profile.
CREATE OR REPLACE FUNCTION public.handle_new_user()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO public.users (id, email, password_hash, tier, daily_reviews_reset_at, monthly_reset_at, created_at, updated_at)
    VALUES (
        NEW.id,
        NEW.email,
        '',
        'free',
        NOW(),
        NOW(),
        NOW(),
        NOW()
    )
    ON CONFLICT (id) DO NOTHING;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

-- Create the trigger on auth.users (Supabase CLI can reference auth schema)
CREATE TRIGGER on_auth_user_created
    AFTER INSERT ON auth.users
    FOR EACH ROW
    EXECUTE FUNCTION public.handle_new_user();
