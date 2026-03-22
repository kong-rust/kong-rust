//! ai-cache 插件单元测试

use kong_ai::plugins::ai_cache::extract_cache_key;

#[test]
fn test_cache_key_last_question() {
    let body = r#"{
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi there"},
            {"role": "user", "content": "what is rust?"}
        ]
    }"#;

    let key = extract_cache_key(body, "last_question");
    assert!(key.is_some());

    // 相同问题应产生相同 key（确定性）
    let key2 = extract_cache_key(body, "last_question");
    assert_eq!(key, key2);

    // key 应为 64 字符的十六进制字符串（SHA256）
    let key_val = key.unwrap();
    assert_eq!(key_val.len(), 64);
    assert!(key_val.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_cache_key_all_questions() {
    let body = r#"{
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"},
            {"role": "user", "content": "world"}
        ]
    }"#;

    let key = extract_cache_key(body, "all_questions");
    assert!(key.is_some());

    // key 应为 64 字符的十六进制字符串（SHA256）
    let key_val = key.unwrap();
    assert_eq!(key_val.len(), 64);

    // last_question 和 all_questions 应产生不同的 key
    let last_key = extract_cache_key(body, "last_question").unwrap();
    assert_ne!(key_val, last_key);
}

#[test]
fn test_cache_key_no_user_messages() {
    let body = r#"{
        "messages": [
            {"role": "system", "content": "you are helpful"},
            {"role": "assistant", "content": "hello"}
        ]
    }"#;

    // last_question: 没有 user message
    let key = extract_cache_key(body, "last_question");
    assert!(key.is_none());

    // all_questions: 没有 user message
    let key = extract_cache_key(body, "all_questions");
    assert!(key.is_none());
}

#[test]
fn test_cache_key_invalid_body() {
    let key = extract_cache_key("not json", "last_question");
    assert!(key.is_none());

    let key = extract_cache_key("{}", "last_question");
    assert!(key.is_none());
}

#[test]
fn test_cache_key_unknown_strategy() {
    let body = r#"{
        "messages": [
            {"role": "user", "content": "hello"}
        ]
    }"#;

    let key = extract_cache_key(body, "unknown");
    assert!(key.is_none());
}

#[tokio::test]
async fn test_cache_skip_header() {
    use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
    use kong_ai::plugins::ai_cache::{AiCacheContext, AiCachePlugin};

    let plugin = AiCachePlugin::new();
    let config = PluginConfig {
        name: "ai-cache".to_string(),
        config: serde_json::json!({
            "skip_header": "X-AI-Skip-Cache"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_headers.insert(
        "x-ai-skip-cache".to_string(),
        "true".to_string(),
    );
    ctx.request_body = Some(r#"{"messages":[{"role":"user","content":"hello"}]}"#.to_string());

    plugin.access(&config, &mut ctx).await.unwrap();

    let cache_ctx = ctx.extensions.get::<AiCacheContext>().unwrap();
    assert!(cache_ctx.cache_key.is_none());
    assert!(!cache_ctx.cache_hit);
}

#[tokio::test]
async fn test_cache_normal_access() {
    use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
    use kong_ai::plugins::ai_cache::{AiCacheContext, AiCachePlugin};

    let plugin = AiCachePlugin::new();
    let config = PluginConfig {
        name: "ai-cache".to_string(),
        config: serde_json::json!({
            "cache_key_strategy": "last_question"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(r#"{"messages":[{"role":"user","content":"hello world"}]}"#.to_string());

    plugin.access(&config, &mut ctx).await.unwrap();

    let cache_ctx = ctx.extensions.get::<AiCacheContext>().unwrap();
    assert!(cache_ctx.cache_key.is_some());
    assert!(!cache_ctx.cache_hit);
}
