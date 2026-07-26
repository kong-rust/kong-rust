# 设计文档：Kong-Rust

## 概述

Kong-Rust 是 Kong API 网关的 Rust 重写版本，基于 Cloudflare Pingora 框架构建。设计目标是完全替换 Kong，保持所有数据模型、Admin API、配置格式和 Lua 插件接口的 100% 兼容性。

**核心设计原则：**
- **兼容优先**：所有外部行为（API、配置、插件接口）与 Kong 完全一致
- **Rust 原生**：核心代理引擎、路由匹配、数据库访问等用 Rust 实现，追求极致性能
- **Lua 桥接**：通过 mlua + LuaJIT 运行现有 Lua 插件，提供完整 PDK 兼容层
- **按数据语义分层**：配置、强一致账本、实时共享状态和海量事件分别选择事务库、
  分布式计数后端与外部分析存储，不把 PostgreSQL 或进程内存固化为唯一实现
- **水平扩展优先**：数据面契约不泄漏单节点时钟、内存容器或数据库私有语义；
  每项设计同时说明 local 基线和多副本企业部署的容量、背压与降级路径

## 代码复用分析

### 现有组件复用

- **Pingora**：复用其 HTTP 代理引擎、连接池、TLS 管理、负载均衡、健康检查框架
- **Kong 源码（/Users/dawxy/proj/kong）**：作为行为参考，确保所有模型定义、API 行为、路由匹配规则完全一致
- **Kong 内置 Lua 插件**：直接加载运行，不做修改

### 关键 Rust 依赖

| Crate | 用途 |
|-------|------|
| `pingora` | HTTP 代理引擎核心 |
| `pingora-proxy` | 反向代理 trait 和生命周期 |
| `pingora-load-balancing` | 负载均衡算法 |
| `pingora-cache` | 代理缓存（可选） |
| `mlua` | Lua/LuaJIT 绑定，运行 Lua 插件 |
| `tokio` | 异步运行时（Pingora 内置） |
| `sqlx` | PostgreSQL 异步数据库驱动 |
| `axum` | Admin API HTTP 框架 |
| `serde` / `serde_json` | 序列化/反序列化 |
| `uuid` | UUID 生成 |
| `regex` | 路由正则匹配 |
| `tracing` | 结构化日志和追踪 |
| `moka` | 高性能内存缓存 |

## 架构

### 整体架构

```mermaid
graph TB
    Client[客户端请求] --> ProxyListener[HTTP Proxy Listener<br/>0.0.0.0:8000/8443]
    StreamClient[L4 TCP/TLS 连接] --> StreamListener[Stream Proxy Listener<br/>stream_listen 端口]
    AdminClient[管理请求] --> AdminListener[Admin API Listener<br/>127.0.0.1:8001]
    StatusClient[状态/监控抓取] --> StatusListener[Status API Listener<br/>127.0.0.1:8007]

    subgraph KongRust[Kong-Rust 进程 — Traditional / CP+DP]
        ProxyListener --> Router[HTTP 路由引擎<br/>kong-router]
        Router --> PluginChain[插件链执行<br/>kong-plugin-system]
        PluginChain --> LuaBridge[Lua 桥接层<br/>kong-lua-bridge]
        PluginChain --> Balancer[负载均衡器<br/>kong-proxy]
        Balancer --> UpstreamConn[上游连接池<br/>Pingora ConnPool]

        StreamListener --> StreamRouter[Stream 路由引擎<br/>CIDR/SNI 匹配]
        StreamRouter --> StreamProxy[Stream 代理<br/>TCP/TLS Passthrough]
        StreamProxy --> UpstreamConn

        AdminListener --> AdminAPI[Admin API<br/>kong-admin / axum]
        StatusListener --> StatusAPI[Status API<br/>/status + /metrics]
        AdminAPI --> DAO[数据访问层<br/>kong-db]
        StatusAPI --> DAO
        DAO --> Cache[缓存层<br/>moka]
        DAO --> DB[(PostgreSQL)]

        LuaBridge --> LuaVM[LuaJIT VM<br/>mlua]
        LuaVM --> PDK[PDK 兼容层]
        PDK --> PluginChain
        StatusAPI --> MetricsCollector[Prometheus 指标收集器]
        MetricsCollector --> LuaBridge

        Router --> DAO
        PluginChain --> DAO
        Balancer --> HealthChecker[健康检查器]

        subgraph Cluster[集群通信层 kong-cluster]
            CPServer[CP WebSocket 服务端<br/>0.0.0.0:8005]
            DPClient[DP WebSocket 客户端]
            ConfigHash[多级配置哈希]
            SyncV2[Sync V2<br/>JSON-RPC 2.0]
        end

        AdminAPI -.->|配置变更通知| CPServer
        AdminAPI -.->|路由刷新| StreamRouter
        CPServer -.->|配置推送| DPClient
        DPClient -.->|应用配置| Router
        DPClient -.->|应用配置| PluginChain
        CPServer --> ConfigHash
    end

    UpstreamConn --> Upstream[上游服务]
```

> **Hybrid 模式说明：** Traditional 模式下所有组件在同一进程运行。Hybrid 模式下 CP 节点运行 Admin API + 集群服务端（不运行 Proxy），DP 节点运行 Proxy + 集群客户端（不运行 Admin API，不连接数据库）。

### Workspace 结构

```
kong-rust/
├── Cargo.toml                    # workspace 根配置
├── kong.conf.default             # 默认配置（兼容 Kong 格式）
├── crates/
│   ├── kong-core/                # 核心数据模型和 trait
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── models/           # Service, Route, Consumer 等模型
│   │   │   ├── traits/           # 插件 trait、DAO trait
│   │   │   └── error.rs          # 统一错误类型
│   │   └── Cargo.toml
│   ├── kong-config/              # 配置解析（兼容 kong.conf）
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── parser.rs         # kong.conf 解析器
│   │   │   └── env.rs            # KONG_* 环境变量处理
│   │   └── Cargo.toml
│   ├── kong-db/                  # 数据库 DAO 层
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── dao/              # 各实体 DAO
│   │   │   ├── schema.rs         # Schema 验证
│   │   │   ├── cache.rs          # 多级缓存
│   │   │   └── dbless.rs         # db-less 模式
│   │   └── Cargo.toml
│   ├── kong-router/              # 路由引擎
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── traditional.rs    # 传统路由匹配
│   │   │   ├── expressions.rs    # 表达式路由
│   │   │   └── priority.rs       # 优先级排序
│   │   └── Cargo.toml
│   ├── kong-proxy/               # 基于 Pingora 的代理引擎
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── server.rs         # Pingora Server 配置
│   │   │   ├── service.rs        # HttpProxy trait 实现
│   │   │   ├── balancer.rs       # 负载均衡
│   │   │   └── health_check.rs   # 健康检查
│   │   └── Cargo.toml
│   ├── kong-plugin-system/       # 插件框架
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── registry.rs       # 插件注册表
│   │   │   ├── iterator.rs       # 插件链迭代执行
│   │   │   ├── phases.rs         # 生命周期阶段定义
│   │   │   └── config.rs         # 插件配置验证
│   │   └── Cargo.toml
│   ├── kong-lua-bridge/          # Lua 兼容层
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── vm.rs             # LuaJIT VM 管理（per-worker 池）
│   │   │   ├── pdk/              # PDK 接口实现
│   │   │   │   ├── mod.rs
│   │   │   │   ├── request.rs    # kong.request
│   │   │   │   ├── response.rs   # kong.response
│   │   │   │   ├── service.rs    # kong.service
│   │   │   │   ├── client.rs     # kong.client
│   │   │   │   ├── log.rs        # kong.log
│   │   │   │   ├── ctx.rs        # kong.ctx
│   │   │   │   ├── cache.rs      # kong.cache
│   │   │   │   ├── router.rs     # kong.router
│   │   │   │   ├── node.rs       # kong.node
│   │   │   │   └── ip.rs         # kong.ip
│   │   │   ├── loader.rs         # Lua 插件加载器
│   │   │   ├── schema.rs         # Lua schema 解析
│   │   │   └── ngx_compat.rs     # ngx.* API 兼容层
│   │   └── Cargo.toml
│   ├── kong-admin/               # Admin API
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── app.rs            # axum 应用定义
│   │   │   ├── handlers/         # 各实体 handler
│   │   │   │   ├── mod.rs
│   │   │   │   ├── services.rs
│   │   │   │   ├── routes.rs
│   │   │   │   ├── consumers.rs
│   │   │   │   ├── upstreams.rs
│   │   │   │   ├── targets.rs
│   │   │   │   ├── plugins.rs
│   │   │   │   ├── certificates.rs
│   │   │   │   ├── info.rs       # /, /status, /endpoints
│   │   │   │   ├── schemas.rs    # /schemas/*
│   │   │   │   ├── tags.rs
│   │   │   │   ├── cache.rs
│   │   │   │   ├── debug.rs
│   │   │   │   └── clustering.rs # /clustering/status
│   │   │   ├── pagination.rs     # 分页逻辑
│   │   │   ├── error.rs          # 错误响应格式（兼容 Kong）
│   │   │   └── validation.rs     # 请求验证
│   │   └── Cargo.toml
│   ├── kong-cluster/             # 集群通信层（Hybrid CP/DP）
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── role.rs           # ClusterRole 枚举
│   │   │   ├── cp/               # Control Plane 实现
│   │   │   │   ├── mod.rs
│   │   │   │   ├── ws_server.rs  # WebSocket 服务端
│   │   │   │   ├── config_push.rs # 配置导出/推送
│   │   │   │   ├── client_manager.rs # DP 客户端管理
│   │   │   │   └── hash.rs       # 多级配置哈希
│   │   │   ├── dp/               # Data Plane 实现
│   │   │   │   ├── mod.rs
│   │   │   │   ├── ws_client.rs  # WebSocket 客户端
│   │   │   │   ├── config_apply.rs # 配置接收/应用
│   │   │   │   ├── heartbeat.rs  # PING/PONG 心跳
│   │   │   │   └── reconnect.rs  # 断线重连
│   │   │   ├── sync_v2/          # 增量同步
│   │   │   │   ├── mod.rs
│   │   │   │   ├── rpc.rs        # JSON-RPC 2.0 协议
│   │   │   │   ├── delta.rs      # Delta 计算/应用
│   │   │   │   └── version.rs    # 版本号管理
│   │   │   └── tls.rs            # TLS 双向认证
│   │   └── Cargo.toml
│   └── kong-server/              # 主入口二进制
│       ├── src/
│       │   └── main.rs           # 启动入口
│       └── Cargo.toml
└── lua/                          # Lua 插件目录（从 Kong 复制或指向）
    └── kong/
        └── plugins/              # 内置 Lua 插件
```

