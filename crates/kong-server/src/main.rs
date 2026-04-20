//! Kong-Rust API Gateway — main entry point — Kong-Rust API Gateway — 主入口

pub mod mock_upstream;

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kong_core::traits::PluginHandler;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kong", version = "0.1.0")]
struct Cli {
    /// Configuration file path — 配置文件路径
    #[arg(short, long, default_value = "/etc/kong/kong.conf")]
    conf: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Start,
    /// Container mode start (same as Start, env vars set by entrypoint) — 容器模式启动（行为同 Start，环境变量由 entrypoint 设置）
    DockerStart,
    /// Health check: probe Admin API /status endpoint — 健康检查：探测 Admin API /status 端点
    Health,
    Check,
    /// Database management (alias: migrations) — 数据库管理（别名：migrations）
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Database migrations (alias for db) — 数据库 migration（db 的别名）
    Migrations {
        #[command(subcommand)]
        action: DbAction,
    },
    Config,
    Version,
    /// Start mock upstream server for spec tests — 启动 spec 测试用 mock upstream 服务器
    MockUpstream {
        /// HTTP port (default: 15555) — HTTP 端口（默认：15555）
        #[arg(short, long)]
        port: Option<u16>,
        /// HTTPS port (default: 15556) — HTTPS 端口（默认：15556）
        #[arg(short, long)]
        ssl_port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Initialize database (create all tables) — 初始化数据库（创建所有表）
    Bootstrap,
    /// Execute new migrations — 执行新的 migration
    Up,
    /// Complete teardown of pending migrations — 完成 pending migration 的 teardown
    Finish,
    /// List all migration statuses — 列出所有 migration 状态
    List,
    /// Reset database (drop all tables) — 重置数据库（删除所有表）
    Reset {
        /// Skip confirmation prompt — 跳过确认提示
        #[arg(short, long)]
        yes: bool,
    },
    /// Output migration status as JSON — 输出 migration 状态 JSON
    Status,
}

/// Map Kong log level to tracing EnvFilter string — 将 Kong 日志级别映射到 tracing EnvFilter 字符串
fn kong_log_level_to_filter(level: &str) -> &'static str {
    match level {
        "debug" => "debug",
        "info" | "notice" => "info",
        "warn" => "warn",
        "error" | "crit" | "alert" | "emerg" => "error",
        _ => "info",
    }
}

/// Initialize the logging system based on config, supports file + stderr dual output.
/// 根据配置初始化日志系统，支持文件 + stderr 双写。
///
/// Wraps the `EnvFilter` in a `reload::Layer` so the `/debug/node/log-level/{level}` endpoint
/// can change the filter at runtime. Returns an updater closure and a handle to the current
/// Kong-style level string.
/// 用 `reload::Layer` 包裹 `EnvFilter`，使 `/debug/node/log-level/{level}` 端点能在运行时修改过滤级别；
/// 返回更新闭包和当前 Kong 风格级别字符串的共享句柄。
fn init_logging(
    config: &kong_config::KongConfig,
) -> anyhow::Result<(kong_admin::LogLevelUpdater, Arc<std::sync::RwLock<String>>)> {
    use tracing_subscriber::reload;

    let kong_level = config.log_level.clone();
    let filter_spec = kong_log_level_to_filter(&kong_level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter_spec));

    let (reload_filter, reload_handle) = reload::Layer::new(env_filter);

    let error_log_path = &config.proxy_error_log;

    if error_log_path == "off" {
        tracing_subscriber::registry()
            .with(reload_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        let log_path = Path::new(error_log_path);
        let log_dir = log_path.parent().unwrap_or(Path::new("."));
        let log_file = log_path.file_name().ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid log path: {} — 无效的日志路径: {}",
                error_log_path,
                error_log_path
            )
        })?;

        std::fs::create_dir_all(log_dir)?;
        let file_appender = tracing_appender::rolling::never(log_dir, log_file);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender);
        let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

        tracing_subscriber::registry()
            .with(reload_filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
    }

    let current = Arc::new(std::sync::RwLock::new(kong_level));
    let updater: kong_admin::LogLevelUpdater = Arc::new(move |level: &str| {
        let spec = kong_log_level_to_filter(level);
        let new_filter = EnvFilter::try_new(spec).map_err(|e| e.to_string())?;
        reload_handle
            .modify(|f| *f = new_filter)
            .map_err(|e| e.to_string())
    });

    Ok((updater, current))
}

/// Pingora is incompatible with #[tokio::main] (it creates its own runtime internally), — Pingora 不兼容 #[tokio::main]（它内部创建自己的 runtime），
/// so main is a regular function; non-start commands manually create a tokio runtime. — 所以 main 是普通函数，非 start 命令手动创建 tokio runtime 执行。
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let conf_path = if cli.conf.exists() {
        Some(cli.conf.as_path())
    } else {
        None
    };

    let config = kong_config::load_config(conf_path)?;

    // Health and Version commands don't need logging, execute early — Health 和 Version 命令不需要日志系统，提前执行
    match &cli.command {
        Some(Commands::Version) => {
            println!("kong 0.1.0 (kong-rust)");
            println!("基于 Pingora 和 mlua 的 Kong API Gateway Rust 实现");
            return Ok(());
        }
        Some(Commands::Health) => {
            health_check(&config)?;
            return Ok(());
        }
        Some(Commands::MockUpstream { port, ssl_port }) => {
            // Mock upstream doesn't need full config/logging — Mock upstream 不需要完整配置/日志
            tracing_subscriber::fmt::init();
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mock_upstream::run(port.unwrap_or(15555), *ssl_port))?;
            return Ok(());
        }
        _ => {}
    }

    // Initialize logging based on config (config parse failures output via default panic before this) — 根据配置初始化日志（config 解析失败会在此之前通过默认 panic 输出）
    let (log_updater, current_log_level) = init_logging(&config)?;

    let config = Arc::new(config);

    match cli.command.unwrap_or(Commands::Start) {
        Commands::Check => {
            println!("配置文件检查通过");
            println!("数据库: {}", config.database);
            println!("路由风格: {}", config.router_flavor);
            println!("Admin 监听: {}", format_listen_addrs(&config.admin_listen));
            println!("Proxy 监听: {}", format_listen_addrs(&config.proxy_listen));
        }
        Commands::Config => {
            println!("数据库: {}", config.database);
            println!(
                "PostgreSQL: {}:{}/{}",
                config.pg_host, config.pg_port, config.pg_database
            );
            println!("路由风格: {}", config.router_flavor);
            println!("Admin 监听: {}", format_listen_addrs(&config.admin_listen));
            println!("Proxy 监听: {}", format_listen_addrs(&config.proxy_listen));
            println!("已加载插件: {:?}", config.loaded_plugins());
        }
        Commands::Db { action } | Commands::Migrations { action } => {
            // Non-start commands: manually create tokio runtime — 非 start 命令：手动创建 tokio runtime
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_db_command(&config, action))?;
        }
        Commands::Start => {
            start_gateway(config, false, log_updater, current_log_level)?;
        }
        Commands::DockerStart => {
            // docker-start: auto-run migrations before starting — docker-start：启动前自动执行 migration
            start_gateway(config, true, log_updater, current_log_level)?;
        }
        // Already handled above — 已在上方处理
        Commands::Version | Commands::Health | Commands::MockUpstream { .. } => unreachable!(),
    }

    Ok(())
}

/// Load and register available Lua plugins for the current runtime. — 为当前运行时加载并注册可用的 Lua 插件。
fn build_plugin_registry(config: &kong_config::KongConfig) -> kong_plugin_system::PluginRegistry {
    let mut registry = kong_plugin_system::PluginRegistry::new();
    let plugin_names = config.loaded_plugins();
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&config.prefix);

    tracing::info!(
        "Resolving plugins from directories: {:?} — 从以下目录解析插件: {:?}",
        plugin_dirs,
        plugin_dirs
    );

    match kong_lua_bridge::loader::load_lua_plugins(&plugin_dirs, &plugin_names) {
        Ok(handlers) => {
            for handler in handlers {
                let name = handler.name().to_string();
                registry.register(&name, Arc::new(handler));
            }
        }
        Err(err) => {
            tracing::warn!(
                "Failed to load Lua plugins: {} — 加载 Lua 插件失败: {}",
                err,
                err
            );
        }
    }

    // 注册 Rust 原生 AI 插件
    registry.register("ai-proxy", Arc::new(kong_ai::plugins::AiProxyPlugin::new()));
    registry.register("ai-rate-limit", Arc::new(kong_ai::plugins::AiRateLimitPlugin::new()));
    registry.register("ai-cache", Arc::new(kong_ai::plugins::AiCachePlugin::new()));
    registry.register("ai-prompt-guard", Arc::new(kong_ai::plugins::AiPromptGuardPlugin::new()));

    registry
}

