//! AI Virtual Key 策略覆盖索引。
//!
//! 索引只依赖一次读取到的 Route、Service 与 Plugin 不可变快照。构建阶段按
//! global < service < route < route+service 的覆盖顺序解析有效插件，查询阶段只按
//! workspace 做 O(1) 查找，避免 Virtual Key 列表对每个 key 重复扫描拓扑。

use std::collections::{HashMap, HashSet};

use kong_ai::plugins::ai_key_auth::AiKeyAuthConfig;
use kong_ai::plugins::ai_proxy::AiProxyConfig;
use kong_core::models::{Plugin, Route, Service};
use kong_plugin_system::config_validation::validate_ai_rate_limit_config;
use serde::Serialize;
use uuid::Uuid;

/// 单个 workspace 的 Virtual Key 策略覆盖只读快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AiPolicyCoverageSnapshot {
    pub auth_endpoint_count: u64,
    pub enforced_endpoint_count: u64,
    pub policy_error_count: u64,
    pub coverage_available: bool,
}

impl AiPolicyCoverageSnapshot {
    /// coverage 尚不可用时的安全默认值。
    pub const fn unavailable() -> Self {
        Self {
            auth_endpoint_count: 0,
            enforced_endpoint_count: 0,
            policy_error_count: 0,
            coverage_available: false,
        }
    }

    const fn available_empty() -> Self {
        Self {
            auth_endpoint_count: 0,
            enforced_endpoint_count: 0,
            policy_error_count: 0,
            coverage_available: true,
        }
    }
}