## 组件和接口

### 组件 1：kong-core — 核心数据模型

**职责：** 定义所有与 Kong 完全一致的核心数据结构和 trait 接口。

**核心模型（与 Kong Schema 完全一致）：**

```rust
// Service 模型 — 对应 Kong services 表
pub struct Service {
    pub id: Uuid,
    pub name: Option<String>,
    pub protocol: Protocol,          // http, https, tcp, tls, udp, grpc, grpcs
    pub host: String,
    pub port: u16,                   // 默认 80
    pub path: Option<String>,
    pub retries: i32,                // 默认 5
    pub connect_timeout: i32,        // 默认 60000ms
    pub write_timeout: i32,          // 默认 60000ms
    pub read_timeout: i32,           // 默认 60000ms
    pub client_certificate: Option<Uuid>,
    pub tls_verify: Option<bool>,
    pub tls_verify_depth: Option<i32>,
    pub ca_certificates: Option<Vec<Uuid>>,
    pub enabled: bool,               // 默认 true
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Route 模型 — 对应 Kong routes 表
pub struct Route {
    pub id: Uuid,
    pub name: Option<String>,
    pub protocols: Vec<Protocol>,     // 默认 [http, https]
    pub methods: Option<Vec<String>>,
    pub hosts: Option<Vec<String>>,
    pub paths: Option<Vec<String>>,
    pub headers: Option<HashMap<String, Vec<String>>>,
    pub snis: Option<Vec<String>>,
    pub sources: Option<Vec<CidrPort>>,
    pub destinations: Option<Vec<CidrPort>>,
    pub strip_path: bool,             // 默认 true
    pub preserve_host: bool,          // 默认 false
    pub request_buffering: bool,      // 默认 true
    pub response_buffering: bool,     // 默认 true
    pub https_redirect_status_code: u16, // 默认 426
    pub service: Option<ForeignKey>,
    pub regex_priority: i32,          // 默认 0
    pub path_handling: PathHandling,  // v0 或 v1
    pub expression: Option<String>,
    pub priority: Option<i32>,
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Consumer 模型
pub struct Consumer {
    pub id: Uuid,
    pub username: Option<String>,
    pub custom_id: Option<String>,
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Upstream 模型
pub struct Upstream {
    pub id: Uuid,
    pub name: String,
    pub algorithm: LbAlgorithm,       // round-robin, least-conn, consistent-hashing, latency
    pub hash_on: HashOn,
    pub hash_fallback: HashOn,
    pub hash_on_header: Option<String>,
    pub hash_on_cookie: Option<String>,
    pub hash_on_cookie_path: Option<String>,
    pub hash_on_query_arg: Option<String>,
    pub hash_on_uri_capture: Option<String>,
    pub hash_fallback_header: Option<String>,
    pub hash_fallback_query_arg: Option<String>,
    pub hash_fallback_uri_capture: Option<String>,
    pub slots: i32,                   // 默认 10000
    pub healthchecks: HealthcheckConfig,
    pub tags: Option<Vec<String>>,
    pub host_header: Option<String>,
    pub client_certificate: Option<Uuid>,
    pub use_srv_name: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

// Target 模型
pub struct Target {
    pub id: Uuid,
    pub upstream: ForeignKey,
    pub target: String,               // host:port
    pub weight: i32,                  // 默认 100
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Plugin 模型
pub struct Plugin {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub service: Option<ForeignKey>,
    pub route: Option<ForeignKey>,
    pub consumer: Option<ForeignKey>,
    pub config: serde_json::Value,    // 动态配置
    pub protocols: Vec<Protocol>,
    pub tags: Option<Vec<String>>,
    pub ordering: Option<PluginOrdering>,
    pub instance_name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// Certificate 模型
pub struct Certificate {
    pub id: Uuid,
    pub cert: String,                 // PEM 格式
    pub key: String,                  // PEM 格式
    pub cert_alt: Option<String>,
    pub key_alt: Option<String>,
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

// SNI 模型
pub struct Sni {
    pub id: Uuid,
    pub name: String,
    pub certificate: ForeignKey,
    pub tags: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}
```

**核心 Trait：**

```rust
// 插件生命周期 trait
pub trait PluginHandler: Send + Sync {
    fn priority(&self) -> i32;
    fn version(&self) -> &str;

    async fn init_worker(&self, config: &PluginConfig) -> Result<()> { Ok(()) }
    async fn certificate(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
    async fn rewrite(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
    async fn response(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
    async fn header_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
    async fn body_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx, body: &mut Bytes) -> Result<()> { Ok(()) }
    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> { Ok(()) }
}

// DAO trait — 通用数据访问接口
pub trait Dao<T: Entity>: Send + Sync {
    async fn insert(&self, entity: &T) -> Result<T>;
    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>>;
    async fn select_by_key(&self, key: &str) -> Result<Option<T>>;
    async fn page(&self, size: usize, offset: Option<String>) -> Result<Page<T>>;
    async fn update(&self, pk: &PrimaryKey, entity: &T) -> Result<T>;
    async fn upsert(&self, pk: &PrimaryKey, entity: &T) -> Result<T>;
    async fn delete(&self, pk: &PrimaryKey) -> Result<()>;
}
```

### 组件 2：kong-config — 配置解析

**职责：** 解析 kong.conf 配置文件和 KONG_* 环境变量，与 Kong 配置格式完全兼容。

**接口：**

```rust
pub struct KongConfig {
    // 监听配置
    pub proxy_listen: Vec<ListenAddr>,   // 默认 0.0.0.0:8000
    pub admin_listen: Vec<ListenAddr>,   // 默认 127.0.0.1:8001
    pub status_listen: Vec<ListenAddr>,  // 默认 127.0.0.1:8007，设为 off 表示禁用

    // 数据库
    pub database: DatabaseType,          // postgres 或 off（db-less）
    pub pg_host: String,
    pub pg_port: u16,
    pub pg_user: String,
    pub pg_password: Option<String>,
    pub pg_database: String,
    pub pg_ssl: bool,

    // 插件
    pub plugins: PluginsConfig,          // bundled 或指定列表

    // 路由
    pub router_flavor: RouterFlavor,     // traditional, expressions, traditional_compatible

    // 运行时
    pub nginx_worker_processes: WorkerCount,
    pub mem_cache_size: ByteSize,

    // 日志
    pub log_level: LogLevel,
    pub proxy_access_log: String,
    pub proxy_error_log: String,

    // 其他与 Kong 一致的配置项...
}

impl KongConfig {
    // 从 kong.conf 文件加载
    pub fn from_file(path: &Path) -> Result<Self>;
    // 应用 KONG_* 环境变量覆盖
    pub fn apply_env_overrides(&mut self) -> Result<()>;
    // 从默认值创建
    pub fn default() -> Self;
}
```

### 组件 3：kong-db — 数据库访问层

**职责：** 提供与 Kong 数据库 Schema 完全兼容的数据访问，支持 PostgreSQL 和 db-less 模式。

**关键设计：**

- 使用 `sqlx` 直接操作 Kong 的 PostgreSQL 表，不引入 ORM 以确保 Schema 完全一致
- 实现 `moka` 内存缓存，模拟 Kong 的 `kong.cache` 行为
- db-less 模式通过声明式 YAML/JSON 配置文件加载数据到内存

```rust
pub struct Database {
    pool: PgPool,
    cache: Cache<String, CachedValue>,
}

impl Database {
    pub fn services(&self) -> ServiceDao;
    pub fn routes(&self) -> RouteDao;
    pub fn consumers(&self) -> ConsumerDao;
    pub fn upstreams(&self) -> UpstreamDao;
    pub fn targets(&self) -> TargetDao;
    pub fn plugins(&self) -> PluginDao;
    pub fn certificates(&self) -> CertificateDao;
    pub fn snis(&self) -> SniDao;
    pub fn ca_certificates(&self) -> CaCertificateDao;
    pub fn key_sets(&self) -> KeySetDao;
    pub fn keys(&self) -> KeyDao;
    pub fn vaults(&self) -> VaultDao;
}
```

### 组件 4：kong-router — 路由引擎

**职责：** 实现与 Kong 完全一致的路由匹配逻辑，支持 traditional 和 expressions 两种风格。

**关键设计：**

- 传统路由：根据 hosts → paths → methods → headers → snis 的优先级进行匹配，与 Kong 的优先级排序规则完全一致
- 表达式路由：解析 Kong 的 ATC 表达式语法
- 路由表更新：监听数据库变更，增量更新路由表

```rust
pub struct Router {
    flavor: RouterFlavor,
    traditional: Option<TraditionalRouter>,
    expressions: Option<ExpressionsRouter>,
}

impl Router {
    // 匹配请求到路由和服务
    pub fn match_route(&self, req: &RequestContext) -> Option<RouteMatch>;
    // 从数据库加载/重建路由表
    pub fn rebuild(&mut self, routes: &[Route], services: &[Service]) -> Result<()>;
}

pub struct RouteMatch {
    pub route: Route,
    pub service: Service,
    pub matched_path: Option<String>,
    pub matched_host: Option<String>,
    pub uri_captures: Option<HashMap<String, String>>,
}
```

### 组件 5：kong-proxy — 代理引擎

**职责：** 基于 Pingora 实现 HTTP 反向代理，包括负载均衡和健康检查。

**关键设计：**

- 实现 Pingora 的 `ProxyHttp` trait，将 Pingora 的请求生命周期映射到 Kong 的插件阶段
- 负载均衡器支持 round-robin、least-conn、consistent-hashing、latency 算法
- 健康检查器支持主动（HTTP/TCP/gRPC 探测）和被动（请求错误计数）两种模式

**Pingora 生命周期 → Kong 插件阶段映射：**

| Pingora 阶段 | Kong 插件阶段 | 说明 |
|--------------|-------------|------|
| `early_request_filter` | `rewrite` | 请求重写 |
| `request_filter` | `access` | 访问控制、认证 |
| `upstream_peer` | 负载均衡选择 | 选择上游 Target |
| `upstream_request_filter` | 上游请求修改 | 修改发往上游的请求 |
| `response_filter` | `header_filter` | 响应头处理 |
| `response_body_filter` | `body_filter` | 响应体处理 |
| `logging` | `log` | 日志记录 |