/// Handle db subcommands — 处理 db 子命令
async fn handle_db_command(
    config: &kong_config::KongConfig,
    action: DbAction,
) -> anyhow::Result<()> {
    if config.is_dbless() {
        anyhow::bail!("Database mode is off, migration commands unavailable — 数据库模式为 off，migration 命令不可用");
    }

    let db = kong_db::Database::connect(config).await?;
    let pool = db.pool();

    match action {
        DbAction::Bootstrap => {
            tracing::info!("执行数据库 bootstrap...");
            match kong_db::migrations::bootstrap(pool).await {
                Ok(()) => {
                    println!("数据库 bootstrap 完成");
                }
                Err(e) => {
                    println!("数据库已初始化: {}", e);
                }
            }
        }
        DbAction::Up => {
            tracing::info!("执行数据库 migration...");
            kong_db::migrations::up(pool).await?;
            println!("数据库 migration 完成");
        }
        DbAction::Finish => {
            kong_db::migrations::finish(pool).await?;
            println!("Finish 完成");
        }
        DbAction::List => {
            let state = kong_db::migrations::schema_state(pool).await?;
            if state.needs_bootstrap {
                println!("数据库未初始化，请先运行 'db bootstrap'");
                return Ok(());
            }
            println!("已执行的 migration:");
            for name in &state.executed {
                println!("  [x] {}", name);
            }
            if !state.pending.is_empty() {
                println!("等待 finish 的 migration:");
                for name in &state.pending {
                    println!("  [-] {}", name);
                }
            }
            if !state.new_migrations.is_empty() {
                println!("待执行的 migration:");
                for name in &state.new_migrations {
                    println!("  [ ] {}", name);
                }
            }
        }
        DbAction::Reset { yes } => {
            if !yes {
                eprint!("警告：此操作将删除所有数据库表和数据！确认继续？[y/N] ");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if input.trim().to_lowercase() != "y" {
                    println!("操作已取消");
                    return Ok(());
                }
            }
            kong_db::migrations::reset(pool).await?;
            println!("数据库已重置");
        }
        DbAction::Status => {
            let state = kong_db::migrations::schema_state(pool).await?;
            let status = serde_json::json!({
                "needs_bootstrap": state.needs_bootstrap,
                "executed": state.executed,
                "pending": state.pending,
                "new_migrations": state.new_migrations,
            });
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
    }

    db.close().await;
    Ok(())
}

/// Health check: TCP connect to Admin API and send GET /status — 健康检查：TCP 连接 Admin API 并发送 GET /status
fn health_check(config: &kong_config::KongConfig) -> anyhow::Result<()> {
    let addr = if let Some(a) = config.admin_listen.first() {
        // 将 0.0.0.0 替换为 127.0.0.1，因为不能连接 0.0.0.0 — Replace 0.0.0.0 with 127.0.0.1 since we can't connect to 0.0.0.0
        let ip = if a.ip == "0.0.0.0" {
            "127.0.0.1"
        } else {
            &a.ip
        };
        format!("{}:{}", ip, a.port)
    } else {
        "127.0.0.1:8001".to_string()
    };

    let timeout = std::time::Duration::from_secs(5);
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr.parse()?, timeout).map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to Admin API at {}: {} — 无法连接 Admin API {}: {}",
                addr,
                e,
                addr,
                e
            )
        })?;

    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let request = format!("GET /status HTTP/1.0\r\nHost: {}\r\n\r\n", addr);
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    if response.contains("200") {
        println!(
            "kong is healthy at {} — kong-rust 健康检查通过 {}",
            addr, addr
        );
        Ok(())
    } else {
        anyhow::bail!("kong is NOT healthy: unexpected response from {} — kong-rust 健康检查失败: {} 返回异常响应", addr, addr);
    }
}

