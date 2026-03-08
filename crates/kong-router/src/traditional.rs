//! Traditional routing engine — fully compatible with Kong traditional router — 传统路由匹配引擎 — 与 Kong traditional router 完全一致
//!
//! Match rule priority (bit values) — 匹配规则优先级（位值）：
//! HOST(0x40) > HEADER(0x20) > URI(0x10) > METHOD(0x08) > SNI(0x04)
//!
//! Route sorting order — 路由排序：
//! submatch_weight > header count > regex_priority > URI length > created_at

use regex::Regex;
use std::collections::HashMap;
use uuid::Uuid;

use crate::{RequestContext, RouteMatch};
use kong_core::models::Route;

// ============ Match rule bit value definitions — 匹配规则位值定义 ============

const MATCH_HOST: u32 = 0x40;
const MATCH_HEADER: u32 = 0x20;
const MATCH_URI: u32 = 0x10;
const MATCH_METHOD: u32 = 0x08;
const MATCH_SNI: u32 = 0x04;

/// Sub-match rules — 子匹配规则
const SUB_HAS_REGEX_URI: u32 = 0x01;
const SUB_PLAIN_HOSTS_ONLY: u32 = 0x02;
#[allow(dead_code)]
const SUB_HAS_WILDCARD_HOST_PORT: u32 = 0x04;

/// HTTP match rule sort order — HTTP 匹配规则排序顺序
#[allow(dead_code)]
const SORTED_MATCH_RULES: &[u32] = &[MATCH_HOST, MATCH_HEADER, MATCH_URI, MATCH_METHOD, MATCH_SNI];

// ============ Internal route representation — 内部路由表示 ============

/// Processed route (used for matching) — 已处理的路由（用于匹配）
#[derive(Debug, Clone)]
struct ProcessedRoute {
    /// Original route ID — 原始路由 ID
    route_id: Uuid,
    /// Associated Service ID — 关联的 Service ID
    service_id: Option<Uuid>,
    /// Route name — 路由名称
    name: Option<String>,

    /// Match weight (number of specified match conditions) — 匹配权重（指定了多少个匹配条件）
    match_weight: u32,
    /// Sub-match weight (feature bit flags) — 子匹配权重（特征位标志）
    submatch_weight: u32,
    /// Match rules bitmask — 匹配规则位掩码
    match_rules: u32,

    /// Exact host list (lowercased) — 精确 host 列表（小写）
    plain_hosts: Vec<String>,
    /// Wildcard host regex list — 通配符 host 正则列表
    wildcard_hosts: Vec<(String, Regex)>,
    /// Whether only exact hosts are present — 是否只有精确 host
    #[allow(dead_code)]
    plain_hosts_only: bool,

    /// Exact path list (prefix matching) — 精确 path 列表（前缀匹配）
    prefix_paths: Vec<String>,
    /// Regex path list — 正则 path 列表
    regex_paths: Vec<(String, Regex)>,
    /// Maximum URI length — 最长 URI 长度
    max_uri_length: usize,

    /// Allowed HTTP methods (uppercased) — 允许的 HTTP 方法（大写）
    methods: HashMap<String, bool>,

    /// Header match rules — Header 匹配规则
    headers: Vec<HeaderMatcher>,
    /// Header count — Header 数量
    header_count: usize,

    /// SNI list — SNI 列表
    snis: HashMap<String, bool>,

    /// Regex priority — 正则优先级
    regex_priority: i64,
    /// Creation time (for FIFO ordering) — 创建时间（用于 FIFO 排序）
    created_at: i64,

    /// strip_path setting — strip_path 设置
    strip_path: bool,
    /// preserve_host setting — preserve_host 设置
    preserve_host: bool,
    /// path_handling setting — path_handling 设置
    path_handling: String,
    /// Protocol list — 协议列表
    protocols: Vec<String>,
}

/// Header match rule — Header 匹配规则
#[derive(Debug, Clone)]
struct HeaderMatcher {
    /// Header name (lowercased) — header 名称（小写）
    name: String,
    /// Exact match value set (lowercased) — 精确匹配值集合（小写）
    values: HashMap<String, bool>,
    /// Regex match pattern — 正则匹配模式
    regex_pattern: Option<Regex>,
}

