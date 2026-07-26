-- REQ-AI-003：Virtual Key 精确预算与强一致账务基础 schema。
-- 本迁移是 forward-only；迁移前先审计旧 DOUBLE PRECISION 金额，拒绝静默舍入。

-- migration runner 会把整份脚本包在一个事务中；先阻止审计与类型转换之间出现并发脏写。
LOCK TABLE ai_virtual_keys IN ACCESS EXCLUSIVE MODE;

DO $migration$
DECLARE
    candidate RECORD;
    exact_value NUMERIC;
BEGIN
    FOR candidate IN
        SELECT id, 'budget_limit'::TEXT AS column_name,
               budget_limit::TEXT AS raw_value
          FROM ai_virtual_keys
         WHERE budget_limit IS NOT NULL
        UNION ALL
        SELECT id, 'budget_used'::TEXT AS column_name,
               budget_used::TEXT AS raw_value
          FROM ai_virtual_keys
    LOOP
        IF candidate.raw_value IN ('NaN', 'Infinity', '-Infinity') THEN
            RAISE EXCEPTION USING
                ERRCODE = '22003',
                MESSAGE = format(
                    'migration 007 cannot convert ai_virtual_keys.%s for key id %s: invalid value %s',
                    candidate.column_name,
                    candidate.id,
                    candidate.raw_value
                );
        END IF;

        exact_value := candidate.raw_value::NUMERIC;
        IF exact_value < 0
           OR exact_value >= 10000000000000000::NUMERIC
           OR exact_value <> round(exact_value, 12)
        THEN
            RAISE EXCEPTION USING
                ERRCODE = '22003',
                MESSAGE = format(
                    'migration 007 cannot convert ai_virtual_keys.%s for key id %s: invalid value %s',
                    candidate.column_name,
                    candidate.id,
                    candidate.raw_value
                );
        END IF;
    END LOOP;

    FOR candidate IN
        SELECT id, 'rpm_limit'::TEXT AS column_name, rpm_limit::TEXT AS raw_value
          FROM ai_virtual_keys
         WHERE rpm_limit IS NOT NULL AND rpm_limit <= 0
        UNION ALL
        SELECT id, 'tpm_limit'::TEXT AS column_name, tpm_limit::TEXT AS raw_value
          FROM ai_virtual_keys
         WHERE tpm_limit IS NOT NULL AND tpm_limit <= 0
    LOOP
        RAISE EXCEPTION USING
            ERRCODE = '22003',
            MESSAGE = format(
                'migration 007 cannot constrain ai_virtual_keys.%s for key id %s: invalid value %s',
                candidate.column_name,
                candidate.id,
                candidate.raw_value
            );
    END LOOP;
END
$migration$;

ALTER TABLE ai_virtual_keys
    ALTER COLUMN budget_used DROP DEFAULT,
    ALTER COLUMN budget_limit TYPE NUMERIC(28,12)
        USING ((budget_limit::TEXT)::NUMERIC(28,12)),
    ALTER COLUMN budget_used TYPE NUMERIC(28,12)
        USING ((budget_used::TEXT)::NUMERIC(28,12)),
    ALTER COLUMN budget_used SET DEFAULT 0.000000000000,
    ADD CONSTRAINT ai_virtual_keys_budget_limit_valid
        CHECK (
            budget_limit IS NULL
            OR (
                budget_limit <> 'NaN'::NUMERIC
                AND budget_limit >= 0
                AND budget_limit < 10000000000000000::NUMERIC
            )
        ),
    ADD CONSTRAINT ai_virtual_keys_budget_used_valid
        CHECK (
            budget_used <> 'NaN'::NUMERIC
            AND budget_used >= 0
            AND budget_used < 10000000000000000::NUMERIC
        ),
    ADD CONSTRAINT ai_virtual_keys_rpm_limit_valid
        CHECK (rpm_limit IS NULL OR rpm_limit BETWEEN 1 AND 2147483647),
    ADD CONSTRAINT ai_virtual_keys_tpm_limit_valid
        CHECK (tpm_limit IS NULL OR tpm_limit BETWEEN 1 AND 2147483647),
    ADD COLUMN budget_pending_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN budget_unresolved_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN budget_accounting_revision BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN budget_checkpoint_tail_events BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN budget_state_updated_at TIMESTAMPTZ(3)
        NOT NULL DEFAULT clock_timestamp(),
    ADD COLUMN budget_accounting_state TEXT
        GENERATED ALWAYS AS (
            CASE
                WHEN budget_unresolved_count > 0 THEN 'unresolved'
                WHEN budget_pending_count > 0 THEN 'pending'
                ELSE 'clean'
            END
        ) STORED,
    ADD CONSTRAINT ai_virtual_keys_budget_pending_count_nonnegative
        CHECK (budget_pending_count >= 0),
    ADD CONSTRAINT ai_virtual_keys_budget_unresolved_count_nonnegative
        CHECK (budget_unresolved_count >= 0),
    ADD CONSTRAINT ai_virtual_keys_budget_accounting_revision_nonnegative
        CHECK (budget_accounting_revision >= 0),
    ADD CONSTRAINT ai_virtual_keys_budget_checkpoint_tail_events_nonnegative
        CHECK (budget_checkpoint_tail_events >= 0);