/// Format listen address list — 格式化监听地址列表
fn format_listen_addrs(addrs: &[kong_config::ListenAddr]) -> String {
    addrs
        .iter()
        .map(|a| {
            let mut s = format!("{}:{}", a.ip, a.port);
            if a.ssl {
                s.push_str(" ssl");
            }
            if a.http2 {
                s.push_str(" http2");
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Start the gateway: Pingora manages the entire application lifecycle — 启动网关：Pingora 管理整个应用生命周期
/// auto_migrate: if true, auto-run bootstrap + up before starting (for docker-start) — auto_migrate: 为 true 时启动前自动执行 bootstrap + up（用于 docker-start）
fn start_gateway(
    config: Arc<kong_config::KongConfig>,
    auto_migrate: bool,
    log_updater: kong_admin::LogLevelUpdater,
    current_log_level: Arc<std::sync::RwLock<String>>,
) -> anyhow::Result<()> {
    // Resolve cluster role — 解析集群角色
    let cluster_role = config.role;
    tracing::info!("Cluster role: {} — 集群角色: {}", cluster_role, cluster_role);

    tracing::info!("Kong-Rust API Gateway 启动中...");
    tracing::info!("数据库模式: {}", config.database);
    tracing::info!("路由风格: {}", config.router_flavor);

    // Phase 1: Use tokio runtime for async initialization (DB connection, data loading) — 阶段 1：用 tokio runtime 做异步初始化（DB 连接、数据加载）
    // Note: rt must survive until run_forever(), otherwise the sqlx connection pool background tasks — 注意：rt 必须存活到 run_forever()，否则 sqlx 连接池后台任务会因 runtime drop 而终止，
    // will terminate when runtime is dropped, requiring connection rebuilds and causing slow startup responses. — 导致首次 DB 查询需要重建连接，造成启动后响应缓慢。
    let rt = tokio::runtime::Runtime::new()?;
    let (mut kong_proxy, mut admin_state, refresh_rx) = rt.block_on(init_proxy_and_admin(
        &config,
        auto_migrate,
        log_updater,
        current_log_level,
    ))?;

    // Initialize access log async writer (must be created inside tokio runtime since it needs spawn) — 初始化 access log 异步写入器（必须在 tokio runtime 内创建，因为需要 spawn）
    let access_log_writer = rt
        .block_on(async { kong_proxy::access_log::AccessLogWriter::new(&config.proxy_access_log) });
    kong_proxy.access_log_writer = access_log_writer.clone();

    // Phase 2: Create Pingora Server with graceful-shutdown config — 阶段 2：创建带优雅关闭配置的 Pingora Server
    //
    // Pingora handles SIGINT/SIGTERM internally inside `run_forever()` and broadcasts a shutdown — Pingora 在 `run_forever()` 内部处理 SIGINT/SIGTERM，
    // signal to every service via `ShutdownWatch`. All Admin / CP / DP background services in — 并通过 `ShutdownWatch` 广播给所有 service。Admin / CP / DP 等后台服务
    // this crate already react to that watch. We only need to tell Pingora how long to wait — 已经响应 shutdown.changed()，这里只需告知 Pingora 宽限期：
    // for in-flight requests to drain before hard-killing them. — 存量请求多久内必须完成，超过则强制终止。
    //
    // Mapping: — 映射关系：
    //   nginx_main_worker_shutdown_timeout → graceful_shutdown_timeout_seconds — 宽限完成期
    //   grace_period_seconds = 0                                              — 立即开始引流，不再接受新连接
    let mut server_conf = pingora_core::server::configuration::ServerConf::default();
    server_conf.grace_period_seconds = Some(0);
    server_conf.graceful_shutdown_timeout_seconds = Some(config.nginx_main_worker_shutdown_timeout);
    let mut server = pingora::server::Server::new_with_opt_and_conf(None, server_conf);

    // Phase 3: Create Proxy Service, bind all proxy_listen addresses — 阶段 3：创建 Proxy Service，绑定所有 proxy_listen 地址
    let mut proxy_service =
        pingora_proxy::http_proxy_service(&server.configuration, kong_proxy.clone());

    // Enable h2c (plaintext HTTP/2) for gRPC support — 启用 h2c（明文 HTTP/2）以支持 gRPC
    // Pingora will peek for the h2 preface on plaintext connections and auto-detect HTTP/2
    if let Some(proxy) = proxy_service.app_logic_mut() {
        let mut server_opts = pingora_core::apps::HttpServerOptions::default();
        server_opts.h2c = true;
        proxy.server_options = Some(server_opts);
    }
    for addr in &config.proxy_listen {
        let listen_addr = format!("{}:{}", addr.ip, addr.port);
        if addr.ssl {
            // SSL port: register with add_tls, Pingora handles TLS termination — SSL 端口：使用 add_tls 注册，Pingora 负责 TLS 终止
            if let (Some(cert), Some(key)) = (config.ssl_cert.first(), config.ssl_cert_key.first())
            {
                let mut tls_settings =
                    match pingora_core::listeners::tls::TlsSettings::intermediate(cert, key) {
                        Ok(settings) => settings,
                        Err(e) => {
                            tracing::error!("Proxy TLS 配置失败 {}: {}", listen_addr, e);
                            // Fallback to TCP — 回退到 TCP
                            proxy_service.add_tcp(&listen_addr);
                            tracing::warn!("Proxy 回退为 TCP 监听: {}", listen_addr);
                            continue;
                        }
                    };

                if addr.http2 {
                    tls_settings.enable_h2();
                    tracing::info!("Proxy 监听于: {} (TLS+HTTP/2)", listen_addr);
                } else {
                    tracing::info!("Proxy 监听于: {} (TLS)", listen_addr);
                }

                proxy_service.add_tls_with_settings(&listen_addr, None, tls_settings);
            } else {
                tracing::warn!(
                    "Proxy SSL port {} missing ssl_cert/ssl_cert_key config, falling back to TCP — Proxy SSL 端口 {} 缺少 ssl_cert/ssl_cert_key 配置，回退为 TCP",
                    listen_addr,
                    listen_addr
                );
                proxy_service.add_tcp(&listen_addr);
            }
        } else {
            proxy_service.add_tcp(&listen_addr);
            tracing::info!("Proxy 监听于: {}", listen_addr);
        }
    }

    // Phase 3.5: Create Stream Proxy Service (L4 TCP/TLS proxy) — 阶段 3.5：创建 Stream Proxy Service（L4 TCP/TLS 代理）
    // Get initial route list for StreamRouter build (inconvenient to get from KongProxy's router, — 获取初始路由列表用于 StreamRouter 构建（从 KongProxy 的路由器中获取不便，
    // reload from AdminState's DAO directly; data is already in DB during init phase) — 直接从 AdminState 的 DAO 重新加载，init 阶段数据已在 DB 中）
    let stream_router_ref = if !config.stream_listen.is_empty() {
        // Get route data from AdminState to initialize Stream routing — 从 AdminState 获取路由数据初始化 Stream 路由
        let routes = rt.block_on(async {
            use kong_core::traits::PageParams;
            let params = PageParams {
                size: 10000,
                ..Default::default()
            };
            match admin_state.routes.page(&params).await {
                Ok(page) => page.data,
                Err(e) => {
                    tracing::error!(
                        "Failed to load Stream routes: {} — 加载 Stream 路由失败: {}",
                        e,
                        e
                    );
                    Vec::new()
                }
            }
        });

        let mut stream_proxy = kong_proxy::stream::KongStreamProxy::new(
            &routes,
            kong_proxy.balancers.clone(),
            kong_proxy.services.clone(),
            kong_proxy.cert_manager.clone(),
            kong_proxy.dns_resolver.clone(),
        );
        // Stream access log async writer — Stream access log 异步写入器
        stream_proxy.access_log_writer = rt.block_on(async {
            kong_proxy::access_log::AccessLogWriter::new(&config.proxy_stream_access_log)
        });

        // Save the Arc reference to stream_router for AdminState to use for hot-reloading — 保存 stream_router 的 Arc 引用，后续传给 AdminState 用于热更新
        let stream_router = stream_proxy.stream_router.clone();

        let mut stream_service = pingora_core::services::listening::Service::new(
            "Stream Proxy".to_string(),
            stream_proxy,
        );
        for addr in &config.stream_listen {
            let listen_addr = format!("{}:{}", addr.ip, addr.port);
            stream_service.add_tcp(&listen_addr);
            tracing::info!("Stream Proxy 监听于: {}", listen_addr);
        }
        server.add_service(stream_service);
        Some(stream_router)
    } else {
        None
    };

    // Inject stream_router reference into AdminState so route hot-updates sync to Stream Proxy — 将 stream_router 引用注入 AdminState，使路由热更新同步到 Stream Proxy
    admin_state.stream_router = stream_router_ref;

    // Phase 4.5: Create CP if role=control_plane, inject into AdminState before it's consumed
    // 阶段 4.5：如果是 CP 角色，创建 ControlPlane 并注入 AdminState（必须在 AdminState 被消费前）
    let cp_arc = if cluster_role.is_control_plane() {
        let cp = Arc::new(kong_cluster::cp::ControlPlane::new());
        admin_state.cp = Some(Arc::clone(&cp));
        Some(cp)
    } else {
        None
    };

    // Clone fields for DP before AdminBgService takes ownership — 在 AdminBgService 取走所有权之前为 DP 克隆字段
    let admin_state_dbless_store = admin_state.dbless_store.clone();
    let admin_state_refresh_tx = admin_state.refresh_tx.clone();

    // Phase 4: Create Admin API BackgroundService — 阶段 4：创建 Admin API BackgroundService
    let admin_bg = AdminBgService {
        state: admin_state,
        config: Arc::clone(&config),
        refresh_rx: std::sync::Mutex::new(Some(refresh_rx)),
    };
    let admin_service =
        pingora_core::services::background::background_service("Admin API", admin_bg);

    // Phase 5: Register services — 阶段 5：注册服务
    server.add_service(proxy_service);
    server.add_service(admin_service);

    // Phase 5.5: Role-specific startup — 阶段 5.5：角色特定启动逻辑
    if let Some(cp) = cp_arc {
        // CP mode: start WebSocket server on cluster_listen for DP connections
        // CP 模式：在 cluster_listen 上启动 WebSocket 服务，接受 DP 连接
        let cluster_listen = config.cluster_listen.first()
            .map(|l| format!("{}:{}", l.ip, l.port))
            .unwrap_or_else(|| "0.0.0.0:8005".to_string());

        let cp_clone = Arc::clone(&cp);
        // Try to build TLS config from KongConfig — 尝试从 KongConfig 构建 TLS 配置
        let cluster_tls_config = match kong_cluster::tls::ClusterTlsConfig::from_kong_config(&config) {
            Ok(tls) => Some(tls),
            Err(e) => {
                tracing::error!(
                    "CP TLS config build failed, refusing to start without TLS: {} — CP TLS 配置构建失败，拒绝在无 TLS 的情况下启动: {}",
                    e, e
                );
                anyhow::bail!("control_plane requires valid TLS config: {}", e);
            }
        };
        // Spawn CP WebSocket server as background task in Pingora's runtime
        // 在 Pingora 的 runtime 中作为后台任务启动 CP WebSocket 服务
        let cp_bg = CpBgService {
            cp: cp_clone,
            cluster_listen,
            cluster_tls_config,
        };
        let cp_service = pingora_core::services::background::background_service(
            "CP WebSocket",
            cp_bg,
        );
        server.add_service(cp_service);
    }

    if cluster_role.is_data_plane() {
        // DP mode: connect to CP and receive config
        // DP 模式：连接 CP 并接收配置
        let dp_tls_config = kong_cluster::tls::ClusterTlsConfig::from_kong_config(&config).ok();
        let dp_use_tls = dp_tls_config.is_some();
        let dp = Arc::new(kong_cluster::dp::DataPlane::with_tls(
            &config.cluster_control_plane,
            &config.prefix,
            config.loaded_plugins(),
            uuid::Uuid::parse_str(config.node_id.as_deref().unwrap_or(""))
                .unwrap_or_else(|_| uuid::Uuid::new_v4()),
            gethostname::gethostname().to_string_lossy().to_string(),
            dp_use_tls,
        ));

        let dp_bg = DpBgService {
            dp: Arc::clone(&dp),
            config: Arc::clone(&config),
            dbless_store: admin_state_dbless_store.clone(),
            refresh_tx: admin_state_refresh_tx.clone(),
            tls_config: dp_tls_config,
        };
        let dp_service = pingora_core::services::background::background_service(
            "DP WebSocket",
            dp_bg,
        );
        server.add_service(dp_service);
    }

    // Phase 6: Bootstrap — 阶段 6：启动
    server.bootstrap();
    tracing::info!("Kong-Rust 启动完成");

    // Phase 7: Block and run forever — 阶段 7：阻塞运行（永不返回）
    server.run_forever();
}

/// Async initialization: connect DB, load data, build KongProxy and AdminState — 异步初始化：连接 DB、加载数据、构建 KongProxy 和 AdminState
/// auto_migrate: if true, auto-run bootstrap + up instead of failing on pending migrations — auto_migrate: 为 true 时自动执行 bootstrap + up，而非报错退出
async fn init_proxy_and_admin(
    config: &Arc<kong_config::KongConfig>,
    auto_migrate: bool,
    log_updater: kong_admin::LogLevelUpdater,
    current_log_level: Arc<std::sync::RwLock<String>>,
) -> anyhow::Result<(
    kong_proxy::KongProxy,
    kong_admin::AdminState,
    tokio::sync::mpsc::UnboundedReceiver<&'static str>,
)> {
    use kong_core::models::*;
    use kong_core::traits::{Dao, PageParams};
    use kong_db::*;

    let plugin_registry = build_plugin_registry(config);
    // Use configured node_id if provided, otherwise generate a new one — 如果配置了 node_id 则使用，否则生成新的
    let node_id = match &config.node_id {
        Some(id) => uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
        None => uuid::Uuid::new_v4(),
    };
    let (refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel();

    // Shared Kong cache — exposed via /cache Admin endpoints, sized from config
    // 共享 Kong 缓存 — 通过 /cache Admin 端点暴露，容量来自配置
    let kong_cache = Arc::new(kong_db::KongCache::from_kong_config(config));

    // Create shared async DNS resolver — 创建共享异步 DNS 解析器
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(config));

    if config.is_dbless() {
        // db-less mode: empty routing table, in-memory store — db-less 模式：空路由表，内存存储
        let store = Arc::new(DblessStore::new());

        if let Some(ref path) = config.declarative_config {
            tracing::info!("加载声明式配置: {}", path);
            store.load_from_file(path)?;
        }

        let kong_proxy = kong_proxy::KongProxy::new(
            &[],
            &config.router_flavor,
            plugin_registry,
            kong_proxy::tls::CertificateManager::new(),
            Vec::new(),
            dns_resolver,
            Arc::clone(config),
        );

        let admin_state = kong_admin::AdminState {
            services: Arc::new(DblessDao::<Service>::new(Arc::clone(&store))),
            routes: Arc::new(DblessDao::<Route>::new(Arc::clone(&store))),
            consumers: Arc::new(DblessDao::<Consumer>::new(Arc::clone(&store))),
            plugins: Arc::new(DblessDao::<Plugin>::new(Arc::clone(&store))),
            upstreams: Arc::new(DblessDao::<Upstream>::new(Arc::clone(&store))),
            targets: Arc::new(DblessDao::<Target>::new(Arc::clone(&store))),
            certificates: Arc::new(DblessDao::<Certificate>::new(Arc::clone(&store))),
            snis: Arc::new(DblessDao::<Sni>::new(Arc::clone(&store))),
            ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(Arc::clone(&store))),
            vaults: Arc::new(DblessDao::<Vault>::new(Arc::clone(&store))),
            ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(Arc::clone(&store))),
            ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(Arc::clone(&store))),
            ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(Arc::clone(&store))),
            node_id,
            config: Arc::clone(config),
            proxy: kong_proxy.clone(),
            refresh_tx,
            stream_router: None, // Set as needed in start_gateway — start_gateway 中按需设置
            configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
            dbless_store: Some(Arc::clone(&store)),
            target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            cp: None, // Set in start_gateway if role=control_plane — start_gateway 中按需设置（仅 CP 模式）
            cache: Arc::clone(&kong_cache),
            log_updater: Some(Arc::clone(&log_updater)),
            current_log_level: Arc::clone(&current_log_level),
        };

        Ok((kong_proxy, admin_state, refresh_rx))
    } else {
        // PostgreSQL mode — PostgreSQL 模式
        let db = Database::connect(config).await?;

        // Check schema state and auto-migrate if in docker-start mode — 检查 schema 状态，docker-start 模式下自动执行 migration
        let migration_state = kong_db::migrations::schema_state(db.pool()).await?;
        if auto_migrate {
            // docker-start mode: auto-run bootstrap + up — docker-start 模式：自动执行 bootstrap + up
            if migration_state.needs_bootstrap {
                tracing::info!("Auto-running database bootstrap... — 自动执行数据库 bootstrap...");
                kong_db::migrations::bootstrap(db.pool()).await?;
            }
            // Re-check state after bootstrap — bootstrap 后重新检查状态
            let state = kong_db::migrations::schema_state(db.pool()).await?;
            if !state.new_migrations.is_empty() {
                tracing::info!("Auto-running database migrations ({} pending)... — 自动执行数据库 migration（{} 个待执行）...",
                    state.new_migrations.len(), state.new_migrations.len());
                kong_db::migrations::up(db.pool()).await?;
            }
            if !state.pending.is_empty() {
                tracing::info!(
                    "Auto-finishing pending migrations... — 自动完成 pending migration..."
                );
                kong_db::migrations::finish(db.pool()).await?;
            }
        } else {
            // Normal start mode: fail if migrations are needed — 普通 start 模式：需要 migration 时报错
            if migration_state.needs_bootstrap {
                anyhow::bail!("Database not initialized, please run 'kong db bootstrap' / 'kong migrations bootstrap' first — 数据库未初始化，请先运行 'kong db bootstrap' / 'kong migrations bootstrap'");
            }
            if !migration_state.new_migrations.is_empty() {
                anyhow::bail!("New migrations pending, please run 'kong db up' / 'kong migrations up' first — 有新的 migration 待执行，请先运行 'kong db up' / 'kong migrations up'");
            }
        }

        // Full data load from DB — 从 DB 全量加载初始数据
        let all_params = PageParams {
            size: 1000,
            ..Default::default()
        };

        let routes_dao = PgDao::<Route>::new(db.clone(), route_schema());
        let services_dao = PgDao::<Service>::new(db.clone(), service_schema());
        let upstreams_dao = PgDao::<Upstream>::new(db.clone(), upstream_schema());
        let targets_dao = PgDao::<Target>::new(db.clone(), target_schema());
        let plugins_dao = PgDao::<Plugin>::new(db.clone(), plugin_schema());
        let certificates_dao = PgDao::<Certificate>::new(db.clone(), certificate_schema());
        let snis_dao = PgDao::<Sni>::new(db.clone(), sni_schema());
        let ca_certificates_dao = PgDao::<CaCertificate>::new(db.clone(), ca_certificate_schema());

        let routes_page = routes_dao.page(&all_params).await?;
        let services_page = services_dao.page(&all_params).await?;
        let upstreams_page = upstreams_dao.page(&all_params).await?;
        let targets_page = targets_dao.page(&all_params).await?;
        let plugins_page = plugins_dao.page(&all_params).await?;
        let certificates_page = certificates_dao.page(&all_params).await?;
        let snis_page = snis_dao.page(&all_params).await?;
        let ca_certificates_page = ca_certificates_dao.page(&all_params).await?;

        tracing::info!(
            "从数据库加载: {} routes, {} services, {} upstreams, {} targets, {} plugins, {} certs, {} CAs",
            routes_page.data.len(),
            services_page.data.len(),
            upstreams_page.data.len(),
            targets_page.data.len(),
            plugins_page.data.len(),
            certificates_page.data.len(),
            ca_certificates_page.data.len(),
        );

        // Build CertificateManager and load certificates — 构建 CertificateManager 并加载证书
        let cert_manager = kong_proxy::tls::CertificateManager::new();
        cert_manager.load_certificates(&certificates_page.data, &snis_page.data);

        // Build KongProxy and populate data — 构建 KongProxy 并填充数据
        let kong_proxy = kong_proxy::KongProxy::new(
            &routes_page.data,
            &config.router_flavor,
            plugin_registry,
            cert_manager,
            ca_certificates_page.data.clone(),
            dns_resolver,
            Arc::clone(config),
        );
        kong_proxy.update_services(services_page.data);
        kong_proxy.update_upstreams(upstreams_page.data, targets_page.data);
        kong_proxy.update_plugins(plugins_page.data);

        let admin_state = kong_admin::AdminState {
            services: Arc::new(PgDao::<Service>::new(db.clone(), service_schema())),
            routes: Arc::new(PgDao::<Route>::new(db.clone(), route_schema())),
            consumers: Arc::new(PgDao::<Consumer>::new(db.clone(), consumer_schema())),
            plugins: Arc::new(PgDao::<Plugin>::new(db.clone(), plugin_schema())),
            upstreams: Arc::new(PgDao::<Upstream>::new(db.clone(), upstream_schema())),
            targets: Arc::new(PgDao::<Target>::new(db.clone(), target_schema())),
            certificates: Arc::new(PgDao::<Certificate>::new(db.clone(), certificate_schema())),
            snis: Arc::new(PgDao::<Sni>::new(db.clone(), sni_schema())),
            ca_certificates: Arc::new(PgDao::<CaCertificate>::new(
                db.clone(),
                ca_certificate_schema(),
            )),
            vaults: Arc::new(PgDao::<Vault>::new(db.clone(), vault_schema())),
            ai_providers: Arc::new(PgDao::<kong_ai::models::AiProviderConfig>::new(db.clone(), ai_provider_schema())),
            ai_models: Arc::new(PgDao::<kong_ai::models::AiModel>::new(db.clone(), ai_model_schema())),
            ai_virtual_keys: Arc::new(PgDao::<kong_ai::models::AiVirtualKey>::new(db.clone(), ai_virtual_key_schema())),
            node_id,
            config: Arc::clone(config),
            proxy: kong_proxy.clone(),
            refresh_tx,
            stream_router: None, // Set as needed in start_gateway — start_gateway 中按需设置
            // DB mode: empty string = no configuration_hash in /status — DB 模式：空字符串 = /status 不返回 configuration_hash
            configuration_hash: Arc::new(std::sync::RwLock::new(String::new())),
            dbless_store: None,
            target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            cp: None, // Set in start_gateway if role=control_plane — start_gateway 中按需设置（仅 CP 模式）
            cache: Arc::clone(&kong_cache),
            log_updater: Some(Arc::clone(&log_updater)),
            current_log_level: Arc::clone(&current_log_level),
        };

        Ok((kong_proxy, admin_state, refresh_rx))
    }
}

