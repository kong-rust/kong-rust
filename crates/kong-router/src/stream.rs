//! Stream (L4) 路由引擎 — 基于 source/destination/SNI 匹配
//!
//! 与 HTTP 路由器完全独立，仅处理 protocols 包含 tcp/tls/tls_passthrough 的路由。
//!
//! 匹配优先级：
//! 1. SNI 精确匹配 > 通配符匹配
//! 2. Source IP/Port 匹配（CIDR）
//! 3. Destination IP/Port 匹配
//! 4. 更具体的规则优先（匹配维度越多优先级越高）

use std::net::IpAddr;
use uuid::Uuid;

use kong_core::models::{CidrPort, Protocol, Route};

/// Stream 请求上下文 — 从 L4 连接中提取的匹配字段
#[derive(Debug, Clone, Default)]
pub struct StreamRequestContext {
    /// 客户端 IP
    pub source_ip: Option<IpAddr>,
    /// 客户端端口
    pub source_port: Option<u16>,
    /// 监听地址 IP（目标 IP）
    pub dest_ip: Option<IpAddr>,
    /// 监听地址端口（目标端口）
    pub dest_port: Option<u16>,
    /// TLS SNI（如果是 TLS 连接）
    pub sni: Option<String>,
}

/// Stream 路由匹配结果
#[derive(Debug, Clone)]
pub struct StreamRouteMatch {
    /// 匹配到的路由 ID
    pub route_id: Uuid,
    /// 关联的 Service ID
    pub service_id: Option<Uuid>,
    /// 路由名称
    pub route_name: Option<String>,
    /// 协议列表（tcp/tls/tls_passthrough）
    pub protocols: Vec<Protocol>,
}

// ============ 匹配权重位值 ============

const MATCH_SNI: u32 = 0x08;
const MATCH_SOURCE: u32 = 0x04;
const MATCH_DEST: u32 = 0x02;

/// 已处理的 Stream 路由
#[derive(Debug, Clone)]
struct ProcessedStreamRoute {
    route_id: Uuid,
    service_id: Option<Uuid>,
    name: Option<String>,
    protocols: Vec<Protocol>,

    /// 匹配规则位掩码
    match_rules: u32,
    /// 匹配权重（指定的匹配条件数量）
    match_weight: u32,

    /// SNI 列表（小写，支持通配符）
    snis: Vec<String>,

    /// Source IP/Port 匹配规则
    sources: Vec<CidrPortMatcher>,

    /// Destination IP/Port 匹配规则
    destinations: Vec<CidrPortMatcher>,

    /// 创建时间（FIFO 排序）
    created_at: i64,
}

/// 预处理后的 CIDR + Port 匹配器
#[derive(Debug, Clone)]
struct CidrPortMatcher {
    /// 解析后的网络地址和前缀长度
    network: Option<(IpAddr, u8)>,
    /// 端口匹配
    port: Option<u16>,
}

impl CidrPortMatcher {
    fn from_cidr_port(cp: &CidrPort) -> Self {
        let network = cp.ip.as_ref().and_then(|ip_str| parse_cidr(ip_str));
        Self {
            network,
            port: cp.port,
        }
    }