```rust
pub struct KongProxy {
    router: Arc<RwLock<Router>>,
    db: Arc<Database>,
    plugin_system: Arc<PluginSystem>,
    balancers: Arc<RwLock<HashMap<Uuid, Balancer>>>,
}

impl ProxyHttp for KongProxy {
    type CTX = KongRequestCtx;

    fn new_ctx(&self) -> Self::CTX;
    async fn early_request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()>;
    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool>;
    async fn upstream_peer(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<Box<HttpPeer>>;
    async fn response_filter(&self, session: &mut Session, upstream_response: &mut ResponseHeader, ctx: &mut Self::CTX) -> Result<()>;
    async fn logging(&self, session: &mut Session, error: Option<&Error>, ctx: &mut Self::CTX);
}
```

### 组件 5b：kong-proxy/stream — L4 Stream 代理

**职责：** 基于 Pingora `ServerApp` trait 实现 L4 TCP/TLS 代理，与 HTTP 代理共享负载均衡和证书管理。

**关键设计：**

- 所有 `stream_listen` 端口统一注册为 `add_tcp()`，TLS 处理由应用层决定（因 TLS Passthrough 不能终止 TLS）
- 三种代理模式：TCP 明文转发、TLS Passthrough（peek SNI 后透传）、TLS Termination（终止 TLS 后转发，TODO）
- 与 HTTP 代理共享 `balancers`、`services`、`cert_manager`（Arc<RwLock<...>>）
- 路由热更新通过 AdminState 持有的 `Arc<RwLock<StreamRouter>>` 同步

**Stream 代理处理流程：**

```
TCP 连接到达
  → peek 首字节判断 TLS（0x16）
  → TLS 连接：peek ClientHello 解析 SNI
  → 构建 StreamRequestContext（source/dest IP:port + SNI）
  → StreamRouter.find_route() 匹配路由
  → 查找关联 Service，解析上游地址（复用 LoadBalancer）
  → 判断代理模式：
    - TLS Passthrough → 不终止 TLS，bidirectional_copy 透传
    - TLS Termination → TODO（暂作 TCP 透传）
    - TCP → bidirectional_copy 明文转发
  → 记录 access log
```

```rust
pub struct KongStreamProxy {
    pub stream_router: Arc<RwLock<StreamRouter>>,
    pub balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
    pub services: Arc<RwLock<HashMap<Uuid, Service>>>,
    pub cert_manager: Arc<CertificateManager>,
    pub connector: TransportConnector,
}

#[async_trait]
impl ServerApp for KongStreamProxy {
    async fn process_new(self: &Arc<Self>, session: Stream, _shutdown: &ShutdownWatch) -> Option<Stream>;
}
```

### 组件 6：kong-plugin-system — 插件框架

**职责：** 管理插件的注册、配置验证和生命周期执行。

**关键设计：**

- 插件优先级执行顺序与 Kong 完全一致（PRIORITY 值越大越先执行）
- 支持全局、Service、Route、Consumer 四个级别的插件配置
- 插件迭代器在每个阶段按优先级顺序执行匹配的插件

```rust
pub struct PluginSystem {
    registry: HashMap<String, Box<dyn PluginFactory>>,
    lua_bridge: Arc<LuaBridge>,
}

impl PluginSystem {
    // 注册插件工厂
    pub fn register(&mut self, name: &str, factory: Box<dyn PluginFactory>);
    // 加载 Lua 插件
    pub fn load_lua_plugin(&mut self, name: &str, path: &Path) -> Result<()>;
    // 获取请求匹配的插件链（按优先级排序）
    pub fn get_plugin_chain(&self, route: &Route, service: &Service, consumer: Option<&Consumer>) -> Vec<PluginInstance>;
    // 执行某阶段的所有插件
    pub async fn execute_phase(&self, phase: Phase, chain: &[PluginInstance], ctx: &mut RequestCtx) -> Result<()>;
}

// 插件实例 = 插件 handler + 该实例的配置
pub struct PluginInstance {
    pub handler: Arc<dyn PluginHandler>,
    pub config: PluginConfig,
    pub plugin_id: Uuid,
}
```

### 组件 7：kong-lua-bridge — Lua 兼容层

**职责：** 通过 mlua 嵌入 LuaJIT，加载并执行 Kong 的 Lua 插件，提供完整的 PDK 接口。

**关键设计：**

- **LuaJIT VM 池**：每个 worker 线程维护一个 LuaJIT VM 池，避免跨线程共享 Lua 状态
- **PDK 注入**：在 Lua 全局表中注入 `kong` 对象，所有方法通过 Rust 回调实现
- **ngx.* 兼容**：提供常用 ngx.* API 的兼容实现（ngx.say、ngx.exit、ngx.var 等）
- **共享字典语义对齐**：`ngx.shared` 使用进程级共享存储，保证业务请求阶段写入的指标可被独立 status 请求读取
- **Prometheus 收集器**：通过独立 Lua VM 执行官方 `kong.plugins.prometheus.exporter` 生命周期，在 status 端口输出文本指标

```rust
pub struct LuaBridge {
    vm_pools: Vec<LuaVmPool>,  // per-worker VM 池
}

impl LuaBridge {
    // 加载 Lua 插件的 handler.lua 和 schema.lua
    pub fn load_plugin(&self, name: &str, plugin_dir: &Path) -> Result<LuaPluginHandler>;
    // 在 Lua VM 中注入 PDK
    fn inject_pdk(&self, lua: &Lua, ctx: &RequestCtx) -> Result<()>;
    // 在 Lua VM 中注入 ngx.* 兼容层
    fn inject_ngx_compat(&self, lua: &Lua) -> Result<()>;
}

// Lua 插件 handler 实现了 PluginHandler trait
pub struct LuaPluginHandler {
    name: String,
    priority: i32,
    version: String,
    // Lua 代码引用
    handler_code: Vec<u8>,
    schema_code: Vec<u8>,
}

impl PluginHandler for LuaPluginHandler {
    // 各阶段方法通过 LuaBridge 调用 Lua 代码
    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 从 VM 池获取 Lua VM
        // 2. 注入当前请求的 PDK context
        // 3. 调用 handler:access(config)
        // 4. 归还 VM 到池
    }
}
```

**PDK 接口映射表：**

| Kong PDK | Rust 实现 | 数据来源 |
|----------|----------|---------|
| `kong.request.get_method()` | `ctx.request.method` | Pingora Session |
| `kong.request.get_headers()` | `ctx.request.headers` | Pingora Session |
| `kong.request.get_body()` | `ctx.request.body` | Pingora Session（缓冲） |
| `kong.request.get_query()` | `ctx.request.query_params` | URL 解析 |
| `kong.response.exit(status, body)` | 设置 ctx.response + 短路 | 中断请求链 |
| `kong.response.set_header(k, v)` | `ctx.response_headers.set()` | 响应头修改队列 |
| `kong.service.request.set_header(k, v)` | `ctx.upstream_headers.set()` | 上游请求修改 |
| `kong.client.get_ip()` | `session.client_addr()` | Pingora Session |
| `kong.log.info(msg)` | `tracing::info!(msg)` | tracing 日志 |
| `kong.ctx.shared` | `ctx.shared_data` | 请求级 HashMap |
| `kong.cache:get(key, ...)` | `database.cache.get()` | moka 缓存 |
| `kong.db.consumers:select(pk)` | `database.consumers().select()` | DAO 层 |
| `kong.router.get_route()` | `ctx.matched_route` | 路由匹配结果 |
| `kong.router.get_service()` | `ctx.matched_service` | 路由匹配结果 |

### 组件 8：kong-admin — Admin API

**职责：** 使用 axum 实现与 Kong 完全兼容的 Admin API，并提供与官方 Kong 对齐的独立 Status API。

**关键设计：**

- 使用泛型 CRUD handler 减少重复代码，类似 Kong 的 `endpoints.lua` 自动生成机制
- 错误响应格式与 Kong 完全一致（`{ "message": "...", "name": "...", "code": ... }`）
- 分页响应格式与 Kong 完全一致（`{ "data": [...], "next": "/path?offset=..." }`）
- `admin_listen` 与 `status_listen` 分离：`8001` 负责管理接口，`8007` 默认仅监听本机并暴露 `/status`、`/metrics`
- `GET /metrics` 不直接手写指标，而是读取已启用的 `prometheus` 插件实例配置，调用官方 exporter 生成 exposition 文本

```rust
// 泛型 CRUD 路由注册
fn register_entity_routes<T: Entity + CrudHandler>(router: Router, path: &str) -> Router {
    router
        .route(path, get(list::<T>).post(create::<T>))
        .route(&format!("{path}/{{id}}"), get(read::<T>).put(upsert::<T>).patch(update::<T>).delete(delete::<T>))
}

// Kong 兼容的错误响应
pub struct KongError {
    pub status: StatusCode,
    pub message: String,
    pub name: String,           // 如 "not found", "unique violation"
    pub code: Option<u32>,
}

// Kong 兼容的分页响应
pub struct PageResponse<T: Serialize> {
    pub data: Vec<T>,
    pub next: Option<String>,   // 下一页 URL
    pub offset: Option<String>, // 当前偏移量
}
```

### 组件 9：kong-cluster — 集群通信层

**职责：** 实现 Kong Hybrid 模式的 CP/DP 通信，包括全量推送（Sync V1）、增量同步（Sync V2）、TLS 双向认证、心跳管理和断线重连。

**整体架构（Hybrid 模式通信链路）：**

```mermaid
graph TB
    subgraph ControlPlane[Control Plane 节点]
        AdminAPI2[Admin API<br/>127.0.0.1:8001] --> DAO2[DAO 层]
        StatusAPI2[Status API<br/>127.0.0.1:8007] --> DAO2
        DAO2 --> DB2[(PostgreSQL)]
        DAO2 --> ConfigExporter[配置导出器]
        ConfigExporter --> HashCalculator[多级哈希计算器]
        ConfigExporter --> WSServer[WebSocket 服务端<br/>0.0.0.0:8005]
        WSServer --> ClientManager[DP 客户端管理器]
    end

    subgraph DataPlane[Data Plane 节点]
        Proxy2[Proxy 代理<br/>0.0.0.0:8000] --> Router2[路由引擎]
        WSClient[WebSocket 客户端] --> ConfigApplier[配置应用器]
        ConfigApplier --> Router2
        ConfigApplier --> PluginSystem2[插件系统]
        WSClient --> HeartbeatManager[心跳管理器]
        WSClient --> ReconnectManager[重连管理器]
    end

    WSServer <-->|mTLS + WebSocket| WSClient
    ClientManager -->|配置推送/Delta| WSClient
    WSClient -->|PING + 配置哈希| WSServer
```

