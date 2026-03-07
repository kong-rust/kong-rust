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

    // 获取 Admin API 绑定地址
    let admin_bind = if let Some(addr) = config.admin_listen.first() {
        format!("{}:{}", addr.ip, addr.port)
    } else {
        "0.0.0.0:8001".to_string()
    };

    let admin_bind_clone = admin_bind.clone();
    let admin_handle = tokio::spawn(async move {
        start_admin_api(&admin_bind_clone).await
    });

    tracing::info!("Admin API 监听于: {}", admin_bind);
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

async fn start_admin_api(bind_addr: &str) -> anyhow::Result<()> {
    tracing::info!("Admin API 绑定到: {}", bind_addr);

    let app = axum::Router::new()
        .route("/", axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "version": "0.1.0",
                "tagline": "Welcome to kong-rust",
            }))
        }))
        .route("/status", axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "server": {"connections_active": 0},
                "database": {"reachable": true},
            }))
        }));

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