/// Admin API background service, lifecycle managed by Pingora — Admin API 后台服务，由 Pingora 管理生命周期
struct AdminBgService {
    state: kong_admin::AdminState,
    config: Arc<kong_config::KongConfig>,
    /// Cache refresh debounce receiver, wrapped in Mutex<Option<...>> for take in &self methods — 缓存刷新防抖接收端，用 Mutex<Option<...>> 包装以便在 &self 方法中 take
    refresh_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<&'static str>>>,
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for AdminBgService {
    async fn start(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let bind_addr = if let Some(addr) = self.config.admin_listen.first() {
            format!("{}:{}", addr.ip, addr.port)
        } else {
            "0.0.0.0:8001".to_string()
        };
        tracing::info!("Admin API 监听于: {}", bind_addr);

        let status_bind = self
            .config
            .status_listen
            .first()
            .map(|addr| format!("{}:{}", addr.ip, addr.port));

        // Start cache refresh debounce background task — 启动缓存刷新防抖后台任务
        if let Some(rx) = self.refresh_rx.lock().unwrap().take() {
            let state = self.state.clone();
            tokio::spawn(kong_admin::run_cache_refresher(rx, state));
            tracing::info!("缓存刷新防抖任务已启动（100ms 窗口合并）");
        }

        // Start GUI server if admin_gui_listen is configured and GUI directory exists — 如果配置了 admin_gui_listen 且 GUI 目录存在，启动 GUI 服务器
        let gui_dir =
            std::env::var("KONG_GUI_DIR").unwrap_or_else(|_| "/usr/local/kong/gui".to_string());
        if !self.config.admin_gui_listen.is_empty()
            && Path::new(&gui_dir).join("index.html").exists()
        {
            let gui_bind = if let Some(addr) = self.config.admin_gui_listen.first() {
                format!("{}:{}", addr.ip, addr.port)
            } else {
                "0.0.0.0:8002".to_string()
            };

            // Derive Admin API URL for kconfig.js — 推导 Admin API URL 用于 kconfig.js
            // 使用 admin_listen 的端口，schema 和 host 从 admin_gui_url 推导 — Use admin_listen port, derive scheme/host from admin_gui_url
            let admin_port = self
                .config
                .admin_listen
                .first()
                .map(|a| a.port)
                .unwrap_or(8001);
            let admin_api_url = format!("http://localhost:{}", admin_port);

            let gui_app = kong_admin::build_gui_router(&gui_dir, &admin_api_url);
            tracing::info!(
                "Kong Manager GUI 监听于: {} (Admin API: {})",
                gui_bind,
                admin_api_url
            );

            tokio::spawn(async move {
                match tokio::net::TcpListener::bind(&gui_bind).await {
                    Ok(listener) => {
                        if let Err(e) = axum::serve(listener, gui_app).await {
                            tracing::error!("Kong Manager GUI 异常退出: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Kong Manager GUI 绑定失败 {}: {e}", gui_bind);
                    }
                }
            });
        }

        let app = kong_admin::build_admin_router(self.state.clone());
        if let Some(status_bind_addr) = status_bind {
            let status_app = kong_admin::build_status_router(self.state.clone());
            let status_addr = self.config.status_listen.first().cloned();
            let ssl_cert = self.config.ssl_cert.first().cloned();
            let ssl_cert_key = self.config.ssl_cert_key.first().cloned();
            tracing::info!("Status API 监听于: {}", status_bind_addr);
            tokio::spawn(async move {
                let needs_tls = status_addr.as_ref().map_or(false, |a| a.ssl);
                if needs_tls {
                    // TLS + HTTP/2 mode for Status API — Status API 的 TLS + HTTP/2 模式
                    // If no explicit cert, generate self-signed — 如果没有显式证书，生成自签证书
                    let (cert_path, key_path) = if let (Some(c), Some(k)) = (ssl_cert, ssl_cert_key) {
                        (c, k)
                    } else {
                        // Generate self-signed cert — 生成自签证书
                        let cert_file = "/tmp/kong_status_self_signed.pem";
                        let key_file = "/tmp/kong_status_self_signed_key.pem";
                        let _ = std::process::Command::new("openssl")
                            .args(["req", "-x509", "-newkey", "rsa:2048",
                                   "-keyout", key_file, "-out", cert_file,
                                   "-days", "365", "-nodes", "-subj", "/CN=localhost"])
                            .output();
                        (cert_file.to_string(), key_file.to_string())
                    };
                    match start_tls_server(
                        &status_bind_addr,
                        &cert_path,
                        &key_path,
                        status_app,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(e) => tracing::error!("Status API TLS 异常退出: {e}"),
                    }
                } else {
                    match tokio::net::TcpListener::bind(&status_bind_addr).await {
                        Ok(listener) => {
                            if let Err(e) = axum::serve(listener, status_app).await {
                                tracing::error!("Status API 异常退出: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::error!("Status API 绑定失败 {}: {e}", status_bind_addr);
                        }
                    }
                }
            });
        }

        let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Admin API 绑定失败: {e}");
                return;
            }
        };

        // Use tokio::select to wait for both axum serve and shutdown signal — 用 tokio::select 同时等待 axum serve 和 shutdown 信号
        tokio::select! {
            result = axum::serve(listener, app) => {
                if let Err(e) = result {
                    tracing::error!("Admin API 异常退出: {e}");
                }
            }
            _ = shutdown.changed() => {
                tracing::info!("Admin API 收到关闭信号，正在停止...");
            }
        }
    }
}

// ==================== Hybrid mode background services — 混合模式后台服务 ====================

/// CP WebSocket background service — CP WebSocket 后台服务
struct CpBgService {
    cp: Arc<kong_cluster::cp::ControlPlane>,
    cluster_listen: String,
    /// Optional TLS config for mTLS — 可选的 mTLS 配置
    cluster_tls_config: Option<kong_cluster::tls::ClusterTlsConfig>,
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for CpBgService {
    async fn start(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let cp = Arc::clone(&self.cp);

        // Spawn periodic stale DP purge task (every 30s, 60s timeout) — 启动周期性清理过期 DP 任务（每 30 秒检查，60 秒超时）
        {
            let cp_purge = Arc::clone(&cp);
            let mut purge_shutdown = shutdown.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            cp_purge.purge_stale_dps(60).await;
                        }
                        _ = purge_shutdown.changed() => {
                            tracing::info!("Stale DP purge task shutting down — 过期 DP 清理任务正在关闭...");
                            break;
                        }
                    }
                }
            });
        }

        let app = axum::Router::new()
            .route("/v1/outlet", axum::routing::get({
                let cp = Arc::clone(&cp);
                move |ws: axum::extract::WebSocketUpgrade,
                      axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
                      query: axum::extract::Query<HashMap<String, String>>| {
                    let cp = Arc::clone(&cp);
                    async move {
                        let ip = addr.ip().to_string();
                        ws.on_upgrade(move |socket| handle_dp_connection(socket, cp, query.0, ip))
                    }
                }
            }))
            .route("/v2/outlet", axum::routing::get({
                let cp = Arc::clone(&cp);
                move |ws: axum::extract::WebSocketUpgrade,
                      axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
                      query: axum::extract::Query<HashMap<String, String>>| {
                    let cp = Arc::clone(&cp);
                    async move {
                        let ip = addr.ip().to_string();
                        ws.on_upgrade(move |socket| handle_v2_dp_connection(socket, cp, query.0, ip))
                    }
                }
            }));

        let listener = match tokio::net::TcpListener::bind(&self.cluster_listen).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(
                    "CP WebSocket bind failed {}: {} — CP WebSocket 绑定失败 {}: {}",
                    self.cluster_listen, e, self.cluster_listen, e
                );
                return;
            }
        };

        // Build TLS acceptor if config is present — 如果有 TLS 配置则构建 TLS acceptor
        let tls_acceptor = if let Some(ref tls_config) = self.cluster_tls_config {
            match build_cluster_tls_acceptor(tls_config) {
                Ok(acceptor) => {
                    tracing::info!(
                        "CP WebSocket server listening on {} with mTLS ({:?} mode, cert={}) — CP WebSocket 服务监听于 {}，已启用 mTLS（{:?} 模式，证书={}）",
                        self.cluster_listen, tls_config.mode, tls_config.cert_path,
                        self.cluster_listen, tls_config.mode, tls_config.cert_path,
                    );
                    Some(acceptor)
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to build TLS acceptor, aborting cluster listener: {} — 构建 TLS acceptor 失败，终止集群监听: {}",
                        e, e
                    );
                    return;
                }
            }
        } else {
            None
        };

        if tls_acceptor.is_none() {
            tracing::info!(
                "CP WebSocket server listening on {} (plain TCP, no TLS) — CP WebSocket 服务监听于 {}（纯 TCP，无 TLS）",
                self.cluster_listen, self.cluster_listen
            );
        }

        if let Some(acceptor) = tls_acceptor {
            // TLS mode: manual accept loop with hyper — TLS 模式：使用 hyper 手动 accept 循环
            let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
            cp_serve_tls(listener, acceptor, make_service, shutdown).await;
        } else {
            // Plain TCP fallback — 纯 TCP 回退
            let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
            tokio::select! {
                result = axum::serve(listener, make_service) => {
                    if let Err(e) = result {
                        tracing::error!("CP WebSocket server exited: {} — CP WebSocket 服务异常退出: {}", e, e);
                    }
                }
                _ = shutdown.changed() => {
                    tracing::info!("CP WebSocket server shutting down — CP WebSocket 服务正在关闭...");
                }
            }
        }
    }
}

