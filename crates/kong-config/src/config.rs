use crate::listen::{parse_listen_addresses, ListenAddr};
use std::collections::HashMap;

/// Kong configuration, fully compatible with Kong's kong.conf — Kong 配置，与 Kong 的 kong.conf 完全兼容
/// All field names and defaults strictly follow Kong source (kong/templates/kong_defaults.lua) — 所有字段名和默认值严格参照 Kong 源码（kong/templates/kong_defaults.lua）
#[derive(Debug, Clone)]
pub struct KongConfig {
    // ========== General — 通用配置 ==========
    /// Working directory prefix — 工作目录前缀
    pub prefix: String,
    /// Log level: debug, info, notice, warn, error, crit, alert, emerg — 日志级别: debug, info, notice, warn, error, crit, alert, emerg
    pub log_level: String,
    /// Node ID (auto-generated UUID) — 节点 ID（自动生成 UUID）
    pub node_id: Option<String>,
    /// Whether to enable anonymous reports — 是否启用匿名报告
    pub anonymous_reports: bool,

    // ========== Listeners — 监听器 ==========
    /// Proxy listener list — 代理监听器列表
    pub proxy_listen: Vec<ListenAddr>,
    /// Admin API listener list — Admin API 监听器列表
    pub admin_listen: Vec<ListenAddr>,
    /// Status API listener list — Status API 监听器列表
    pub status_listen: Vec<ListenAddr>,
    /// Stream proxy listener list — Stream 代理监听器列表
    pub stream_listen: Vec<ListenAddr>,
    /// Admin GUI (Kong Manager) listener list — Admin GUI（Kong Manager）监听器列表
    pub admin_gui_listen: Vec<ListenAddr>,
    /// Admin GUI URL (Kong Manager access address) — Admin GUI URL（Kong Manager 访问地址）
    pub admin_gui_url: String,

    // ========== Log configuration — 日志配置 ==========
    pub proxy_access_log: String,
    pub proxy_error_log: String,
    pub admin_access_log: String,
    pub admin_error_log: String,
    pub status_access_log: String,
    pub status_error_log: String,
    pub proxy_stream_access_log: String,
    pub proxy_stream_error_log: String,

    // ========== Database configuration — 数据库配置 ==========
    /// Database type: postgres, off — 数据库类型: postgres, off
    pub database: String,
    pub pg_host: String,
    pub pg_port: u16,
    pub pg_database: String,
    pub pg_user: String,
    pub pg_password: Option<String>,
    pub pg_schema: Option<String>,
    pub pg_timeout: u64,
    pub pg_ssl: bool,
    pub pg_ssl_verify: bool,
    pub pg_max_concurrent_queries: u32,
    pub pg_semaphore_timeout: u64,
    pub pg_keepalive_timeout: Option<u64>,
    pub pg_pool_size: Option<u32>,
    pub pg_backlog: Option<u32>,

    // Read-only replica — 只读副本
    pub pg_ro_host: Option<String>,
    pub pg_ro_port: Option<u16>,
    pub pg_ro_database: Option<String>,
    pub pg_ro_user: Option<String>,
    pub pg_ro_password: Option<String>,
    pub pg_ro_schema: Option<String>,
    pub pg_ro_timeout: Option<u64>,
    pub pg_ro_ssl: Option<bool>,
    pub pg_ro_ssl_verify: Option<bool>,
    pub pg_ro_max_concurrent_queries: Option<u32>,
    pub pg_ro_semaphore_timeout: Option<u64>,
    pub pg_ro_keepalive_timeout: Option<u64>,
    pub pg_ro_pool_size: Option<u32>,
    pub pg_ro_backlog: Option<u32>,

    // ========== DB-less mode — 无数据库模式 ==========
    pub declarative_config: Option<String>,
    pub declarative_config_string: Option<String>,
    /// Cluster role: traditional, control_plane, data_plane — 集群角色: traditional, control_plane, data_plane
    pub role: String,

    // ========== Cache configuration — 缓存配置 ==========
    /// In-memory cache size (bytes) — 内存缓存大小（字节）
    pub mem_cache_size: String,
    /// Entity cache TTL (seconds), 0=never expire — 实体缓存 TTL（秒），0=永不过期
    pub db_cache_ttl: u64,
    /// Negative cache TTL (seconds) — 负缓存 TTL（秒）
    pub db_cache_neg_ttl: Option<u64>,
    /// Stale entity resurrection TTL (seconds) — 陈旧实体复活 TTL（秒）
    pub db_resurrect_ttl: u64,
    /// Entities to warm up in cache at startup — 启动时预热缓存的实体列表
    pub db_cache_warmup_entities: Vec<String>,
    /// Database polling frequency (seconds) — 数据库轮询频率（秒）
    pub db_update_frequency: u64,
    /// Database update propagation delay (seconds) — 数据库更新传播延迟（秒）
    pub db_update_propagation: u64,

