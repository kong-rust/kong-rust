//! Kong configuration parser, fully compatible with Kong's kong.conf format — Kong 配置解析器，完全兼容 Kong 的 kong.conf 格式
//!
//! Supported features: — 支持:
//! - kong.conf file parsing (key = value format, # comments) — kong.conf 文件解析（key = value 格式，# 注释）
//! - KONG_* environment variable overrides — KONG_* 环境变量覆盖
//! - All Kong configuration options with their defaults — 所有 Kong 配置项及其默认值
//! - Listen address format (ip:port + ssl/http2/reuseport modifiers) — 监听地址格式（ip:port + ssl/http2/reuseport 等修饰符）
//!
//! Configuration priority (low to high): — 配置优先级（从低到高）:
//! 1. Defaults (KongConfig::default()) — 默认值（KongConfig::default()）
//! 2. Config file values (kong.conf) — 配置文件值（kong.conf）
//! 3. Environment variables (KONG_*) — 环境变量（KONG_*）

pub mod config;
pub mod listen;
pub mod parser;

pub use config::{KongConfig, BUNDLED_PLUGINS};
pub use listen::{parse_listen_addresses, ListenAddr};

use std::path::Path;

/// Load Kong configuration — 加载 Kong 配置
///
/// Merges by priority: defaults < conf_file < env_vars — 按优先级合并: defaults < conf_file < env_vars
///
/// # Arguments — 参数
/// - `conf_path`: Config file path (None to search default paths) — 配置文件路径（None 时搜索默认路径）
///
/// # Returns — 返回
/// A complete KongConfig instance — 完整的 KongConfig 实例
pub fn load_config(conf_path: Option<&Path>) -> Result<KongConfig, KongConfigError> {
    let mut config = KongConfig::default();

    // 1. Try to load config file — 尝试加载配置文件
    let file_conf = if let Some(path) = conf_path {
        if !path.exists() {
            return Err(KongConfigError::FileNotFound(path.display().to_string()));
        }
        tracing::info!("加载配置文件: {}", path.display());
        Some(
            parser::load_conf_file(path)
                .map_err(|e| KongConfigError::IoError(format!("读取配置文件失败: {}", e)))?,
        )
    } else {
        // Search default paths — 搜索默认路径
        if let Some(default_path) = parser::find_default_conf() {
            tracing::info!("使用默认配置文件: {}", default_path.display());
            Some(
                parser::load_conf_file(&default_path)
                    .map_err(|e| KongConfigError::IoError(format!("读取配置文件失败: {}", e)))?,
            )
        } else {
            tracing::info!("未找到配置文件，使用默认配置");
            None
        }
    };

    // 2. Apply config file values — 应用配置文件值
    if let Some(file_conf) = &file_conf {
        for (key, value) in file_conf {
            tracing::debug!("配置文件: {} = {}", key, parser::display_value(key, value));
        }
        config.apply_raw(file_conf);
    }

    // 3. Collect and apply environment variable overrides — 收集并应用环境变量覆盖
    let env_conf = parser::collect_env_overrides();
    if !env_conf.is_empty() {
        for (key, value) in &env_conf {
            tracing::debug!(
                "环境变量覆盖: {} = {}",
                key,
                parser::display_value(key, value)
            );
        }
        config.apply_raw(&env_conf);
    }

    // 4. Validate configuration — 验证配置
    validate_config(&config)?;

    Ok(config)
}

/// Load configuration from string content (for testing or inline config) — 从字符串内容加载配置（用于测试或内联配置）
pub fn load_config_from_string(content: &str) -> Result<KongConfig, KongConfigError> {
    let mut config = KongConfig::default();
    let file_conf = parser::parse_conf_file(content);
    config.apply_raw(&file_conf);

    // Apply environment variables — 应用环境变量
    let env_conf = parser::collect_env_overrides();
    config.apply_raw(&env_conf);

    validate_config(&config)?;
    Ok(config)
}