**Workspace 结构添加：**

```
crates/
├── kong-cluster/                 # 集群通信层
│   ├── src/
│   │   ├── lib.rs
│   │   ├── role.rs              # 角色枚举（Traditional/ControlPlane/DataPlane）
│   │   ├── cp/                  # Control Plane 实现
│   │   │   ├── mod.rs
│   │   │   ├── ws_server.rs     # WebSocket 服务端（cluster_listen）
│   │   │   ├── config_push.rs   # 配置导出和推送（Sync V1）
│   │   │   ├── client_manager.rs # DP 客户端注册/状态追踪
│   │   │   └── hash.rs          # 多级配置哈希计算
│   │   ├── dp/                  # Data Plane 实现
│   │   │   ├── mod.rs
│   │   │   ├── ws_client.rs     # WebSocket 客户端
│   │   │   ├── config_apply.rs  # 配置接收和应用
│   │   │   ├── heartbeat.rs     # PING/PONG 心跳
│   │   │   └── reconnect.rs     # 断线重连策略
│   │   ├── sync_v2/             # 增量同步（Sync V2）
│   │   │   ├── mod.rs
│   │   │   ├── rpc.rs           # JSON-RPC 2.0 协议实现
│   │   │   ├── delta.rs         # Delta 计算和应用
│   │   │   └── version.rs       # 版本号管理
│   │   └── tls.rs               # TLS 双向认证配置
│   └── Cargo.toml
```

**关键设计：**

**1. 角色启动差异**

```rust
/// 节点角色
pub enum ClusterRole {
    /// 传统模式：Admin API + Proxy（默认）
    Traditional,
    /// 控制平面：Admin API + WebSocket 配置推送服务，不处理代理流量
    ControlPlane,
    /// 数据平面：Proxy only，从 CP 接收配置，不暴露 Admin API
    DataPlane,
}

// kong-server/src/main.rs 启动流程根据角色分支：
// Traditional → 启动 Admin API + Proxy + DB
// ControlPlane → 启动 Admin API + DB + WebSocket 服务端（cluster_listen）
// DataPlane → 启动 Proxy + WebSocket 客户端（连接 cluster_control_plane）
```

**2. Control Plane 设计**

```rust
/// CP WebSocket 服务端
pub struct ControlPlaneServer {
    /// 监听地址（默认 0.0.0.0:8005）
    listen_addr: SocketAddr,
    /// 已连接的 DP 客户端管理
    clients: Arc<RwLock<HashMap<String, DpClientInfo>>>,
    /// 数据库引用（导出配置用）
    db: Arc<Database>,
    /// TLS 配置（mTLS）
    tls_config: Arc<ServerTlsConfig>,
}

/// DP 客户端信息
pub struct DpClientInfo {
    pub node_id: String,
    pub hostname: String,
    pub kong_version: String,
    pub connected_at: i64,
    pub last_seen: i64,
    pub config_hash: String,
    pub sync_status: SyncStatus,  // normal, unknown, off
}

impl ControlPlaneServer {
    /// 启动 WebSocket 服务端，监听 /v1/outlet 和 /v2/outlet
    pub async fn start(&self) -> Result<()>;
    /// 处理新 DP 连接（mTLS 握手 → WebSocket 升级 → 推送当前配置）
    async fn handle_dp_connection(&self, ws: WebSocket, cert_info: CertInfo) -> Result<()>;
    /// 向所有已连接 DP 广播配置更新
    pub async fn broadcast_config(&self) -> Result<()>;
    /// 导出当前完整配置（GZIP 压缩）
    fn export_config(&self) -> Result<Vec<u8>>;
}
```

**3. Data Plane 设计**

```rust
/// DP WebSocket 客户端
pub struct DataPlaneClient {
    /// CP 地址（cluster_control_plane 配置）
    cp_addr: String,
    /// TLS 客户端配置（mTLS）
    tls_config: Arc<ClientTlsConfig>,
    /// 当前配置哈希
    current_hash: Arc<RwLock<String>>,
    /// 配置应用回调
    config_applier: Arc<dyn ConfigApplier>,
}

/// 配置应用接口
pub trait ConfigApplier: Send + Sync {
    /// 应用全量配置（Sync V1）
    async fn apply_full_config(&self, config: DeclarativeConfig) -> Result<()>;
    /// 应用增量 delta（Sync V2）
    async fn apply_delta(&self, deltas: Vec<DeltaEntry>) -> Result<()>;
}

impl DataPlaneClient {
    /// 启动 DP 客户端（连接 CP → 接收配置 → 心跳循环）
    pub async fn start(&self) -> Result<()>;

    /// 三线程模型（与 Kong 一致）：
    /// - config_thread：配置接收和应用
    /// - read_thread：从 WebSocket 读取帧（PONG、配置数据）
    /// - write_thread：向 WebSocket 写入帧（PING + 哈希）
    async fn run_connection(&self, ws: WebSocket) -> Result<()>;

    /// 心跳：每 30 秒发送 PING，负载为当前配置 MD5 哈希（32 字符）
    async fn heartbeat_loop(&self, ws_tx: &mut WsSender) -> Result<()>;

    /// 断线重连：5-10 秒随机延迟后重试，避免雷鸣羊群效应
    async fn reconnect(&self) -> Result<WebSocket>;
}
```

**4. Sync V1 协议（全量推送）**

```
连接流程：
  DP → CP: WebSocket 连接 wss://<cp_addr>/v1/outlet（mTLS 握手）
  CP → DP: 推送全量配置（JSON + GZIP 压缩的 Binary 帧）
  DP → CP: 每 30s 发送 PING 帧（负载 = 32 字符 MD5 配置哈希）
  CP → DP: 回复 PONG 帧
  CP → DP: 配置变更时推送新的全量配置

PING 帧哈希对比：
  CP 收到 PING 后解析 32 字符哈希
  若哈希与 CP 当前配置哈希不匹配 → 推送最新配置
  若匹配 → 不做操作（配置已同步）
```

**5. Sync V2 协议（增量同步，JSON-RPC 2.0）**

```rust
/// JSON-RPC 2.0 请求
pub struct JsonRpcRequest {
    pub jsonrpc: String,  // "2.0"
    pub method: String,
    pub params: serde_json::Value,
    pub id: u64,
}

/// JSON-RPC 2.0 响应
pub struct JsonRpcResponse {
    pub jsonrpc: String,  // "2.0"
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
    pub id: u64,
}
```

```
RPC 方法列表：
  kong.meta.v1.hello          — 双向握手，交换元信息
  kong.sync.v2.get_delta      — DP 请求增量 delta
  kong.sync.v2.notify_new_version   — CP 通知新版本
  kong.sync.v2.notify_validation_error — DP 报告验证错误

连接流程：
  DP → CP: WebSocket 连接 wss://<cp_addr>/v2/outlet（mTLS + Sec-WebSocket-Protocol: kong.meta.v1）
  DP → CP: kong.meta.v1.hello { rpc_capabilities, rpc_frame_encodings: ["x-snappy-framed"], kong_version, kong_node_id, kong_hostname }
  CP → DP: hello 响应 { rpc_capabilities, rpc_frame_encoding: "x-snappy-framed" }
  CP → DP: kong.sync.v2.notify_new_version { default: { new_version: "<version>" } }
  DP → CP: kong.sync.v2.get_delta { default: { version: "<current_version>" } }
  CP → DP: 返回 delta 数据列表

增量同步重试：
  单次同步最多重试 5 次（MAX_RETRY = 5）
  重试间隔 0.1 秒
```

**6. 多级配置哈希计算**

```rust
/// 配置哈希结果
pub struct ConfigHash {
    /// 总体配置哈希 = MD5(routes_hash + services_hash + plugins_hash + upstreams_hash + targets_hash + rest_hash)
    pub config: String,
    pub routes: String,
    pub services: String,
    pub plugins: String,
    pub upstreams: String,
    pub targets: String,
    /// rest 包含所有其他实体的哈希
    pub rest: String,
}

impl ConfigHash {
    /// 计算配置哈希
    /// 1. 对每类实体的数据使用 to_sorted_string() 序列化
    /// 2. 排序规则：对象按键名排序，数组元素用 ";" 分隔，null → "/null/"
    /// 3. 当缓冲超过 1MB 时，截断为 MD5（防止内存膨胀）
    /// 4. 对每类实体分别计算 MD5 子哈希
    /// 5. 拼接所有子哈希后计算总 MD5
    pub fn calculate(config: &DeclarativeConfig) -> Self;
}
```

**7. TLS 双向认证**

```rust
/// 集群 TLS 配置
pub struct ClusterTlsConfig {
    /// cluster_cert: 节点证书（PEM）
    pub cert_path: PathBuf,
    /// cluster_cert_key: 节点私钥（PEM）
    pub key_path: PathBuf,
    /// 可选：CA 证书（用于验证对端）
    pub ca_cert_path: Option<PathBuf>,
}
```

**8. 配置项**

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `role` | string | `traditional` | 节点角色：traditional / control_plane / data_plane |
| `cluster_listen` | listen_addr | `0.0.0.0:8005` | CP 的 WebSocket 监听地址 |
| `cluster_control_plane` | string | — | DP 连接的 CP 地址（如 `cp.example.com:8005`） |
| `cluster_cert` | path | — | mTLS 证书路径 |
| `cluster_cert_key` | path | — | mTLS 私钥路径 |
| `cluster_data_plane_purge_delay` | integer | `1209600` (14天) | 断连 DP 信息保留时长（秒） |
| `cluster_max_payload` | integer | `16777216` (16MB) | 单次推送最大负载 |

**9. 角色启动流程变更（kong-server/main.rs）**

```
match config.role {
    Traditional => {
        启动 Database 连接
        启动 Admin API（admin_listen）
        启动 Status API（status_listen，默认 127.0.0.1:8007）
        启动 Proxy（proxy_listen）
    }
    ControlPlane => {
        启动 Database 连接
        启动 Admin API（admin_listen）
        启动 Status API（status_listen）
        启动 CP WebSocket 服务端（cluster_listen）
        // 不启动 Proxy
    }
    DataPlane => {
        启动 DP WebSocket 客户端（连接 cluster_control_plane）
        等待首次配置同步完成
        启动 Proxy（proxy_listen）
        // 不启动 Admin API / Status API，不连接数据库
    }
}
```

### 组件 10：kong-ai — AI Gateway