CREATE TABLE ai_budget_runtime_settings (
    deployment_namespace        TEXT PRIMARY KEY,
    checkpoint_hard_tail_events BIGINT NOT NULL,
    config_fingerprint          CHAR(64) NOT NULL,
    updated_at                  TIMESTAMPTZ(3) NOT NULL,
    CONSTRAINT ai_budget_runtime_settings_namespace_nonempty
        CHECK (length(btrim(deployment_namespace)) > 0),
    CONSTRAINT ai_budget_runtime_settings_hard_tail_positive
        CHECK (checkpoint_hard_tail_events > 0),
    CONSTRAINT ai_budget_runtime_settings_fingerprint_format
        CHECK (config_fingerprint ~ '^[0-9a-f]{64}$')
);

CREATE TABLE ai_budget_owner_sessions (
    session_id           UUID PRIMARY KEY,
    deployment_namespace TEXT NOT NULL
        REFERENCES ai_budget_runtime_settings(deployment_namespace)
        ON DELETE RESTRICT,
    node_id              UUID NOT NULL,
    started_at           TIMESTAMPTZ(3) NOT NULL,
    last_heartbeat_at    TIMESTAMPTZ(3) NOT NULL,
    expires_at           TIMESTAMPTZ(3) NOT NULL,
    stopped_at           TIMESTAMPTZ(3),
    CONSTRAINT ai_budget_owner_sessions_time_order
        CHECK (
            last_heartbeat_at >= started_at
            AND expires_at > last_heartbeat_at
            AND (stopped_at IS NULL OR stopped_at >= started_at)
        )
);

CREATE INDEX ai_budget_owner_sessions_expiry_idx
    ON ai_budget_owner_sessions(deployment_namespace, expires_at)
    WHERE stopped_at IS NULL;

