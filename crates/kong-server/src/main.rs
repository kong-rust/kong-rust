//! Kong-Rust API Gateway — 主入口

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kong-rust", version = "0.1.0")]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "/etc/kong/kong.conf")]
    conf: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Start,
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
    /// 初始化数据库（创建所有表）
    Bootstrap,
    /// 执行新的 migration
    Up,
    /// 完成 pending migration 的 teardown
    Finish,
    /// 列出所有 migration 状态
    List,
    /// 重置数据库（删除所有表）
    Reset {
        /// 跳过确认提示
        #[arg(short, long)]
        yes: bool,
    },
    /// 输出 migration 状态 JSON
    Status,
}

/// 将 Kong 日志级别映射到 tracing EnvFilter 字符串
fn kong_log_level_to_filter(level: &str) -> &'static str {
    match level {
        "debug" => "debug",
        "info" | "notice" => "info",
        "warn" => "warn",
        "error" | "crit" | "alert" | "emerg" => "error",
        _ => "info",
    }
}

/// 根据配置初始化日志系统，支持文件 + stderr 双写
fn init_logging(config: &kong_config::KongConfig) -> anyhow::Result<()> {
    let level = kong_log_level_to_filter(&config.log_level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let error_log_path = &config.proxy_error_log;

    if error_log_path == "off" {
        // 仅 stderr 输出
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        // 文件 + stderr 双写
        let log_path = Path::new(error_log_path);
        let log_dir = log_path.parent().unwrap_or(Path::new("."));
        let log_file = log_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("无效的日志路径: {}", error_log_path))?;

        // 自动创建日志目录
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

/// Pingora 不兼容 #[tokio::main]（它内部创建自己的 runtime），
/// 所以 main 是普通函数，非 start 命令手动创建 tokio runtime 执行。
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let conf_path = if cli.conf.exists() {
        Some(cli.conf.as_path())
    } else {
        None
    };

    let config = kong_config::load_config(conf_path)?;

    // 根据配置初始化日志（config 解析失败会在此之前通过默认 panic 输出）
    init_logging(&config)?;

    let config = Arc::new(config);

    match cli.command.unwrap_or(Commands::Start) {
        Commands::Version => {
            println!("kong-rust 0.1.0");
            println!("基于 Pingora 和 mlua 的 Kong API Gateway Rust 实现");
        }
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
            // 非 start 命令：手动创建 tokio runtime
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_db_command(&config, action))?;
        }
        Commands::Start => {
            start_gateway(config)?;
        }
    }

    Ok(())
}

/// 处理 db 子命令
async fn handle_db_command(
    config: &kong_config::KongConfig,
    action: DbAction,
) -> anyhow::Result<()> {
    if config.is_dbless() {
        anyhow::bail!("数据库模式为 off，migration 命令不可用");
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

/// 格式化监听地址列表
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

/// 启动网关：Pingora 管理整个应用生命周期
fn start_gateway(config: Arc<kong_config::KongConfig>) -> anyhow::Result<()> {
    tracing::info!("Kong-Rust API Gateway 启动中...");
    tracing::info!("数据库模式: {}", config.database);
    tracing::info!("路由风格: {}", config.router_flavor);

    // 阶段 1：用临时 tokio runtime 做异步初始化（DB 连接、数据加载）
    let (kong_proxy, admin_state) = {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(init_proxy_and_admin(&config))?
    };

    // 阶段 2：创建 Pingora Server
    let mut server = pingora::server::Server::new(None)?;

    // 阶段 3：创建 Proxy Service，绑定所有 proxy_listen 地址
    let mut proxy_service =
        pingora_proxy::http_proxy_service(&server.configuration, kong_proxy);
    for addr in &config.proxy_listen {
        let listen_addr = format!("{}:{}", addr.ip, addr.port);
        proxy_service.add_tcp(&listen_addr);
        tracing::info!("Proxy 监听于: {}", listen_addr);
    }

    // 阶段 4：创建 Admin API BackgroundService
    let admin_bg = AdminBgService {
        state: admin_state,
        config: Arc::clone(&config),
    };
    let admin_service =
        pingora_core::services::background::background_service("Admin API", admin_bg);

    // 阶段 5：注册服务
    server.add_service(proxy_service);
    server.add_service(admin_service);

    // 阶段 6：启动
    server.bootstrap();
    tracing::info!("Kong-Rust 启动完成");

    // 阶段 7：阻塞运行（永不返回）
    server.run_forever();
}

/// 异步初始化：连接 DB、加载数据、构建 KongProxy 和 AdminState
async fn init_proxy_and_admin(
    config: &Arc<kong_config::KongConfig>,
) -> anyhow::Result<(kong_proxy::KongProxy, kong_admin::AdminState)> {
    use kong_core::models::*;
    use kong_core::traits::{Dao, PageParams};
    use kong_db::*;

    let plugin_registry = kong_plugin_system::PluginRegistry::new();
    let node_id = uuid::Uuid::new_v4();

    if config.is_dbless() {
        // db-less 模式：空路由表，内存存储
        let store = Arc::new(DblessStore::new());

        if let Some(ref path) = config.declarative_config {
            tracing::info!("加载声明式配置: {}", path);
            store.load_from_file(path)?;
        }

        let kong_proxy =
            kong_proxy::KongProxy::new(&[], &config.router_flavor, plugin_registry);

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
        };

        Ok((kong_proxy, admin_state))
    } else {
        // PostgreSQL 模式
        let db = Database::connect(config).await?;

        // 检查 schema 状态
        let migration_state = kong_db::migrations::schema_state(db.pool()).await?;
        if migration_state.needs_bootstrap {
            anyhow::bail!("数据库未初始化，请先运行 'kong-rust db bootstrap'");
        }
        if !migration_state.new_migrations.is_empty() {
            anyhow::bail!("有新的 migration 待执行，请先运行 'kong-rust db up'");
        }

        // 从 DB 全量加载初始数据
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

        let routes_page = routes_dao.page(&all_params).await?;
        let services_page = services_dao.page(&all_params).await?;
        let upstreams_page = upstreams_dao.page(&all_params).await?;
        let targets_page = targets_dao.page(&all_params).await?;
        let plugins_page = plugins_dao.page(&all_params).await?;

        tracing::info!(
            "从数据库加载: {} routes, {} services, {} upstreams, {} targets, {} plugins",
            routes_page.data.len(),
            services_page.data.len(),
            upstreams_page.data.len(),
            targets_page.data.len(),
            plugins_page.data.len(),
        );

        // 构建 KongProxy 并填充数据
        let kong_proxy = kong_proxy::KongProxy::new(
            &routes_page.data,
            &config.router_flavor,
            plugin_registry,
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
        };

        Ok((kong_proxy, admin_state))
    }
}

/// Admin API 后台服务，由 Pingora 管理生命周期
struct AdminBgService {
    state: kong_admin::AdminState,
    config: Arc<kong_config::KongConfig>,
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

        let app = kong_admin::build_admin_router(self.state.clone());

        let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Admin API 绑定失败: {e}");
                return;
            }
        };

        // 用 tokio::select 同时等待 axum serve 和 shutdown 信号
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