**职责：** 提供 Rust 原生的 LLM 请求代理、provider 适配、协议转换、流式事件处理、模型组路由、token 归一化与调用统计。`kong-server` 将 AI 插件与 Lua 插件并列注册到 `kong-plugin-system`；AI 热路径不经过 Lua 桥接。

#### 10.1 数据实体与关系

AI 配置实体使用通用 `Dao<T>` / `PgDao<T>` 和 DB-less DAO。PostgreSQL 表由
migration `002_ai_gateway` 创建，`004_ai_model_max_input_tokens` 为模型补充输入
token 上限；migration `006_ai_usage_logs` 将 Model 价格迁移到
`NUMERIC(28,12)`，并新增独立的请求级事实表 `ai_usage_logs`。migration
`007_ai_virtual_key_budget_accounting` 将 Virtual Key 预算迁移到
`NUMERIC(28,12)`，并创建 request ledger、owner session、checkpoint 和运行时
settings；migration `008_ai_budget_overflow_idempotency` 收紧 numeric overflow
未决账务的幂等约束。analytics 事实表不对 Route、Service、Provider、Model、
Virtual Key 或 Consumer 建外键；预算 ledger 也不对已删除的 Virtual Key 或 usage
fact 建外键，配置删除和 analytics 归档均不会级联删除审计历史。

| 实体 | 当前字段 | 关系与用途 |
|------|----------|------------|
| `ai_providers` | `id`, `name`, `provider_type`, `endpoint_url`, `auth_config`, `default_model`, `config`, `enabled`, `tags`, `ws_id`, timestamps | 保存上游类型、端点和认证信息；`provider_type` 选择 driver |
| `ai_models` | `id`, `name`, `provider_id`, `model_name`, `priority`, `weight`, `input_cost`, `output_cost`, `max_tokens`, `max_input_tokens`, `config`, `enabled`, `tags`, `ws_id`, timestamps | `provider_id` 外键指向 provider；相同 `name` 构成一个 model group，`model_name` 是发给上游的真实模型名 |
| `ai_virtual_keys` | `id`, `name`, `key_hash`, `key_prefix`, `consumer_id`, `allowed_models`, `tpm_limit`, `rpm_limit`, `budget_limit`, `budget_used`, `budget_pending_count`, `budget_unresolved_count`, `budget_accounting_revision`, checkpoint tail/state，`enabled`, `expires_at`, `tags`, `ws_id`, timestamps | 可选关联 Consumer；保存密钥哈希和模型范围，并作为 PostgreSQL 预算账户 aggregate |

```
Plugin config.model_group / request.model
             │
             ▼
      model group name
             │
             ├── ai_models (same name, priority + weight + max_input_tokens)
             │          │
             │          └── provider_id
             │                    │
             ▼                    ▼
        selected model ───► ai_providers (driver + endpoint + auth)

ai_virtual_keys ── optional consumer_id ──► consumers
```

Provider 删除会级联删除其模型；Consumer 删除只会清空 virtual key 的 `consumer_id`。认证数据当前以 JSONB 保存，Admin API 返回 provider 时会遮蔽 `header_value`、`param_value`、`aws_secret_access_key` 和 `gcp_service_account_json`。

#### 10.2 Model Resolver 与 Driver 边界

`ai-proxy` 按以下顺序解析上游，命中后不再继续：

1. `model_routes`：第一条匹配请求模型名的正则规则生效，在规则的 targets 内交错加权轮转。
2. 显式数据库 model group：非空 `config.model_group` 会跳过所有内联 provider 字段，强制由服务端 AI Model / AI Provider 实体解析。
3. 插件内联 provider：支持旧版 kong-rust 的 `config.provider`，也支持 Kong 风格的 `config.model.provider` + `config.auth`。
4. 数据库回退：没有内联 provider 时，从旧字符串 `config.model` 或 `model_source=request` 的请求 `model` 读取 group name。`ModelGroupResolver` 按 group name 加载启用的 model/provider，缓存 2 秒；先选较高 `priority`，同一档按 `weight` 交错轮转，并用 provider 无关的预路由 prompt 估值与 `max_input_tokens` 过滤无法容纳请求的候选。单模型权重范围为 `0..=10000`，总权重不要求等于 100。Admin schema 将 `config.model` 保留为 Kong 官方 record，页面和新配置统一使用 `model_group`。

数据库 resolver 当前只读取 `ws_id IS NULL` 的全局 AI 实体。`AiDriver` 是 provider 与代理生命周期的稳定边界：

```rust
trait AiDriver {
    fn transform_request(...) -> Result<ProviderRequest>;
    fn transform_response(...) -> Result<ChatResponse>;
    fn transform_stream_event(...) -> Result<Option<SseEvent>>;
    fn configure_upstream(...) -> Result<UpstreamConfig>;
    fn extract_usage(...) -> Option<TokenUsage>;
    fn extract_stream_usage(...) -> Option<TokenUsage>;
}
```

内置 `DriverRegistry` 注册 `openai`、`anthropic`、`gemini` 和 `openai_compat`。driver 负责 provider 原生 JSON、SSE 事件、端点、路径和认证头；`ai-proxy` 只依赖统一的 `ChatRequest` / `ChatResponse` / `SseEvent` 和 `UpstreamConfig`。OpenAI-compatible driver 复用 OpenAI codec，但要求显式 `endpoint_url`。

#### 10.3 规范化协议与流式处理

OpenAI Chat Completions 是跨 provider 的内部规范格式：

- OpenAI 客户端请求直接解析为 `ChatRequest`；Anthropic Messages 先由 `AnthropicCodec` 转为 `ChatRequest`。
- OpenAI、Anthropic、Gemini 和 OpenAI-compatible driver 将规范请求转成各自上游格式，并将响应还原成规范 `ChatResponse`。
- OpenAI-compatible Provider 可以填写完整 Chat Completions URL，也可以只填写服务根地址；根地址会按 OpenAI 兼容约定补齐 `/v1/chat/completions`，显式路径保持不变。
- `route_type=llm/v1/responses` 且上游为 OpenAI 时，请求和响应走 `/v1/responses` pass-through；其他 provider 先降级到 Chat Completions，再把响应升级为 Responses API 形态。
- Responses 翻译路径拒绝 `background=true`，只保留 function tools；被剥离的内置 tool 类型通过非流式响应的 `metadata.warnings.unsupported_tools` 返回。

流式路径如下：

```
upstream chunk
    │
    ▼
SseParser.feed() ──跨 chunk 重组──► provider-native SseEvent
    │
    ├── driver.extract_stream_usage() ──► TokenUsage
    │
    ▼
driver.transform_stream_event()
    │
    ├── OpenAI client ────────────────► OpenAI SSE
    ├── Anthropic client ─────────────► Anthropic event sequence
    └── Responses translation ────────► Responses event state machine
```

`SseParser` 可解析标准 SSE 和 NDJSON，并在流结束时 flush 残留帧；当前 `ai-proxy` 的响应路径统一实例化标准 SSE parser。`header_filter` 移除可能失效的 `Content-Length` / `Content-Encoding`，流式响应统一声明 `text/event-stream`。AI 上游连接强制 HTTP/1.1，以避免 provider 侧 HTTP/2 多路复用、GOAWAY 和长流停滞问题。非流式响应在 `body_filter` 中完整缓冲后转换。

Token usage 优先采用 provider 响应值。选定 provider/model 后，`TokenizerRegistry` 可使用远端 count API、HuggingFace tokenizer 或 tiktoken，并受单请求 deadline 约束；不可用或超时时降级为字符估算，该结果用于 TPM 预扣和请求上下文。数据库 model group 必须在 provider 选定前完成候选过滤，因此 `max_input_tokens` 当前使用规范化 `ChatRequest` 的 provider 无关字符估值。

#### 10.4 插件顺序与请求生命周期

同名插件配置先按关联范围解析，再按 handler priority 降序执行。当前 AI 插件顺序为：

| Priority | 插件 | 当前阶段与行为 |
|----------|------|----------------|
| 774 | `ai-key-auth` | `access`：按 Bearer / `x-api-key` / 自定义头提取虚拟密钥，SHA-256 查表（进程内缓存 TTL 1s + 负缓存，Admin CUD/rotate 即时失效），校验 enabled/expiry/allowed_models，注入 `AiAuthContext` 与 `consumer_id`；失败以 OpenAI 或 Anthropic 风格错误体短路 401/403 |
| 773 | `ai-prompt-guard` | `access`：检查 user 消息长度、deny/allow 正则；可阻断或仅记录 |
| 772 | `ai-cache` | `access`：处理 skip header，按最后一条或全部 user 消息计算 SHA-256 cache key；`log` 当前不写缓存 |
| 771 | `ai-rate-limit` | `access`：通过异步 `RateLimitStore` 联合预扣 RPM/TPM；`virtual_key` 模式读取已认证 key 自身额度并先检查权威预算状态；`header_filter` 注入 quota snapshot |
| 770 | `ai-proxy` | `access`：解析、选模、转换并覆写 Pingora 上游；预算启用时冻结 pricing snapshot；`header_filter`：识别流式响应；`body_filter`：转换完整 JSON 或逐事件转换 SSE；`log`：注入 AI 日志字段 |

插件间通过 `RequestCtx.extensions` 传递类型化的 `AiAuthContext`、
`AiRequestState`、限流上下文和缓存上下文。`AiRequestState` 保存 driver、
model/provider、协议模式、SSE parser、usage、响应缓冲和计时状态。

代理同时维护与插件无关的 `RequestLifecycle`，以单一 request ID、UTC 起点和单调
时钟覆盖 Route/Service 快照、策略短路、上游尝试、上下游状态、transport error
与最终发送结果。`kong-plugin-system::RequestLifecycleObserver` 在有效插件链解析后
和请求 finalizing 时同步回调；traditional 模式下
`kong-ai::usage::AiUsageCollector` 作为 observer，仅为包含启用 `ai-proxy` 的链
建立草稿。该收口不依赖普通插件 `log` 的执行顺序，因此 access 早期拒绝、解析/
选模失败、上游错误、客户端断开和流中断也可生成一条事实。

collector 的 finalize 幂等且不执行 I/O：它归一化 usage、解析请求时价格、用
Decimal 计算成本，把最终 `AiUsageFact` 放回 extensions 供 `ai-proxy.log` 复用，
随后只执行一次有界 `try_send`。`ai-proxy.logging.log_statistics=false` 仍只关闭
Kong 兼容序列化日志，不关闭调用事实采集。

