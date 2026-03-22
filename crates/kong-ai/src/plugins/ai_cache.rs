//! ai-cache 插件 — 语义缓存键提取 + skip header（Redis 集成后续实现）

use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

// ============ 插件配置 ============

/// ai-cache 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiCacheConfig {
    /// 缓存 TTL（秒）
    pub cache_ttl: u64,
    /// 缓存键策略："last_question" | "all_questions"
    pub cache_key_strategy: String,
    /// 跳过缓存的请求头名称
    pub skip_header: String,
}

impl Default for AiCacheConfig {
    fn default() -> Self {
        Self {
            cache_ttl: 300,
            cache_key_strategy: "last_question".to_string(),
            skip_header: "X-AI-Skip-Cache".to_string(),
        }
    }
}

/// 存储在 ctx.extensions 中的缓存上下文
pub struct AiCacheContext {
    /// 缓存键（SHA256 哈希）
    pub cache_key: Option<String>,
    /// 是否命中缓存
    pub cache_hit: bool,
}

// ============ 插件结构体 ============

/// AI 缓存插件
pub struct AiCachePlugin;

impl AiCachePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AiCachePlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiCachePlugin {
    fn name(&self) -> &str {
        "ai-cache"
    }

    fn priority(&self) -> i32 {
        // 高于 ai-rate-limit (771)，先检查缓存
        772
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiCacheConfig = crate::parse_plugin_config(config)?;

        // 1. 检查 skip header
        if ctx
            .request_headers
            .get(&cfg.skip_header.to_lowercase())
            .is_some()
        {
            ctx.extensions.insert(AiCacheContext {
                cache_key: None,
                cache_hit: false,
            });
            return Ok(());
        }

        // 2. 从请求体提取缓存键
        let cache_key = if let Some(body) = &ctx.request_body {
            extract_cache_key(body, &cfg.cache_key_strategy)
        } else {
            None
        };

        // 3. MVP 阶段：仅设置缓存键基础设施，实际 Redis 查找后续实现
        ctx.extensions.insert(AiCacheContext {
            cache_key,
            cache_hit: false,
        });

        Ok(())
    }

    async fn log(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        // 缓存回写将在 Redis 集成后实现
        Ok(())
    }
}

/// 从请求体提取缓存键（SHA256 哈希）
pub fn extract_cache_key(body: &str, strategy: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let messages = parsed.get("messages")?.as_array()?;

    match strategy {
        "last_question" => {
            // 最后一条 user message 的 content
            messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(|s| format!("{:x}", Sha256::digest(s.as_bytes())))
        }
        "all_questions" => {
            // 所有 user messages 拼接
            let all: String = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if all.is_empty() {
                None
            } else {
                Some(format!("{:x}", Sha256::digest(all.as_bytes())))
            }
        }
        _ => None,
    }
}