/// Build OpenSSL SslAcceptor for cluster mTLS — 为集群 mTLS 构建 OpenSSL SslAcceptor
fn build_cluster_tls_acceptor(
    tls_config: &kong_cluster::tls::ClusterTlsConfig,
) -> Result<openssl::ssl::SslAcceptor, openssl::error::ErrorStack> {
    use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod, SslVerifyMode};

    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls_server())?;
    // Load server certificate and key — 加载服务端证书和密钥
    builder.set_certificate_file(&tls_config.cert_path, SslFiletype::PEM)?;
    builder.set_private_key_file(&tls_config.key_path, SslFiletype::PEM)?;

    // Both modes require client certificate verification — 两种模式都要求验证客户端证书
    match tls_config.mode {
        kong_cluster::tls::ClusterTlsMode::Pki => {
            // PKI mode: use CA cert for chain verification — PKI 模式：使用 CA 证书进行链验证
            if let Some(ref ca_path) = tls_config.ca_cert_path {
                builder.set_ca_file(ca_path)?;
            }
        }
        kong_cluster::tls::ClusterTlsMode::Shared => {
            // Shared mode: use own cert as CA (both sides share the same certificate)
            // 共享模式：使用自身证书作为 CA（双方使用相同证书）
            builder.set_ca_file(&tls_config.cert_path)?;
        }
    }
    builder.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);

    Ok(builder.build())
}

