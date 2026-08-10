CREATE TABLE chat_settings (
    chat_id BIGINT PRIMARY KEY,
    auto_model_switch BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE messages (
    id BIGSERIAL PRIMARY KEY,
    chat_id BIGINT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX messages_chat_recent_idx
    ON messages (chat_id, created_at DESC, id DESC);

CREATE TABLE processed_updates (
    update_id BIGINT PRIMARY KEY,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE jobs (
    update_id BIGINT PRIMARY KEY REFERENCES processed_updates(update_id) ON DELETE CASCADE,
    chat_id BIGINT NOT NULL,
    chat_kind TEXT NOT NULL,
    actor_user_id BIGINT,
    message_id BIGINT NOT NULL,
    thread_id BIGINT,
    kind TEXT NOT NULL,
    input TEXT,
    answer TEXT,
    model_used TEXT,
    sent_chunks INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'processing')),
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX jobs_available_idx
    ON jobs (available_at, created_at)
    WHERE status = 'pending';

CREATE INDEX jobs_chat_processing_idx
    ON jobs (chat_id, lease_until)
    WHERE status = 'processing';

CREATE TABLE model_usage (
    model TEXT NOT NULL,
    usage_date DATE NOT NULL,
    requests BIGINT NOT NULL DEFAULT 0,
    prompt_tokens BIGINT NOT NULL DEFAULT 0,
    completion_tokens BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (model, usage_date)
);

