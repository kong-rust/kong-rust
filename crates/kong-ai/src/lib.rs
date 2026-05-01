//! Kong AI Gateway — AI 网关核心 crate
//!
//! 包含 AI Provider 抽象、协议编解码、Token 计数、限流器和 4 个 AI 插件

pub mod models;
pub mod dao;
pub mod provider;
pub mod codec;
pub mod token;
pub mod ratelimit;
pub mod embedding;
pub mod plugins;

/// 解析插件配置 JSON — 统一错误处理
pub fn parse_plugin_config<T: serde::de::DeserializeOwned>(
    config: &kong_core::traits::PluginConfig,
) -> kong_core::error::Result<T> {
    serde_json::from_value(config.config.clone()).map_err(|e| {
        kong_core::error::KongError::PluginError {
            plugin_name: config.name.clone(),
            message: format!("invalid plugin config: {}", e),
        }
    })
}