// ============ Category ============

/// Route category (grouped by match rules) — 路由类别（按匹配规则分组）
struct Category {
    /// Category bitmask — 类别的位掩码
    #[allow(dead_code)]
    match_rules: u32,
    /// Match weight — 匹配权重
    match_weight: u32,
    /// Route indices in this category — 该类别下的路由索引
    routes: Vec<usize>,
}

// ============ Traditional router — 传统路由器 ============

/// Traditional routing engine — 传统路由匹配引擎
pub struct TraditionalRouter {
    /// All processed routes — 所有已处理的路由
    routes: Vec<ProcessedRoute>,
    /// Routes grouped by category (sorted) — 按 category 分组的路由（已排序）
    categories: Vec<Category>,
    /// Exact host -> route indices (for fast index optimization) — 精确 host -> 路由索引（用于快速索引优化）
    #[allow(dead_code)]
    host_index: HashMap<String, Vec<usize>>,
    /// Exact path -> route indices — 精确 path -> 路由索引
    #[allow(dead_code)]
    path_index: HashMap<String, Vec<usize>>,
    /// method -> route indices — method -> 路由索引
    #[allow(dead_code)]
    method_index: HashMap<String, Vec<usize>>,
    /// sni -> route indices — sni -> 路由索引
    #[allow(dead_code)]
    sni_index: HashMap<String, Vec<usize>>,
}

impl TraditionalRouter {
    /// Build router from route list — 从路由列表构建路由器
    pub fn new(routes: &[Route]) -> Self {
        let mut processed = Vec::with_capacity(routes.len());

        // 1. Process each route — 处理每个路由
        for route in routes {
            if let Some(pr) = process_route(route) {
                processed.push(pr);
            }
        }

        // 2. Sort routes — 排序路由
        processed.sort_by(|a, b| sort_routes(a, b));

        // 3. Build indices — 构建索引
        let mut host_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut path_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut method_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut sni_index: HashMap<String, Vec<usize>> = HashMap::new();

        for (i, route) in processed.iter().enumerate() {
            for host in &route.plain_hosts {
                host_index.entry(host.clone()).or_default().push(i);
            }
            for path in &route.prefix_paths {
                path_index.entry(path.clone()).or_default().push(i);
            }
            for method in route.methods.keys() {
                method_index.entry(method.clone()).or_default().push(i);
            }
            for sni in route.snis.keys() {
                sni_index.entry(sni.clone()).or_default().push(i);
            }
        }

        // 4. Group by match_rules — 按 match_rules 分类
        let mut category_map: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, route) in processed.iter().enumerate() {
            category_map
                .entry(route.match_rules)
                .or_default()
                .push(i);
        }

        let mut categories: Vec<Category> = category_map
            .into_iter()
            .map(|(match_rules, routes)| {
                let match_weight = processed[routes[0]].match_weight;
                Category {
                    match_rules,
                    match_weight,
                    routes,
                }
            })
            .collect();

        // Sort categories: higher match_weight first — 排序 categories：match_weight 高的优先
        categories.sort_by(|a, b| {
            b.match_weight
                .cmp(&a.match_weight)
                .then(b.match_rules.cmp(&a.match_rules))
        });

        tracing::info!(
            "传统路由器初始化完成: {} 条路由, {} 个类别",
            processed.len(),
            categories.len()
        );

