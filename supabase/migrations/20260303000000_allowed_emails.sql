-- Migration: Restrict signups to an email allowlist
-- Drop the trigger to open signups: DROP TRIGGER on_auth_user_signup_check ON auth.users;

CREATE TABLE public.allowed_emails (
    email TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

ALTER TABLE allowed_emails ENABLE ROW LEVEL SECURITY;

CREATE OR REPLACE FUNCTION public.check_email_allowed()
RETURNS TRIGGER AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM public.allowed_emails WHERE email = NEW.email
    ) THEN
        RAISE EXCEPTION 'Signups are currently invite-only.';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

CREATE TRIGGER on_auth_user_signup_check
    BEFORE INSERT ON auth.users
    FOR EACH ROW
    EXECUTE FUNCTION public.check_email_allowed();