    // ========== DNS configuration — DNS 配置 ==========
    pub dns_resolver: Vec<String>,
    pub dns_hostsfile: String,
    pub dns_order: Vec<String>,
    pub dns_valid_ttl: Option<u64>,
    pub dns_stale_ttl: u64,
    pub dns_cache_size: u32,
    pub dns_not_found_ttl: u64,
    pub dns_error_ttl: u64,
    pub dns_no_sync: bool,

    // ========== SSL/TLS configuration — SSL/TLS 配置 ==========
    pub ssl_cipher_suite: String,
    pub ssl_ciphers: Option<String>,
    pub ssl_protocols: String,
    pub ssl_prefer_server_ciphers: bool,
    pub ssl_dhparam: Option<String>,
    pub ssl_session_tickets: bool,
    pub ssl_session_timeout: String,
    pub ssl_session_cache_size: String,

    pub ssl_cert: Vec<String>,
    pub ssl_cert_key: Vec<String>,
    pub admin_ssl_cert: Vec<String>,
    pub admin_ssl_cert_key: Vec<String>,
    pub status_ssl_cert: Vec<String>,
    pub status_ssl_cert_key: Vec<String>,

    pub client_ssl: bool,
    pub client_ssl_cert: Option<String>,
    pub client_ssl_cert_key: Option<String>,

    pub lua_ssl_trusted_certificate: Vec<String>,
    pub lua_ssl_verify_depth: u32,
    pub lua_ssl_protocols: String,

    // ========== HTTP headers and upstream connections — HTTP 头和上游连接 ==========
    pub headers: Vec<String>,
    pub headers_upstream: Vec<String>,
    pub trusted_ips: Vec<String>,
    pub real_ip_header: String,
    pub real_ip_recursive: bool,
    pub upstream_keepalive_pool_size: u32,
    pub upstream_keepalive_max_requests: u32,
    pub upstream_keepalive_idle_timeout: u64,

    // ========== Routing configuration — 路由配置 ==========
    /// Router engine: traditional, traditional_compatible, expressions — 路由引擎: traditional, traditional_compatible, expressions
    pub router_flavor: String,
    /// State consistency mode: strict, eventual — 状态一致性模式: strict, eventual
    pub worker_consistency: String,
    /// Worker state update frequency (seconds) — worker 状态更新频率（秒）
    pub worker_state_update_frequency: u64,

    // ========== Plugins and code loading — 插件和代码加载 ==========
    /// Loaded plugin list (bundled means load all built-in plugins) — 加载的插件列表（bundled 表示加载所有内置插件）
    pub plugins: Vec<String>,
    /// Loaded Vault list — 加载的 Vault 列表
    pub vaults: Vec<String>,

    pub lua_socket_pool_size: u32,
    pub lua_package_path: String,
    pub lua_package_cpath: Option<String>,
    pub lua_max_req_headers: u32,
    pub lua_max_resp_headers: u32,
    pub lua_max_uri_args: u32,
    pub lua_max_post_args: u32,

    // ========== Nginx worker configuration — Nginx worker 配置 ==========
    pub nginx_user: String,
    pub nginx_worker_processes: String,
    pub nginx_daemon: bool,

    // ========== Error configuration — 错误配置 ==========
    pub error_default_type: String,

    // ========== Port mapping (container environments) — 端口映射（容器环境） ==========
    pub port_maps: Vec<String>,

    // ========== Hybrid mode configuration — 混合模式配置 ==========
    pub cluster_control_plane: String,
    pub cluster_listen: Vec<ListenAddr>,
    pub cluster_mtls: String,
    pub cluster_cert: Option<String>,
    pub cluster_cert_key: Option<String>,
    pub cluster_ca_cert: Option<String>,
    pub cluster_server_name: Option<String>,
    pub cluster_data_plane_purge_delay: u64,

    // ========== Observability — 可观测性 ==========
    pub tracing_instrumentations: Vec<String>,
    pub tracing_sampling_rate: f64,

    // ========== Proxy 扩展配置（Nginx 能力对齐） ==========
    /// 注入 X-Real-IP / X-Forwarded-* 请求头到上游
    /// 默认全部注入（与 Kong 一致），支持 on/off/指定头名列表
    pub proxy_real_ip_headers: Vec<String>,
    /// 隐藏上游 Server 响应头（默认隐藏）
    pub proxy_hide_server_header: bool,
    /// 自定义注入的响应头列表，每项格式为 "Header-Name: Header-Value"
    pub proxy_response_headers: Vec<String>,
    // ========== Nginx dynamic directive injection — Nginx 动态指令注入 ==========
    /// Stores dynamic configuration with nginx_* prefix — 存储 nginx_* 前缀的动态配置
    pub nginx_directives: HashMap<String, String>,

