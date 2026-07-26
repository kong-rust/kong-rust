-- REQ-AI-003：允许 numeric-overflow unresolved 保存终态命令指纹。
-- 其他 unresolved 仍禁止携带 terminal 字段，避免把可重算成本误判为终态。

ALTER TABLE ai_budget_ledger
    DROP CONSTRAINT ai_budget_ledger_request_terminal_consistency,
    ADD CONSTRAINT ai_budget_ledger_request_terminal_consistency
        CHECK (
            (
                kind = 'request'
                AND status IN ('settled', 'resolved')
                AND terminal_operation_id IS NOT NULL
                AND terminal_command_fingerprint IS NOT NULL
            )
            OR (
                kind = 'request'
                AND status = 'pending'
                AND terminal_operation_id IS NULL
                AND terminal_command_fingerprint IS NULL
            )
            OR (
                kind = 'request'
                AND status = 'unresolved'
                AND (
                    (
                        terminal_operation_id IS NULL
                        AND terminal_command_fingerprint IS NULL
                    )
                    OR (
                        terminal_operation_id IS NOT NULL
                        AND terminal_command_fingerprint IS NOT NULL
                        AND cost_status = 'unavailable'
                        AND 'budget_numeric_overflow' = ANY(cost_reasons)
                    )
                )
            )
            OR (
                kind <> 'request'
                AND terminal_operation_id IS NULL
                AND terminal_command_fingerprint IS NULL
            )
        );
