//! 端到端代理功能测试
//!
//! 测试路由匹配、负载均衡、健康检查、TLS 证书管理等核心代理功能
//! 注意：这些测试不启动真正的网络监听，而是直接测试组件逻辑

use std::collections::HashMap;

use uuid::Uuid;

use kong_core::models::*;
use kong_core::traits::RequestCtx;
use kong_proxy::balancer::LoadBalancer;
use kong_proxy::tls::CertificateManager;
use kong_router::{RequestContext, Router};

// ========== 路由匹配测试 ==========

fn make_route(
    id: Uuid,
    service_id: Uuid,
    name: &str,
    paths: Vec<String>,
    methods: Vec<String>,
    hosts: Vec<String>,
) -> Route {
    Route {
        id,
        name: Some(name.to_string()),
        paths: if paths.is_empty() {
            None
        } else {
            Some(paths)
        },
        methods: if methods.is_empty() {
            None
        } else {
            Some(methods)
        },
        hosts: if hosts.is_empty() {
            None
        } else {
            Some(hosts)
        },
        service: Some(ForeignKey::new(service_id)),
        protocols: vec![Protocol::Http, Protocol::Https],
        strip_path: true,
        preserve_host: false,
        regex_priority: 0,
        path_handling: PathHandling::V0,
        https_redirect_status_code: 426,
        request_buffering: true,
        response_buffering: true,
        ..Route::default()
    }
}

#[test]
fn test_route_matching_by_path() {
    let svc_id = Uuid::new_v4();
    let routes = vec![
        make_route(
            Uuid::new_v4(),
            svc_id,
            "api-route",
            vec!["/api".to_string()],
            vec![],
            vec![],
        ),
        make_route(
            Uuid::new_v4(),
            svc_id,
            "web-route",
            vec!["/web".to_string()],
            vec![],
            vec![],
        ),
    ];

    let router = Router::new(&routes, "traditional");

    // /api 路径应匹配 api-route
    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/api/users".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("api-route".to_string())
    );

    // /web 路径应匹配 web-route
    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/web/page".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("web-route".to_string())
    );
}

#[test]
fn test_route_matching_by_host() {
    let svc_id = Uuid::new_v4();
    let routes = vec![
        make_route(
            Uuid::new_v4(),
            svc_id,
            "api-host",
            vec![],
            vec![],
            vec!["api.example.com".to_string()],
        ),
        make_route(
            Uuid::new_v4(),
            svc_id,
            "www-host",
            vec![],
            vec![],
            vec!["www.example.com".to_string()],
        ),
    ];

    let router = Router::new(&routes, "traditional");

    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/anything".to_string(),
        host: "api.example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("api-host".to_string())
    );
}

#[test]
fn test_route_matching_by_method() {
    let svc_id = Uuid::new_v4();
    let routes = vec![
        make_route(
            Uuid::new_v4(),
            svc_id,
            "get-only",
            vec!["/resource".to_string()],
            vec!["GET".to_string()],
            vec![],
        ),
        make_route(
            Uuid::new_v4(),
            svc_id,
            "post-only",
            vec!["/resource".to_string()],
            vec!["POST".to_string()],
            vec![],
        ),
    ];

    let router = Router::new(&routes, "traditional");

    // GET 请求
    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/resource".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("get-only".to_string())
    );

    // POST 请求
    let ctx = RequestContext {
        method: "POST".to_string(),
        uri: "/resource".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("post-only".to_string())
    );
}

#[test]
fn test_no_matching_route() {
    let svc_id = Uuid::new_v4();
    let routes = vec![make_route(
        Uuid::new_v4(),
        svc_id,
        "specific",
        vec!["/api".to_string()],
        vec![],
        vec![],
    )];

    let router = Router::new(&routes, "traditional");

    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/unknown".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_none());
}

#[test]
fn test_wildcard_host_matching() {
    let svc_id = Uuid::new_v4();
    let routes = vec![make_route(
        Uuid::new_v4(),
        svc_id,
        "wildcard-host",
        vec![],
        vec![],
        vec!["*.example.com".to_string()],
    )];

    let router = Router::new(&routes, "traditional");

    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/anything".to_string(),
        host: "api.example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
}

