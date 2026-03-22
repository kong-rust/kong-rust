CREATE TABLE IF NOT EXISTS ai_providers (
    id            UUID                     PRIMARY KEY,
    created_at    TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at    TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name          TEXT                     NOT NULL UNIQUE,
    provider_type TEXT                     NOT NULL,
    endpoint_url  TEXT,
    auth_config   JSONB                    NOT NULL DEFAULT '{}',
    default_model TEXT,
    config        JSONB                    NOT NULL DEFAULT '{}',
    enabled       BOOLEAN                  NOT NULL DEFAULT true,
    tags          TEXT[],
    ws_id         UUID                     REFERENCES workspaces(id)
);

CREATE TABLE IF NOT EXISTS ai_models (
    id            UUID                     PRIMARY KEY,
    created_at    TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at    TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name          TEXT                     NOT NULL,
    provider_id   UUID                     NOT NULL REFERENCES ai_providers(id) ON DELETE CASCADE,
    model_name    TEXT                     NOT NULL,
    priority      INTEGER                  NOT NULL DEFAULT 0,
    weight        INTEGER                  NOT NULL DEFAULT 100,
    input_cost    DOUBLE PRECISION,
    output_cost   DOUBLE PRECISION,
    max_tokens    INTEGER,
    config        JSONB                    NOT NULL DEFAULT '{}',
    enabled       BOOLEAN                  NOT NULL DEFAULT true,
    tags          TEXT[],
    ws_id         UUID                     REFERENCES workspaces(id),
    UNIQUE(name, provider_id, ws_id)
);

CREATE TABLE IF NOT EXISTS ai_virtual_keys (
    id             UUID                     PRIMARY KEY,
    created_at     TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at     TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name           TEXT                     NOT NULL UNIQUE,
    key_hash       TEXT                     NOT NULL UNIQUE,
    key_prefix     TEXT                     NOT NULL,
    consumer_id    UUID                     REFERENCES consumers(id) ON DELETE SET NULL,
    allowed_models TEXT[]                   DEFAULT '{}',
    tpm_limit      INTEGER,
    rpm_limit      INTEGER,
    budget_limit   DOUBLE PRECISION,
    budget_used    DOUBLE PRECISION         NOT NULL DEFAULT 0,
    enabled        BOOLEAN                  NOT NULL DEFAULT true,
    expires_at     TIMESTAMP WITH TIME ZONE,
    tags           TEXT[],
    ws_id          UUID                     REFERENCES workspaces(id)
);

CREATE INDEX IF NOT EXISTS idx_ai_models_name ON ai_models(name);
CREATE INDEX IF NOT EXISTS idx_ai_models_provider_id ON ai_models(provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_virtual_keys_key_hash ON ai_virtual_keys(key_hash);
CREATE INDEX IF NOT EXISTS idx_ai_virtual_keys_consumer_id ON ai_virtual_keys(consumer_id);