    // ========== Raw configuration data (for custom access) — 原始配置数据（用于自定义访问） ==========
    pub raw: HashMap<String, String>,
}

impl Default for KongConfig {
    fn default() -> Self {
        Self {
            // General — 通用
            prefix: "/usr/local/kong/".to_string(),
            log_level: "notice".to_string(),
            node_id: None,
            anonymous_reports: true,

            // Listeners — 监听器
            proxy_listen: parse_listen_addresses(
                "0.0.0.0:8000 reuseport backlog=16384, 0.0.0.0:8443 http2 ssl reuseport backlog=16384",
            ).unwrap_or_default(),
            admin_listen: parse_listen_addresses(
                "127.0.0.1:8001 reuseport backlog=16384, 127.0.0.1:8444 http2 ssl reuseport backlog=16384",
            ).unwrap_or_default(),
            status_listen: parse_listen_addresses("127.0.0.1:8007 reuseport backlog=16384")
                .unwrap_or_default(),
            stream_listen: parse_listen_addresses("off").unwrap_or_default(),
            admin_gui_listen: parse_listen_addresses(
                "0.0.0.0:8002, 0.0.0.0:8445 ssl",
            ).unwrap_or_default(),
            admin_gui_url: "http://localhost:8002".to_string(),

            // Logs — 日志
            proxy_access_log: "logs/access.log".to_string(),
            proxy_error_log: "logs/error.log".to_string(),
            admin_access_log: "logs/admin_access.log".to_string(),
            admin_error_log: "logs/error.log".to_string(),
            status_access_log: "off".to_string(),
            status_error_log: "logs/status_error.log".to_string(),
            proxy_stream_access_log: "logs/stream_access.log".to_string(),
            proxy_stream_error_log: "logs/stream_error.log".to_string(),

            // Database — 数据库
            database: "postgres".to_string(),
            pg_host: "127.0.0.1".to_string(),
            pg_port: 5432,
            pg_database: "kong".to_string(),
            pg_user: "kong".to_string(),
            pg_password: None,
            pg_schema: None,
            pg_timeout: 5000,
            pg_ssl: false,
            pg_ssl_verify: false,
            pg_max_concurrent_queries: 0,
            pg_semaphore_timeout: 60000,
            pg_keepalive_timeout: None,
            pg_pool_size: None,
            pg_backlog: None,

            // Read-only replica — 只读副本
            pg_ro_host: None,
            pg_ro_port: None,
            pg_ro_database: None,
            pg_ro_user: None,
            pg_ro_password: None,
            pg_ro_schema: None,
            pg_ro_timeout: None,
            pg_ro_ssl: None,
            pg_ro_ssl_verify: None,
            pg_ro_max_concurrent_queries: None,
            pg_ro_semaphore_timeout: None,
            pg_ro_keepalive_timeout: None,
            pg_ro_pool_size: None,
            pg_ro_backlog: None,

            // DB-less mode — 无数据库模式
            declarative_config: None,
            declarative_config_string: None,
            role: "traditional".to_string(),

            // Cache — 缓存
            mem_cache_size: "128m".to_string(),
            db_cache_ttl: 0,
            db_cache_neg_ttl: None,
            db_resurrect_ttl: 30,
            db_cache_warmup_entities: vec!["services".to_string()],
            db_update_frequency: 5,
            db_update_propagation: 0,

            // DNS
            dns_resolver: vec![],
            dns_hostsfile: "/etc/hosts".to_string(),
            dns_order: vec![
                "LAST".to_string(),
                "SRV".to_string(),
                "A".to_string(),
                "CNAME".to_string(),
            ],
            dns_valid_ttl: None,
            dns_stale_ttl: 3600,
            dns_cache_size: 10000,
            dns_not_found_ttl: 30,
            dns_error_ttl: 1,
            dns_no_sync: false,

            // SSL/TLS
            ssl_cipher_suite: "intermediate".to_string(),
            ssl_ciphers: None,
            ssl_protocols: "TLSv1.2 TLSv1.3".to_string(),
            ssl_prefer_server_ciphers: true,
            ssl_dhparam: None,
            ssl_session_tickets: true,
            ssl_session_timeout: "1d".to_string(),
            ssl_session_cache_size: "10m".to_string(),

            ssl_cert: vec![],
            ssl_cert_key: vec![],
            admin_ssl_cert: vec![],
            admin_ssl_cert_key: vec![],
            status_ssl_cert: vec![],
            status_ssl_cert_key: vec![],

            client_ssl: false,
            client_ssl_cert: None,
            client_ssl_cert_key: None,

            lua_ssl_trusted_certificate: vec!["system".to_string()],
            lua_ssl_verify_depth: 1,
            lua_ssl_protocols: "TLSv1.2 TLSv1.3".to_string(),

            // HTTP headers and upstream — HTTP 头和上游
            headers: vec![
                "server_tokens".to_string(),
                "latency_tokens".to_string(),
                "x-kong-request-id".to_string(),
            ],
            headers_upstream: vec!["x-kong-request-id".to_string()],
            trusted_ips: vec![],
            real_ip_header: "X-Real-IP".to_string(),
            real_ip_recursive: false,
            upstream_keepalive_pool_size: 512,
            upstream_keepalive_max_requests: 10000,
            upstream_keepalive_idle_timeout: 60,

            // Routing — 路由
            router_flavor: "traditional_compatible".to_string(),
            worker_consistency: "eventual".to_string(),
            worker_state_update_frequency: 5,

            // Plugins — 插件
            plugins: vec!["bundled".to_string()],
            vaults: vec!["bundled".to_string()],

            lua_socket_pool_size: 256,
            lua_package_path: "./?.lua;./?/init.lua;".to_string(),
            lua_package_cpath: None,
            lua_max_req_headers: 100,
            lua_max_resp_headers: 100,
            lua_max_uri_args: 100,
            lua_max_post_args: 100,

            // Nginx worker
            nginx_user: "kong kong".to_string(),
            nginx_worker_processes: "auto".to_string(),
            nginx_daemon: true,

            // Error — 错误
            error_default_type: "text/plain".to_string(),

            // Port mapping — 端口映射
            port_maps: vec![],

            // Hybrid mode — 混合模式
            cluster_control_plane: "127.0.0.1:8005".to_string(),
            cluster_listen: parse_listen_addresses("0.0.0.0:8005").unwrap_or_default(),
            cluster_mtls: "shared".to_string(),
            cluster_cert: None,
            cluster_cert_key: None,
            cluster_ca_cert: None,
            cluster_server_name: None,
            cluster_data_plane_purge_delay: 1209600,

            // Observability — 可观测性
            tracing_instrumentations: vec!["off".to_string()],
            tracing_sampling_rate: 0.01,

            // Proxy 扩展配置
            proxy_real_ip_headers: vec![
                "X-Real-IP".to_string(),
                "X-Forwarded-For".to_string(),
                "X-Forwarded-Proto".to_string(),
                "X-Forwarded-Host".to_string(),
                "X-Forwarded-Port".to_string(),
                "X-Forwarded-Path".to_string(),
                "X-Forwarded-Prefix".to_string(),
            ],
            proxy_hide_server_header: false,
            proxy_response_headers: vec![],

            // Nginx dynamic directives — Nginx 动态指令
            nginx_directives: HashMap::new(),

            // Raw data — 原始数据
            raw: HashMap::new(),
        }
    }
}