REQ-AI-003 在同步 observer 之外增加通用 async dispatch hook、dispatch abort
compensator 和 request finalizer。预算 hook 在真正允许上游联网前把 prepared
intent 幂等推进到 dispatching；critical hook 的 error、timeout 或 panic 会先设置
不可逆 `dispatch_forbidden`，再在响应前补偿 quota 和未联网 intent。客户端结果
确定后，quota finalizer 使用 Store 签发的 opaque reservation 修正 token，budget
finalizer 使用同一 `AiUsageFact` 结算账本。任一 finalizer 失败不会跳过另一个，
analytics writer 的队列或 PostgreSQL 写入结果也不参与预算事务。

Virtual Key quota 当前由有界 `MemoryRateLimitStore` 提供：同一 key 的 RPM/TPM
在一个 60 秒、首次命中起算的固定窗口内联合原子准入；request ID 和 settlement
operation ID 分别保证重放幂等，window generation 防止长请求修改新窗口。该
adapter 是本进程状态，多节点不共享且重启清零；Redis adapter 由 REQ-AI-009
交付。

`kong-plugin-system` 的解析器支持 Global / Service / Route / Consumer 关联，同名配置由更具体关联覆盖；但是当前代理链在 Consumer 身份可用前以 `consumer_id=None` 构建，因此运行时实际生效的是 Global / Service / Route 配置，Consumer 级 AI 插件配置尚未接入动态重解析。`ai-key-auth` 会在 access 阶段写入 `ctx.consumer_id`，因此同一请求内的下游插件（如 `ai-rate-limit` 的 `limit_by=consumer`）能读到 Consumer 身份，但插件链本身不会因此重新解析。

#### 10.5 Admin API

| 资源 | 当前端点 |
|------|----------|
| Provider | `GET/POST /ai-providers`; `GET/PATCH/PUT/DELETE /ai-providers/{id_or_name}`; `GET /ai-providers/{id}/ai-models` |
| Model | `GET/POST /ai-models`; `GET/PATCH/PUT/DELETE /ai-models/{id}`; `GET /ai-model-groups` |
| Virtual Key | `GET/POST /ai-virtual-keys`; `GET/PATCH/DELETE /ai-virtual-keys/{id_or_name}`; `POST /ai-virtual-keys/{id}/rotate`; `GET /ai-virtual-keys/{id}/budget-ledger`; `POST /ai-virtual-keys/{id}/budget-reconciliations`; `POST /ai-virtual-keys/{id}/budget-ledger/rebuild` |
| Usage | `GET /ai-usage`; `GET /ai-usage/summary` |

创建或轮换 virtual key 时生成 `sk-kr-...` 明文，只在该次响应返回；持久化内容是
SHA-256 `key_hash` 和可识别的 `key_prefix`。列表、查询和更新响应不返回
`key_hash`。AI 实体在 PostgreSQL 和 DB-less 启动路径中都注入 Admin state；
`ai-proxy` 共享相同的 model/provider DAO。

PostgreSQL 模式的 Virtual Key create/PATCH/delete 使用
`BudgetAccountGovernance`，普通客户端不能写 `budget_used`、账务 count/state/
revision 或 `key_hash/key_prefix`。金额的规范字段为固定 12 位字符串
`budget_limit_decimal/budget_used_decimal`；legacy number 只作可无损兼容投影。
有 pending/unresolved intent 时，清空预算或删除 key 返回 409。ledger 查询提供
状态/时间/cursor，人工 reconciliation 使用稳定 operation ID 执行 settle/waive，
rebuild 默认 dry-run，并通过 revision CAS 防止覆盖并发账务。

`/ai-usage` 与 `/ai-usage/summary` 使用严格的独立 query parser，不接受未知或
重复参数，也不允许调用方选择 workspace。两者共享 `[start,end)`、精确过滤器与
snapshot 水位；明细以 `(started_at,id)` 倒序复合游标分页，summary 每次只允许一种
时间或分类 breakdown。`AiUsageRuntime::UnsupportedHybrid` 在 query 解析前固定
返回 501，避免 Hybrid 被表示为一组伪造的零值。

Model 公共响应保持原有 number 类型的 `input_cost/output_cost`，同时新增 12 位
字符串 `input_cost_decimal/output_cost_decimal` 与服务端解析的
`effective_pricing`；Manager 不复制价表匹配算法。

#### 10.6 Kong Manager 管理页面

Kong Manager 主菜单将 **AI Gateway** 作为左侧第一个入口，Overview 紧随其后。
`/ai-gateway` 默认展示
AI Endpoint 聚合视图，二级导航提供 AI 接口、服务商连接、调用统计、高级模型和
虚拟密钥：

- AI 接口页面以单页表单编排 Provider、Model、Service、Route 和 route-scoped
  `ai-proxy` Plugin，提供完整地址、状态、模型流量摘要、配置、删除和测试操作。
- Provider 和 Model 页面使用结构化字段管理类型、地址、凭据、模型、权重、成本和
  token 上限；常规创建和编辑页面不显示可编辑 JSON。
- Virtual Key 页面显示一次性明文、要求先 dismiss 才能继续创建或轮换，并可按
  `virtual_key_id` 下钻调用统计；同时显示服务端派生的本节点 quota/Endpoint
  coverage、精确 Decimal 预算进度、预警/耗尽/未决/unsupported，以及 ledger、
  settle/waive 和 verify/rebuild 操作。预算扣减由独立强一致账本执行，不由
  analytics 事实执行。
- 调用统计包含“用量分析”和“调用日志”：前者展示可计算成本、已知 token 小计与
  覆盖率、原生 SVG 趋势和模型/Virtual Key 排行，后者展示可过滤、稳定分页的
  元数据事实与详情。
- Manager 顶部提供全局 English / 简体中文切换，侧栏、Overview、实体页面标题、
  提示、详情页签和 AI Gateway 共用同一语言状态。没有保存过选择时，
  `navigator.language` 以 `zh` 开头则默认中文，否则默认英文；用户选择以
  `kong-rust-manager-locale` 保存在 local storage，并同步更新文档 `lang`。
- 左上角品牌统一为 **Kong Rust Manager**，右上角 GitHub 入口指向
  `https://github.com/kong-rust/kong-rust`。独立 Manager 不展示 Kong Konnect
  推广卡片、文案或素材。

页面创建的代理路由只覆盖 `llm/v1/chat`。Responses、Anthropic 客户端协议、
全局/Service 级插件及高级策略仍通过 Admin API 配置。

##### 10.6.1 AI Endpoint 交互

默认交互以用户任务为中心，只回答“从哪里调用、转发到哪个模型、如何验证”，底层
资源仍保持独立。

页面引入 **AI Endpoint** 作为只读聚合视图和交互概念，不新增承担代理职责的运行时实体：

```
AI Endpoint
├── 入口身份与路径 ───────────────► Service + Route
├── 模型池 ──────────────────────► 同名 AI Model（model group）
├── 服务商连接与凭据 ────────────► AI Provider
└── 协议转换与转发策略 ──────────► route-scoped ai-proxy Plugin
```

AI Endpoint 页面通过现有 Admin API 识别和编排这些资源。底层资源继续作为事实来源，用户仍可在“高级资源”中单独管理；聚合视图不得复制 Provider 凭据、模型配置或 Route 配置。

###### 信息架构

AI Gateway 默认导航调整为：

1. **AI 接口**：默认入口，列出已发布 Endpoint，并提供创建、测试、复制地址和查看配置操作。
2. **服务商连接**：管理可复用的 Provider 连接和凭据，使用结构化表单。
3. **调用统计**：展示真实 Admin API 数据，并明确区分 PostgreSQL、DB-less 与
   Hybrid 能力状态。
4. **高级资源**：容纳 Models、Virtual Keys 及相关 Routes/Plugins 的专家视图。

Endpoint 列表卡片只突出用户决策所需的信息：名称、方法和完整调用路径、运行状态、模型池摘要以及测试/复制/配置操作。Provider ID、Plugin ID、`model_group` 等实现字段不出现在默认视图。

###### 单页创建流程

创建 Endpoint 使用一个页面内的渐进式表单，不在多个资源页面之间跳转：

1. **接口信息**
   - 用户填写接口名称和路径短名。
   - 页面实时预览最终 `POST /ai/{slug}/v1/chat/completions` 地址。
   - 客户端协议首版固定为 OpenAI Chat Completions；支持更多协议后再显示协议选择。
2. **选择模型**
   - 用户选择已有服务商连接，或在原位创建连接。
   - 内置 Provider 只要求 API Key 和模型名；服务端点使用 driver 默认值。
   - 只有 OpenAI-compatible Provider 展示必填的自定义服务地址；允许填写服务根地址，运行时会在未提供路径时补齐 `/v1/chat/completions`。
   - 可以添加多个模型，并清楚显示每个模型对应的 Provider 和真实模型名。
3. **流量策略**
   - 单模型为默认模式，无额外配置。
   - 多模型首版支持同一优先级内按相对权重分流；单项最大 10000，总权重不设固定值。
   - “故障时切换备用模型”必须等 `ai-proxy` 的跨 Provider 失败回报、重试和健康 fallback 完整接通后才能开放，不能仅通过设置 `priority` 在 UI 中制造已支持的假象。
4. **发布与验证**
   - 发布前以人类可读摘要展示入口、模型和流量比例。
   - 明确列出将创建或复用的底层资源，但不要求用户编辑它们。
   - 发布成功后直接显示完整 Endpoint、复制按钮和内置请求测试台。

页面保持单页，将接口信息、模型选择、流量策略和发布摘要连续呈现。单模型时流量
策略退化为只读的 100% 流量，多模型时才要求调整相对权重；用户无需在步骤间维护临时资源。

###### 页面零 JSON 原则

面向任务的 AI Gateway 页面不得要求用户输入、修改或理解 JSON。所有配置必须映射为带验证和帮助文本的结构化控件：

| 底层配置 | 页面交互 |
|----------|----------|
| `provider_type` | 带服务商名称的下拉选择 |
| `auth_config.header_value` | 密码型 API Key 输入框 |
| `auth_config.param_value` | Gemini API Key 输入框 |
| `endpoint_url` | 仅自定义兼容服务展示的 URL 输入框 |
| `model_name` | 可搜索选择；无法发现模型时允许文本输入 |
| `priority` | 在运行时支持后映射为主备模型拖动顺序 |
| `weight` | 相对权重数字控件，范围 `0..=10000`，总和不限 |
| `response_streaming` | “允许流式响应”开关 |
| `timeout` / `retries` | 带单位、范围和默认值的数字控件 |
| `max_input_tokens` | “最大输入 Token”数字控件 |

