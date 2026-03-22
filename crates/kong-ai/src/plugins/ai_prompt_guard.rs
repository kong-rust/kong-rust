//! ai-prompt-guard 插件 — 正则 deny/allow 模式匹配 + 消息长度限制

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use tracing::warn;

use kong_core::error::{KongError, Result};
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

// ============ 插件配置 ============

/// ai-prompt-guard 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiPromptGuardConfig {
    /// 拒绝模式列表（正则表达式）
    pub deny_patterns: Vec<String>,
    /// 允许模式列表（正则表达式，白名单模式）
    pub allow_patterns: Vec<String>,
    /// 最大消息长度（字节）
    pub max_message_length: usize,
    /// 触发行为："block" | "log_only"
    pub action: String,
    /// 拒绝错误码
    pub error_code: u16,
    /// 拒绝错误消息
    pub error_message: String,
}

impl Default for AiPromptGuardConfig {
    fn default() -> Self {
        Self {
            deny_patterns: Vec::new(),
            allow_patterns: Vec::new(),
            max_message_length: 32768,
            action: "block".to_string(),
            error_code: 400,
            error_message: "request blocked by ai-prompt-guard".to_string(),
        }
    }
}

// ============ 插件结构体 ============

/// AI 提示词防护插件
pub struct AiPromptGuardPlugin;

impl AiPromptGuardPlugin {
    pub fn new() -> Self {
        Self
    }

    /// 处理违规：根据 action 决定阻断或仅记录
    fn handle_violation(
        &self,
        ctx: &mut RequestCtx,
        cfg: &AiPromptGuardConfig,
        reason: &str,
    ) -> Result<()> {
        if cfg.action == "block" {
            ctx.short_circuited = true;
            ctx.exit_status = Some(cfg.error_code);
            ctx.exit_body = Some(
                serde_json::json!({
                    "message": cfg.error_message,
                    "reason": reason
                })
                .to_string(),
            );
        } else {
            // log_only：记录但不阻断
            warn!("ai-prompt-guard violation (log_only): {}", reason);
        }
        Ok(())
    }
}

impl Default for AiPromptGuardPlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiPromptGuardPlugin {
    fn name(&self) -> &str {
        "ai-prompt-guard"
    }

    fn priority(&self) -> i32 {
        // 高于 ai-cache (772)，先执行安全检查
        773
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiPromptGuardConfig = crate::parse_plugin_config(config)?;

        // 解析请求体
        let body = ctx.request_body.as_ref().ok_or_else(|| KongError::PluginError {
            plugin_name: "ai-prompt-guard".to_string(),
            message: "missing request body".to_string(),
        })?;

        let parsed: serde_json::Value =
            serde_json::from_str(body).map_err(|e| KongError::PluginError {
                plugin_name: "ai-prompt-guard".to_string(),
                message: format!("invalid request body: {}", e),
            })?;

        // 提取 messages 数组
        if let Some(messages) = parsed.get("messages").and_then(|m| m.as_array()) {
            // 编译正则（生产环境应缓存，MVP 先每次编译）
            let deny_regexes: Vec<Regex> = cfg
                .deny_patterns
                .iter()
                .filter_map(|p| Regex::new(p).ok())
                .collect();
            let allow_regexes: Vec<Regex> = cfg
                .allow_patterns
                .iter()
                .filter_map(|p| Regex::new(p).ok())
                .collect();

            for msg in messages {
                // 只检查 user 角色的消息
                if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                    continue;
                }
                let content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");

                // 长度检查
                if content.len() > cfg.max_message_length {
                    return self.handle_violation(
                        ctx,
                        &cfg,
                        &format!(
                            "message length {} exceeds limit {}",
                            content.len(),
                            cfg.max_message_length
                        ),
                    );
                }

                // 拒绝模式匹配
                for regex in &deny_regexes {
                    if regex.is_match(content) {
                        return self.handle_violation(
                            ctx,
                            &cfg,
                            &format!("matched deny pattern: {}", regex.as_str()),
                        );
                    }
                }

                // 允许模式（白名单）：如果配置了 allow_patterns，必须至少匹配一个
                if !allow_regexes.is_empty() {
                    let any_match = allow_regexes.iter().any(|r| r.is_match(content));
                    if !any_match {
                        return self.handle_violation(ctx, &cfg, "no allow pattern matched");
                    }
                }
            }
        }

        Ok(())
    }
}
