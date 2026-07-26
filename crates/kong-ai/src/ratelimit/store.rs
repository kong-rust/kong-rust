//! 异步实时配额 Store 契约。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::metrics::RateLimitStoreStatsSnapshot;
use super::types::{
    AdmissionDecision, AdmitCommand, InspectQuery, InspectResult, RateLimitBackendDescriptor,
    SettleCommand, SettlementResult,
};

/// 后端错误类别。HTTP 映射必须留在调用方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitStoreErrorKind {
    Unavailable,
    Timeout,
    OutcomeUnknown,
    Overloaded,
    Corrupt,
    Unsupported,
}

/// 后端无关错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitStoreError {
    kind: RateLimitStoreErrorKind,
    message: Arc<str>,
}

impl RateLimitStoreError {
    pub fn new(kind: RateLimitStoreErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RateLimitStoreErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn corrupt(message: &'static str) -> Self {
        Self::new(RateLimitStoreErrorKind::Corrupt, message)
    }

    pub(crate) fn overloaded(message: &'static str) -> Self {
        Self::new(RateLimitStoreErrorKind::Overloaded, message)
    }
}

impl fmt::Display for RateLimitStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for RateLimitStoreError {}

/// 可替换的实时配额后端。
#[async_trait]
pub trait RateLimitStore: Send + Sync {
    fn descriptor(&self) -> RateLimitBackendDescriptor;

    async fn admit(&self, command: AdmitCommand) -> Result<AdmissionDecision, RateLimitStoreError>;

    async fn settle(&self, command: SettleCommand)
        -> Result<SettlementResult, RateLimitStoreError>;

    async fn inspect(&self, query: InspectQuery) -> Result<InspectResult, RateLimitStoreError>;

    fn stats(&self) -> RateLimitStoreStatsSnapshot;
}
