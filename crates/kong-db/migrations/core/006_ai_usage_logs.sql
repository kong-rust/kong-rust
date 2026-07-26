DO $migration$
DECLARE
    invalid_model_id UUID;
    invalid_column TEXT;
    invalid_value TEXT;
BEGIN
    SELECT candidate.id, candidate.column_name, candidate.cost::text
      INTO invalid_model_id, invalid_column, invalid_value
      FROM (
          SELECT id, 'input_cost'::TEXT AS column_name, input_cost AS cost
            FROM ai_models
          UNION ALL
          SELECT id, 'output_cost'::TEXT AS column_name, output_cost AS cost
            FROM ai_models
      ) AS candidate
     WHERE candidate.cost IS NOT NULL
       AND CASE
           WHEN candidate.cost::text IN ('NaN', 'Infinity', '-Infinity') THEN TRUE
           ELSE candidate.cost < 0
             OR (candidate.cost::text)::numeric >= 10000000000000000::numeric
             OR (candidate.cost::text)::numeric
                  <> round((candidate.cost::text)::numeric, 12)
       END
     ORDER BY candidate.id::text, candidate.column_name
     LIMIT 1;

    IF invalid_model_id IS NOT NULL THEN
        RAISE EXCEPTION
            'migration 006 cannot convert ai_models.% for model id %: invalid value %',
            invalid_column,
            invalid_model_id,
            invalid_value
            USING ERRCODE = '22003';
    END IF;
END
$migration$;

ALTER TABLE ai_models
    ALTER COLUMN input_cost TYPE NUMERIC(28,12)
        USING ((input_cost::text)::numeric(28,12)),
    ALTER COLUMN output_cost TYPE NUMERIC(28,12)
        USING ((output_cost::text)::numeric(28,12));

ALTER TABLE ai_models
    ADD CONSTRAINT ai_models_input_cost_valid
        CHECK (
            input_cost IS NULL
            OR (input_cost >= 0 AND input_cost <> 'NaN'::numeric)
        ),
    ADD CONSTRAINT ai_models_output_cost_valid
        CHECK (
            output_cost IS NULL
            OR (output_cost >= 0 AND output_cost <> 'NaN'::numeric)
        );