impl Default for AiPolicyCoverageSnapshot {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// 预计算、后端无关的 Virtual Key 策略覆盖索引。
///
/// `None` workspace 会在构建和查询边界统一归一到 `default_workspace_id`。
#[derive(Debug, Clone)]
pub struct AiPolicyCoverageIndex {
    default_workspace_id: Uuid,
    coverage_available: bool,
    by_workspace: HashMap<Uuid, AiPolicyCoverageSnapshot>,
}

impl AiPolicyCoverageIndex {
    /// 从同一代 routes/services/plugins 快照构建可用索引。
    pub fn build(
        routes: &[Route],
        services: &[Service],
        plugins: &[Plugin],
        default_workspace_id: Uuid,
    ) -> Self {
        let service_by_id: HashMap<Uuid, &Service> = services
            .iter()
            .map(|service| (service.id, service))
            .collect();
        let route_by_id: HashMap<Uuid, &Route> =
            routes.iter().map(|route| (route.id, route)).collect();
        let mut route_ids_by_service: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for route in routes {
            if let Some(service_id) = route.service.as_ref().map(|foreign_key| foreign_key.id) {
                route_ids_by_service
                    .entry(service_id)
                    .or_default()
                    .push(route.id);
            }
        }

        let mut invalid_routes = HashSet::new();
        let mut scoped_plugins = HashMap::new();
        for plugin in plugins.iter().filter(|plugin| plugin.enabled) {
            let Some(kind) = AiPluginKind::from_name(&plugin.name) else {
                continue;
            };
            // consumer 关联插件在认证前无法成为通用 Virtual Key Endpoint 策略。
            if plugin.consumer.is_some() {
                continue;
            }

            let workspace_id = normalize_workspace(plugin.ws_id, default_workspace_id);
            let route_id = plugin.route.as_ref().map(|foreign_key| foreign_key.id);
            let service_id = plugin.service.as_ref().map(|foreign_key| foreign_key.id);

            if let Some(route_id) = route_id {
                let Some(route) = route_by_id.get(&route_id) else {
                    // 没有关联到实际 Endpoint 的孤儿记录不进入 endpoint 口径。
                    continue;
                };
                if normalize_workspace(route.ws_id, default_workspace_id) != workspace_id {
                    invalid_routes.insert(route_id);
                    continue;
                }
            }

            if let Some(service_id) = service_id {
                let Some(service) = service_by_id.get(&service_id) else {
                    if let Some(route_id) = route_id {
                        invalid_routes.insert(route_id);
                    }
                    continue;
                };
                if normalize_workspace(service.ws_id, default_workspace_id) != workspace_id {
                    if let Some(route_id) = route_id {
                        invalid_routes.insert(route_id);
                    } else {
                        mark_service_routes_invalid(
                            &route_ids_by_service,
                            service_id,
                            &mut invalid_routes,
                        );
                    }
                    continue;
                }
            }

            if let (Some(route_id), Some(service_id)) = (route_id, service_id) {
                let route_service_id = route_by_id
                    .get(&route_id)
                    .and_then(|route| route.service.as_ref())
                    .map(|foreign_key| foreign_key.id);
                if route_service_id != Some(service_id) {
                    invalid_routes.insert(route_id);
                    continue;
                }
            }

            // 与运行时 resolver 一致：同一精确 scope 的后出现记录覆盖先出现记录。
            scoped_plugins.insert(
                ScopedPluginKey {
                    workspace_id,
                    route_id,
                    service_id,
                    kind,
                },
                plugin,
            );
        }

        let mut by_workspace = HashMap::new();
        for route in routes {
            let workspace_id = normalize_workspace(route.ws_id, default_workspace_id);
            let coverage = by_workspace
                .entry(workspace_id)
                .or_insert_with(AiPolicyCoverageSnapshot::available_empty);

            let service_id = route.service.as_ref().map(|foreign_key| foreign_key.id);
            if let Some(service_id) = service_id {
                let Some(service) = service_by_id.get(&service_id) else {
                    coverage.policy_error_count += 1;
                    continue;
                };
                if normalize_workspace(service.ws_id, default_workspace_id) != workspace_id {
                    coverage.policy_error_count += 1;
                    continue;
                }
                // 被禁用 Service 不属于当前可调用 Endpoint，不计入 coverage 或错误。
                if !service.enabled {
                    continue;
                }
            }

            if invalid_routes.contains(&route.id) {
                coverage.policy_error_count += 1;
                continue;
            }

            let auth = effective_plugin(
                &scoped_plugins,
                workspace_id,
                route.id,
                service_id,
                AiPluginKind::KeyAuth,
            );
            let proxy = effective_plugin(
                &scoped_plugins,
                workspace_id,
                route.id,
                service_id,
                AiPluginKind::Proxy,
            );
            let rate_limit = effective_plugin(
                &scoped_plugins,
                workspace_id,
                route.id,
                service_id,
                AiPluginKind::RateLimit,
            );

            let auth_valid = auth.is_some_and(valid_ai_key_auth);
            let proxy_valid = proxy.is_some_and(valid_ai_proxy);
            let rate_limit_state = rate_limit.map(classify_ai_rate_limit);

            if auth_valid {
                coverage.auth_endpoint_count += 1;
            }

            let has_invalid_config = auth.is_some() && !auth_valid
                || proxy.is_some() && !proxy_valid
                || rate_limit_state == Some(AiRateLimitState::Invalid);
            let requests_virtual_key_policy =
                rate_limit_state == Some(AiRateLimitState::VirtualKey);
            let incomplete_virtual_key_chain =
                requests_virtual_key_policy && (!auth_valid || !proxy_valid);

            if has_invalid_config || incomplete_virtual_key_chain {
                coverage.policy_error_count += 1;
                continue;
            }

            if auth_valid && proxy_valid && requests_virtual_key_policy {
                coverage.enforced_endpoint_count += 1;
            }
        }

        Self {
            default_workspace_id,
            coverage_available: true,
            by_workspace,
        }
    }

    /// 构造显式不可用索引，供 topology 首次加载失败等场景 fail-safe 展示。
    pub fn unavailable(default_workspace_id: Uuid) -> Self {
        Self {
            default_workspace_id,
            coverage_available: false,
            by_workspace: HashMap::new(),
        }
    }