impl KongConfig {
    /// Apply configuration values from raw key-value map — 从原始 key-value 映射应用配置值
    pub fn apply_raw(&mut self, raw: &HashMap<String, String>) {
        for (key, value) in raw {
            self.set(key, value);
        }
        self.raw.extend(raw.clone());
    }

    /// Set a single configuration item — 设置单个配置项
    pub fn set(&mut self, key: &str, value: &str) {
        let value = value.trim();
        match key {
            // General — 通用
            "prefix" => self.prefix = value.to_string(),
            "log_level" => self.log_level = value.to_string(),
            "node_id" => self.node_id = none_if_empty(value),
            "anonymous_reports" => self.anonymous_reports = parse_bool(value),

            // Listeners — 监听器
            "proxy_listen" => self.proxy_listen = parse_listen_addresses(value).unwrap_or_default(),
            "admin_listen" => self.admin_listen = parse_listen_addresses(value).unwrap_or_default(),
            "status_listen" => {
                self.status_listen = parse_listen_addresses(value).unwrap_or_default()
            }
            "stream_listen" => {
                self.stream_listen = parse_listen_addresses(value).unwrap_or_default()
            }
            "admin_gui_listen" => {
                self.admin_gui_listen = parse_listen_addresses(value).unwrap_or_default()
            }
            "admin_gui_url" => self.admin_gui_url = value.to_string(),

            // Logs — 日志
            "proxy_access_log" => self.proxy_access_log = value.to_string(),
            "proxy_error_log" => self.proxy_error_log = value.to_string(),
            "admin_access_log" => self.admin_access_log = value.to_string(),
            "admin_error_log" => self.admin_error_log = value.to_string(),
            "status_access_log" => self.status_access_log = value.to_string(),
            "status_error_log" => self.status_error_log = value.to_string(),
            "proxy_stream_access_log" => self.proxy_stream_access_log = value.to_string(),
            "proxy_stream_error_log" => self.proxy_stream_error_log = value.to_string(),

            // Database — 数据库
            "database" => self.database = value.to_string(),
            "pg_host" => self.pg_host = value.to_string(),
            "pg_port" => self.pg_port = value.parse().unwrap_or(5432),
            "pg_database" => self.pg_database = value.to_string(),
            "pg_user" => self.pg_user = value.to_string(),
            "pg_password" => self.pg_password = none_if_empty(value),
            "pg_schema" => self.pg_schema = none_if_empty(value),
            "pg_timeout" => self.pg_timeout = value.parse().unwrap_or(5000),
            "pg_ssl" => self.pg_ssl = parse_bool(value),
            "pg_ssl_verify" => self.pg_ssl_verify = parse_bool(value),
            "pg_max_concurrent_queries" => {
                self.pg_max_concurrent_queries = value.parse().unwrap_or(0)
            }
            "pg_semaphore_timeout" => self.pg_semaphore_timeout = value.parse().unwrap_or(60000),
            "pg_keepalive_timeout" => self.pg_keepalive_timeout = value.parse().ok(),
            "pg_pool_size" => self.pg_pool_size = value.parse().ok(),
            "pg_backlog" => self.pg_backlog = value.parse().ok(),

            // Read-only replica — 只读副本
            "pg_ro_host" => self.pg_ro_host = none_if_empty(value),
            "pg_ro_port" => self.pg_ro_port = value.parse().ok(),
            "pg_ro_database" => self.pg_ro_database = none_if_empty(value),
            "pg_ro_user" => self.pg_ro_user = none_if_empty(value),
            "pg_ro_password" => self.pg_ro_password = none_if_empty(value),
            "pg_ro_schema" => self.pg_ro_schema = none_if_empty(value),
            "pg_ro_timeout" => self.pg_ro_timeout = value.parse().ok(),
            "pg_ro_ssl" => self.pg_ro_ssl = Some(parse_bool(value)),
            "pg_ro_ssl_verify" => self.pg_ro_ssl_verify = Some(parse_bool(value)),
            "pg_ro_max_concurrent_queries" => {
                self.pg_ro_max_concurrent_queries = value.parse().ok()
            }
            "pg_ro_semaphore_timeout" => self.pg_ro_semaphore_timeout = value.parse().ok(),
            "pg_ro_keepalive_timeout" => self.pg_ro_keepalive_timeout = value.parse().ok(),
            "pg_ro_pool_size" => self.pg_ro_pool_size = value.parse().ok(),
            "pg_ro_backlog" => self.pg_ro_backlog = value.parse().ok(),

            // DB-less mode — 无数据库模式
            "declarative_config" => self.declarative_config = none_if_empty(value),
            "declarative_config_string" => self.declarative_config_string = none_if_empty(value),
            "role" => self.role = value.to_string(),

            // Cache — 缓存
            "mem_cache_size" => self.mem_cache_size = value.to_string(),
            "db_cache_ttl" => self.db_cache_ttl = value.parse().unwrap_or(0),
            "db_cache_neg_ttl" => self.db_cache_neg_ttl = value.parse().ok(),
            "db_resurrect_ttl" => self.db_resurrect_ttl = value.parse().unwrap_or(30),
            "db_cache_warmup_entities" => self.db_cache_warmup_entities = parse_array(value),
            "db_update_frequency" => self.db_update_frequency = value.parse().unwrap_or(5),
            "db_update_propagation" => self.db_update_propagation = value.parse().unwrap_or(0),

            // DNS
            "dns_resolver" => self.dns_resolver = parse_array(value),
            "dns_hostsfile" => self.dns_hostsfile = value.to_string(),
            "dns_order" => self.dns_order = parse_array(value),
            "dns_valid_ttl" => self.dns_valid_ttl = value.parse().ok(),
            "dns_stale_ttl" => self.dns_stale_ttl = value.parse().unwrap_or(3600),
            "dns_cache_size" => self.dns_cache_size = value.parse().unwrap_or(10000),
            "dns_not_found_ttl" => self.dns_not_found_ttl = value.parse().unwrap_or(30),
            "dns_error_ttl" => self.dns_error_ttl = value.parse().unwrap_or(1),
            "dns_no_sync" => self.dns_no_sync = parse_bool(value),

            // SSL/TLS
            "ssl_cipher_suite" => self.ssl_cipher_suite = value.to_string(),
            "ssl_ciphers" => self.ssl_ciphers = none_if_empty(value),
            "ssl_protocols" => self.ssl_protocols = value.to_string(),
            "ssl_prefer_server_ciphers" => self.ssl_prefer_server_ciphers = parse_bool(value),
            "ssl_dhparam" => self.ssl_dhparam = none_if_empty(value),
            "ssl_session_tickets" => self.ssl_session_tickets = parse_bool(value),
            "ssl_session_timeout" => self.ssl_session_timeout = value.to_string(),
            "ssl_session_cache_size" => self.ssl_session_cache_size = value.to_string(),

            "ssl_cert" => self.ssl_cert = parse_array(value),
            "ssl_cert_key" => self.ssl_cert_key = parse_array(value),
            "admin_ssl_cert" => self.admin_ssl_cert = parse_array(value),
            "admin_ssl_cert_key" => self.admin_ssl_cert_key = parse_array(value),
            "status_ssl_cert" => self.status_ssl_cert = parse_array(value),
            "status_ssl_cert_key" => self.status_ssl_cert_key = parse_array(value),

            "client_ssl" => self.client_ssl = parse_bool(value),
            "client_ssl_cert" => self.client_ssl_cert = none_if_empty(value),
            "client_ssl_cert_key" => self.client_ssl_cert_key = none_if_empty(value),

            "lua_ssl_trusted_certificate" => self.lua_ssl_trusted_certificate = parse_array(value),
            "lua_ssl_verify_depth" => self.lua_ssl_verify_depth = value.parse().unwrap_or(1),
            "lua_ssl_protocols" => self.lua_ssl_protocols = value.to_string(),

            // HTTP headers and upstream — HTTP 头和上游
            // When headers=off, also clear headers_upstream — headers=off 时同时清除 headers_upstream
            "headers" => {
                self.headers = parse_array(value);
                if value.trim() == "off" {
                    self.headers_upstream = vec![];
                }
            }
            "headers_upstream" => self.headers_upstream = parse_array(value),
            "trusted_ips" => self.trusted_ips = parse_array(value),
            "real_ip_header" => self.real_ip_header = value.to_string(),
            "real_ip_recursive" => self.real_ip_recursive = parse_bool(value),
            "upstream_keepalive_pool_size" => {
                self.upstream_keepalive_pool_size = value.parse().unwrap_or(512)
            }
            "upstream_keepalive_max_requests" => {
                self.upstream_keepalive_max_requests = value.parse().unwrap_or(10000)
            }
            "upstream_keepalive_idle_timeout" => {
                self.upstream_keepalive_idle_timeout = value.parse().unwrap_or(60)
            }

            // Routing — 路由
            "router_flavor" => self.router_flavor = value.to_string(),
            "worker_consistency" => self.worker_consistency = value.to_string(),
            "worker_state_update_frequency" => {
                self.worker_state_update_frequency = value.parse().unwrap_or(5)
            }

            // Plugins — 插件
            "plugins" => self.plugins = parse_array(value),
            "vaults" => self.vaults = parse_array(value),

            "lua_socket_pool_size" => self.lua_socket_pool_size = value.parse().unwrap_or(256),
            "lua_package_path" => self.lua_package_path = value.to_string(),
            "lua_package_cpath" => self.lua_package_cpath = none_if_empty(value),
            "lua_max_req_headers" => self.lua_max_req_headers = value.parse().unwrap_or(100),
            "lua_max_resp_headers" => self.lua_max_resp_headers = value.parse().unwrap_or(100),
            "lua_max_uri_args" => self.lua_max_uri_args = value.parse().unwrap_or(100),
            "lua_max_post_args" => self.lua_max_post_args = value.parse().unwrap_or(100),

            // Nginx worker
            "nginx_user" => self.nginx_user = value.to_string(),
            "nginx_worker_processes" => self.nginx_worker_processes = value.to_string(),
            "nginx_daemon" => self.nginx_daemon = parse_bool(value),

            // Error — 错误
            "error_default_type" => self.error_default_type = value.to_string(),

            // Port mapping — 端口映射
            "port_maps" => self.port_maps = parse_array(value),

            // Hybrid mode — 混合模式
            "cluster_control_plane" => self.cluster_control_plane = value.to_string(),
            "cluster_listen" => {
                self.cluster_listen = parse_listen_addresses(value).unwrap_or_default()
            }
            "cluster_mtls" => self.cluster_mtls = value.to_string(),
            "cluster_cert" => self.cluster_cert = none_if_empty(value),
            "cluster_cert_key" => self.cluster_cert_key = none_if_empty(value),
            "cluster_ca_cert" => self.cluster_ca_cert = none_if_empty(value),
            "cluster_server_name" => self.cluster_server_name = none_if_empty(value),
            "cluster_data_plane_purge_delay" => {
                self.cluster_data_plane_purge_delay = value.parse().unwrap_or(1209600)
            }

            // Observability — 可观测性
            "tracing_instrumentations" => self.tracing_instrumentations = parse_array(value),
            "tracing_sampling_rate" => self.tracing_sampling_rate = value.parse().unwrap_or(0.01),

            // Proxy 扩展配置
            // Proxy 扩展配置
            "proxy_real_ip_headers" => {
                let v = value.trim().to_lowercase();
                if v == "on" || v == "true" || v == "yes" || v == "1" {
                    // 全部注入
                    self.proxy_real_ip_headers = vec![
                        "X-Real-IP".to_string(),
                        "X-Forwarded-For".to_string(),
                        "X-Forwarded-Proto".to_string(),
                        "X-Forwarded-Host".to_string(),
                        "X-Forwarded-Port".to_string(),
                        "X-Forwarded-Path".to_string(),
                        "X-Forwarded-Prefix".to_string(),
                    ];
                } else if v == "off" || v == "false" || v == "no" || v == "0" || v.is_empty() {
                    self.proxy_real_ip_headers = vec![];
                } else {
                    // 指定头名列表，逗号分隔
                    self.proxy_real_ip_headers = parse_array(value);
                }
            }
            "proxy_hide_server_header" => self.proxy_hide_server_header = parse_bool(value),
            "proxy_response_headers" => self.proxy_response_headers = parse_header_list(value),

            // nginx_* dynamic directives — nginx_* 动态指令
            _ if key.starts_with("nginx_") => {
                self.nginx_directives
                    .insert(key.to_string(), value.to_string());
            }

            // Store other unknown config in raw — 其他未知配置存入 raw
            _ => {
                tracing::debug!("未识别的配置项: {} = {}", key, value);
            }
        }
    }