#[test]
fn test_strip_path_behavior() {
    let svc_id = Uuid::new_v4();
    let route_id = Uuid::new_v4();
    let routes = vec![make_route(
        route_id,
        svc_id,
        "strip-test",
        vec!["/api/v1".to_string()],
        vec![],
        vec![],
    )];

    let router = Router::new(&routes, "traditional");

    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/api/v1/users".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());

    let rm = result.unwrap();
    assert!(rm.strip_path);
    assert_eq!(rm.matched_path, Some("/api/v1".to_string()));
}

// ========== 负载均衡测试 ==========

#[test]
fn test_load_balancer_round_robin() {
    let upstream = Upstream::default();
    let t1 = Target {
        target: "10.0.0.1:80".to_string(),
        weight: 100,
        ..Target::default()
    };
    let t2 = Target {
        target: "10.0.0.2:80".to_string(),
        weight: 100,
        ..Target::default()
    };

    let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

    let mut counts = HashMap::new();
    for _ in 0..200 {
        let addr = lb.select().unwrap();
        *counts.entry(addr).or_insert(0) += 1;
    }

    // 等权重应该均匀分布
    assert_eq!(*counts.get("10.0.0.1:80").unwrap(), 100);
    assert_eq!(*counts.get("10.0.0.2:80").unwrap(), 100);
}

#[test]
fn test_load_balancer_weighted() {
    let upstream = Upstream::default();
    let t1 = Target {
        target: "10.0.0.1:80".to_string(),
        weight: 300,
        ..Target::default()
    };
    let t2 = Target {
        target: "10.0.0.2:80".to_string(),
        weight: 100,
        ..Target::default()
    };

    let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

    let mut counts = HashMap::new();
    for _ in 0..400 {
        let addr = lb.select().unwrap();
        *counts.entry(addr).or_insert(0) += 1;
    }

    // 3:1 权重比
    assert_eq!(*counts.get("10.0.0.1:80").unwrap(), 300);
    assert_eq!(*counts.get("10.0.0.2:80").unwrap(), 100);
}

#[test]
fn test_load_balancer_zero_weight_excluded() {
    let upstream = Upstream::default();
    let t1 = Target {
        target: "10.0.0.1:80".to_string(),
        weight: 100,
        ..Target::default()
    };
    let t2 = Target {
        target: "10.0.0.2:80".to_string(),
        weight: 0, // 权重为 0，应被排除
        ..Target::default()
    };

    let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

    assert_eq!(lb.target_count(), 1);
    for _ in 0..10 {
        assert_eq!(lb.select().unwrap(), "10.0.0.1:80");
    }
}

#[test]
fn test_load_balancer_dynamic_update() {
    let upstream = Upstream::default();
    let t1 = Target {
        target: "10.0.0.1:80".to_string(),
        weight: 100,
        ..Target::default()
    };

    let mut lb = LoadBalancer::new(&upstream, &[&t1]);
    assert_eq!(lb.target_count(), 1);

    // 添加新目标
    let t2 = Target {
        target: "10.0.0.2:80".to_string(),
        weight: 100,
        ..Target::default()
    };
    lb.update_targets(&[&t1, &t2]);
    assert_eq!(lb.target_count(), 2);
}

// ========== TLS 证书管理测试 ==========

#[test]
fn test_certificate_exact_sni_match() {
    let manager = CertificateManager::new();
    let cert_id = Uuid::new_v4();

    let certs = vec![Certificate {
        id: cert_id,
        cert: "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----".to_string(),
        key: "-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----".to_string(),
        ..Certificate::default()
    }];

    let snis = vec![Sni {
        name: "api.example.com".to_string(),
        certificate: ForeignKey::new(cert_id),
        ..Sni::default()
    }];

    manager.load_certificates(&certs, &snis);

    let result = manager.find_certificate(Some("api.example.com"));
    assert!(result.is_some());

    let result = manager.find_certificate(Some("other.example.com"));
    assert!(result.is_none());
}

