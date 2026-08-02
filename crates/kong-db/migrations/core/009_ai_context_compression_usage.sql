ALTER TABLE ai_usage_logs
    ADD COLUMN context_compression_status TEXT,
    ADD COLUMN context_compression_reason TEXT,
    ADD COLUMN context_compression_backend TEXT,
    ADD COLUMN context_compression_ccr BOOLEAN,
    ADD COLUMN context_compression_tokens_before BIGINT,
    ADD COLUMN context_compression_tokens_after BIGINT,
    ADD COLUMN context_compression_tokens_saved BIGINT,
    ADD COLUMN context_compression_hop_latency_ms BIGINT,
    ADD CONSTRAINT ai_usage_logs_context_compression_status_value
        CHECK (
            context_compression_status IS NULL
            OR context_compression_status IN (
                'pending', 'applied', 'bypassed', 'degraded', 'rejected'
            )
        ),
    ADD CONSTRAINT ai_usage_logs_context_compression_reason_value
        CHECK (
            context_compression_reason IS NULL
            OR context_compression_reason IN (
                'pending',
                'applied',
                'below_threshold',
                'body_too_large',
                'streaming',
                'unsupported_provider',
                'unsupported_protocol',
                'tool_choice_unsupported',
                'unsupported_path',
                'backend_not_configured',
                'backend_unhealthy',
                'metrics_invalid',
                'compression_failed'
            )
        ),
    ADD CONSTRAINT ai_usage_logs_context_compression_bundle_consistency
        CHECK (
            (
                context_compression_status IS NULL
                AND context_compression_reason IS NULL
                AND context_compression_backend IS NULL
                AND context_compression_ccr IS NULL
                AND context_compression_tokens_before IS NULL
                AND context_compression_tokens_after IS NULL
                AND context_compression_tokens_saved IS NULL
                AND context_compression_hop_latency_ms IS NULL
            )
            OR (
                context_compression_status IS NOT NULL
                AND context_compression_reason IS NOT NULL
                AND context_compression_ccr IS NOT NULL
            )
        ),
    ADD CONSTRAINT ai_usage_logs_context_compression_token_bundle
        CHECK (
            (
                context_compression_tokens_before IS NULL
                AND context_compression_tokens_after IS NULL
                AND context_compression_tokens_saved IS NULL
            )
            OR (
                context_compression_tokens_before IS NOT NULL
                AND context_compression_tokens_after IS NOT NULL
                AND context_compression_tokens_saved IS NOT NULL
                AND context_compression_tokens_before >= 0
                AND context_compression_tokens_after >= 0
                AND context_compression_tokens_saved >= 0
                AND context_compression_tokens_after <= context_compression_tokens_before
                AND context_compression_tokens_saved <= context_compression_tokens_before
                AND context_compression_tokens_before - context_compression_tokens_after
                    = context_compression_tokens_saved
            )
        ),
    ADD CONSTRAINT ai_usage_logs_context_compression_hop_latency_nonnegative
        CHECK (
            context_compression_hop_latency_ms IS NULL
            OR context_compression_hop_latency_ms >= 0
        ),
    ADD CONSTRAINT ai_usage_logs_context_compression_backend_shape
        CHECK (
            context_compression_backend IS NULL
            OR (
                octet_length(context_compression_backend) BETWEEN 1 AND 64
                AND context_compression_backend ~ '^[a-z0-9][a-z0-9_-]*$'
            )
        );
