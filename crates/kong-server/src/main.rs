//! Kong-Rust API Gateway — 主入口

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
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
    Bootstrap,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let conf_path = if cli.conf.exists() {
        Some(cli.conf.as_path())
    } else {
        None
    };

    let config = kong_config::load_config(conf_path)?;
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
        Commands::Db { action } => match action {
            DbAction::Bootstrap => {
                tracing::info!("执行数据库迁移...");
                let _db = kong_db::Database::connect(&config).await?;
                tracing::info!("数据库迁移完成");
            }
        },
        Commands::Start => {
            start_gateway(config).await?;
        }
    }

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

async fn start_gateway(config: Arc<kong_config::KongConfig>) -> anyhow::Result<()> {
    tracing::info!("Kong-Rust API Gateway 启动中...");
    tracing::info!("数据库模式: {}", config.database);
    tracing::info!("路由风格: {}", config.router_flavor);

    let admin_config = Arc::clone(&config);
    let admin_handle = tokio::spawn(async move {
        start_admin_api(admin_config).await
    });

    tracing::info!(
        "Admin API 监听于: {}",
        format_listen_addrs(&config.admin_listen)
    );
    tracing::info!(
        "Proxy 监听于: {}",
        format_listen_addrs(&config.proxy_listen)
    );
    tracing::info!("Kong-Rust 启动完成");

    // 等待 Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("收到关闭信号，正在优雅关闭...");

    admin_handle.abort();

    tracing::info!("Kong-Rust 已关闭");
    Ok(())
}

async fn start_admin_api(config: Arc<kong_config::KongConfig>) -> anyhow::Result<()> {
    use kong_core::models::*;
    use kong_db::*;

    let bind_addr = if let Some(addr) = config.admin_listen.first() {
        format!("{}:{}", addr.ip, addr.port)
    } else {
        "0.0.0.0:8001".to_string()
    };
    tracing::info!("Admin API 绑定到: {}", bind_addr);

    let state = if config.is_dbless() {
        // db-less 模式：内存存储
        let store = Arc::new(DblessStore::new());

        // 如果配置了声明式配置文件，加载它
        if let Some(ref path) = config.declarative_config {
            tracing::info!("加载声明式配置: {}", path);
            store.load_from_file(path)?;
        }

        kong_admin::AdminState {
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
            node_id: uuid::Uuid::new_v4(),
            config: Arc::clone(&config),
        }
    } else {
        // PostgreSQL 模式
        let db = Database::connect(&config).await?;
        kong_admin::AdminState {
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
            node_id: uuid::Uuid::new_v4(),
            config: Arc::clone(&config),
        }
    };

    let app = kong_admin::build_admin_router(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
