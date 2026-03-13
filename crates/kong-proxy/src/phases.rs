//! PhaseRunner — Kong phase runner — Kong 阶段运行器
//!
//! Decouples Kong plugin phases from Pingora lifecycle callbacks, — 将 Kong 插件阶段与 Pingora 生命周期回调解耦，
//! providing a unified phase execution entry point. — 提供统一的阶段执行入口。
//!
//! Pingora callback → Kong phase mapping: — Pingora 回调 → Kong 阶段映射:
//!   request_filter()           → route matching + rewrite + access — 路由匹配 + rewrite + access
//!   upstream_peer()            → load balancer selection — 负载均衡选择
//!   upstream_request_filter()  → request header modification — 请求头修改
//!   upstream_response_filter() → header_filter
//!   response_body_filter()     → body_filter
//!   logging()                  → log (always executes) — log（总是执行）

use kong_core::error::Result;
use kong_core::traits::{Phase, RequestCtx};
use kong_plugin_system::{PluginExecutor, ResolvedPlugin};

/// Kong phase runner — Kong 阶段运行器
pub struct PhaseRunner;

impl PhaseRunner {
    /// Rewrite phase (called in request_filter, after route matching) — rewrite 阶段（request_filter 中调用，路由匹配后）
    pub async fn run_rewrite(plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) -> Result<()> {
        PluginExecutor::execute_phase(plugins, Phase::Rewrite, ctx).await
    }

    /// Access phase (called in request_filter, after rewrite) — access 阶段（request_filter 中调用，rewrite 之后）
    pub async fn run_access(plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) -> Result<()> {
        PluginExecutor::execute_phase(plugins, Phase::Access, ctx).await
    }

    /// header_filter phase (called in upstream_response_filter) — header_filter 阶段（upstream_response_filter 中调用）
    pub async fn run_header_filter(plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) -> Result<()> {
        PluginExecutor::execute_phase(plugins, Phase::HeaderFilter, ctx).await
    }

    /// body_filter phase (called in response_body_filter, streaming) — body_filter 阶段（response_body_filter 中调用，流式处理）
    pub async fn run_body_filter(
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
        body: &mut bytes::Bytes,
        end_of_stream: bool,
    ) -> Result<()> {
        PluginExecutor::execute_body_filter(plugins, ctx, body, end_of_stream).await
    }

    /// Log phase (called in logging, always executes, even after short-circuit) — log 阶段（logging 中调用，总是执行，即使之前短路）
    pub async fn run_log(plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) -> Result<()> {
        PluginExecutor::execute_phase(plugins, Phase::Log, ctx).await
    }
}