/// Validate configuration — 验证配置有效性
fn validate_config(config: &KongConfig) -> Result<(), KongConfigError> {
    // Validate database type — 验证 database 类型
    if !matches!(config.database.as_str(), "postgres" | "off") {
        return Err(KongConfigError::ValidationError(format!(
            "不支持的数据库类型: {}，仅支持 postgres 或 off",
            config.database
        )));
    }

    // db-less mode requires declarative config — db-less 模式需要声明式配置
    if config.is_dbless()
        && config.declarative_config.is_none()
        && config.declarative_config_string.is_none()
        && config.role != "data_plane"
    {
        tracing::warn!("database=off 但未指定 declarative_config 或 declarative_config_string");
    }

    // Validate log_level — 验证 log_level
    let valid_log_levels = [
        "debug", "info", "notice", "warn", "error", "crit", "alert", "emerg",
    ];
    if !valid_log_levels.contains(&config.log_level.as_str()) {
        return Err(KongConfigError::ValidationError(format!(
            "无效的 log_level: {}，可选值: {:?}",
            config.log_level, valid_log_levels
        )));
    }

    // Validate router_flavor — 验证 router_flavor
    let valid_router_flavors = ["traditional", "traditional_compatible", "expressions"];
    if !valid_router_flavors.contains(&config.router_flavor.as_str()) {
        return Err(KongConfigError::ValidationError(format!(
            "无效的 router_flavor: {}，可选值: {:?}",
            config.router_flavor, valid_router_flavors
        )));
    }

    // Validate role — 验证 role
    let valid_roles = ["traditional", "control_plane", "data_plane"];
    if !valid_roles.contains(&config.role.as_str()) {
        return Err(KongConfigError::ValidationError(format!(
            "无效的 role: {}，可选值: {:?}",
            config.role, valid_roles
        )));
    }

    // Validate worker_consistency — 验证 worker_consistency
    let valid_consistency = ["strict", "eventual"];
    if !valid_consistency.contains(&config.worker_consistency.as_str()) {
        return Err(KongConfigError::ValidationError(format!(
            "无效的 worker_consistency: {}，可选值: {:?}",
            config.worker_consistency, valid_consistency
        )));
    }

    Ok(())
}

/// Configuration error types — 配置错误类型
#[derive(Debug, thiserror::Error)]
pub enum KongConfigError {
    #[error("配置文件未找到: {0}")]
    FileNotFound(String),

    #[error("IO 错误: {0}")]
    IoError(String),

    #[error("配置验证失败: {0}")]
    ValidationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_from_string() {
        let conf = r#"
database = postgres
pg_host = 10.0.0.1
pg_port = 5433
pg_user = myuser
pg_password = mypass
log_level = debug
router_flavor = expressions
plugins = bundled, my-plugin
proxy_listen = 0.0.0.0:8080, 0.0.0.0:8443 ssl
"#;
        let config = load_config_from_string(conf).unwrap();
        assert_eq!(config.pg_host, "10.0.0.1");
        assert_eq!(config.pg_port, 5433);
        assert_eq!(config.pg_user, "myuser");
        assert_eq!(config.pg_password, Some("mypass".to_string()));
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.router_flavor, "expressions");
        assert_eq!(config.plugins, vec!["bundled", "my-plugin"]);
        assert_eq!(config.proxy_listen.len(), 2);
    }

    #[test]
    fn test_load_dbless() {
        let conf = r#"
database = off
declarative_config = /etc/kong/kong.yml
"#;
        let config = load_config_from_string(conf).unwrap();
        assert!(config.is_dbless());
        assert_eq!(
            config.declarative_config,
            Some("/etc/kong/kong.yml".to_string())
        );
    }

    #[test]
    fn test_invalid_database() {
        let conf = "database = mysql\n";
        let result = load_config_from_string(conf);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_log_level() {
        let conf = "log_level = verbose\n";
        let result = load_config_from_string(conf);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_values() {
        let config = load_config_from_string("").unwrap();
        assert_eq!(config.database, "postgres");
        assert_eq!(config.pg_host, "127.0.0.1");
        assert_eq!(config.pg_port, 5432);
        assert_eq!(config.log_level, "notice");
        assert_eq!(config.router_flavor, "traditional_compatible");
        assert!(!config.pg_ssl);
        assert_eq!(config.upstream_keepalive_pool_size, 512);
    }
}