CREATE TABLE ai_budget_ledger (
    id                           UUID PRIMARY KEY,
    virtual_key_id               UUID NOT NULL,
    virtual_key_name             TEXT NOT NULL,
    virtual_key_prefix           TEXT NOT NULL,
    workspace_id                 UUID,

    kind                         TEXT NOT NULL,
    status                       TEXT NOT NULL,
    request_id                   VARCHAR(32) UNIQUE,
    operation_id                 VARCHAR(128) NOT NULL UNIQUE,
    command_fingerprint          CHAR(64),
    dispatch_operation_id        VARCHAR(128) UNIQUE,
    terminal_operation_id        VARCHAR(128) UNIQUE,
    terminal_command_fingerprint CHAR(64),
    last_account_revision        BIGINT NOT NULL,
    parent_intent_id             UUID
        REFERENCES ai_budget_ledger(id) ON DELETE RESTRICT,
    usage_fact_id                UUID,
    attempt_no                   SMALLINT NOT NULL DEFAULT 0,

    observed_cost_usd            NUMERIC(28,12),
    accounted_cost_usd           NUMERIC(28,12),
    cost_status                  TEXT,
    cost_reasons                 TEXT[] NOT NULL DEFAULT '{}',
    pricing_fingerprint          CHAR(64),
    pricing_snapshot             JSONB,

    dispatch_state               TEXT,
    node_id                      UUID,
    owner_session_id             UUID,
    stale_not_before             TIMESTAMPTZ(3),

    resolution_reason            TEXT,
    resolution_actor             TEXT,
    resolution_entry_id          UUID
        REFERENCES ai_budget_ledger(id) ON DELETE RESTRICT,

    created_at                   TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
    updated_at                   TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
    settled_at                   TIMESTAMPTZ(3),
    resolved_at                  TIMESTAMPTZ(3),

    CONSTRAINT ai_budget_ledger_kind_value
        CHECK (
            kind IN (
                'request',
                'opening_balance',
                'reconciliation',
                'reconciliation_attempt',
                'account_issue',
                'rebuild_audit'
            )
        ),
    CONSTRAINT ai_budget_ledger_status_value
        CHECK (status IN ('pending', 'unresolved', 'settled', 'resolved', 'waived')),
    CONSTRAINT ai_budget_ledger_kind_status_valid
        CHECK (
            (kind = 'request' AND status IN ('pending', 'unresolved', 'settled', 'resolved'))
            OR (kind = 'opening_balance' AND status = 'settled')
            OR (kind = 'reconciliation' AND status IN ('settled', 'waived'))
            OR (kind = 'reconciliation_attempt' AND status = 'resolved')
            OR (kind = 'account_issue' AND status IN ('unresolved', 'resolved'))
            OR (kind = 'rebuild_audit' AND status = 'resolved')
        ),
    CONSTRAINT ai_budget_ledger_request_id_format
        CHECK (
            (kind = 'request' AND request_id ~ '^[0-9a-f]{32}$')
            OR (kind <> 'request' AND request_id IS NULL)
        ),
    CONSTRAINT ai_budget_ledger_operation_id_nonempty
        CHECK (length(btrim(operation_id)) > 0),
    CONSTRAINT ai_budget_ledger_command_fingerprint_format
        CHECK (
            command_fingerprint IS NULL
            OR command_fingerprint ~ '^[0-9a-f]{64}$'
        ),
    CONSTRAINT ai_budget_ledger_terminal_fingerprint_format
        CHECK (
            terminal_command_fingerprint IS NULL
            OR terminal_command_fingerprint ~ '^[0-9a-f]{64}$'
        ),
    CONSTRAINT ai_budget_ledger_pricing_fingerprint_format
        CHECK (
            pricing_fingerprint IS NULL
            OR pricing_fingerprint ~ '^[0-9a-f]{64}$'
        ),
    CONSTRAINT ai_budget_ledger_required_command_fingerprint
        CHECK (
            kind = 'opening_balance'
            OR command_fingerprint IS NOT NULL
        ),
    CONSTRAINT ai_budget_ledger_last_account_revision_nonnegative
        CHECK (last_account_revision >= 0),
    CONSTRAINT ai_budget_ledger_attempt_supported
        CHECK (attempt_no = 0),
    CONSTRAINT ai_budget_ledger_observed_cost_valid
        CHECK (
            observed_cost_usd IS NULL
            OR (
                observed_cost_usd <> 'NaN'::NUMERIC
                AND observed_cost_usd >= 0
                AND observed_cost_usd < 10000000000000000::NUMERIC
            )
        ),
    CONSTRAINT ai_budget_ledger_accounted_cost_valid
        CHECK (
            accounted_cost_usd IS NULL
            OR (
                accounted_cost_usd <> 'NaN'::NUMERIC
                AND accounted_cost_usd >= 0
                AND accounted_cost_usd < 10000000000000000::NUMERIC
            )
        ),
    CONSTRAINT ai_budget_ledger_cost_status_value
        CHECK (
            cost_status IS NULL
            OR cost_status IN ('calculated', 'estimated', 'not_incurred', 'unavailable')
        ),
    CONSTRAINT ai_budget_ledger_pricing_snapshot_size
        CHECK (
            pricing_snapshot IS NULL
            OR octet_length(pricing_snapshot::TEXT) <= 4096
        ),
    CONSTRAINT ai_budget_ledger_dispatch_state_value
        CHECK (dispatch_state IS NULL OR dispatch_state IN ('prepared', 'dispatching')),
    CONSTRAINT ai_budget_ledger_request_dispatch_fields
        CHECK (
            (
                kind = 'request'
                AND (
                    status <> 'pending'
                    OR (
                        dispatch_state IS NOT NULL
                        AND owner_session_id IS NOT NULL
                        AND stale_not_before IS NOT NULL
                    )
                )
            )
            OR (
                kind <> 'request'
                AND dispatch_state IS NULL
                AND dispatch_operation_id IS NULL
                AND node_id IS NULL
                AND owner_session_id IS NULL
                AND stale_not_before IS NULL
            )
        ),
    CONSTRAINT ai_budget_ledger_pending_amounts_empty
        CHECK (
            status <> 'pending'
            OR (observed_cost_usd IS NULL AND accounted_cost_usd IS NULL)
        ),
    CONSTRAINT ai_budget_ledger_unresolved_not_accounted
        CHECK (status <> 'unresolved' OR accounted_cost_usd IS NULL),
    CONSTRAINT ai_budget_ledger_settled_has_amount
        CHECK (status <> 'settled' OR accounted_cost_usd IS NOT NULL),
    CONSTRAINT ai_budget_ledger_waived_zero
        CHECK (
            status <> 'waived'
            OR accounted_cost_usd = 0.000000000000
        ),
    CONSTRAINT ai_budget_ledger_parent_consistency
        CHECK (
            (
                kind IN ('reconciliation', 'reconciliation_attempt')
                AND parent_intent_id IS NOT NULL
            )
            OR (
                kind NOT IN ('reconciliation', 'reconciliation_attempt')
                AND parent_intent_id IS NULL
            )
        ),
    CONSTRAINT ai_budget_ledger_resolution_entry_consistency
        CHECK (
            (
                (
                    (kind = 'request' AND status = 'resolved')
                    OR (kind = 'account_issue' AND status = 'resolved')
                )
                AND resolution_entry_id IS NOT NULL
            )
            OR (
                NOT (
                    (kind = 'request' AND status = 'resolved')
                    OR (kind = 'account_issue' AND status = 'resolved')
                )
                AND resolution_entry_id IS NULL
            )
        ),
    CONSTRAINT ai_budget_ledger_request_terminal_consistency
        CHECK (
            (
                kind = 'request'
                AND status IN ('settled', 'resolved')
                AND terminal_operation_id IS NOT NULL
                AND terminal_command_fingerprint IS NOT NULL
            )
            OR (
                kind = 'request'
                AND status IN ('pending', 'unresolved')
                AND terminal_operation_id IS NULL
                AND terminal_command_fingerprint IS NULL
            )
            OR (
                kind <> 'request'
                AND terminal_operation_id IS NULL
                AND terminal_command_fingerprint IS NULL
            )
        ),
    CONSTRAINT ai_budget_ledger_reconciliation_audit_fields
        CHECK (
            kind NOT IN ('reconciliation', 'reconciliation_attempt', 'rebuild_audit')
            OR (
                resolution_reason IS NOT NULL
                AND octet_length(btrim(resolution_reason)) BETWEEN 1 AND 1024
                AND resolution_actor IS NOT NULL
                AND length(btrim(resolution_actor)) > 0
            )
        ),
    CONSTRAINT ai_budget_ledger_resolution_reason_size
        CHECK (
            resolution_reason IS NULL
            OR octet_length(btrim(resolution_reason)) BETWEEN 1 AND 1024
        ),
    CONSTRAINT ai_budget_ledger_resolution_actor_nonempty
        CHECK (
            resolution_actor IS NULL
            OR length(btrim(resolution_actor)) > 0
        ),
    CONSTRAINT ai_budget_ledger_terminal_time_consistency
        CHECK (
            (status <> 'settled' OR settled_at IS NOT NULL)
            AND (status <> 'resolved' OR resolved_at IS NOT NULL)
        )
);