`auth_config`、Model `config` 和 Plugin `config` 只由前端适配层生成，不提供原始 JSON 编辑器。高级资源页也应优先使用 schema 驱动表单；诊断场景可以显示脱敏后的只读配置，但不能把 JSON 作为完成常规任务的必填入口。

Provider 凭据遵循以下规则：

- 复用已有 Provider 时不回填已经遮蔽的密钥。
- 新建连接时只提交当前 Provider 类型需要的认证字段。
- 编辑连接时，空白密钥表示保留原值；新密钥必须由用户显式输入。
- 页面不缓存、复制或写入 Plugin 的 Provider 密钥。
- 保存连接前提供“测试连接”；若后端尚无安全的连接测试端点，按钮不进入首版，不能通过创建临时代理资源模拟。

###### 前端模块边界

页面拆成职责单一的组件和 composable：

| 模块 | 唯一职责 |
|------|----------|
| `EndpointIdentityForm` | 名称、路径和最终调用地址预览 |
| `ModelPoolBuilder` | 模型成员的增加、删除、Provider 绑定及原位新建连接 |
| `TrafficPolicyEditor` | 单模型或相对权重分流配置与单项范围约束 |
| `Endpoints` | 列表、编辑器和发布摘要的页面组合，不直接实现持久化 |
| `useEndpointPublisher` | Endpoint 投影以及 Provider、Model、Service、Route、Plugin 的写入、更新、删除与失败回滚 |
| `endpointUtils` | tag、名称编码、路径、Provider 认证和表单映射 |
| `EndpointPlayground` | 调用已发布 Endpoint 并生成 curl 示例 |
| `useLocale` | 全局浏览器语言检测、选择持久化和文档 `lang` 同步 |
| `useI18n` | Manager 公共页面词典解析、参数插值与英文回退 |
| `useAiGatewayI18n` | 复用全局 locale，并提供 AI Gateway 文案映射 |

表单组件不直接调用 Admin API；API 请求和实体到视图模型的转换集中在 composable/service 层。`useEndpointPublisher` 只负责编排，不实现运行时路由，也不持久化第二份配置。

###### 发布事务与资源所有权

发布顺序为 Provider（需要时）→ AI Model → Service → Route → `ai-proxy` Plugin。已有 Provider 只复用不修改。任何写入失败时，发布器按逆序删除本次创建的资源；复用资源不得进入回滚集合。

为了可靠识别由向导创建的资源，向导为 Service、Route、Plugin 和 AI Model 添加一致的受控 tag，包括版本化的 Endpoint 标识。聚合视图根据这些 tag 和实体关系重建，不依赖名称猜测。缺失或被专家页面修改的成员显示为“配置不完整”，不得静默创建替代资源。

删除 Endpoint 必须先展示将删除和保留的资源：

- 删除该 Endpoint 独占的 Service、Route 和 Plugin。
- 只在 AI Model 带有同一 Endpoint 所有权 tag 且未被其他 Endpoint 引用时删除。
- Provider 默认保留，因为连接和凭据可以复用。
- 任何存在歧义的共享资源都保留并提示用户前往高级资源处理。

###### 内置测试台

发布成功后，测试台允许输入消息、切换流式响应并发送真实请求。浏览器将请求交给
`POST /ai-endpoint-test`；该 Admin helper 只接受
`/ai/{slug}/v1/chat/completions` 形态且带 `kr-ai-endpoint-v1` tag 的现有 Route，
再转发到本机配置的 Proxy listener，因此不会绕过 Route 或直接调用 Provider，也
不能被用作任意 URL 转发器。结果显示 HTTP 状态、实际模型、总耗时和响应正文；
页面同时生成直接调用公开 Proxy Endpoint 的 curl 示例。首版 helper 会缓冲流式
响应后再交给页面展示。

###### 首版验收边界

- 新用户无需预先进入 Providers、Models、Routes 或 Plugins 页面即可发布一个可调用的 OpenAI-compatible Chat Endpoint。
- 创建主流程和服务商连接页面不存在可编辑 JSON。
- 单 Provider、单模型的必填输入不超过名称、路径、服务商、API Key 和模型名。
- 多模型使用 `0..=10000` 的相对权重，总和无需等于 100，并正确映射为同名 model group 的交错加权轮转。
- 发布失败不会遗留本次新建的孤立资源，也不会删除复用资源。
- Endpoint 列表能够区分正常、禁用和配置不完整状态。
- 发布后可以在同一页面复制地址并完成一次真实代理请求。
- Providers、Models、Routes、Plugins 和 AI 运行时职责保持不变；Endpoint 层只提供投影、表单适配和发布编排。

#### 10.7 调用事实、计价与专用 Store

`kong-ai::usage` 是 analytics 领域边界，包含：

```text
collector       生命周期草稿、结果分类与事实定稿
normalizer      OpenAI / Anthropic / Gemini usage 标准化
pricing         版本化静态价表、Model 分方向覆盖与 Decimal 成本
cursor          snapshot / offset 编解码、filter hash 与严格校验
store           AiUsageStore、共享汇总口径与时区桶计划
postgres        批量持久化、稳定读取与 PostgreSQL 查询
memory          DB-less 有界 ring、淘汰 generation 与本节点查询
writer          bounded mpsc、定时/按量 flush、重试、关闭 drain 与指标
```

配置实体继续使用通用 `Dao<T>`；usage 不复用该抽象，因为事实查询需要时间窗、
倒序复合游标、批写、稳定 snapshot、高精度聚合和 breakdown。`AiUsageStore`
统一 `snapshot/list/summary/insert_batch` 契约，PG 与 Memory 返回同一 API DTO。

一次 AI 客户端请求只有一条事实，当前 `attempt_count` 为 0 或 1。事实保存
Route/Service、provider/model、Virtual Key/Consumer 的请求时快照，标准化
prompt/completion/total usage 及来源，reasoning/cache breakdown，分方向价格
快照、最终成本、状态/outcome、E2E、TTFT、stream 与 cache status。无 Route 的
404 和不含 `ai-proxy` 的普通代理请求不创建事实。

Provider 官方 usage 优先，未知字段保持 null；只有 prompt 与 completion 均已知时
才派生 total。OpenAI 保留官方 total 和 cached/reasoning 明细；Anthropic prompt
包含 input/cache-creation/cache-read，stream output 取最后累计值；Gemini
completion 包含 candidate + thinking 并另存 thinking/cache breakdown。请求级
`usage_source` 区分 `provider/estimated/mixed/unavailable`。

价格以 USD / 1M tokens 表示，input/output 分方向执行 Model 显式覆盖 > 内置价表 >
unmatched；`openai_compat` 不继承 OpenAI 价格。价表以
`crates/kong-ai/src/usage/data/model_prices.json` 编译进二进制，包含版本、官方
来源、快照日、有效期与条件。单价和成本全程使用 `Decimal`，PG 使用
`NUMERIC(28,12)`，新增 API 返回固定 12 位字符串。事实固化每个方向的来源、版本、
快照日与有效期，历史成本不随配置或价表更新漂移。

定价状态为 `matched/unmatched/unsupported/not_applicable`，成本状态为
`calculated/estimated/not_incurred/unavailable`。Provider cache、非标准 service
tier、内置工具、非文本模态、附加计价以及未覆盖的长上下文阶梯不会按基础价格伪装
成完整成本；usage 仍保存，pricing 标为 unsupported，cost 保持 unavailable 并带
机器可读原因。

summary 同时返回 token 已知小计、known/unknown 请求数与 coverage，可计算成本
小计、完整状态计数、outcome、平均/P95 延迟及可选 breakdown。金额和可能超过
`i64` 的 token 聚合使用 BigDecimal/BigInt 语义，避免浏览器或数据库浮点求和。
时间桶由 `chrono-tz` 生成真实 IANA 时区边界，覆盖 DST 的 23/25 小时日与重复小时。

运行模式明确分叉：

| 模式 | collector / writer | Store 与 API |
|---|---|---|
| traditional + PostgreSQL | 启用，后台批写 | 持久 `ai_usage_logs`，`ephemeral=false` |
| traditional + DB-less | 启用 | 本节点有界内存 ring，容量淘汰、重启清空 |
| control plane / data plane | 禁用 | 不上传、不聚合；Admin 可达时返回 501 |

PG writer 在代理热路径之外按 256 条或 500ms flush，有界队列默认 8192；批写事务
使用 advisory lock 使已提交 `MAX(ingest_seq)` 成为安全 snapshot 水位，并以
`UNIQUE(request_id)` 提供落库幂等。失败采用有界重试，队列满或 receiver 关闭时
立即丢弃事实而不背压代理。DB-less 默认保留 10000 条，snapshot 同时绑定写入水位、
淘汰 generation 和 Store instance；淘汰或重启后返回 409，不继续提供可能缺页的
结果。

五项 `kong.conf` 配置为
`ai_usage_queue_capacity=8192`、`ai_usage_batch_size=256`、
`ai_usage_flush_interval_ms=500`、`ai_usage_shutdown_timeout_ms=5000` 和
`ai_usage_dbless_capacity=10000`。所有值必须大于 0，且 batch 不得大于 queue；
校验失败会中止启动。

analytics 事实、API、cursor、Manager 和 writer 日志只包含元数据，不保存 prompt、
响应正文、请求/响应 header、Authorization、`x-api-key`、Provider auth、
Virtual Key 明文或 `key_hash`。首版 PG 表没有自动 retention、partition、archive、
export 或 delete API，会持续增长；查询窗口上限 90 天不等于数据保留期。

预算是独立于 `AiUsageStore` 的强一致领域。`BudgetStore` 覆盖 inspect、intent、
dispatch、settlement、owner lease、stale recovery 和 checkpoint；
`BudgetAdminStore` 覆盖 ledger、reconciliation 与 verify/rebuild；
`BudgetAccountGovernance` 让 limit/clear/delete 和普通 Virtual Key 字段在同一
key-first 事务中提交。PostgreSQL adapter 以 request/operation ID 幂等，aggregate、
open intent count、ledger 和 revision 同事务推进。opening balance 与 genesis
checkpoint 保留升级前的累计金额，checkpoint + bounded revision tail 避免普通
rebuild 永久退化为全历史扫描。

Traditional 启动路径分别装配有上限的 budget hot、heartbeat/owner 与 Admin
PostgreSQL pool。owner session 使用数据库时钟、lease 和 fencing；进程内有界
registry 在任务取消、commit 结果不确定或 finalizer 暂时失败时保留 safe-zero、
retry fact 或 unresolved 恢复状态。失联 owner 的 prepared intent 可以自动结算
`not_incurred=0`，dispatching intent 保守转 unresolved。三个 pool 当前仍可能共享
同一 PostgreSQL 实例和物理 I/O；这是资源优先级隔离，不是无限容量或完全物理
隔离。