    /// Get effective pg_ro value (falls back to primary connection value if not set) — 获取有效的 pg_ro 值（未设置时回退到主连接值）
    pub fn effective_pg_ro_host(&self) -> Option<&str> {
        self.pg_ro_host.as_deref()
    }

    pub fn effective_pg_ro_port(&self) -> u16 {
        self.pg_ro_port.unwrap_or(self.pg_port)
    }

    pub fn effective_pg_ro_database(&self) -> &str {
        self.pg_ro_database.as_deref().unwrap_or(&self.pg_database)
    }

    pub fn effective_pg_ro_user(&self) -> &str {
        self.pg_ro_user.as_deref().unwrap_or(&self.pg_user)
    }

    pub fn effective_pg_ro_password(&self) -> Option<&str> {
        if self.pg_ro_password.is_some() {
            self.pg_ro_password.as_deref()
        } else {
            self.pg_password.as_deref()
        }
    }

    pub fn effective_pg_ro_ssl(&self) -> bool {
        self.pg_ro_ssl.unwrap_or(self.pg_ssl)
    }

    pub fn effective_pg_ro_ssl_verify(&self) -> bool {
        self.pg_ro_ssl_verify.unwrap_or(self.pg_ssl_verify)
    }

    /// Check if in db-less mode — 判断是否为 db-less 模式
    pub fn is_dbless(&self) -> bool {
        self.database == "off"
    }