CREATE INDEX ai_budget_ledger_key_time_idx
    ON ai_budget_ledger(virtual_key_id, created_at DESC, id DESC);

CREATE INDEX ai_budget_ledger_open_idx
    ON ai_budget_ledger(virtual_key_id, status, created_at)
    WHERE status IN ('pending', 'unresolved');

CREATE INDEX ai_budget_ledger_owner_open_idx
    ON ai_budget_ledger(owner_session_id, status, stale_not_before)
    WHERE status = 'pending';

CREATE INDEX ai_budget_ledger_parent_idx
    ON ai_budget_ledger(parent_intent_id);

CREATE INDEX ai_budget_ledger_revision_idx
    ON ai_budget_ledger(virtual_key_id, last_account_revision)
    WHERE status = 'settled';

CREATE TABLE ai_budget_checkpoints (
    virtual_key_id      UUID NOT NULL,
    checkpoint_revision BIGINT NOT NULL,
    accounted_cost_usd  NUMERIC(28,12) NOT NULL,
    operation_id        VARCHAR(128) NOT NULL UNIQUE,
    created_at          TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (virtual_key_id, checkpoint_revision),
    CONSTRAINT ai_budget_checkpoints_revision_nonnegative
        CHECK (checkpoint_revision >= 0),
    CONSTRAINT ai_budget_checkpoints_amount_valid
        CHECK (
            accounted_cost_usd <> 'NaN'::NUMERIC
            AND accounted_cost_usd >= 0
            AND accounted_cost_usd < 10000000000000000::NUMERIC
        ),
    CONSTRAINT ai_budget_checkpoints_operation_id_nonempty
        CHECK (length(btrim(operation_id)) > 0)
);