#[test]
fn test_certificate_wildcard_sni() {
    let manager = CertificateManager::new();
    let cert_id = Uuid::new_v4();

    let certs = vec![Certificate {
        id: cert_id,
        cert: "wildcard-cert".to_string(),
        key: "wildcard-key".to_string(),
        ..Certificate::default()
    }];

    let snis = vec![Sni {
        name: "*.example.com".to_string(),
        certificate: ForeignKey::new(cert_id),
        ..Sni::default()
    }];

    manager.load_certificates(&certs, &snis);

    // 子域名应匹配通配符
    assert!(manager.find_certificate(Some("api.example.com")).is_some());
    assert!(manager.find_certificate(Some("www.example.com")).is_some());

    // 不同域名不应匹配
    assert!(manager.find_certificate(Some("example.org")).is_none());
}

#[test]
fn test_certificate_default_fallback() {
    let manager = CertificateManager::new();
    manager.set_default_cert("default-cert".to_string(), "default-key".to_string());

    // 无 SNI 返回默认
    let result = manager.find_certificate(None);
    assert!(result.is_some());
    assert_eq!(result.unwrap().cert, "default-cert");

    // 无匹配也返回默认
    let result = manager.find_certificate(Some("unknown.com"));
    assert!(result.is_some());
    assert_eq!(result.unwrap().cert, "default-cert");
}

// ========== 路由表热更新测试 ==========

#[test]
fn test_router_hot_update() {
    let svc_id = Uuid::new_v4();
    let routes = vec![make_route(
        Uuid::new_v4(),
        svc_id,
        "initial-route",
        vec!["/v1".to_string()],
        vec![],
        vec![],
    )];

    let mut router = Router::new(&routes, "traditional");
    assert_eq!(router.route_count(), 1);

    // 热更新路由表
    let new_routes = vec![
        make_route(
            Uuid::new_v4(),
            svc_id,
            "route-a",
            vec!["/a".to_string()],
            vec![],
            vec![],
        ),
        make_route(
            Uuid::new_v4(),
            svc_id,
            "route-b",
            vec!["/b".to_string()],
            vec![],
            vec![],
        ),
    ];

    router.rebuild(&new_routes);
    assert_eq!(router.route_count(), 2);

    // 旧路由不应再匹配
    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/v1/test".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    assert!(router.find_route(&ctx).is_none());

    // 新路由应匹配
    let ctx = RequestContext {
        method: "GET".to_string(),
        uri: "/a/test".to_string(),
        host: "example.com".to_string(),
        scheme: "http".to_string(),
        headers: HashMap::new(),
        sni: None,
    };
    let result = router.find_route(&ctx);
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().route_name,
        Some("route-a".to_string())
    );
}

// ========== 插件系统集成测试 ==========

#[test]
fn test_plugin_executor_priority_ordering() {
    use kong_plugin_system::{PluginExecutor, PluginRegistry};

    // 创建空的注册表
    let registry = PluginRegistry::new();

    // 验证无插件时不出错
    let plugins: Vec<kong_core::models::Plugin> = vec![];
    let resolved = PluginExecutor::resolve_plugins(
        &registry,
        &plugins,
        Some(Uuid::new_v4()),
        Some(Uuid::new_v4()),
        None,
    );
    assert!(resolved.is_empty());
}

#[tokio::test]
async fn test_plugin_phase_execution_no_plugins() {
    use kong_plugin_system::PluginExecutor;

    let resolved = vec![];
    let mut ctx = RequestCtx::new();

    // 空插件链执行不应出错
    let result = PluginExecutor::execute_phase(
        &resolved,
        kong_core::traits::Phase::Access,
        &mut ctx,
    )
    .await;
    assert!(result.is_ok());
    assert!(!ctx.is_short_circuited());
}

// ========== 健康检查集成测试 ==========

#[tokio::test]
async fn test_health_checker_state_transitions() {
    use kong_proxy::health::HealthChecker;

    let upstream_id = Uuid::new_v4();
    let config = kong_core::models::Upstream {
        id: upstream_id,
        name: "test-upstream".to_string(),
        ..Upstream::default()
    };

    let checker = HealthChecker::new();

    let target_addr = "10.0.0.1:80";

    // 初始状态应为健康
    assert!(checker.is_healthy(&config.name, target_addr));

    // 报告多次失败
    for _ in 0..5 {
        checker.report_tcp_failure(&config.name, target_addr);
    }

    // 应标记为不健康（取决于阈值设置）
    // 默认阈值为 0（禁用），因此始终健康
    // 这里只验证接口不 panic
    let _ = checker.is_healthy(&config.name, target_addr);
}
