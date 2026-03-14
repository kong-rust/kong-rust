//! Test helper module — provides MockUpstream, TestPlugin and other test infrastructure — 测试辅助模块 — 提供 MockUpstream、TestPlugin 等测试基础设施

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

/// Test plugin — sets flags in each phase to verify phase execution order and behavior — 测试插件 — 在各阶段设置标记，用于验证阶段执行顺序和行为
#[derive(Clone)]
pub struct TestPlugin {
    pub name: String,
    pub priority: i32,
    /// Flags indicating whether each phase was called — 各阶段是否被调用的标记
    pub rewrite_called: Arc<AtomicBool>,
    pub access_called: Arc<AtomicBool>,
    pub header_filter_called: Arc<AtomicBool>,
    pub body_filter_called: Arc<AtomicBool>,
    pub log_called: Arc<AtomicBool>,
    /// Call count — 调用计数
    pub call_count: Arc<AtomicU32>,
    /// Whether to short-circuit in access phase — 是否在 access 阶段短路
    pub short_circuit_in_access: bool,
    /// Short-circuit status code — 短路状态码
    pub short_circuit_status: u16,
    /// Short-circuit response body — 短路响应体
    pub short_circuit_body: Option<String>,
    /// Whether to modify response headers in header_filter phase — 是否在 header_filter 阶段修改响应头
    pub modify_response_header: Option<(String, String)>,
    /// Whether to set ctx.shared in rewrite phase — 是否在 rewrite 阶段设置 ctx.shared
    pub set_shared_in_rewrite: Option<(String, serde_json::Value)>,
}

impl TestPlugin {
    pub fn new(name: &str, priority: i32) -> Self {
        Self {
            name: name.to_string(),
            priority,
            rewrite_called: Arc::new(AtomicBool::new(false)),
            access_called: Arc::new(AtomicBool::new(false)),
            header_filter_called: Arc::new(AtomicBool::new(false)),
            body_filter_called: Arc::new(AtomicBool::new(false)),
            log_called: Arc::new(AtomicBool::new(false)),
            call_count: Arc::new(AtomicU32::new(0)),
            short_circuit_in_access: false,
            short_circuit_status: 403,
            short_circuit_body: None,
            modify_response_header: None,
            set_shared_in_rewrite: None,
        }
    }

    /// Create a test plugin that short-circuits in the access phase — 创建一个在 access 阶段短路的测试插件
    pub fn with_short_circuit(name: &str, priority: i32, status: u16) -> Self {
        let mut p = Self::new(name, priority);
        p.short_circuit_in_access = true;
        p.short_circuit_status = status;
        p
    }

    /// Create a test plugin that modifies response headers — 创建一个修改响应头的测试插件
    pub fn with_header_modify(
        name: &str,
        priority: i32,
        header_name: &str,
        header_value: &str,
    ) -> Self {
        let mut p = Self::new(name, priority);
        p.modify_response_header = Some((header_name.to_string(), header_value.to_string()));
        p
    }
}

#[async_trait]
impl PluginHandler for TestPlugin {
    fn priority(&self) -> i32 {
        self.priority
    }

    fn version(&self) -> &str {
        "1.0.0-test"
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn rewrite(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        self.rewrite_called.store(true, Ordering::SeqCst);
        self.call_count.fetch_add(1, Ordering::SeqCst);

        if let Some((ref key, ref value)) = self.set_shared_in_rewrite {
            ctx.shared.insert(key.clone(), value.clone());
        }

        Ok(())
    }

    async fn access(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        self.access_called.store(true, Ordering::SeqCst);
        self.call_count.fetch_add(1, Ordering::SeqCst);

        if self.short_circuit_in_access {
            ctx.short_circuited = true;
            ctx.exit_status = Some(self.short_circuit_status);
            ctx.exit_body = self.short_circuit_body.clone();
        }

        Ok(())
    }

    async fn header_filter(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        self.header_filter_called.store(true, Ordering::SeqCst);
        self.call_count.fetch_add(1, Ordering::SeqCst);

        if let Some((ref name, ref value)) = self.modify_response_header {
            ctx.response_headers_to_set
                .push((name.clone(), value.clone()));
        }

        Ok(())
    }

    async fn body_filter(
        &self,
        _config: &PluginConfig,
        _ctx: &mut RequestCtx,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
    ) -> Result<()> {
        self.body_filter_called.store(true, Ordering::SeqCst);
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn log(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        self.log_called.store(true, Ordering::SeqCst);
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Build a ResolvedPlugin for testing — 构建测试用的 ResolvedPlugin
pub fn make_resolved_plugin(handler: Arc<dyn PluginHandler>) -> kong_plugin_system::ResolvedPlugin {
    let name = handler.name().to_string();
    kong_plugin_system::ResolvedPlugin {
        handler,
        config: PluginConfig {
            name,
            config: serde_json::Value::Object(serde_json::Map::new()),
        },
        plugin_id: uuid::Uuid::new_v4(),
        route_id: None,
        service_id: None,
        consumer_id: None,
    }
}