    /// 检查 IP 和端口是否匹配
    fn matches(&self, ip: Option<IpAddr>, port: Option<u16>) -> bool {
        // IP 匹配
        if let Some((net_addr, prefix_len)) = &self.network {
            match ip {
                Some(client_ip) => {
                    if !ip_in_cidr(client_ip, *net_addr, *prefix_len) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // 端口匹配
        if let Some(expected_port) = self.port {
            match port {
                Some(p) if p == expected_port => {}
                _ => return false,
            }
        }

        true
    }
}

/// Stream 路由引擎
pub struct StreamRouter {
    /// 所有已处理的 Stream 路由（按优先级排序）
    routes: Vec<ProcessedStreamRoute>,
}

impl StreamRouter {
    /// 从路由列表构建 Stream 路由器
    ///
    /// 仅索引 protocols 包含 tcp/tls/tls_passthrough 的路由
    pub fn new(routes: &[Route]) -> Self {
        let mut processed = Vec::new();

        for route in routes {
            if !is_stream_route(route) {
                continue;
            }
            if let Some(pr) = process_stream_route(route) {
                processed.push(pr);
            }
        }

        // 排序：match_weight 高 → created_at 早
        processed.sort_by(|a, b| {
            b.match_weight
                .cmp(&a.match_weight)
                .then(a.created_at.cmp(&b.created_at))
        });

        tracing::info!(
            "Stream 路由器初始化完成: {} 条路由",
            processed.len()
        );

        Self { routes: processed }
    }

    /// 匹配 Stream 请求
    pub fn find_route(&self, ctx: &StreamRequestContext) -> Option<StreamRouteMatch> {
        for route in &self.routes {
            if self.match_route(route, ctx) {
                return Some(StreamRouteMatch {
                    route_id: route.route_id,
                    service_id: route.service_id,
                    route_name: route.name.clone(),
                    protocols: route.protocols.clone(),
                });
            }
        }
        None
    }

    /// 路由数量
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// 重建路由表
    pub fn rebuild(&mut self, routes: &[Route]) {
        *self = Self::new(routes);
    }

    /// 检查单个路由是否匹配
    fn match_route(&self, route: &ProcessedStreamRoute, ctx: &StreamRequestContext) -> bool {
        // SNI 匹配
        if route.match_rules & MATCH_SNI != 0 {
            match &ctx.sni {
                Some(sni) => {
                    if !match_sni(&route.snis, sni) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // Source 匹配
        if route.match_rules & MATCH_SOURCE != 0 {
            let mut any_match = false;
            for src in &route.sources {
                if src.matches(ctx.source_ip, ctx.source_port) {
                    any_match = true;
                    break;
                }
            }
            if !any_match {
                return false;
            }
        }

        // Destination 匹配
        if route.match_rules & MATCH_DEST != 0 {
            let mut any_match = false;
            for dst in &route.destinations {
                if dst.matches(ctx.dest_ip, ctx.dest_port) {
                    any_match = true;
                    break;
                }
            }
            if !any_match {
                return false;
            }
        }

        true
    }
}

// ============ 辅助函数 ============

/// 判断路由是否为 Stream 路由
fn is_stream_route(route: &Route) -> bool {
    route.protocols.iter().any(|p| {
        matches!(
            p,
            Protocol::Tcp | Protocol::Tls | Protocol::TlsPassthrough
        )
    })
}

/// 处理单个 Stream 路由
fn process_stream_route(route: &Route) -> Option<ProcessedStreamRoute> {
    let mut match_rules = 0u32;
    let mut match_weight = 0u32;

    // SNI
    let snis: Vec<String> = route
        .snis
        .as_ref()
        .map(|s| s.iter().map(|v| v.to_lowercase()).collect())
        .unwrap_or_default();

    if !snis.is_empty() {
        match_rules |= MATCH_SNI;
        match_weight += 1;
    }

    // Sources
    let sources: Vec<CidrPortMatcher> = route
        .sources
        .as_ref()
        .map(|s| s.iter().map(|cp| CidrPortMatcher::from_cidr_port(cp)).collect())
        .unwrap_or_default();

    if !sources.is_empty() {
        match_rules |= MATCH_SOURCE;
        match_weight += 1;
    }

    // Destinations
    let destinations: Vec<CidrPortMatcher> = route
        .destinations
        .as_ref()
        .map(|d| d.iter().map(|cp| CidrPortMatcher::from_cidr_port(cp)).collect())
        .unwrap_or_default();

    if !destinations.is_empty() {
        match_rules |= MATCH_DEST;
        match_weight += 1;
    }

    let service_id = route.service.as_ref().map(|fk| fk.id);

    Some(ProcessedStreamRoute {
        route_id: route.id,
        service_id,
        name: route.name.clone(),
        protocols: route.protocols.clone(),
        match_rules,
        match_weight,
        snis,
        sources,
        destinations,
        created_at: route.created_at,
    })
}

/// SNI 匹配（支持通配符 *.example.com）
fn match_sni(patterns: &[String], sni: &str) -> bool {
    let sni_lower = sni.to_lowercase();
    for pattern in patterns {
        if pattern == &sni_lower {
            return true;
        }
        // 通配符匹配：*.example.com 匹配 foo.example.com（但不匹配 example.com）
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // SNI 需要至少有一个点，且点后面的部分与 suffix 匹配
            if let Some(dot_pos) = sni_lower.find('.') {
                if &sni_lower[dot_pos + 1..] == suffix {
                    return true;
                }
            }
        }
    }
    false
}

/// 解析 CIDR 表示（如 "192.168.0.0/16" 或 "10.0.0.1"）
fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    if let Some((ip_str, prefix_str)) = cidr.split_once('/') {
        let ip: IpAddr = ip_str.parse().ok()?;
        let prefix: u8 = prefix_str.parse().ok()?;
        Some((ip, prefix))
    } else {
        // 无前缀长度，精确匹配
        let ip: IpAddr = cidr.parse().ok()?;
        let prefix = if ip.is_ipv4() { 32 } else { 128 };
        Some((ip, prefix))
    }
}

/// 检查 IP 是否在 CIDR 范围内
fn ip_in_cidr(ip: IpAddr, network: IpAddr, prefix_len: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            if prefix_len == 0 {
                return true;
            }
            if prefix_len > 32 {
                return false;
            }
            let ip_bits = u32::from(ip);
            let net_bits = u32::from(net);
            let mask = !0u32 << (32 - prefix_len);
            (ip_bits & mask) == (net_bits & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            if prefix_len == 0 {
                return true;
            }
            if prefix_len > 128 {
                return false;
            }
            let ip_bits = u128::from(ip);
            let net_bits = u128::from(net);
            let mask = !0u128 << (128 - prefix_len);
            (ip_bits & mask) == (net_bits & mask)
        }
        _ => false, // IPv4 和 IPv6 不互相匹配
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kong_core::models::ForeignKey;

    fn make_stream_route(
        name: &str,
        protocols: Vec<Protocol>,
        snis: Option<Vec<&str>>,
        sources: Option<Vec<CidrPort>>,
        destinations: Option<Vec<CidrPort>>,
    ) -> Route {
        Route {
            id: Uuid::new_v4(),
            name: Some(name.to_string()),
            protocols,
            snis: snis.map(|s| s.into_iter().map(|v| v.to_string()).collect()),
            sources,
            destinations,
            service: Some(ForeignKey::new(Uuid::new_v4())),
            created_at: 1609459200,
            ..Route::default()
        }
    }

    #[test]
    fn test_sni_match() {
        let routes = vec![make_stream_route(
            "tls-route",
            vec![Protocol::Tls],
            Some(vec!["api.example.com"]),
            None,
            None,
        )];
        let router = StreamRouter::new(&routes);
        assert_eq!(router.route_count(), 1);

        let ctx = StreamRequestContext {
            sni: Some("api.example.com".to_string()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        let ctx2 = StreamRequestContext {
            sni: Some("other.example.com".to_string()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx2).is_none());
    }

    #[test]
    fn test_wildcard_sni_match() {
        let routes = vec![make_stream_route(
            "wildcard-tls",
            vec![Protocol::TlsPassthrough],
            Some(vec!["*.example.com"]),
            None,
            None,
        )];
        let router = StreamRouter::new(&routes);

        let ctx = StreamRequestContext {
            sni: Some("api.example.com".to_string()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        // 不匹配 example.com 本身
        let ctx2 = StreamRequestContext {
            sni: Some("example.com".to_string()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx2).is_none());
    }

    #[test]
    fn test_source_ip_match() {
        let routes = vec![make_stream_route(
            "tcp-route",
            vec![Protocol::Tcp],
            None,
            Some(vec![CidrPort {
                ip: Some("192.168.0.0/16".to_string()),
                port: None,
            }]),
            None,
        )];
        let router = StreamRouter::new(&routes);

        let ctx = StreamRequestContext {
            source_ip: Some("192.168.1.100".parse().unwrap()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        let ctx2 = StreamRequestContext {
            source_ip: Some("10.0.0.1".parse().unwrap()),
            ..Default::default()
        };
        assert!(router.find_route(&ctx2).is_none());
    }

    #[test]
    fn test_destination_port_match() {
        let routes = vec![make_stream_route(
            "tcp-port",
            vec![Protocol::Tcp],
            None,
            None,
            Some(vec![CidrPort {
                ip: None,
                port: Some(9000),
            }]),
        )];
        let router = StreamRouter::new(&routes);

        let ctx = StreamRequestContext {
            dest_port: Some(9000),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        let ctx2 = StreamRequestContext {
            dest_port: Some(9001),
            ..Default::default()
        };
        assert!(router.find_route(&ctx2).is_none());
    }

    #[test]
    fn test_more_specific_route_wins() {
        let r1 = make_stream_route(
            "dest-only",
            vec![Protocol::Tcp],
            None,
            None,
            Some(vec![CidrPort { ip: None, port: Some(9000) }]),
        );
        let r2 = make_stream_route(
            "src-and-dest",
            vec![Protocol::Tcp],
            None,
            Some(vec![CidrPort {
                ip: Some("10.0.0.0/8".to_string()),
                port: None,
            }]),
            Some(vec![CidrPort { ip: None, port: Some(9000) }]),
        );
        let router = StreamRouter::new(&[r1, r2]);

        let ctx = StreamRequestContext {
            source_ip: Some("10.0.0.5".parse().unwrap()),
            dest_port: Some(9000),
            ..Default::default()
        };
        let result = router.find_route(&ctx).unwrap();
        assert_eq!(result.route_name, Some("src-and-dest".to_string()));
    }

    #[test]
    fn test_http_routes_ignored() {
        let routes = vec![Route {
            id: Uuid::new_v4(),
            protocols: vec![Protocol::Http, Protocol::Https],
            paths: Some(vec!["/api".to_string()]),
            ..Route::default()
        }];
        let router = StreamRouter::new(&routes);
        assert_eq!(router.route_count(), 0);
    }

    #[test]
    fn test_cidr_matching() {
        assert!(ip_in_cidr(
            "192.168.1.100".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));
        assert!(!ip_in_cidr(
            "10.0.0.1".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));
        // 精确匹配
        assert!(ip_in_cidr(
            "10.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            32
        ));
        assert!(!ip_in_cidr(
            "10.0.0.2".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            32
        ));
    }

    #[test]
    fn test_parse_cidr() {
        let (ip, prefix) = parse_cidr("192.168.0.0/16").unwrap();
        assert_eq!(ip, "192.168.0.0".parse::<IpAddr>().unwrap());
        assert_eq!(prefix, 16);

        let (ip, prefix) = parse_cidr("10.0.0.1").unwrap();
        assert_eq!(ip, "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(prefix, 32);
    }
}