/// Build native-tls TlsConnector for DP connecting to CP — 为 DP 连接 CP 构建 native-tls TlsConnector
fn build_dp_tls_connector(
    tls_config: &kong_cluster::tls::ClusterTlsConfig,
) -> anyhow::Result<native_tls::TlsConnector> {
    // Read client certificate (PEM → PKCS12) — 读取客户端证书
    let cert_pem = std::fs::read(&tls_config.cert_path)?;
    let key_pem = std::fs::read(&tls_config.key_path)?;
    let identity = native_tls::Identity::from_pkcs8(&cert_pem, &key_pem)?;

    let mut builder = native_tls::TlsConnector::builder();
    builder.identity(identity);

    // Add CA cert for verification — 添加 CA 证书用于验证
    let ca_path = match tls_config.mode {
        kong_cluster::tls::ClusterTlsMode::Pki => tls_config.ca_cert_path.as_deref(),
        // Shared mode: use own cert as CA — 共享模式：使用自身证书作为 CA
        kong_cluster::tls::ClusterTlsMode::Shared => Some(tls_config.cert_path.as_str()),
    };
    if let Some(ca) = ca_path {
        let ca_pem = std::fs::read(ca)?;
        let ca_cert = native_tls::Certificate::from_pem(&ca_pem)?;
        builder.add_root_certificate(ca_cert);
    }

    Ok(builder.build()?)
}

/// Serve axum app over TLS connections using hyper — 通过 TLS 连接使用 hyper 提供 axum 服务
async fn cp_serve_tls(
    listener: tokio::net::TcpListener,
    acceptor: openssl::ssl::SslAcceptor,
    mut make_service: axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    use tower::Service;

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (tcp_stream, remote_addr) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("CP TLS accept error: {} — CP TLS 接受连接错误: {}", e, e);
                        continue;
                    }
                };

                let ssl = match openssl::ssl::Ssl::new(acceptor.context()) {
                    Ok(ssl) => ssl,
                    Err(e) => {
                        tracing::error!("Failed to create SSL instance: {} — 创建 SSL 实例失败: {}", e, e);
                        continue;
                    }
                };

                let mut tls_stream = match tokio_openssl::SslStream::new(ssl, tcp_stream) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("Failed to create SslStream: {} — 创建 SslStream 失败: {}", e, e);
                        continue;
                    }
                };

                // Perform TLS handshake — 执行 TLS 握手
                if let Err(e) = Pin::new(&mut tls_stream).accept().await {
                    tracing::warn!(
                        "TLS handshake failed from {}: {} — 来自 {} 的 TLS 握手失败: {}",
                        remote_addr, e, remote_addr, e
                    );
                    continue;
                }

                let tower_service = make_service
                    .call(remote_addr)
                    .await
                    .unwrap();

                // Spawn a task to serve this connection — 启动任务处理此连接
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let hyper_service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        tower_service.clone().call(request)
                    });
                    if let Err(e) = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection_with_upgrades(io, hyper_service)
                        .await
                    {
                        // Ignore normal connection close errors — 忽略正常的连接关闭错误
                        let msg = e.to_string();
                        if !msg.contains("connection closed") && !msg.contains("not connected") {
                            tracing::debug!("CP TLS connection error: {} — CP TLS 连接错误: {}", e, e);
                        }
                    }
                });
            }
            _ = shutdown.changed() => {
                tracing::info!("CP WebSocket TLS server shutting down — CP WebSocket TLS 服务正在关闭...");
                break;
            }
        }
    }
}