预算语义是已持久化消费截止线：准入时若 `budget_used >= budget_limit` 则在
provider 前返回 403；已准入请求按最终标准成本结算，可能把使用率推到 100% 以上，
下一请求才开始阻断。它不是最大成本预留、预付余额或供应商账单级严格零超支。

#### 10.8 可观测性

- `ai-proxy.log` 合并 `ai.proxy.{provider,model,route_type,stream}`、`ai.usage.{prompt_tokens,completion_tokens,total_tokens}` 和 `ai.latency.e2e_ms` 到 `ctx.log_serialize`，供现有日志插件消费。
- Responses 路径返回 `X-Kong-AI-Route-Type: responses-pass-through|responses-translation`；成功转换的非流式 Chat 响应返回 `X-Kong-LLM-Model`。
- 流式状态记录 TTFT 并固化到 usage fact；现有兼容序列化日志仍不承诺 TPOT。
- `/status` 的 `ai_usage_writer` 暴露入队、写入、重复、丢失、写失败、重试、
  DB-less 淘汰和队列深度；启用 Prometheus status endpoint 时追加
  `kong_ai_usage_writer_*` counter/gauge。Provider 级 LLM 请求/延迟指标仍由
  REQ-AI-004 单独交付。

#### 10.9 当前限制与延期项

- `ai-cache` 只有 cache key/skip-header 基础设施；Redis 读写、命中短路、回写和语义缓存未实现。
- `ai-rate-limit(limit_by=virtual_key)` 已按认证 key ID 读取实体 RPM/TPM，并通过
  异步 Store 执行联合准入/修正；当前唯一 adapter 仍是本进程 Memory。多节点共享
  Redis 配额、distributed/degraded 状态与 backend 切换治理未实现，归
  REQ-AI-009。
- Virtual Key 的请求认证已由 `ai-key-auth` 插件（priority 774）接入：校验
  enabled、expiry 和 allowed models（支持末尾 `*` 前缀通配），并注入
  `AiAuthContext`、`consumer_id` 与 `authenticated_credential`。调用统计可关联
  Virtual Key；Traditional PostgreSQL 已通过独立 Decimal ledger 累加
  `budget_used` 并执行 403/503 fail-closed。best-effort usage fact 仍不能承担
  硬预算账本，DB-less/Hybrid 持久预算明确不支持。
- Hybrid 模式 CP→DP 配置同步不包含任何 AI 实体，因此 DP 节点上没有 Virtual Key 数据，`ai-key-auth` 在 DP 上无法认证。
- Prompt Guard 当前只有正则/长度规则；语义 guard、embedding 检测和分类评分未实现。
- Model balancer 已实现 priority、weight、token-size 过滤和健康冷却结构，但 `ai-proxy` 尚未回报成功/失败，也未使用插件配置中的 `retries`，因此运行时没有跨 provider 失败重试或健康 fallback。
- 数据库 model group 的 `max_input_tokens` 预路由过滤使用 provider 无关字符估值；选定 provider/model 后才执行 `TokenizerRegistry` 的精细计数。临界阈值附近可能保守或乐观选模。
- `ai-proxy` 接受 `timeout`、`model_name_header`、`log_payloads` 和 `log_statistics` 配置字段，但这些开关尚未完整驱动运行时行为；连接和读写超时仍主要服从 Service/Pingora 配置。
- 调用统计首版仅查询 default workspace；DB-less 不跨节点聚合，Hybrid 不做
  DP→CP 上传；provider 账单对账、自动保留/分区/归档/导出/删除、多币种和附加
  计价均为后续工作。
- Workspace-scoped AI model/provider 解析、Consumer 级插件动态选择、Provider 级
  AI Prometheus、MCP Gateway 和 Agent Gateway 均为后续工作。

#### 10.10 企业级规模化演进约束

下表同时标注当前可运行基线和企业目标。REQ-AI-003 已完成核心编码但仍处于验证
收口，Redis 与外部 analytics 后端尚未实现，不能在 Admin/Manager 或文档中误报
为当前能力：

| 数据语义 | 当前状态 / 已规划首版 | 企业规模目标 | 稳定领域边界 |
|---|---|---|---|
| 配置实体 | 已实现：PostgreSQL / DB-less 声明式配置 | PostgreSQL 或兼容事务存储 | `Dao<T>` 与配置快照 |
| 预算账本 | 已实现：PostgreSQL primary、Decimal aggregate/ledger、owner lease、recovery/checkpoint、Admin reconciliation；REQ-AI-003 验证中 | 可分片的强一致事务账本，必要时配合独立热状态 | `BudgetStore` / `BudgetAdminStore` / `BudgetAccountGovernance` |
| RPM/TPM 实时配额 | 已实现：Virtual Key + 有界 Memory adapter，本进程 60 秒窗口 | REQ-AI-009：Redis 等跨节点原子计数后端 | 异步 `RateLimitStore` |
| 调用事实与成本分析 | 已实现：PostgreSQL `ai_usage_logs` / DB-less ring | 外部日志或分析存储，可经消息管道解耦 | `AiUsageSink` + `AiUsageQueryBackend` |
| 指标与追踪 | 已实现：Prometheus 文本与结构化日志 | Prometheus remote write / OTLP 等 | 低基数 metrics 与 trace exporter |

规模化存储遵循以下约束：

1. usage collector、标准化、计价和 Admin DTO 不依赖具体存储。现有
   `AiUsageStore` 在外部后端落地前拆分为写入 sink 与查询 backend；PG/Memory
   可同时实现两者，Kafka 类管道可以只实现 sink，Elasticsearch/OpenSearch 或
   ClickHouse 类后端按 capability 提供查询。部署模式、持久/易失属性与物理
   backend ID 必须分离，sink 与 query backend 独立配置；首个企业拓扑至少包含
   一个外部可查询存储，不能以 Kafka-only 加长期 PG 查询完成外部化。
2. usage event 使用带 `schema_version`、确定性 event ID/request ID 和 Decimal
   字符串的稳定 envelope。每个 sink 拥有独立有界队列、批量条数/字节、并发、
   超时、重试和可选 spool/DLQ；慢或失败的 sink 不能阻塞代理或其他 sink。
   snapshot/cursor 由 backend 持有不透明 token，公共契约不得假设 PG sequence
   或内存 ring generation。外部 backend 还必须声明 retention/index rollover、
   可见性 SLA、查询能力、lag 和迁移/双写对账语义。
3. REQ-AI-003 冻结异步 `RateLimitStore` 的联合准入、结算和查询契约。一次
   `admit` 原子检查并预扣 RPM/TPM，Store 返回 opaque window/reservation token；
   `request_id` 与 settlement operation ID 分别保证准入和修正幂等。时间、
   generation、TTL 和原子快照由 backend 依照上层 `WindowSpec` 实现，插件不能
   自行拼 key 或依赖 `Instant`；同一 Virtual Key 跨 Endpoint 共享 bucket。
   REQ-AI-009 在不改变上层语义的前提下提供 Redis 实现。
4. 预算与 analytics 完全解耦：外部 usage sink 成功与否不影响预算。首版 PG
   预算实现通过 `BudgetStore`/Admin/governance traits 隔离 SQL，锁粒度限定为
   单 key，禁止全局串行热点；未来更换分片事务账本或独立 admission 热状态时
   仍必须保持
   request ID 幂等、账本审计和 fail-closed 契约。Redis/ES 不能直接替代权威
   财务账本。
5. 每个后续数据面 `design.md` 必须给出容量模型：QPS/并发、节点数、key/租户
   基数、事件大小与增长、保留期、远程调用次数、连接池和超时、热点/倾斜、
   背压/降级、低基数指标，以及单节点、多节点、高基数、后端超时/故障的负载
   验证结果或明确基线。

外部 usage backend 与生命周期治理由 REQ-AI-013 跟踪；Redis 分布式实时配额由
REQ-AI-009 跟踪。当前已实现的 usage PG/Memory、本地 quota 和 PostgreSQL
预算账本是默认部署档位；在 REQ-AI-003 的真实 HTTP、运行模式和容量验证完成前，
不得把它们表述为已经验收的企业规模 SLA。

## 错误处理

### 错误场景

1. **上游连接失败**
   - **处理：** 按 Service.retries 配置重试其他 Target，所有重试失败后返回 502
   - **用户影响：** 收到 502 Bad Gateway 响应

2. **路由无匹配**
   - **处理：** 返回 404，响应体与 Kong 一致：`{ "message": "no Route matched" }`
   - **用户影响：** 收到 404 Not Found

3. **插件执行错误**
   - **处理：** 记录错误日志，根据插件类型决定是中断请求（认证类）还是继续（日志类）
   - **用户影响：** 认证失败返回 401/403，其他错误返回 500

4. **数据库连接失败**
   - **处理：** 使用缓存数据继续服务，后台持续尝试重连
   - **用户影响：** 代理请求正常处理（使用缓存），Admin API 写操作返回 503

5. **Lua 插件运行时错误**
   - **处理：** 捕获 Lua 异常，记录错误日志，返回 500
   - **用户影响：** 收到 500 Internal Server Error

6. **Admin API 请求验证失败**
   - **处理：** 返回 400，错误格式与 Kong 一致：`{ "message": "schema violation (...)", "name": "schema violation", "code": 2 }`
   - **用户影响：** 收到 400 Bad Request，包含详细的字段验证错误信息

## 测试策略

### 单元测试

- 各 crate 内部单元测试，覆盖核心逻辑
- 路由匹配：用 Kong 的测试用例验证匹配行为一致性
- 模型序列化/反序列化：验证 JSON 格式与 Kong 完全一致
- 配置解析：验证各种 kong.conf 格式的正确解析

### 集成测试

- Admin API 集成测试：验证所有 CRUD 端点的请求/响应与 Kong 一致
- 数据库集成测试：验证 DAO 层对 PostgreSQL 的读写与 Kong 数据格式一致
- 插件执行集成测试：验证 Lua 插件通过桥接层的正确执行

### 端到端测试

- **Kong 兼容性测试**：使用 Kong 的官方测试用例子集（spec/ 目录），验证 Kong-Rust 的行为与 Kong 完全一致
- **迁移测试**：从 Kong 导出配置（decK dump），导入 Kong-Rust，验证代理行为一致
- **性能基准测试**：使用 wrk/vegeta 对比 Kong 和 Kong-Rust 的吞吐量和延迟
