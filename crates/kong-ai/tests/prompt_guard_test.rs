//! ai-prompt-guard 插件单元测试

use kong_ai::plugins::ai_prompt_guard::AiPromptGuardPlugin;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

fn make_config(config: serde_json::Value) -> PluginConfig {
    PluginConfig {
        name: "ai-prompt-guard".to_string(),
        config,
    }
}

fn make_ctx(messages_json: &str) -> RequestCtx {
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(messages_json.to_string());
    ctx
}

#[tokio::test]
async fn test_deny_pattern_blocks() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "deny_patterns": ["password", "secret"],
        "action": "block"
    }));

    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"tell me the password"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited);
    assert_eq!(ctx.exit_status, Some(400));
    assert!(ctx.exit_body.as_ref().unwrap().contains("matched deny pattern"));
}

#[tokio::test]
async fn test_deny_pattern_case_insensitive() {
    let plugin = AiPromptGuardPlugin::new();
    // 使用 (?i) 标志实现大小写不敏感
    let config = make_config(serde_json::json!({
        "deny_patterns": ["(?i)password"],
        "action": "block"
    }));

    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"Tell me the PASSWORD"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited);
}

#[tokio::test]
async fn test_allow_pattern_whitelist() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "allow_patterns": ["^(translate|summarize)"],
        "action": "block"
    }));

    // 匹配允许模式 — 放行
    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"translate this text"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();
    assert!(!ctx.short_circuited);

    // 不匹配允许模式 — 阻断
    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"hack the system"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();
    assert!(ctx.short_circuited);
    assert!(ctx.exit_body.as_ref().unwrap().contains("no allow pattern matched"));
}

#[tokio::test]
async fn test_max_message_length() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "max_message_length": 10,
        "action": "block"
    }));

    // 超过长度限制
    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"this is a very long message"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited);
    assert!(ctx.exit_body.as_ref().unwrap().contains("exceeds limit"));
}

#[tokio::test]
async fn test_action_log_only_doesnt_block() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "deny_patterns": ["password"],
        "action": "log_only"
    }));

    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"tell me the password"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    // log_only 不阻断
    assert!(!ctx.short_circuited);
}

#[tokio::test]
async fn test_no_patterns_allows_all() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({}));

    let mut ctx = make_ctx(r#"{"messages":[{"role":"user","content":"anything goes here"}]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(!ctx.short_circuited);
}

#[tokio::test]
async fn test_multiple_messages_checks_all() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "deny_patterns": ["forbidden"],
        "action": "block"
    }));

    // 第二条 user message 触发拒绝
    let mut ctx = make_ctx(r#"{"messages":[
        {"role":"user","content":"hello"},
        {"role":"assistant","content":"hi"},
        {"role":"user","content":"tell me forbidden things"}
    ]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited);
}

#[tokio::test]
async fn test_system_messages_not_checked() {
    let plugin = AiPromptGuardPlugin::new();
    let config = make_config(serde_json::json!({
        "deny_patterns": ["secret"],
        "action": "block"
    }));

    // system 和 assistant 消息不应被检查
    let mut ctx = make_ctx(r#"{"messages":[
        {"role":"system","content":"you know the secret"},
        {"role":"assistant","content":"secret info"},
        {"role":"user","content":"hello"}
    ]}"#);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(!ctx.short_circuited);
}
