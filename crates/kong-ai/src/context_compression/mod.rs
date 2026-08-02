//! 上下文压缩后端契约与 Headroom adapter。

mod headroom;
mod metrics;

use async_trait::async_trait;
use serde::Serialize;

pub use headroom::{HeadroomProxyAdapter, HeadroomProxyConfig};
pub use metrics::{context_compression_prometheus_metrics, observe_context_compression};

/// Headroom 当前可透明处理 CCR 的 Provider wire 协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionProtocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

/// Kong 已经解析并冻结的单个 Provider 目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTarget {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// CCR store 的水平扩展能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionStoreScope {
    Local,
    Cluster,
}

impl CompressionStoreScope {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("cluster") {
            Self::Cluster
        } else {
            Self::Local
        }
    }
}

/// 后端能力描述；不包含 URL、token 或其他敏感部署值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressionBackendDescriptor {
    pub backend: &'static str,
    pub transparent_ccr: bool,
    pub streaming: bool,
    pub store_scope: CompressionStoreScope,
}

/// adapter 完整准备好的 Headroom 上游覆写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressionRoute {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub control_headers: Vec<(String, String)>,
    pub body_transform: Option<CompressionBodyTransform>,
}

/// 压缩后端要求 Kong 在转发前执行的受控请求体变换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionBodyTransform {
    /// Headroom 0.33.0 的 Responses transport 能处理 CCR continuation，
    /// 但不会自行注入扁平格式的 retrieve tool，由 Kong 补齐该契约。
    InjectOpenAiResponsesCcrTool,
}

/// 后端失败类别保持低基数，避免把内部 URL/网络错误写入客户端或指标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CompressionBackendError {
    #[error("context compression backend is unavailable")]
    Unavailable,
    #[error("provider protocol is not supported by the context compression backend")]
    UnsupportedProtocol,
    #[error("provider target is not supported by the context compression backend")]
    UnsupportedTarget,
}

/// 可替换的上下文压缩 transport adapter。
///
/// 首版 adapter 把请求路由到 Headroom proxy，以便响应侧也能完成透明 CCR；只有
/// 能实现等价 continuation 的后端才允许声明 `transparent_ccr=true`。
#[async_trait]
pub trait ContextCompressionBackend: Send + Sync {
    fn descriptor(&self) -> CompressionBackendDescriptor;

    async fn prepare_route(
        &self,
        protocol: CompressionProtocol,
        provider: ProviderTarget,
    ) -> Result<CompressionRoute, CompressionBackendError>;
}
