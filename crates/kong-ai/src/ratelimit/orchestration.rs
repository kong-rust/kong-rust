//! `RateLimitStore` 结果不确定时的有界查询与幂等重放编排。

use super::{
    AdmissionDecision, AdmitCommand, InspectQuery, InspectResult, RateLimitStore,
    RateLimitStoreError, RateLimitStoreErrorKind, SettleCommand, SettlementResult,
};

/// 执行准入；ACK 结果未知时，先查询 request ID，再以完全相同的命令最多重放一次。
pub async fn admit_with_recovery(
    store: &dyn RateLimitStore,
    command: AdmitCommand,
) -> Result<AdmissionDecision, RateLimitStoreError> {
    match store.admit(command.clone()).await {
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
            recover_admission(store, command, error).await
        }
        result => result,
    }
}

/// 执行结算；ACK 结果未知时，先查询 operation ID，再以完全相同的命令最多重放一次。
pub async fn settle_with_recovery(
    store: &dyn RateLimitStore,
    command: SettleCommand,
) -> Result<SettlementResult, RateLimitStoreError> {
    match store.settle(command.clone()).await {
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
            recover_settlement(store, command, error).await
        }
        result => result,
    }
}

async fn recover_admission(
    store: &dyn RateLimitStore,
    command: AdmitCommand,
    initial_error: RateLimitStoreError,
) -> Result<AdmissionDecision, RateLimitStoreError> {
    match inspect_admission(store, &command).await {
        Ok(Some(decision)) => return Ok(decision),
        Ok(None) => {}
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {}
        Err(error) => return Err(error),
    }

    match store.admit(command.clone()).await {
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
            match inspect_admission(store, &command).await {
                Ok(Some(decision)) => Ok(decision),
                Ok(None) => Err(initial_error),
                Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
                    Err(initial_error)
                }
                Err(error) => Err(error),
            }
        }
        result => result,
    }
}

async fn recover_settlement(
    store: &dyn RateLimitStore,
    command: SettleCommand,
    initial_error: RateLimitStoreError,
) -> Result<SettlementResult, RateLimitStoreError> {
    match inspect_settlement(store, &command).await {
        Ok(Some(result)) => return Ok(result),
        Ok(None) => {}
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {}
        Err(error) => return Err(error),
    }

    match store.settle(command.clone()).await {
        Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
            match inspect_settlement(store, &command).await {
                Ok(Some(result)) => Ok(result),
                Ok(None) => Err(initial_error),
                Err(error) if error.kind() == RateLimitStoreErrorKind::OutcomeUnknown => {
                    Err(initial_error)
                }
                Err(error) => Err(error),
            }
        }
        result => result,
    }
}

async fn inspect_admission(
    store: &dyn RateLimitStore,
    command: &AdmitCommand,
) -> Result<Option<AdmissionDecision>, RateLimitStoreError> {
    match store
        .inspect(InspectQuery::Admission {
            key: command.key.clone(),
            request_id: command.request_id.clone(),
        })
        .await?
    {
        InspectResult::NotFound => Ok(None),
        InspectResult::Admission(decision) => Ok(Some(decision)),
        _ => Err(RateLimitStoreError::new(
            RateLimitStoreErrorKind::Corrupt,
            "quota admission inspect returned an incompatible result",
        )),
    }
}

async fn inspect_settlement(
    store: &dyn RateLimitStore,
    command: &SettleCommand,
) -> Result<Option<SettlementResult>, RateLimitStoreError> {
    match store
        .inspect(InspectQuery::Settlement {
            reservation: command.reservation.clone(),
            operation_id: command.operation_id.clone(),
        })
        .await?
    {
        InspectResult::NotFound => Ok(None),
        InspectResult::Settlement(result) => Ok(Some(result)),
        _ => Err(RateLimitStoreError::new(
            RateLimitStoreErrorKind::Corrupt,
            "quota settlement inspect returned an incompatible result",
        )),
    }
}