    /// 按 workspace O(1) 返回只读覆盖快照。
    pub fn coverage_for(&self, workspace_id: Option<Uuid>) -> AiPolicyCoverageSnapshot {
        if !self.coverage_available {
            return AiPolicyCoverageSnapshot::unavailable();
        }

        self.by_workspace
            .get(&normalize_workspace(
                workspace_id,
                self.default_workspace_id,
            ))
            .copied()
            .unwrap_or_else(AiPolicyCoverageSnapshot::available_empty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AiPluginKind {
    KeyAuth,
    RateLimit,
    Proxy,
}

impl AiPluginKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "ai-key-auth" => Some(Self::KeyAuth),
            "ai-rate-limit" => Some(Self::RateLimit),
            "ai-proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ScopedPluginKey {
    workspace_id: Uuid,
    route_id: Option<Uuid>,
    service_id: Option<Uuid>,
    kind: AiPluginKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiRateLimitState {
    Invalid,
    Other,
    VirtualKey,
}

fn effective_plugin<'a>(
    plugins: &'a HashMap<ScopedPluginKey, &'a Plugin>,
    workspace_id: Uuid,
    route_id: Uuid,
    service_id: Option<Uuid>,
    kind: AiPluginKind,
) -> Option<&'a Plugin> {
    let mut effective = plugins
        .get(&ScopedPluginKey {
            workspace_id,
            route_id: None,
            service_id: None,
            kind,
        })
        .copied();

    if let Some(service_id) = service_id {
        if let Some(plugin) = plugins.get(&ScopedPluginKey {
            workspace_id,
            route_id: None,
            service_id: Some(service_id),
            kind,
        }) {
            effective = Some(*plugin);
        }
    }
    if let Some(plugin) = plugins.get(&ScopedPluginKey {
        workspace_id,
        route_id: Some(route_id),
        service_id: None,
        kind,
    }) {
        effective = Some(*plugin);
    }
    if let Some(service_id) = service_id {
        if let Some(plugin) = plugins.get(&ScopedPluginKey {
            workspace_id,
            route_id: Some(route_id),
            service_id: Some(service_id),
            kind,
        }) {
            effective = Some(*plugin);
        }
    }

    effective
}

fn valid_ai_key_auth(plugin: &Plugin) -> bool {
    serde_json::from_value::<AiKeyAuthConfig>(plugin.config.clone()).is_ok()
}

fn valid_ai_proxy(plugin: &Plugin) -> bool {
    serde_json::from_value::<AiProxyConfig>(plugin.config.clone())
        .map(|config| matches!(config.effective_client_protocol(), "openai" | "anthropic"))
        .unwrap_or(false)
}

fn classify_ai_rate_limit(plugin: &Plugin) -> AiRateLimitState {
    if validate_ai_rate_limit_config(&plugin.config).is_err() {
        return AiRateLimitState::Invalid;
    }
    if plugin
        .config
        .get("limit_by")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("consumer")
        == "virtual_key"
    {
        AiRateLimitState::VirtualKey
    } else {
        AiRateLimitState::Other
    }
}

fn normalize_workspace(workspace_id: Option<Uuid>, default_workspace_id: Uuid) -> Uuid {
    workspace_id.unwrap_or(default_workspace_id)
}

fn mark_service_routes_invalid(
    route_ids_by_service: &HashMap<Uuid, Vec<Uuid>>,
    service_id: Uuid,
    invalid_routes: &mut HashSet<Uuid>,
) {
    if let Some(route_ids) = route_ids_by_service.get(&service_id) {
        invalid_routes.extend(route_ids.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use kong_core::models::ForeignKey;
    use serde_json::json;

    use super::*;

    fn service(id: Uuid, workspace_id: Option<Uuid>, enabled: bool) -> Service {
        Service {
            id,
            ws_id: workspace_id,
            enabled,
            ..Service::default()
        }
    }

    fn route(id: Uuid, service_id: Uuid, workspace_id: Option<Uuid>) -> Route {
        Route {
            id,
            service: Some(ForeignKey::new(service_id)),
            ws_id: workspace_id,
            ..Route::default()
        }
    }

    fn plugin(
        name: &str,
        config: serde_json::Value,
        route_id: Option<Uuid>,
        service_id: Option<Uuid>,
        workspace_id: Option<Uuid>,
    ) -> Plugin {
        Plugin {
            id: Uuid::new_v4(),
            name: name.to_string(),
            config,
            route: route_id.map(ForeignKey::new),
            service: service_id.map(ForeignKey::new),
            ws_id: workspace_id,
            enabled: true,
            ..Plugin::default()
        }
    }

    fn virtual_key_chain(workspace_id: Option<Uuid>) -> Vec<Plugin> {
        vec![
            plugin("ai-key-auth", json!({}), None, None, workspace_id),
            plugin("ai-proxy", json!({}), None, None, workspace_id),
            plugin(
                "ai-rate-limit",
                json!({"limit_by": "virtual_key"}),
                None,
                None,
                workspace_id,
            ),
        ]
    }

    #[test]
    fn resolves_global_service_and_route_overrides() {
        let workspace_id = Uuid::new_v4();
        let service_id = Uuid::new_v4();
        let route_a = Uuid::new_v4();
        let route_b = Uuid::new_v4();
        let routes = vec![
            route(route_a, service_id, Some(workspace_id)),
            route(route_b, service_id, Some(workspace_id)),
        ];
        let services = vec![service(service_id, Some(workspace_id), true)];
        let mut plugins = virtual_key_chain(Some(workspace_id));
        plugins.push(plugin(
            "ai-rate-limit",
            json!({"limit_by": "route", "rpm_limit": 10}),
            None,
            Some(service_id),
            Some(workspace_id),
        ));
        plugins.push(plugin(
            "ai-rate-limit",
            json!({"limit_by": "virtual_key"}),
            Some(route_b),
            None,
            Some(workspace_id),
        ));

        let index = AiPolicyCoverageIndex::build(&routes, &services, &plugins, Uuid::new_v4());

        assert_eq!(
            index.coverage_for(Some(workspace_id)),
            AiPolicyCoverageSnapshot {
                auth_endpoint_count: 2,
                enforced_endpoint_count: 1,
                policy_error_count: 0,
                coverage_available: true,
            }
        );
    }

    #[test]
    fn ignores_disabled_plugins_and_services() {
        let service_id = Uuid::new_v4();
        let disabled_service_id = Uuid::new_v4();
        let routes = vec![
            route(Uuid::new_v4(), service_id, None),
            route(Uuid::new_v4(), disabled_service_id, None),
        ];
        let services = vec![
            service(service_id, None, true),
            service(disabled_service_id, None, false),
        ];
        let mut plugins = virtual_key_chain(None);
        plugins
            .iter_mut()
            .find(|plugin| plugin.name == "ai-rate-limit")
            .unwrap()
            .enabled = false;

        let index = AiPolicyCoverageIndex::build(&routes, &services, &plugins, Uuid::new_v4());
        let coverage = index.coverage_for(None);

        assert_eq!(coverage.auth_endpoint_count, 1);
        assert_eq!(coverage.enforced_endpoint_count, 0);
        assert_eq!(coverage.policy_error_count, 0);
        assert!(coverage.coverage_available);
    }

    #[test]
    fn reports_invalid_effective_virtual_key_chains_once_per_route() {
        let default_workspace_id = Uuid::new_v4();
        let service_id = Uuid::new_v4();
        let route_a = Uuid::new_v4();
        let route_b = Uuid::new_v4();
        let routes = vec![
            route(route_a, service_id, None),
            route(route_b, service_id, None),
        ];
        let services = vec![service(service_id, None, true)];
        let mut plugins = vec![plugin("ai-key-auth", json!({}), None, None, None)];
        plugins.push(plugin(
            "ai-rate-limit",
            json!({"limit_by": "virtual_key"}),
            Some(route_a),
            None,
            None,
        ));
        plugins.push(plugin(
            "ai-proxy",
            json!({"client_protocol": "future"}),
            Some(route_b),
            None,
            None,
        ));
        plugins.push(plugin(
            "ai-rate-limit",
            json!({"limit_by": "virtual_key"}),
            Some(route_b),
            None,
            None,
        ));

        let index =
            AiPolicyCoverageIndex::build(&routes, &services, &plugins, default_workspace_id);
        let coverage = index.coverage_for(None);

        assert_eq!(coverage.auth_endpoint_count, 2);
        assert_eq!(coverage.enforced_endpoint_count, 0);
        assert_eq!(coverage.policy_error_count, 2);
    }

    #[test]
    fn normalizes_default_workspace_and_keeps_workspaces_isolated() {
        let default_workspace_id = Uuid::new_v4();
        let other_workspace_id = Uuid::new_v4();
        let default_service_id = Uuid::new_v4();
        let other_service_id = Uuid::new_v4();
        let routes = vec![
            route(Uuid::new_v4(), default_service_id, None),
            route(Uuid::new_v4(), other_service_id, Some(other_workspace_id)),
        ];
        let services = vec![
            service(default_service_id, None, true),
            service(other_service_id, Some(other_workspace_id), true),
        ];
        let mut plugins = virtual_key_chain(None);
        plugins.extend(virtual_key_chain(Some(other_workspace_id)));

        let index =
            AiPolicyCoverageIndex::build(&routes, &services, &plugins, default_workspace_id);

        assert_eq!(index.coverage_for(None).enforced_endpoint_count, 1);
        assert_eq!(
            index
                .coverage_for(Some(default_workspace_id))
                .enforced_endpoint_count,
            1
        );
        assert_eq!(
            index
                .coverage_for(Some(other_workspace_id))
                .enforced_endpoint_count,
            1
        );
        assert_eq!(
            index.coverage_for(Some(Uuid::new_v4())),
            AiPolicyCoverageSnapshot::available_empty()
        );
    }

    #[test]
    fn unavailable_index_never_exposes_stale_counts() {
        let index = AiPolicyCoverageIndex::unavailable(Uuid::new_v4());
        assert_eq!(
            index.coverage_for(None),
            AiPolicyCoverageSnapshot::unavailable()
        );
    }
}