    /// Check if this is a control plane — 判断是否为控制面
    pub fn is_control_plane(&self) -> bool {
        self.role == "control_plane"
    }

    /// Check if this is a data plane — 判断是否为数据面
    pub fn is_data_plane(&self) -> bool {
        self.role == "data_plane"
    }

    /// Parse mem_cache_size to bytes — 解析 mem_cache_size 为字节数
    pub fn mem_cache_size_bytes(&self) -> u64 {
        parse_size_string(&self.mem_cache_size)
    }

    /// Get the list of plugins to load (expanding bundled) — 获取需要加载的插件列表（展开 bundled）
    pub fn loaded_plugins(&self) -> Vec<String> {
        let mut result = Vec::new();
        for p in &self.plugins {
            if p == "bundled" {
                result.extend(BUNDLED_PLUGINS.iter().map(|s| s.to_string()));
            } else {
                result.push(p.clone());
            }
        }
        result
    }
}

/// Kong built-in plugin list — Kong 内置插件列表
pub const BUNDLED_PLUGINS: &[&str] = &[
    "cors",
    "grpc-gateway",
    "grpc-web",
    "ip-restriction",
    "request-size-limiting",
    "acl",
    "basic-auth",
    "hmac-auth",
    "jwt",
    "key-auth",
    "ldap-auth",
    "oauth2",
    "session",
    "bot-detection",
    "request-termination",
    "correlation-id",
    "zipkin",
    "opentelemetry",
    "prometheus",
    "datadog",
    "statsd",
    "request-transformer",
    "response-transformer",
    "rate-limiting",
    "response-ratelimiting",
    "azure-functions",
    "aws-lambda",
    "pre-function",
    "post-function",
    "tcp-log",
    "udp-log",
    "http-log",
    "file-log",
    "syslog",
    "loggly",
    "acme",
    "ai-proxy",
    "ai-prompt-template",
    "ai-prompt-decorator",
    "ai-prompt-guard",
    "ai-request-transformer",
    "ai-response-transformer",
    "proxy-cache",
    "xml-threat-protection",
    "redirect",
    "standard-webhooks",
];

