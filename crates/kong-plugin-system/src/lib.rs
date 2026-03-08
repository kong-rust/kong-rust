//! Plugin framework — Kong 插件框架 — Plugin registry and execution engine — 插件注册表和执行引擎
//!
//! Responsibilities: — 职责:
//! - Manage registered plugin handlers (Rust native + Lua bridge) — 管理已注册的插件 handler（Rust 原生 + Lua 桥接）
//! - Find effective plugin configs by route/service/consumer/global associations — 根据 route/service/consumer/global 关联查找生效的插件配置
//! - Sort by priority and execute phase callbacks in order — 按优先级排序并依次执行各阶段回调

use std::collections::HashMap;
use std::sync::Arc;

use kong_core::error::{KongError, Result};
use kong_core::models::Plugin;
use kong_core::traits::{Phase, PluginConfig, PluginHandler, RequestCtx};
use uuid::Uuid;

/// Resolved plugin instance for runtime use — 已解析的插件实例 — 运行时使用
#[derive(Clone)]
pub struct ResolvedPlugin {
    /// Plugin handler — 插件 handler
    pub handler: Arc<dyn PluginHandler>,
    /// Plugin configuration — 插件配置
    pub config: PluginConfig,
    /// Source plugin record ID — 来源插件记录 ID
    pub plugin_id: Uuid,
    /// Associated route — 关联的 route
    pub route_id: Option<Uuid>,
    /// Associated service — 关联的 service
    pub service_id: Option<Uuid>,
    /// Associated consumer — 关联的 consumer
    pub consumer_id: Option<Uuid>,
}

/// Plugin registry — manages all registered plugin handlers — 插件注册表 — 管理所有已注册的插件 handler
pub struct PluginRegistry {
    /// Plugin name -> handler — 插件名 -> handler
    handlers: HashMap<String, Arc<dyn PluginHandler>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a plugin handler — 注册插件 handler
    pub fn register(&mut self, name: &str, handler: Arc<dyn PluginHandler>) {
        tracing::info!("注册插件: {}", name);
        self.handlers.insert(name.to_string(), handler);
    }

    /// Get a registered handler — 获取已注册的 handler
    pub fn get(&self, name: &str) -> Option<&Arc<dyn PluginHandler>> {
        self.handlers.get(name)
    }

    /// List of registered plugin names — 已注册插件列表
    pub fn registered_names(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }

    /// Check if a plugin is registered — 是否已注册
    pub fn is_registered(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Plugin executor — executes resolved plugin chains by phase — 插件执行器 — 按阶段执行已解析的插件链
pub struct PluginExecutor;

impl PluginExecutor {
    /// Resolve plugins relevant to the request — 解析请求相关的插件列表
    ///
    /// Matching priority (consistent with Kong): — 匹配优先级（与 Kong 一致）:
    /// More specific configs override earlier ones (global < service < route < consumer < combo) — 越特异性高的配置覆盖前面的（global < service < route < consumer < 组合）
    pub fn resolve_plugins(
        registry: &PluginRegistry,
        plugins: &[Plugin],
        route_id: Option<Uuid>,
        service_id: Option<Uuid>,
        consumer_id: Option<Uuid>,
    ) -> Vec<ResolvedPlugin> {
        let mut resolved: HashMap<String, ResolvedPlugin> = HashMap::new();

        let mut candidates: Vec<&Plugin> = plugins.iter().filter(|p| p.enabled).collect();

        // Sort by association specificity (more specific processed later, overriding earlier ones) — 按关联特异性排序（越特异性高的越后处理，覆盖前面的）
        candidates.sort_by_key(|p| {
            let has_route = p.route.is_some();
            let has_service = p.service.is_some();
            let has_consumer = p.consumer.is_some();
            (has_consumer as u8, has_route as u8, has_service as u8)
        });

        for plugin in candidates {
            if !plugin_matches(plugin, route_id, service_id, consumer_id) {
                continue;
            }

            let handler = match registry.get(&plugin.name) {
                Some(h) => h.clone(),
                None => continue,
            };

            let config = PluginConfig {
                name: plugin.name.clone(),
                config: plugin.config.clone(),
            };

            resolved.insert(
                plugin.name.clone(),
                ResolvedPlugin {
                    handler,
                    config,
                    plugin_id: plugin.id,
                    route_id: plugin.route.as_ref().map(|fk| fk.id),
                    service_id: plugin.service.as_ref().map(|fk| fk.id),
                    consumer_id: plugin.consumer.as_ref().map(|fk| fk.id),
                },
            );
        }

        // Sort by handler priority descending (higher priority executes first) — 按 handler 优先级降序排列（priority 越大越先执行）
        let mut result: Vec<ResolvedPlugin> = resolved.into_values().collect();
        result.sort_by(|a, b| b.handler.priority().cmp(&a.handler.priority()));
        result
    }

    /// Execute all plugins for the specified phase — 执行指定阶段的所有插件
    pub async fn execute_phase(
        plugins: &[ResolvedPlugin],
        phase: Phase,
        ctx: &mut RequestCtx,
    ) -> Result<()> {
        for plugin in plugins {
            // After short-circuit, only Log phase continues — 短路后只有 Log 阶段继续执行
            if ctx.is_short_circuited() && phase != Phase::Log {
                break;
            }

            let result = match phase {
                Phase::InitWorker => plugin.handler.init_worker(&plugin.config).await,
                Phase::Certificate => plugin.handler.certificate(&plugin.config, ctx).await,
                Phase::Rewrite => plugin.handler.rewrite(&plugin.config, ctx).await,
                Phase::Access => plugin.handler.access(&plugin.config, ctx).await,
                Phase::Response => plugin.handler.response(&plugin.config, ctx).await,
                Phase::HeaderFilter => plugin.handler.header_filter(&plugin.config, ctx).await,
                Phase::BodyFilter => {
                    // body_filter requires body argument, use execute_body_filter instead — body_filter 需要 body 参数，使用 execute_body_filter 代替
                    Ok(())
                }
                Phase::Log => plugin.handler.log(&plugin.config, ctx).await,
            };

            if let Err(e) = result {
                tracing::error!(
                    "插件 {} 在 {:?} 阶段执行失败: {}",
                    plugin.config.name,
                    phase,
                    e
                );
                return Err(KongError::PluginError {
                    plugin_name: plugin.config.name.clone(),
                    message: e.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Execute body_filter phase (requires additional body and end_of_stream params) — 执行 body_filter 阶段（需要额外的 body 和 end_of_stream 参数）
    pub async fn execute_body_filter(
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
        body: &mut bytes::Bytes,
        end_of_stream: bool,
    ) -> Result<()> {
        for plugin in plugins {
            if ctx.is_short_circuited() {
                break;
            }

            let result = plugin
                .handler
                .body_filter(&plugin.config, ctx, body, end_of_stream)
                .await;

            if let Err(e) = result {
                tracing::error!(
                    "插件 {} 在 BodyFilter 阶段执行失败: {}",
                    plugin.config.name,
                    e
                );
                return Err(KongError::PluginError {
                    plugin_name: plugin.config.name.clone(),
                    message: e.to_string(),
                });
            }
        }

        Ok(())
    }
}

/// Check if a plugin matches the current request context — 检查插件是否匹配当前请求上下文
fn plugin_matches(
    plugin: &Plugin,
    route_id: Option<Uuid>,
    service_id: Option<Uuid>,
    consumer_id: Option<Uuid>,
) -> bool {
    if let Some(ref fk) = plugin.route {
        match route_id {
            Some(rid) if rid == fk.id => {}
            _ => return false,
        }
    }

    if let Some(ref fk) = plugin.service {
        match service_id {
            Some(sid) if sid == fk.id => {}
            _ => return false,
        }
    }

    if let Some(ref fk) = plugin.consumer {
        match consumer_id {
            Some(cid) if cid == fk.id => {}
            _ => return false,
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use kong_core::models::ForeignKey;

    #[test]
    fn test_plugin_matches_global() {
        let plugin = Plugin {
            route: None,
            service: None,
            consumer: None,
            ..Plugin::default()
        };
        assert!(plugin_matches(&plugin, None, None, None));
        assert!(plugin_matches(&plugin, Some(Uuid::new_v4()), None, None));
    }

    #[test]
    fn test_plugin_matches_route_specific() {
        let route_id = Uuid::new_v4();
        let plugin = Plugin {
            route: Some(ForeignKey::new(route_id)),
            service: None,
            consumer: None,
            ..Plugin::default()
        };
        assert!(plugin_matches(&plugin, Some(route_id), None, None));
        assert!(!plugin_matches(&plugin, Some(Uuid::new_v4()), None, None));
        assert!(!plugin_matches(&plugin, None, None, None));
    }
}
