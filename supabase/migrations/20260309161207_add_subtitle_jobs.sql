-- Background job tracking for subtitle deck creation
CREATE TABLE subtitle_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(20) NOT NULL DEFAULT 'queued',
    -- Progress tracking
    progress_current INT NOT NULL DEFAULT 0,
    progress_total INT NOT NULL DEFAULT 0,
    progress_stage VARCHAR(50) NOT NULL DEFAULT 'queued',
    -- Result data (populated on completion)
    deck_ids JSONB DEFAULT '[]'::jsonb,
    cards_created INT NOT NULL DEFAULT 0,
    sentences_created INT NOT NULL DEFAULT 0,
    i_plus_one_found INT NOT NULL DEFAULT 0,
    words_skipped_duplicate INT NOT NULL DEFAULT 0,
    -- Error info
    error_message TEXT,
    -- Original request (so the background task has everything it needs)
    request_data JSONB NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    -- status: queued, downloading, processing, complete, failed
    CONSTRAINT subtitle_jobs_status_check CHECK (status IN ('queued', 'downloading', 'processing', 'complete', 'failed'))
);

CREATE INDEX idx_subtitle_jobs_user_id ON subtitle_jobs(user_id);
CREATE INDEX idx_subtitle_jobs_status ON subtitle_jobs(status);

-- RLS
ALTER TABLE subtitle_jobs ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can view their own subtitle jobs"
  ON subtitle_jobs
  FOR SELECT
  USING (user_id = auth.uid());