/// Parse boolean value (compatible with Kong's on/off/true/false) — 解析布尔值（兼容 Kong 的 on/off/true/false）
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "on" | "true" | "yes" | "1"
    )
}

/// Parse comma-separated array — 解析逗号分隔的数组
fn parse_array(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "off" {
        return vec![];
    }
    trimmed
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Convert empty string to None — 空字符串转 None
fn none_if_empty(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// 解析响应头列表，格式为 "Header: Value, Header2: Value2"
/// 按逗号分隔，但只在 "字母: " 模式前切割（避免切断值中的逗号）
fn parse_header_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "off" {
        return vec![];
    }
    // 按逗号分隔后，每项必须包含冒号才视为合法头
    trimmed
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| s.contains(':'))
        .collect()
}

/// Parse size string (e.g., "128m" -> bytes) — 解析大小字符串（如 "128m" -> 字节数）
pub fn parse_size_string(s: &str) -> u64 {
    let s = s.trim().to_lowercase();
    if let Some(num) = s.strip_suffix('g') {
        num.trim().parse::<u64>().unwrap_or(0) * 1024 * 1024 * 1024
    } else if let Some(num) = s.strip_suffix('m') {
        num.trim().parse::<u64>().unwrap_or(0) * 1024 * 1024
    } else if let Some(num) = s.strip_suffix('k') {
        num.trim().parse::<u64>().unwrap_or(0) * 1024
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = KongConfig::default();
        assert_eq!(config.database, "postgres");
        assert_eq!(config.pg_host, "127.0.0.1");
        assert_eq!(config.pg_port, 5432);
        assert_eq!(config.pg_database, "kong");
        assert_eq!(config.pg_user, "kong");
        assert_eq!(config.router_flavor, "traditional_compatible");
        assert_eq!(config.proxy_listen.len(), 2);
        assert!(!config.proxy_listen[0].ssl);
        assert!(config.proxy_listen[1].ssl);
    }

    #[test]
    fn test_parse_bool() {
        assert!(parse_bool("on"));
        assert!(parse_bool("true"));
        assert!(parse_bool("yes"));
        assert!(parse_bool("1"));
        assert!(!parse_bool("off"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("0"));
    }

    #[test]
    fn test_parse_array() {
        let arr = parse_array("bundled, custom-plugin, my-plugin");
        assert_eq!(arr, vec!["bundled", "custom-plugin", "my-plugin"]);
    }

    #[test]
    fn test_parse_size_string() {
        assert_eq!(parse_size_string("128m"), 128 * 1024 * 1024);
        assert_eq!(parse_size_string("1g"), 1024 * 1024 * 1024);
        assert_eq!(parse_size_string("8k"), 8 * 1024);
        assert_eq!(parse_size_string("1024"), 1024);
    }

    #[test]
    fn test_set_config() {
        let mut config = KongConfig::default();
        config.set("pg_host", "192.168.1.100");
        config.set("pg_port", "5433");
        config.set("pg_ssl", "on");
        config.set("database", "off");

        assert_eq!(config.pg_host, "192.168.1.100");
        assert_eq!(config.pg_port, 5433);
        assert!(config.pg_ssl);
        assert!(config.is_dbless());
    }

    #[test]
    fn test_loaded_plugins() {
        let mut config = KongConfig::default();
        config.plugins = vec!["bundled".to_string(), "my-custom-plugin".to_string()];
        let plugins = config.loaded_plugins();
        assert!(plugins.contains(&"cors".to_string()));
        assert!(plugins.contains(&"key-auth".to_string()));
        assert!(plugins.contains(&"my-custom-plugin".to_string()));
    }

    #[test]
    fn test_nginx_directives() {
        let mut config = KongConfig::default();
        config.set("nginx_http_gzip", "on");
        config.set("nginx_proxy_real_ip_header", "X-Forwarded-For");
        assert_eq!(
            config.nginx_directives.get("nginx_http_gzip"),
            Some(&"on".to_string())
        );
        assert_eq!(
            config.nginx_directives.get("nginx_proxy_real_ip_header"),
            Some(&"X-Forwarded-For".to_string())
        );
    }
}