        Self {
            routes: processed,
            categories,
            host_index,
            path_index,
            method_index,
            sni_index,
        }
    }

    /// Match a request — 匹配请求
    pub fn find_route(&self, ctx: &RequestContext) -> Option<RouteMatch> {
        let req_host = ctx.host.to_lowercase();
        let req_host_no_port = req_host.split(':').next().unwrap_or(&req_host);

        // Iterate categories (starting from highest weight) — 遍历 categories（从权重最高的开始）
        for category in &self.categories {
            for &route_idx in &category.routes {
                let route = &self.routes[route_idx];

                if self.match_route(route, ctx, &req_host, req_host_no_port) {
                    // Calculate the matched path — 计算匹配的路径
                    let matched_path = self.find_matched_path(route, &ctx.uri);

                    return Some(RouteMatch {
                        route_id: route.route_id,
                        service_id: route.service_id,
                        route_name: route.name.clone(),
                        strip_path: route.strip_path,
                        preserve_host: route.preserve_host,
                        path_handling: route.path_handling.clone(),
                        matched_path,
                        protocols: route.protocols.clone(),
                    });
                }
            }
        }

        None
    }

    /// Check if a single route matches the request — 检查单个路由是否匹配请求
    fn match_route(
        &self,
        route: &ProcessedRoute,
        ctx: &RequestContext,
        req_host: &str,
        req_host_no_port: &str,
    ) -> bool {
        // HOST matching — HOST 匹配
        if route.match_rules & MATCH_HOST != 0 {
            if !self.match_host(route, req_host, req_host_no_port) {
                return false;
            }
        }

        // HEADER matching — HEADER 匹配
        if route.match_rules & MATCH_HEADER != 0 {
            if !self.match_headers(route, &ctx.headers) {
                return false;
            }
        }

        // URI matching — URI 匹配
        if route.match_rules & MATCH_URI != 0 {
            if !self.match_uri(route, &ctx.uri) {
                return false;
            }
        }

        // METHOD matching — METHOD 匹配
        if route.match_rules & MATCH_METHOD != 0 {
            if !route.methods.contains_key(&ctx.method.to_uppercase()) {
                return false;
            }
        }

        // SNI matching — SNI 匹配
        if route.match_rules & MATCH_SNI != 0 {
            // HTTP requests unconditionally match SNI — HTTP 请求无条件匹配 SNI
            if ctx.scheme != "http" && ctx.scheme != "https" {
                if let Some(ref sni) = ctx.sni {
                    if !route.snis.contains_key(sni) {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        }

        true
    }

    /// HOST matching — HOST 匹配
    fn match_host(
        &self,
        route: &ProcessedRoute,
        req_host: &str,
        req_host_no_port: &str,
    ) -> bool {
        // 1. Exact match — 精确匹配
        if route.plain_hosts.contains(&req_host.to_string())
            || route.plain_hosts.contains(&req_host_no_port.to_string())
        {
            return true;
        }

        // 2. Wildcard match — 通配符匹配
        for (_, regex) in &route.wildcard_hosts {
            if regex.is_match(req_host) || regex.is_match(req_host_no_port) {
                return true;
            }
        }

        false
    }

    /// HEADER matching (all headers must match = AND logic) — HEADER 匹配（所有 header 都必须匹配 = AND 逻辑）
    fn match_headers(
        &self,
        route: &ProcessedRoute,
        req_headers: &HashMap<String, String>,
    ) -> bool {
        for header_matcher in &route.headers {
            let header_val = req_headers.get(&header_matcher.name);

            match header_val {
                None => return false,
                Some(val) => {
                    let val_lower = val.to_lowercase();
                    let mut found = header_matcher.values.contains_key(&val_lower);

                    if !found {
                        if let Some(ref regex) = header_matcher.regex_pattern {
                            found = regex.is_match(&val_lower);
                        }
                    }

                    if !found {
                        return false;
                    }
                }
            }
        }

        true
    }

    /// URI matching — URI 匹配
    fn match_uri(&self, route: &ProcessedRoute, req_uri: &str) -> bool {
        // 1. Regex match (takes priority) — 正则匹配（优先）
        for (_, regex) in &route.regex_paths {
            if regex.is_match(req_uri) {
                return true;
            }
        }

        // 2. Exact prefix match — 精确前缀匹配
        for path in &route.prefix_paths {
            if req_uri == path || req_uri.starts_with(&format!("{}/", path.trim_end_matches('/')))
                || path == "/"
            {
                return true;
            }
        }

        false
    }

    /// Find the matched path (used for strip_path) — 查找匹配的路径（用于 strip_path）
    fn find_matched_path(&self, route: &ProcessedRoute, req_uri: &str) -> Option<String> {
        // Check regex — 检查正则
        for (pattern, regex) in &route.regex_paths {
            if regex.is_match(req_uri) {
                return Some(pattern.clone());
            }
        }

        // Check prefix (return longest match) — 检查前缀（返回最长匹配）
        let mut best_match: Option<&str> = None;
        for path in &route.prefix_paths {
            if req_uri == path
                || req_uri.starts_with(&format!("{}/", path.trim_end_matches('/')))
                || path == "/"
            {
                if best_match.map_or(true, |b| path.len() > b.len()) {
                    best_match = Some(path);
                }
            }
        }

        best_match.map(|s| s.to_string())
    }

    /// Number of routes — 路由数量
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

// ============ Route processing — 路由处理 ============

/// Convert a Route model to ProcessedRoute — 将 Route 模型转换为 ProcessedRoute
fn process_route(route: &Route) -> Option<ProcessedRoute> {
    let mut match_weight = 0u32;
    let mut submatch_weight = 0u32;
    let mut match_rules = 0u32;

    // Hosts
    let mut plain_hosts = Vec::new();
    let mut wildcard_hosts = Vec::new();
    let mut plain_hosts_only = true;

    if let Some(ref hosts) = route.hosts {
        if !hosts.is_empty() {
            match_weight += 1;
            match_rules |= MATCH_HOST;

            for host in hosts {
                let host_lower = host.to_lowercase();
                if host_lower.contains('*') {
                    plain_hosts_only = false;
                    // Wildcard -> regex — 通配符 -> 正则
                    let regex_str = format!(
                        "^{}$",
                        host_lower
                            .replace('.', "\\.")
                            .replace('*', ".+")
                    );
                    if let Ok(regex) = Regex::new(&regex_str) {
                        wildcard_hosts.push((host_lower, regex));
                    }
                } else {
                    plain_hosts.push(host_lower);
                }
            }

            if plain_hosts_only && !plain_hosts.is_empty() {
                submatch_weight |= SUB_PLAIN_HOSTS_ONLY;
            }
        }
    }

    // Paths
    let mut prefix_paths = Vec::new();
    let mut regex_paths = Vec::new();
    let mut max_uri_length = 0;

    if let Some(ref paths) = route.paths {
        if !paths.is_empty() {
            match_weight += 1;
            match_rules |= MATCH_URI;

            for path in paths {
                if let Some(regex_pattern) = path.strip_prefix('~') {
                    // Regex path — 正则路径
                    submatch_weight |= SUB_HAS_REGEX_URI;
                    if let Ok(regex) = Regex::new(regex_pattern) {
                        regex_paths.push((regex_pattern.to_string(), regex));
                    }
                } else {
                    // Prefix path — 前缀路径
                    max_uri_length = max_uri_length.max(path.len());
                    prefix_paths.push(path.clone());
                }
            }

            // Sort prefix paths by length descending (longest prefix matches first) — 前缀路径按长度降序排序（最长前缀先匹配）
            prefix_paths.sort_by(|a, b| b.len().cmp(&a.len()));
        }
    }

    // Methods
    let mut methods = HashMap::new();
    if let Some(ref method_list) = route.methods {
        if !method_list.is_empty() {
            match_weight += 1;
            match_rules |= MATCH_METHOD;
            for m in method_list {
                methods.insert(m.to_uppercase(), true);
            }
        }
    }

    // Headers
    let mut headers = Vec::new();
    if let Some(ref header_map) = route.headers {
        if !header_map.is_empty() {
            match_weight += 1;
            match_rules |= MATCH_HEADER;

            for (name, values) in header_map {
                let mut value_map = HashMap::new();
                let mut regex_pattern = None;

                for s in values {
                    if let Some(pattern) = s.strip_prefix("~*") {
                        regex_pattern = Regex::new(pattern).ok();
                    } else {
                        value_map.insert(s.to_lowercase(), true);
                    }
                }

                headers.push(HeaderMatcher {
                    name: name.to_lowercase(),
                    values: value_map,
                    regex_pattern,
                });
            }
        }
    }

    // SNIs
    let mut snis = HashMap::new();
    if let Some(ref sni_list) = route.snis {
        if !sni_list.is_empty() {
            match_weight += 1;
            match_rules |= MATCH_SNI;
            for s in sni_list {
                let sni = s.trim_end_matches('.');
                snis.insert(sni.to_string(), true);
            }
        }
    }

    // Service ID
    let service_id = route.service.as_ref().map(|fk| fk.id);

    // Protocols
    let protocols: Vec<String> = route
        .protocols
        .iter()
        .map(|p| p.to_string())
        .collect();

    let header_count = headers.len();

    Some(ProcessedRoute {
        route_id: route.id,
        service_id,
        name: route.name.clone(),
        match_weight,
        submatch_weight,
        match_rules,
        plain_hosts,
        wildcard_hosts,
        plain_hosts_only,
        prefix_paths,
        regex_paths,
        max_uri_length,
        methods,
        headers,
        header_count,
        snis,
        regex_priority: route.regex_priority as i64,
        created_at: route.created_at,
        strip_path: route.strip_path,
        preserve_host: route.preserve_host,
        path_handling: match &route.path_handling {
            kong_core::models::PathHandling::V0 => "v0".to_string(),
            kong_core::models::PathHandling::V1 => "v1".to_string(),
        },
        protocols,
    })
}

/// Route sorting comparison function (consistent with Kong's sort_routes) — 路由排序比较函数（与 Kong 的 sort_routes 一致）
fn sort_routes(a: &ProcessedRoute, b: &ProcessedRoute) -> std::cmp::Ordering {
    // 1. submatch_weight (bit flags, higher value = higher priority) — submatch_weight（位标志，值越大优先级越高）
    let cmp = b.submatch_weight.cmp(&a.submatch_weight);
    if cmp != std::cmp::Ordering::Equal {
        return cmp;
    }

    // 2. Header count — header 数量
    let cmp = b.header_count.cmp(&a.header_count);
    if cmp != std::cmp::Ordering::Equal {
        return cmp;
    }

    // 3. regex_priority (only for routes with regex URI) — regex_priority（仅对有正则 URI 的路由）
    if a.submatch_weight & SUB_HAS_REGEX_URI != 0 {
        let cmp = b.regex_priority.cmp(&a.regex_priority);
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }

    // 4. max_uri_length (longer paths take priority) — max_uri_length（更长的路径优先）
    let cmp = b.max_uri_length.cmp(&a.max_uri_length);
    if cmp != std::cmp::Ordering::Equal {
        return cmp;
    }

    // 5. created_at (FIFO, earlier creation takes priority) — created_at（FIFO，更早创建的优先）
    a.created_at.cmp(&b.created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kong_core::models::ForeignKey;

    fn make_route(
        name: &str,
        hosts: Option<Vec<&str>>,
        paths: Option<Vec<&str>>,
        methods: Option<Vec<&str>>,
    ) -> Route {
        Route {
            id: Uuid::new_v4(),
            name: Some(name.to_string()),
            hosts: hosts.map(|h| h.into_iter().map(|s| s.to_string()).collect()),
            paths: paths.map(|p| p.into_iter().map(|s| s.to_string()).collect()),
            methods: methods.map(|m| m.into_iter().map(|s| s.to_string()).collect()),
            service: Some(ForeignKey::new(Uuid::new_v4())),
            created_at: 1609459200,
            updated_at: 1609459200,
            ..Route::default()
        }
    }

    #[test]
    fn test_exact_host_match() {
        let routes = vec![make_route(
            "test",
            Some(vec!["example.com"]),
            None,
            None,
        )];
        let router = TraditionalRouter::new(&routes);

        let ctx = RequestContext {
            method: "GET".to_string(),
            uri: "/".to_string(),
            host: "example.com".to_string(),
            scheme: "http".to_string(),
            ..Default::default()
        };

        let result = router.find_route(&ctx);
        assert!(result.is_some());
    }

    #[test]
    fn test_wildcard_host_match() {
        let routes = vec![make_route(
            "wildcard",
            Some(vec!["*.example.com"]),
            None,
            None,
        )];
        let router = TraditionalRouter::new(&routes);

        let ctx = RequestContext {
            method: "GET".to_string(),
            uri: "/".to_string(),
            host: "api.example.com".to_string(),
            scheme: "http".to_string(),
            ..Default::default()
        };

        assert!(router.find_route(&ctx).is_some());

        // Should not match non-subdomains — 不匹配非子域名
        let ctx2 = RequestContext {
            host: "other.com".to_string(),
            ..ctx.clone()
        };
        assert!(router.find_route(&ctx2).is_none());
    }

    #[test]
    fn test_path_prefix_match() {
        let routes = vec![make_route("api", None, Some(vec!["/api"]), None)];
        let router = TraditionalRouter::new(&routes);

        // Exact match — 精确匹配
        let ctx = RequestContext {
            uri: "/api".to_string(),
            host: "localhost".to_string(),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        // Prefix match — 前缀匹配
        let ctx2 = RequestContext {
            uri: "/api/users".to_string(),
            ..ctx.clone()
        };
        assert!(router.find_route(&ctx2).is_some());

        // No match — 不匹配
        let ctx3 = RequestContext {
            uri: "/other".to_string(),
            ..ctx.clone()
        };
        assert!(router.find_route(&ctx3).is_none());
    }

    #[test]
    fn test_method_match() {
        let routes = vec![make_route(
            "post-only",
            None,
            Some(vec!["/"]),
            Some(vec!["POST"]),
        )];
        let router = TraditionalRouter::new(&routes);

        let ctx_post = RequestContext {
            method: "POST".to_string(),
            uri: "/anything".to_string(),
            host: "localhost".to_string(),
            ..Default::default()
        };
        assert!(router.find_route(&ctx_post).is_some());

        let ctx_get = RequestContext {
            method: "GET".to_string(),
            ..ctx_post.clone()
        };
        assert!(router.find_route(&ctx_get).is_none());
    }

    #[test]
    fn test_more_specific_route_wins() {
        let r1 = make_route("general", None, Some(vec!["/api"]), None);
        let mut r2 = make_route(
            "specific",
            Some(vec!["api.example.com"]),
            Some(vec!["/api"]),
            Some(vec!["GET"]),
        );
        r2.created_at = r1.created_at + 1; // Created later — 更晚创建

        let routes = vec![r1, r2];
        let router = TraditionalRouter::new(&routes);

        let ctx = RequestContext {
            method: "GET".to_string(),
            uri: "/api/users".to_string(),
            host: "api.example.com".to_string(),
            scheme: "http".to_string(),
            ..Default::default()
        };

        let result = router.find_route(&ctx).unwrap();
        assert_eq!(result.route_name, Some("specific".to_string()));
    }

    #[test]
    fn test_longest_path_wins() {
        let r1 = make_route("short", None, Some(vec!["/api"]), None);
        let r2 = make_route("long", None, Some(vec!["/api/v2"]), None);

        let routes = vec![r1, r2];
        let router = TraditionalRouter::new(&routes);

        let ctx = RequestContext {
            uri: "/api/v2/users".to_string(),
            host: "localhost".to_string(),
            ..Default::default()
        };

        let result = router.find_route(&ctx).unwrap();
        assert_eq!(result.route_name, Some("long".to_string()));
    }

    #[test]
    fn test_regex_path_match() {
        let routes = vec![make_route(
            "regex",
            None,
            Some(vec!["~/api/v\\d+/users"]),
            None,
        )];
        let router = TraditionalRouter::new(&routes);

        let ctx = RequestContext {
            uri: "/api/v1/users".to_string(),
            host: "localhost".to_string(),
            ..Default::default()
        };
        assert!(router.find_route(&ctx).is_some());

        let ctx2 = RequestContext {
            uri: "/api/vx/users".to_string(),
            ..ctx
        };
        assert!(router.find_route(&ctx2).is_none());
    }
}
