//! 后端无关的实时配额领域类型。

use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use uuid::Uuid;

/// 当前 quota key 与 reservation envelope 的 schema 版本。
pub const RATE_LIMIT_SCHEMA_VERSION: u16 = 1;

/// 配额主体。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RateLimitSubject {
    VirtualKey(Uuid),
    Global,
    Route(Uuid),
    Consumer(Option<Uuid>),
}

/// 结构化配额 key。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RateLimitKey {
    pub schema_version: u16,
    pub deployment_namespace: Arc<str>,
    pub subject: RateLimitSubject,
}

impl RateLimitKey {
    pub fn new(deployment_namespace: impl Into<Arc<str>>, subject: RateLimitSubject) -> Self {
        Self {
            schema_version: RATE_LIMIT_SCHEMA_VERSION,
            deployment_namespace: deployment_namespace.into(),
            subject,
        }
    }
}

/// 首次命中时启动固定窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowAlgorithm {
    FixedFirstHit,
}

/// 窗口配置。活动窗口内的新配置只会在下一代窗口生效。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowSpec {
    pub algorithm: WindowAlgorithm,
    pub duration: Duration,
}

impl WindowSpec {
    pub fn fixed(duration: Duration) -> Self {
        Self {
            algorithm: WindowAlgorithm::FixedFirstHit,
            duration,
        }
    }
}

/// 准入时生效的 RPM/TPM 上限。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct QuotaLimits {
    pub requests: Option<NonZeroU64>,
    pub tokens: Option<NonZeroU64>,
}

/// 一次请求对 RPM/TPM 的收费量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct QuotaCharge {
    pub requests: u64,
    pub tokens: u64,
}

/// Store 实例 ID。它用于把 reservation 路由回签发它的后端实例。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendInstanceId(Uuid);

impl BackendInstanceId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Debug for BackendInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BackendInstanceId(<opaque>)")
    }
}

/// 窗口 ID。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(Uuid);

impl WindowId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// 窗口的稳定身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowIdentity {
    pub id: WindowId,
    pub generation: u64,
}

/// 返回给调用方的权威窗口快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSnapshot {
    /// `None` 表示尚未创建窗口的 prospective snapshot。
    pub identity: Option<WindowIdentity>,
    pub algorithm: WindowAlgorithm,
    pub duration: Duration,
    pub started_at: SystemTime,
    pub reset_at: SystemTime,
    pub reset_after: Duration,
}

/// 单个配额维度的计数快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimensionSnapshot {
    pub limit: u64,
    pub used: u64,
    pub remaining: u64,
}

/// RPM 与 TPM 的同一原子时点快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub window: WindowSnapshot,
    pub requests: Option<DimensionSnapshot>,
    pub tokens: Option<DimensionSnapshot>,
}

/// 被超出的配额维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceededDimension {
    Requests,
    Tokens,
    RequestsAndTokens,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReservationEnvelope {
    pub schema_version: u16,
    pub backend_instance_id: BackendInstanceId,
    pub reservation_id: Uuid,
    pub request_id: Arc<str>,
    pub key: RateLimitKey,
    pub window: WindowIdentity,
    pub limits_at_admission: QuotaLimits,
    pub reserved: QuotaCharge,
}

/// Store 签发的不可序列化 reservation。
///
/// 调用方只能克隆并原样交回 Store，无法从公开 API 拼装 token。
#[derive(Clone, PartialEq, Eq)]
pub struct ReservationToken(pub(crate) Arc<ReservationEnvelope>);

impl fmt::Debug for ReservationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReservationToken(<opaque>)")
    }
}

impl ReservationToken {
    pub(crate) fn envelope(&self) -> &ReservationEnvelope {
        &self.0
    }
}

/// 原子准入命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmitCommand {
    pub request_id: Arc<str>,
    pub key: RateLimitKey,
    pub window: WindowSpec,
    pub limits: QuotaLimits,
    pub reserve: QuotaCharge,
}

/// 原子准入结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allowed {
        reservation: ReservationToken,
        snapshot: RateLimitSnapshot,
        replayed: bool,
    },
    Rejected {
        reason: ExceededDimension,
        snapshot: RateLimitSnapshot,
        replayed: bool,
    },
}

impl AdmissionDecision {
    pub(crate) fn as_replayed(&self) -> Self {
        match self {
            Self::Allowed {
                reservation,
                snapshot,
                ..
            } => Self::Allowed {
                reservation: reservation.clone(),
                snapshot: snapshot.clone(),
                replayed: true,
            },
            Self::Rejected {
                reason, snapshot, ..
            } => Self::Rejected {
                reason: *reason,
                snapshot: snapshot.clone(),
                replayed: true,
            },
        }
    }
}

/// reservation 终态修正命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettleCommand {
    pub operation_id: Arc<str>,
    pub reservation: ReservationToken,
    pub final_charge: QuotaCharge,
}

/// 修正结果的处理方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementDisposition {
    Applied,
    StaleWindowNoop,
    Replayed,
}

/// reservation 修正结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementResult {
    pub disposition: SettlementDisposition,
    pub snapshot: Option<RateLimitSnapshot>,
}

/// 结果不确定时使用的只读查询。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectQuery {
    Current {
        key: RateLimitKey,
        window: WindowSpec,
        limits: QuotaLimits,
    },
    Admission {
        key: RateLimitKey,
        request_id: Arc<str>,
    },
    Settlement {
        reservation: ReservationToken,
        operation_id: Arc<str>,
    },
}

/// 只读查询结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectResult {
    NotFound,
    Current(RateLimitSnapshot),
    Admission(AdmissionDecision),
    Settlement(SettlementResult),
}

/// 后端的水平扩展状态归属。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitBackendScope {
    ProcessLocal,
    Distributed,
}

/// 后端能力描述，不向领域层泄漏具体数据结构。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitBackendDescriptor {
    pub backend: Arc<str>,
    pub instance_id: BackendInstanceId,
    pub scope: RateLimitBackendScope,
    pub ephemeral: bool,
}
