//! Kong-Rust API Gateway — main entry point — Kong-Rust API Gateway — 主入口

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kong-rust", version = "0.1.0")]
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
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    Config,
    Version,
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

/// Initialize the logging system based on config, supports file + stderr dual output — 根据配置初始化日志系统，支持文件 + stderr 双写
fn init_logging(config: &kong_config::KongConfig) -> anyhow::Result<()> {
    let level = kong_log_level_to_filter(&config.log_level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let error_log_path = &config.proxy_error_log;

    if error_log_path == "off" {
        // stderr output only — 仅 stderr 输出
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        // File + stderr dual output — 文件 + stderr 双写
        let log_path = Path::new(error_log_path);
        let log_dir = log_path.parent().unwrap_or(Path::new("."));
        let log_file = log_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid log path: {} — 无效的日志路径: {}", error_log_path, error_log_path))?;

        // Auto-create log directory — 自动创建日志目录
        std::fs::create_dir_all(log_dir)?;

        let file_appender = tracing_appender::rolling::never(log_dir, log_file);

        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender);

        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
    }

    Ok(())
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
            println!("kong-rust 0.1.0");
            println!("基于 Pingora 和 mlua 的 Kong API Gateway Rust 实现");
            return Ok(());
        }
        Some(Commands::Health) => {
            health_check(&config)?;
            return Ok(());
        }
        _ => {}
    }

    // Initialize logging based on config (config parse failures output via default panic before this) — 根据配置初始化日志（config 解析失败会在此之前通过默认 panic 输出）
    init_logging(&config)?;

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
            println!("PostgreSQL: {}:{}/{}", config.pg_host, config.pg_port, config.pg_database);
            println!("路由风格: {}", config.router_flavor);
            println!("Admin 监听: {}", format_listen_addrs(&config.admin_listen));
            println!("Proxy 监听: {}", format_listen_addrs(&config.proxy_listen));
            println!("已加载插件: {:?}", config.loaded_plugins());
        }
        Commands::Db { action } => {
            // Non-start commands: manually create tokio runtime — 非 start 命令：手动创建 tokio runtime
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_db_command(&config, action))?;
        }
        Commands::Start | Commands::DockerStart => {
            start_gateway(config)?;
        }
        // Already handled above — 已在上方处理
        Commands::Version | Commands::Health => unreachable!(),
    }

    Ok(())
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
        let ip = if a.ip == "0.0.0.0" { "127.0.0.1" } else { &a.ip };
        format!("{}:{}", ip, a.port)
    } else {
        "127.0.0.1:8001".to_string()
    };

    let timeout = std::time::Duration::from_secs(5);
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse()?, timeout)
        .map_err(|e| anyhow::anyhow!("Failed to connect to Admin API at {}: {} — 无法连接 Admin API {}: {}", addr, e, addr, e))?;

    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let request = format!("GET /status HTTP/1.0\r\nHost: {}\r\n\r\n", addr);
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    if response.contains("200") {
        println!("kong-rust is healthy at {} — kong-rust 健康检查通过 {}", addr, addr);
        Ok(())
    } else {
        anyhow::bail!("kong-rust is NOT healthy: unexpected response from {} — kong-rust 健康检查失败: {} 返回异常响应", addr, addr);
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
fn start_gateway(config: Arc<kong_config::KongConfig>) -> anyhow::Result<()> {
    tracing::info!("Kong-Rust API Gateway 启动中...");
    tracing::info!("数据库模式: {}", config.database);
    tracing::info!("路由风格: {}", config.router_flavor);

    // Phase 1: Use tokio runtime for async initialization (DB connection, data loading) — 阶段 1：用 tokio runtime 做异步初始化（DB 连接、数据加载）
    // Note: rt must survive until run_forever(), otherwise the sqlx connection pool background tasks — 注意：rt 必须存活到 run_forever()，否则 sqlx 连接池后台任务会因 runtime drop 而终止，
    // will terminate when runtime is dropped, requiring connection rebuilds and causing slow startup responses. — 导致首次 DB 查询需要重建连接，造成启动后响应缓慢。
    let rt = tokio::runtime::Runtime::new()?;
    let (mut kong_proxy, mut admin_state, refresh_rx) =
        rt.block_on(init_proxy_and_admin(&config))?;

    // Initialize access log async writer (must be created inside tokio runtime since it needs spawn) — 初始化 access log 异步写入器（必须在 tokio runtime 内创建，因为需要 spawn）
    let access_log_writer = rt.block_on(async {
        kong_proxy::access_log::AccessLogWriter::new(&config.proxy_access_log)
    });
    kong_proxy.access_log_writer = access_log_writer.clone();

    // Phase 2: Create Pingora Server — 阶段 2：创建 Pingora Server
    let mut server = pingora::server::Server::new(None)?;

    // Phase 3: Create Proxy Service, bind all proxy_listen addresses — 阶段 3：创建 Proxy Service，绑定所有 proxy_listen 地址
    let mut proxy_service =
        pingora_proxy::http_proxy_service(&server.configuration, kong_proxy.clone());
    for addr in &config.proxy_listen {
        let listen_addr = format!("{}:{}", addr.ip, addr.port);
        if addr.ssl {
            // SSL port: register with add_tls, Pingora handles TLS termination — SSL 端口：使用 add_tls 注册，Pingora 负责 TLS 终止
            if let (Some(cert), Some(key)) = (config.ssl_cert.first(), config.ssl_cert_key.first())
            {
                match proxy_service.add_tls(&listen_addr, cert, key) {
                    Ok(_) => tracing::info!("Proxy 监听于: {} (TLS)", listen_addr),
                    Err(e) => {
                        tracing::error!("Proxy TLS 监听失败 {}: {}", listen_addr, e);
                        // Fallback to TCP — 回退到 TCP
                        proxy_service.add_tcp(&listen_addr);
                        tracing::warn!("Proxy 回退为 TCP 监听: {}", listen_addr);
                    }
                }
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
            let params = PageParams { size: 10000, offset: None, tags: None };
            match admin_state.routes.page(&params).await {
                Ok(page) => page.data,
                Err(e) => {
                    tracing::error!("Failed to load Stream routes: {} — 加载 Stream 路由失败: {}", e, e);
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

    // Phase 6: Bootstrap — 阶段 6：启动
    server.bootstrap();
    tracing::info!("Kong-Rust 启动完成");

    // Phase 7: Block and run forever — 阶段 7：阻塞运行（永不返回）
    server.run_forever();
}

/// Async initialization: connect DB, load data, build KongProxy and AdminState — 异步初始化：连接 DB、加载数据、构建 KongProxy 和 AdminState
async fn init_proxy_and_admin(
    config: &Arc<kong_config::KongConfig>,
) -> anyhow::Result<(
    kong_proxy::KongProxy,
    kong_admin::AdminState,
    tokio::sync::mpsc::UnboundedReceiver<&'static str>,
)> {
    use kong_core::models::*;
    use kong_core::traits::{Dao, PageParams};
    use kong_db::*;

    let plugin_registry = kong_plugin_system::PluginRegistry::new();
    let node_id = uuid::Uuid::new_v4();
    let (refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel();

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
            node_id,
            config: Arc::clone(config),
            proxy: kong_proxy.clone(),
            refresh_tx,
            stream_router: None, // Set as needed in start_gateway — start_gateway 中按需设置
        };

        Ok((kong_proxy, admin_state, refresh_rx))
    } else {
        // PostgreSQL mode — PostgreSQL 模式
        let db = Database::connect(config).await?;

        // Check schema state — 检查 schema 状态
        let migration_state = kong_db::migrations::schema_state(db.pool()).await?;
        if migration_state.needs_bootstrap {
            anyhow::bail!("Database not initialized, please run 'kong-rust db bootstrap' first — 数据库未初始化，请先运行 'kong-rust db bootstrap'");
        }
        if !migration_state.new_migrations.is_empty() {
            anyhow::bail!("New migrations pending, please run 'kong-rust db up' first — 有新的 migration 待执行，请先运行 'kong-rust db up'");
        }

        // Full data load from DB — 从 DB 全量加载初始数据
        let all_params = PageParams {
            size: 1000,
            offset: None,
            tags: None,
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
            ca_certificates: Arc::new(PgDao::<CaCertificate>::new(db.clone(), ca_certificate_schema())),
            vaults: Arc::new(PgDao::<Vault>::new(db.clone(), vault_schema())),
            node_id,
            config: Arc::clone(config),
            proxy: kong_proxy.clone(),
            refresh_tx,
            stream_router: None, // Set as needed in start_gateway — start_gateway 中按需设置
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

        // Start cache refresh debounce background task — 启动缓存刷新防抖后台任务
        if let Some(rx) = self.refresh_rx.lock().unwrap().take() {
            let state = self.state.clone();
            tokio::spawn(kong_admin::run_cache_refresher(rx, state));
            tracing::info!("缓存刷新防抖任务已启动（100ms 窗口合并）");
        }

        let app = kong_admin::build_admin_router(self.state.clone());

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