/// Handle incoming DP WebSocket connection — 处理传入的 DP WebSocket 连接
async fn handle_dp_connection(
    socket: axum::extract::ws::WebSocket,
    cp: Arc<kong_cluster::cp::ControlPlane>,
    query: HashMap<String, String>,
    ip: String,
) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};

    let dp_id = query.get("node_id")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);
    let dp_hostname = query.get("node_hostname").cloned().unwrap_or_default();
    let dp_version = query.get("node_version").cloned().unwrap_or_default();

    let dp_info = kong_cluster::DataPlaneInfo {
        id: dp_id,
        ip, // Extracted from ConnectInfo<SocketAddr> — 从 ConnectInfo<SocketAddr> 中提取
        hostname: dp_hostname,
        version: dp_version,
        sync_status: kong_cluster::SyncStatus::Unknown,
        config_hash: kong_cluster::EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: HashMap::new(),
    };

    let mut config_rx = cp.register_dp(dp_info).await;

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Wait for basic_info from DP — 等待 DP 发送 basic_info
    if let Some(Ok(msg)) = ws_receiver.next().await {
        if let Message::Binary(_data) = msg {
            tracing::debug!(
                "Received basic_info from DP {} — 收到 DP {} 的 basic_info",
                dp_id, dp_id
            );
        }
    }

    // Send current config immediately if available — 如果有当前配置则立即推送给新连接的 DP
    if let Some(payload) = cp.current_payload().await {
        let bytes: Vec<u8> = payload.as_ref().to_vec();
        let msg: Message = Message::Binary(bytes.into());
        if ws_sender.send(msg).await.is_err() {
            cp.unregister_dp(&dp_id).await;
            return;
        }
        tracing::info!(
            "Sent current config to newly connected DP {} — 已向新连接的 DP {} 推送当前配置",
            dp_id, dp_id
        );
    }

    // Read/write loop: handle ping/pong and config broadcast
    // 读写循环：处理 ping/pong 和配置广播
    loop {
        tokio::select! {
            // Receive from DP (ping with hash) — 接收 DP 消息（带哈希的 ping）
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        // DP sends PING with config hash — DP 发送带配置哈希的 PING
                        let hash = String::from_utf8_lossy(&data).to_string();
                        cp.update_dp_status(&dp_id, &hash).await;
                        // Send PONG — 发送 PONG
                        let pong: Message = Message::Pong(data);
                        if ws_sender.send(pong).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::info!("DP {} disconnected — DP {} 已断开", dp_id, dp_id);
                        break;
                    }
                    _ => {}
                }
            }
            // Send config updates to DP — 发送配置更新给 DP
            payload = config_rx.recv() => {
                match payload {
                    Ok(data) => {
                        let bytes: Vec<u8> = data.as_ref().to_vec();
                        let msg: Message = Message::Binary(bytes.into());
                        if ws_sender.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    cp.unregister_dp(&dp_id).await;
}

/// Handle incoming V2 DP WebSocket connection (JSON-RPC 2.0 protocol)
/// 处理传入的 V2 DP WebSocket 连接（JSON-RPC 2.0 协议）
async fn handle_v2_dp_connection(
    socket: axum::extract::ws::WebSocket,
    cp: Arc<kong_cluster::cp::ControlPlane>,
    query: HashMap<String, String>,
    ip: String,
) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    use kong_cluster::protocol::{
        self, JsonRpcRequest, V2_METHOD_INIT, V2_METHOD_GET_DELTA,
        build_v2_init_response, build_v2_delta_response, build_v2_notify_new_version,
    };

    let dp_id = query.get("node_id")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);
    let dp_hostname = query.get("node_hostname").cloned().unwrap_or_default();
    let dp_version = query.get("node_version").cloned().unwrap_or_default();

    tracing::info!(
        "V2 DP {} connected (hostname={}, version={}) — V2 DP {} 已连接",
        dp_id, dp_hostname, dp_version, dp_id
    );

    let dp_info = kong_cluster::DataPlaneInfo {
        id: dp_id,
        ip, // Extracted from ConnectInfo<SocketAddr> — 从 ConnectInfo<SocketAddr> 中提取
        hostname: dp_hostname,
        version: dp_version,
        sync_status: kong_cluster::SyncStatus::Unknown,
        config_hash: kong_cluster::EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: HashMap::new(),
    };

    let mut config_rx = cp.register_dp(dp_info).await;

    let (mut ws_sender, mut ws_receiver) = socket.split();

    loop {
        tokio::select! {
            // Incoming message from DP — 来自 DP 的消息
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Parse JSON-RPC request — 解析 JSON-RPC 请求
                        match serde_json::from_str::<JsonRpcRequest>(&text) {
                            Ok(req) => {
                                match req.method.as_str() {
                                    V2_METHOD_INIT => {
                                        let resp = build_v2_init_response(req.id);
                                        if ws_sender.send(Message::Text(
                                            String::from_utf8_lossy(&resp).into_owned().into()
                                        )).await.is_err() {
                                            break;
                                        }
                                        tracing::debug!(
                                            "V2 init completed for DP {} — V2 初始化完成 DP {}",
                                            dp_id, dp_id
                                        );
                                    }
                                    V2_METHOD_GET_DELTA => {
                                        // Return current full config as delta — 返回当前全量配置作为 delta
                                        match cp.current_payload().await {
                                            Some(payload) => {
                                                match protocol::parse_v1_payload(&payload) {
                                                    Ok(parsed) => {
                                                        let resp = build_v2_delta_response(
                                                            req.id,
                                                            &parsed.config_table,
                                                            cp.config_version(),
                                                        );
                                                        if ws_sender.send(Message::Text(
                                                            String::from_utf8_lossy(&resp).into_owned().into()
                                                        )).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        // Payload parse failed — 配置解析失败，返回 JSON-RPC 错误
                                                        tracing::error!(
                                                            "V2 get_delta parse error for DP {}: {} — V2 配置解析失败: {}",
                                                            dp_id, e, e
                                                        );
                                                        let error_resp = protocol::JsonRpcResponse {
                                                            jsonrpc: "2.0".to_string(),
                                                            result: None,
                                                            error: Some(protocol::JsonRpcError {
                                                                code: -32603,
                                                                message: format!("Internal error: failed to parse config: {}", e),
                                                                data: None,
                                                            }),
                                                            id: req.id,
                                                        };
                                                        if let Ok(resp_json) = serde_json::to_string(&error_resp) {
                                                            if ws_sender.send(Message::Text(resp_json.into())).await.is_err() {
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            None => {
                                                // No config available yet — 尚无配置，返回 JSON-RPC 错误
                                                let error_resp = protocol::JsonRpcResponse {
                                                    jsonrpc: "2.0".to_string(),
                                                    result: None,
                                                    error: Some(protocol::JsonRpcError {
                                                        code: -32002,
                                                        message: "Config not ready".to_string(),
                                                        data: None,
                                                    }),
                                                    id: req.id,
                                                };
                                                if let Ok(resp_json) = serde_json::to_string(&error_resp) {
                                                    if ws_sender.send(Message::Text(resp_json.into())).await.is_err() {
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    other => {
                                        tracing::debug!(
                                            "V2 unknown method '{}' from DP {} — V2 未知方法 '{}' 来自 DP {}",
                                            other, dp_id, other, dp_id
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                // JSON-RPC parse error (-32700) — JSON-RPC 解析错误
                                tracing::warn!(
                                    "V2 JSON-RPC parse error from DP {}: {} — V2 来自 DP {} 的 JSON-RPC 解析错误: {}",
                                    dp_id, e, dp_id, e
                                );
                                let error_resp = protocol::JsonRpcResponse {
                                    jsonrpc: "2.0".to_string(),
                                    result: None,
                                    error: Some(protocol::JsonRpcError {
                                        code: -32700,
                                        message: format!("Parse error: {}", e),
                                        data: None,
                                    }),
                                    id: 0,
                                };
                                if let Ok(resp_json) = serde_json::to_string(&error_resp) {
                                    if ws_sender.send(Message::Text(resp_json.into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        // Same as V1: update DP status — 同 V1：更新 DP 状态
                        let hash = String::from_utf8_lossy(&data).to_string();
                        cp.update_dp_status(&dp_id, &hash).await;
                        if ws_sender.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::info!("V2 DP {} disconnected — V2 DP {} 已断开", dp_id, dp_id);
                        break;
                    }
                    _ => {}
                }
            }
            // Config broadcast from CP — 来自 CP 的配置广播
            payload = config_rx.recv() => {
                match payload {
                    Ok(_data) => {
                        let notification = build_v2_notify_new_version(cp.config_version());
                        if ws_sender.send(Message::Text(
                            String::from_utf8_lossy(&notification).into_owned().into()
                        )).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "V2 DP {} lagged {} messages — V2 DP {} 落后 {} 条消息",
                            dp_id, n, dp_id, n
                        );
                        // Continue — DP will get latest config on next broadcast
                        // 继续运行 — DP 将在下次广播时获取最新配置
                    }
                    Err(_) => break, // Channel closed — 通道已关闭
                }
            }
        }
    }

    cp.unregister_dp(&dp_id).await;
}

/// DP WebSocket background service — DP WebSocket 后台服务
struct DpBgService {
    dp: Arc<kong_cluster::dp::DataPlane>,
    config: Arc<kong_config::KongConfig>,
    /// DB-less store for applying config from CP — 用于应用 CP 配置的 DB-less 存储
    dbless_store: Option<Arc<kong_db::dbless::DblessStore>>,
    /// Cache refresh signal sender — 缓存刷新信号发送端
    refresh_tx: tokio::sync::mpsc::UnboundedSender<&'static str>,
    /// TLS config for connecting to CP — 连接 CP 的 TLS 配置
    tls_config: Option<kong_cluster::tls::ClusterTlsConfig>,
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for DpBgService {
    async fn start(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Try disk cache first — 先尝试磁盘缓存
        if let Some((cached_config, cached_hash)) = self.dp.try_load_from_cache().await {
            tracing::info!(
                "Loaded config from disk cache: hash={} — 从磁盘缓存加载配置",
                cached_hash
            );
            // Calculate hashes and mark as applied so proxy has config — 计算哈希并标记已应用
            let hashes = kong_cluster::cp::calculate_config_hash(&cached_config);
            self.dp
                .mark_config_applied(&cached_config, &cached_hash, hashes)
                .await;

            // Apply cached config to live proxy — 将缓存配置应用到实时代理
            if let Some(ref store) = self.dbless_store {
                match store.load_from_json(&cached_config) {
                    Ok(()) => {
                        let _ = self.refresh_tx.send("routes");
                        let _ = self.refresh_tx.send("services");
                        let _ = self.refresh_tx.send("plugins");
                        let _ = self.refresh_tx.send("upstreams");
                        let _ = self.refresh_tx.send("certificates");
                        tracing::info!("Cached config applied to proxy — 缓存配置已应用到代理");
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to apply cached config to proxy — 应用缓存配置到代理失败: {}",
                            e
                        );
                    }
                }
            }
        }

        // Connection loop — 连接循环
        loop {
            // Check shutdown — 检查关闭信号
            if *shutdown.borrow() {
                tracing::info!("DP shutting down — DP 正在关闭");
                break;
            }

            tracing::info!(
                "Connecting to CP at {} — 正在连接 CP",
                self.dp.cp_addr()
            );

            // Try to connect with timeout — 带超时连接
            let ws_url = self.dp.ws_url_v1();
            let connect_result = if let Some(ref tls_cfg) = self.tls_config {
                // Build TLS connector for DP — 为 DP 构建 TLS 连接器
                let tls_connector = match build_dp_tls_connector(tls_cfg) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Failed to build DP TLS connector: {} — 构建 DP TLS 连接器失败: {}", e, e);
                        self.wait_reconnect(&mut shutdown).await;
                        continue;
                    }
                };
                let connector = tokio_tungstenite::Connector::NativeTls(tls_connector);
                tokio::time::timeout(
                    kong_cluster::dp::DataPlane::connect_timeout(),
                    tokio_tungstenite::connect_async_tls_with_config(
                        &ws_url, None, false, Some(connector),
                    ),
                )
                .await
            } else {
                tokio::time::timeout(
                    kong_cluster::dp::DataPlane::connect_timeout(),
                    tokio_tungstenite::connect_async(&ws_url),
                )
                .await
            };

            let ws_stream = match connect_result {
                Ok(Ok((stream, _response))) => {
                    tracing::info!("Connected to CP — 已连接到 CP");
                    *self.dp.connected.write().await = true;
                    stream
                }
                Ok(Err(e)) => {
                    tracing::warn!("Failed to connect to CP: {} — 连接 CP 失败", e);
                    self.wait_reconnect(&mut shutdown).await;
                    continue;
                }
                Err(_) => {
                    tracing::warn!("Connection to CP timed out — 连接 CP 超时");
                    self.wait_reconnect(&mut shutdown).await;
                    continue;
                }
            };

            let (mut ws_write, mut ws_read) = ws_stream.split();

            // Send basic_info — 发送基本信息
            let basic_info = match self.dp.basic_info_message() {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::error!("Failed to build basic_info: {} — 构建 basic_info 失败", e);
                    *self.dp.connected.write().await = false;
                    self.wait_reconnect(&mut shutdown).await;
                    continue;
                }
            };
            if let Err(e) = ws_write.send(basic_info).await {
                tracing::warn!("Failed to send basic_info: {} — 发送 basic_info 失败", e);
                *self.dp.connected.write().await = false;
                self.wait_reconnect(&mut shutdown).await;
                continue;
            }

            // Message loop — 消息循环
            let mut ping_interval =
                tokio::time::interval(kong_cluster::dp::DataPlane::ping_interval());
            ping_interval.tick().await; // skip first immediate tick — 跳过首次立即触发

            let disconnect_reason = loop {
                tokio::select! {
                    // Receive from CP — 接收 CP 消息
                    msg = ws_read.next() => {
                        match msg {
                            Some(Ok(Message::Binary(data))) => {
                                tracing::debug!(
                                    "Received config from CP, len={} — 收到 CP 配置",
                                    data.len()
                                );
                                match kong_cluster::protocol::parse_v1_payload(&data) {
                                    Ok(payload) => {
                                        let current = self.dp.get_current_hash().await;
                                        if current == payload.config_hash {
                                            tracing::debug!(
                                                "Config hash unchanged, skip — 配置哈希未变，跳过"
                                            );
                                        } else {
                                            tracing::info!(
                                                "Applying config: hash={} (was {}) — 应用配置",
                                                payload.config_hash, current
                                            );
                                            let hashes = payload
                                                .hashes
                                                .unwrap_or_else(|| {
                                                    kong_cluster::cp::calculate_config_hash(
                                                        &payload.config_table,
                                                    )
                                                });
                                            self.dp
                                                .mark_config_applied(
                                                    &payload.config_table,
                                                    &payload.config_hash,
                                                    hashes,
                                                )
                                                .await;

                                            // Apply config to live proxy — 将配置应用到实时代理
                                            if let Some(ref store) = self.dbless_store {
                                                match store.load_from_json(&payload.config_table) {
                                                    Ok(()) => {
                                                        // Trigger proxy refresh for all entity types — 触发所有实体类型的代理刷新
                                                        let _ = self.refresh_tx.send("routes");
                                                        let _ = self.refresh_tx.send("services");
                                                        let _ = self.refresh_tx.send("plugins");
                                                        let _ = self.refresh_tx.send("upstreams");
                                                        let _ = self.refresh_tx.send("certificates");
                                                        tracing::info!("Config applied to proxy — 配置已应用到代理");
                                                    }
                                                    Err(e) => {
                                                        tracing::error!(
                                                            "Failed to apply config to proxy, keeping current — 应用配置到代理失败，保持当前: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                            }

                                            // Send immediate PING after config apply — 配置应用后立即 PING
                                            let ping = self.dp.ping_message().await;
                                            if let Err(e) = ws_write.send(ping).await {
                                                break format!("PING send failed: {}", e);
                                            }
                                            ping_interval.reset();
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to parse config, keeping current: {} — 解析配置失败，保持当前配置",
                                            e
                                        );
                                    }
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::debug!("Received PONG from CP — 收到 CP 的 PONG");
                            }
                            Some(Ok(Message::Close(_))) | None => {
                                break "CP closed connection — CP 关闭连接".to_string();
                            }
                            Some(Err(e)) => {
                                break format!("WebSocket error: {}", e);
                            }
                            _ => {}
                        }
                    }
                    // Send PING every 30s — 每 30s 发送 PING
                    _ = ping_interval.tick() => {
                        let ping = self.dp.ping_message().await;
                        tracing::debug!("Sending PING to CP — 发送 PING 给 CP");
                        if let Err(e) = ws_write.send(ping).await {
                            break format!("PING send failed: {}", e);
                        }
                    }
                    // Shutdown signal — 关闭信号
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            break "shutdown".to_string();
                        }
                    }
                }
            };

            // Mark disconnected — 标记断开连接
            *self.dp.connected.write().await = false;
            tracing::info!("Disconnected from CP: {} — 与 CP 断开", disconnect_reason);

            if disconnect_reason == "shutdown" {
                break;
            }

            self.wait_reconnect(&mut shutdown).await;
        }
    }
}

impl DpBgService {
    /// Wait for reconnection delay or shutdown — 等待重连延迟或关闭信号
    async fn wait_reconnect(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        let delay = kong_cluster::dp::DataPlane::reconnect_delay();
        tracing::info!("Reconnecting in {:?} — {:?} 后重连", delay, delay);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {}
        }
    }
}

/// Start a TLS server with HTTP/2 support using openssl — 使用 openssl 启动支持 HTTP/2 的 TLS 服务器
async fn start_tls_server(
    addr: &str,
    cert_path: &str,
    key_path: &str,
    app: axum::Router,
) -> anyhow::Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as HttpBuilder;
    use openssl::ssl::{Ssl, SslAcceptor, SslFiletype, SslMethod};
    use tower::ServiceExt;

    // Build OpenSSL acceptor with ALPN h2 — 构建带 ALPN h2 的 OpenSSL acceptor
    let mut acceptor_builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls())?;
    acceptor_builder.set_certificate_file(cert_path, SslFiletype::PEM)?;
    acceptor_builder.set_private_key_file(key_path, SslFiletype::PEM)?;
    // Server-side ALPN selection callback — 服务端 ALPN 选择回调
    acceptor_builder.set_alpn_select_callback(|_ssl, client_protos| {
        // Prefer h2, fallback to http/1.1 — 优先 h2，回退到 http/1.1
        openssl::ssl::select_next_proto(b"\x02h2\x08http/1.1", client_protos)
            .ok_or(openssl::ssl::AlpnError::NOACK)
    });
    let acceptor = acceptor_builder.build();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Status API TLS+HTTP/2 监听于: {}", addr);

    loop {
        let (stream, _peer) = listener.accept().await?;
        let ssl = Ssl::new(acceptor.context())?;
        let mut tls_stream = tokio_openssl::SslStream::new(ssl, stream)?;
        if Pin::new(&mut tls_stream).accept().await.is_err() {
            continue;
        }
        let app = app.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(tls_stream);
            let builder = HttpBuilder::new(TokioExecutor::new());
            let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let app = app.clone();
                async move {
                    use tower::ServiceExt as _;
                    let req = req.map(axum::body::Body::new);
                    Ok::<_, std::convert::Infallible>(app.oneshot(req).await.unwrap_or_else(|e| {
                        axum::response::IntoResponse::into_response((
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Internal error: {e}"),
                        ))
                    }))
                }
            });
            let _ = builder.serve_connection_with_upgrades(io, svc).await;
        });
    }
}