-- 旧余额以不可变 opening balance 保存；UUID 只由 operation 文本的 MD5 确定，
-- 不依赖 pgcrypto/uuid-ossp extension。
WITH migration_clock AS (
    SELECT clock_timestamp() AS migrated_at
),
opening_balances AS (
    SELECT
        key.id AS virtual_key_id,
        key.name AS virtual_key_name,
        key.key_prefix AS virtual_key_prefix,
        key.ws_id AS workspace_id,
        key.budget_used,
        'opening-balance:v1:' || key.id::TEXT AS operation_id,
        migration_clock.migrated_at
    FROM ai_virtual_keys AS key
    CROSS JOIN migration_clock
    WHERE key.budget_used > 0
)
INSERT INTO ai_budget_ledger (
    id,
    virtual_key_id,
    virtual_key_name,
    virtual_key_prefix,
    workspace_id,
    kind,
    status,
    operation_id,
    last_account_revision,
    accounted_cost_usd,
    cost_status,
    created_at,
    updated_at,
    settled_at
)
SELECT
    (
        substr(md5(operation_id), 1, 8) || '-' ||
        substr(md5(operation_id), 9, 4) || '-' ||
        substr(md5(operation_id), 13, 4) || '-' ||
        substr(md5(operation_id), 17, 4) || '-' ||
        substr(md5(operation_id), 21, 12)
    )::UUID,
    virtual_key_id,
    virtual_key_name,
    virtual_key_prefix,
    workspace_id,
    'opening_balance',
    'settled',
    operation_id,
    0,
    budget_used,
    'calculated',
    migrated_at,
    migrated_at,
    migrated_at
FROM opening_balances;

INSERT INTO ai_budget_checkpoints (
    virtual_key_id,
    checkpoint_revision,
    accounted_cost_usd,
    operation_id
)
SELECT
    id,
    0,
    budget_used,
    'budget-checkpoint-genesis:v1:' || id::TEXT
FROM ai_virtual_keys;