CREATE TABLE ai_usage_logs (
    id                                UUID PRIMARY KEY,
    ingest_seq                        BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    request_id                        VARCHAR(32) NOT NULL UNIQUE,
    node_id                           UUID NOT NULL,
    started_at                        TIMESTAMPTZ(3) NOT NULL,
    finished_at                       TIMESTAMPTZ(3) NOT NULL,
    recorded_at                       TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
    workspace_id                      UUID,
    route_id                          UUID,
    route_name                        TEXT,
    service_id                        UUID,
    service_name                      TEXT,
    provider_id                       UUID,
    provider_name                     TEXT,
    provider_type                     TEXT,
    model_id                          UUID,
    requested_model                   TEXT,
    model_group                       TEXT,
    actual_model                      TEXT,
    attempt_count                     SMALLINT NOT NULL,
    virtual_key_id                    UUID,
    virtual_key_name                  TEXT,
    virtual_key_prefix                TEXT,
    consumer_id                       UUID,
    prompt_tokens                     BIGINT,
    completion_tokens                 BIGINT,
    total_tokens                      BIGINT,
    reasoning_tokens                  BIGINT,
    cache_read_input_tokens           BIGINT,
    cache_write_input_tokens          BIGINT,
    prompt_tokens_source              TEXT,
    completion_tokens_source          TEXT,
    total_tokens_source               TEXT,
    usage_source                      TEXT NOT NULL,
    usage_unavailable_reasons         TEXT[] NOT NULL DEFAULT '{}',
    input_price_per_million           NUMERIC(28,12),
    input_price_source                TEXT,
    input_price_version               TEXT,
    input_price_snapshot_date         DATE,
    input_price_effective_from        TIMESTAMPTZ,
    input_price_effective_to          TIMESTAMPTZ,
    output_price_per_million          NUMERIC(28,12),
    output_price_source               TEXT,
    output_price_version              TEXT,
    output_price_snapshot_date        DATE,
    output_price_effective_from       TIMESTAMPTZ,
    output_price_effective_to         TIMESTAMPTZ,
    pricing_status                    TEXT NOT NULL,
    pricing_unsupported_reasons       TEXT[] NOT NULL DEFAULT '{}',
    currency                          CHAR(3) NOT NULL DEFAULT 'USD',
    cost_usd                          NUMERIC(28,12),
    cost_status                       TEXT NOT NULL,
    cost_unavailable_reasons          TEXT[] NOT NULL DEFAULT '{}',
    status_code                       SMALLINT,
    upstream_status_code              SMALLINT,
    outcome                           TEXT NOT NULL,
    e2e_ms                            BIGINT NOT NULL,
    ttft_ms                           BIGINT,
    upstream_attempted                BOOLEAN NOT NULL,
    stream                            BOOLEAN,
    cache_status                      TEXT NOT NULL,

    CONSTRAINT ai_usage_logs_request_id_format
        CHECK (request_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT ai_usage_logs_prompt_tokens_nonnegative
        CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0),
    CONSTRAINT ai_usage_logs_completion_tokens_nonnegative
        CHECK (completion_tokens IS NULL OR completion_tokens >= 0),
    CONSTRAINT ai_usage_logs_total_tokens_nonnegative
        CHECK (total_tokens IS NULL OR total_tokens >= 0),
    CONSTRAINT ai_usage_logs_reasoning_tokens_nonnegative
        CHECK (reasoning_tokens IS NULL OR reasoning_tokens >= 0),
    CONSTRAINT ai_usage_logs_cache_read_input_tokens_nonnegative
        CHECK (
            cache_read_input_tokens IS NULL
            OR cache_read_input_tokens >= 0
        ),
    CONSTRAINT ai_usage_logs_cache_write_input_tokens_nonnegative
        CHECK (
            cache_write_input_tokens IS NULL
            OR cache_write_input_tokens >= 0
        ),
    CONSTRAINT ai_usage_logs_e2e_ms_nonnegative
        CHECK (e2e_ms >= 0),
    CONSTRAINT ai_usage_logs_ttft_ms_nonnegative
        CHECK (ttft_ms IS NULL OR ttft_ms >= 0),
    CONSTRAINT ai_usage_logs_input_price_nonnegative
        CHECK (
            input_price_per_million IS NULL
            OR (
                input_price_per_million >= 0
                AND input_price_per_million <> 'NaN'::numeric
            )
        ),
    CONSTRAINT ai_usage_logs_output_price_nonnegative
        CHECK (
            output_price_per_million IS NULL
            OR (
                output_price_per_million >= 0
                AND output_price_per_million <> 'NaN'::numeric
            )
        ),
    CONSTRAINT ai_usage_logs_cost_nonnegative
        CHECK (
            cost_usd IS NULL
            OR (cost_usd >= 0 AND cost_usd <> 'NaN'::numeric)
        ),
    CONSTRAINT ai_usage_logs_time_order
        CHECK (finished_at >= started_at),
    CONSTRAINT ai_usage_logs_status_code_range
        CHECK (status_code IS NULL OR status_code BETWEEN 100 AND 599),
    CONSTRAINT ai_usage_logs_upstream_status_code_range
        CHECK (
            upstream_status_code IS NULL
            OR upstream_status_code BETWEEN 100 AND 599
        ),
    CONSTRAINT ai_usage_logs_attempt_count_supported
        CHECK (attempt_count IN (0, 1)),
    CONSTRAINT ai_usage_logs_attempt_upstream_consistency
        CHECK (upstream_attempted = (attempt_count = 1)),
    CONSTRAINT ai_usage_logs_prompt_source_value
        CHECK (
            prompt_tokens_source IS NULL
            OR prompt_tokens_source IN ('provider', 'estimated', 'mixed')
        ),
    CONSTRAINT ai_usage_logs_completion_source_value
        CHECK (
            completion_tokens_source IS NULL
            OR completion_tokens_source IN ('provider', 'estimated', 'mixed')
        ),
    CONSTRAINT ai_usage_logs_total_source_value
        CHECK (
            total_tokens_source IS NULL
            OR total_tokens_source IN ('provider', 'estimated', 'mixed')
        ),
    CONSTRAINT ai_usage_logs_usage_source_value
        CHECK (
            usage_source IN ('provider', 'estimated', 'mixed', 'unavailable')
        ),
    CONSTRAINT ai_usage_logs_pricing_status_value
        CHECK (
            pricing_status IN (
                'matched',
                'unmatched',
                'unsupported',
                'not_applicable'
            )
        ),
    CONSTRAINT ai_usage_logs_cost_status_value
        CHECK (
            cost_status IN (
                'calculated',
                'estimated',
                'not_incurred',
                'unavailable'
            )
        ),
    CONSTRAINT ai_usage_logs_outcome_value
        CHECK (
            outcome IN (
                'success',
                'gateway_rejected',
                'gateway_error',
                'upstream_error',
                'client_disconnected',
                'stream_interrupted'
            )
        ),
    CONSTRAINT ai_usage_logs_cache_status_value
        CHECK (
            cache_status IN (
                'not_configured',
                'unavailable',
                'bypass',
                'miss',
                'hit'
            )
        ),
    CONSTRAINT ai_usage_logs_prompt_token_source_presence
        CHECK (
            (prompt_tokens IS NULL) = (prompt_tokens_source IS NULL)
        ),
    CONSTRAINT ai_usage_logs_completion_token_source_presence
        CHECK (
            (completion_tokens IS NULL) = (completion_tokens_source IS NULL)
        ),
    CONSTRAINT ai_usage_logs_total_token_source_presence
        CHECK (
            (total_tokens IS NULL) = (total_tokens_source IS NULL)
        ),
    CONSTRAINT ai_usage_logs_usage_availability_consistency
        CHECK (
            (
                usage_source = 'unavailable'
                AND prompt_tokens IS NULL
                AND completion_tokens IS NULL
                AND total_tokens IS NULL
                AND cardinality(usage_unavailable_reasons) > 0
            )
            OR (
                usage_source <> 'unavailable'
                AND (
                    prompt_tokens IS NOT NULL
                    OR completion_tokens IS NOT NULL
                    OR total_tokens IS NOT NULL
                )
                AND cardinality(usage_unavailable_reasons) = 0
            )
        ),
    CONSTRAINT ai_usage_logs_provider_source_consistency
        CHECK (
            usage_source <> 'provider'
            OR (
                (prompt_tokens_source IS NULL OR prompt_tokens_source = 'provider')
                AND (
                    completion_tokens_source IS NULL
                    OR completion_tokens_source = 'provider'
                )
                AND (total_tokens_source IS NULL OR total_tokens_source = 'provider')
            )
        ),
    CONSTRAINT ai_usage_logs_estimated_source_consistency
        CHECK (
            usage_source <> 'estimated'
            OR (
                (prompt_tokens_source IS NULL OR prompt_tokens_source = 'estimated')
                AND (
                    completion_tokens_source IS NULL
                    OR completion_tokens_source = 'estimated'
                )
                AND (
                    total_tokens_source IS NULL
                    OR total_tokens_source = 'estimated'
                )
            )
        ),
    CONSTRAINT ai_usage_logs_mixed_source_consistency
        CHECK (
            usage_source <> 'mixed'
            OR COALESCE(prompt_tokens_source = 'mixed', FALSE)
            OR COALESCE(completion_tokens_source = 'mixed', FALSE)
            OR COALESCE(total_tokens_source = 'mixed', FALSE)
            OR (
                (
                    COALESCE(prompt_tokens_source = 'provider', FALSE)
                    OR COALESCE(completion_tokens_source = 'provider', FALSE)
                    OR COALESCE(total_tokens_source = 'provider', FALSE)
                )
                AND (
                    COALESCE(prompt_tokens_source = 'estimated', FALSE)
                    OR COALESCE(completion_tokens_source = 'estimated', FALSE)
                    OR COALESCE(total_tokens_source = 'estimated', FALSE)
                )
            )
        ),
    CONSTRAINT ai_usage_logs_input_price_bundle_consistency
        CHECK (
            (
                input_price_per_million IS NULL
                AND input_price_source IS NULL
                AND input_price_version IS NULL
                AND input_price_snapshot_date IS NULL
                AND input_price_effective_from IS NULL
                AND input_price_effective_to IS NULL
            )
            OR (
                input_price_per_million IS NOT NULL
                AND input_price_source IS NOT NULL
                AND input_price_version IS NOT NULL
                AND input_price_snapshot_date IS NOT NULL
                AND input_price_effective_from IS NOT NULL
                AND (
                    input_price_effective_to IS NULL
                    OR input_price_effective_to > input_price_effective_from
                )
            )
        ),
    CONSTRAINT ai_usage_logs_output_price_bundle_consistency
        CHECK (
            (
                output_price_per_million IS NULL
                AND output_price_source IS NULL
                AND output_price_version IS NULL
                AND output_price_snapshot_date IS NULL
                AND output_price_effective_from IS NULL
                AND output_price_effective_to IS NULL
            )
            OR (
                output_price_per_million IS NOT NULL
                AND output_price_source IS NOT NULL
                AND output_price_version IS NOT NULL
                AND output_price_snapshot_date IS NOT NULL
                AND output_price_effective_from IS NOT NULL
                AND (
                    output_price_effective_to IS NULL
                    OR output_price_effective_to > output_price_effective_from
                )
            )
        ),
    CONSTRAINT ai_usage_logs_pricing_bundle_consistency
        CHECK (
            (
                pricing_status = 'matched'
                AND input_price_per_million IS NOT NULL
                AND output_price_per_million IS NOT NULL
            )
            OR (
                pricing_status = 'unmatched'
                AND (
                    input_price_per_million IS NULL
                    OR output_price_per_million IS NULL
                )
            )
            OR pricing_status = 'unsupported'
            OR (
                pricing_status = 'not_applicable'
                AND input_price_per_million IS NULL
                AND output_price_per_million IS NULL
            )
        ),
    CONSTRAINT ai_usage_logs_pricing_reason_consistency
        CHECK (
            (
                pricing_status = 'unsupported'
                AND cardinality(pricing_unsupported_reasons) > 0
            )
            OR (
                pricing_status <> 'unsupported'
                AND cardinality(pricing_unsupported_reasons) = 0
            )
        ),
    CONSTRAINT ai_usage_logs_not_applicable_cost_consistency
        CHECK (
            (pricing_status = 'not_applicable')
            = (cost_status = 'not_incurred')
        ),
    CONSTRAINT ai_usage_logs_not_incurred_consistency
        CHECK (
            cost_status <> 'not_incurred'
            OR (
                cost_usd IS NOT NULL
                AND cost_usd = 0
                AND upstream_attempted = FALSE
            )
        ),
    CONSTRAINT ai_usage_logs_not_attempted_consistency
        CHECK (
            upstream_attempted
            OR (
                pricing_status = 'not_applicable'
                AND cost_status = 'not_incurred'
            )
        ),
    CONSTRAINT ai_usage_logs_calculated_cost_consistency
        CHECK (
            cost_status <> 'calculated'
            OR (
                pricing_status = 'matched'
                AND usage_source = 'provider'
                AND prompt_tokens IS NOT NULL
                AND completion_tokens IS NOT NULL
                AND cost_usd IS NOT NULL
            )
        ),
    CONSTRAINT ai_usage_logs_estimated_cost_consistency
        CHECK (
            cost_status <> 'estimated'
            OR (
                pricing_status = 'matched'
                AND usage_source IN ('estimated', 'mixed')
                AND prompt_tokens IS NOT NULL
                AND completion_tokens IS NOT NULL
                AND cost_usd IS NOT NULL
            )
        ),
    CONSTRAINT ai_usage_logs_unmatched_unsupported_cost_consistency
        CHECK (
            pricing_status NOT IN ('unmatched', 'unsupported')
            OR cost_status = 'unavailable'
        ),
    CONSTRAINT ai_usage_logs_cost_availability_consistency
        CHECK (
            (
                cost_status = 'unavailable'
                AND cost_usd IS NULL
                AND cardinality(cost_unavailable_reasons) > 0
            )
            OR (
                cost_status <> 'unavailable'
                AND cardinality(cost_unavailable_reasons) = 0
            )
        ),
    CONSTRAINT ai_usage_logs_currency_value
        CHECK (currency = 'USD'),
    CONSTRAINT ai_usage_logs_usage_reason_values
        CHECK (
            array_position(usage_unavailable_reasons, NULL) IS NULL
            AND usage_unavailable_reasons <@ ARRAY[
                'not_attempted',
                'provider_usage_missing',
                'incomplete_response',
                'estimation_unavailable',
                'invalid_token_value'
            ]::TEXT[]
        ),
    CONSTRAINT ai_usage_logs_pricing_reason_values
        CHECK (
            array_position(pricing_unsupported_reasons, NULL) IS NULL
            AND pricing_unsupported_reasons <@ ARRAY[
                'provider_cache_pricing',
                'long_context_pricing',
                'service_tier_pricing',
                'built_in_tool_pricing',
                'non_text_modality_pricing',
                'additional_pricing'
            ]::TEXT[]
        ),
    CONSTRAINT ai_usage_logs_cost_reason_values
        CHECK (
            array_position(cost_unavailable_reasons, NULL) IS NULL
            AND cost_unavailable_reasons <@ ARRAY[
                'missing_prompt_usage',
                'missing_completion_usage',
                'unmatched_input_price',
                'unmatched_output_price',
                'unsupported_pricing',
                'invalid_provider_usage',
                'arithmetic_overflow'
            ]::TEXT[]
        )
);

CREATE INDEX idx_ai_usage_logs_workspace_started
    ON ai_usage_logs (workspace_id, started_at DESC, id DESC);

CREATE INDEX idx_ai_usage_logs_workspace_actual_model_started
    ON ai_usage_logs (workspace_id, actual_model, started_at DESC, id DESC)
    WHERE actual_model IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_model_group_started
    ON ai_usage_logs (workspace_id, model_group, started_at DESC, id DESC)
    WHERE model_group IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_virtual_key_started
    ON ai_usage_logs (workspace_id, virtual_key_id, started_at DESC, id DESC)
    WHERE virtual_key_id IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_route_started
    ON ai_usage_logs (workspace_id, route_id, started_at DESC, id DESC)
    WHERE route_id IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_service_started
    ON ai_usage_logs (workspace_id, service_id, started_at DESC, id DESC)
    WHERE service_id IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_provider_started
    ON ai_usage_logs (workspace_id, provider_id, started_at DESC, id DESC)
    WHERE provider_id IS NOT NULL;

CREATE INDEX idx_ai_usage_logs_workspace_consumer_started
    ON ai_usage_logs (workspace_id, consumer_id, started_at DESC, id DESC)
    WHERE consumer_id IS NOT NULL;
