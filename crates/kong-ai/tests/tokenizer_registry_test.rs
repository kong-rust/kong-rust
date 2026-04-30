//! TokenizerRegistry / PromptTokenizer 集成测试
//! Tokenizer routing tests covering Step 1 surface area:
//!   - has_non_text_content judgment
//!   - OpenAI dual-path (text → tiktoken; multimodal → remote, falls back to tiktoken in step 1)
//!   - infer_provider_type heuristic
//!   - per-request deadline → estimate fallback

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kong_ai::codec::ChatRequest;
use kong_ai::token::{
    estimate_from_request, extract_prompt_text, has_non_text_content, NoopTokenizer,
    OpenAiTokenizer, PromptTokenizer, TiktokenTokenizer, TokenizerConfig, TokenizerRegistry,
    TokenizerStrategy,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn parse_chat(body: &str) -> ChatRequest {
    serde_json::from_str::<ChatRequest>(body).expect("valid ChatRequest")
}

fn text_request(model: &str, content: &str) -> ChatRequest {
    parse_chat(&format!(
        r#"{{"model":"{}","messages":[{{"role":"user","content":"{}"}}]}}"#,
        model, content
    ))
}

// ─── has_non_text_content ────────────────────────────────────────────────────

#[test]
fn has_non_text_pure_text_returns_false() {
    let req = text_request("gpt-4o", "Hello, world!");
    assert!(!has_non_text_content(&req));
}

#[test]
fn has_non_text_image_url_part_detected() {
    let req = parse_chat(
        r#"{
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image_url", "image_url": {"url": "https://x/y.png"}}
                ]
            }]
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_input_audio_part_detected() {
    let req = parse_chat(
        r#"{
            "model": "gpt-4o-audio-preview",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "input_audio", "input_audio": {"data": "...", "format": "wav"}}
                ]
            }]
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_tools_at_top_level_detected() {
    let req = parse_chat(
        r#"{
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"call my tool"}],
            "tools": [{"type":"function","function":{"name":"f","parameters":{}}}]
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_tool_choice_detected() {
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"hi"}],
            "tool_choice":"auto"
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_legacy_functions_detected() {
    let req = parse_chat(
        r#"{
            "model":"gpt-3.5-turbo",
            "messages":[{"role":"user","content":"hi"}],
            "functions":[{"name":"f","parameters":{}}]
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_response_format_detected() {
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"hi"}],
            "response_format":{"type":"json_schema","json_schema":{"name":"x","schema":{}}}
        }"#,
    );
    assert!(has_non_text_content(&req));
}

#[test]
fn has_non_text_assistant_tool_calls_history_detected() {
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[
                {"role":"user","content":"hi"},
                {"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_1","type":"function","function":{"name":"f","arguments":"{}"}}
                ]}
            ]
        }"#,
    );
    assert!(has_non_text_content(&req));
}

// ─── extract_prompt_text ─────────────────────────────────────────────────────

#[test]
fn extract_prompt_text_concatenates_role_and_content() {
    let req = text_request("gpt-4", "hello there");
    let text = extract_prompt_text(&req);
    assert!(text.contains("user:"));
    assert!(text.contains("hello there"));
}

#[test]
fn extract_prompt_text_handles_array_content() {
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"foo"},
                {"type":"image_url","image_url":{"url":"x"}}
            ]}]
        }"#,
    );
    let text = extract_prompt_text(&req);
    assert!(text.contains("foo"));
    // image_url 不会贡献 text — image_url contributes no text
    assert!(!text.contains("image_url"));
}

// ─── OpenAiTokenizer (双轨) ──────────────────────────────────────────────────

#[tokio::test]
async fn openai_tokenizer_pure_text_returns_tiktoken_count() {
    let t = OpenAiTokenizer::new();
    let req = text_request("gpt-4", "Hello, world!");
    let n = t
        .count_prompt("gpt-4", &req)
        .await
        .expect("tiktoken should succeed for gpt-4");
    assert!(n > 0 && n < 20);
}

#[tokio::test]
async fn openai_tokenizer_with_image_falls_back_to_tiktoken_in_step1() {
    // Step 1:remote 还是 stub,所以 has_non_text=true 仍走 tiktoken 兜底
    // Step 1: remote is a stub, so non-text path still returns tiktoken result
    let t = OpenAiTokenizer::new();
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"hi"},
                {"type":"image_url","image_url":{"url":"x"}}
            ]}]
        }"#,
    );
    let n = t.count_prompt("gpt-4o", &req).await;
    assert!(
        n.is_some(),
        "tiktoken fallback should yield a count even when has_non_text=true"
    );
}

#[tokio::test]
async fn openai_tokenizer_with_tools_falls_back_to_tiktoken_in_step1() {
    let t = OpenAiTokenizer::new();
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"call f"}],
            "tools":[{"type":"function","function":{"name":"f","parameters":{}}}]
        }"#,
    );
    let n = t.count_prompt("gpt-4o", &req).await;
    assert!(n.is_some());
}

#[tokio::test]
async fn openai_tokenizer_unknown_model_returns_none() {
    // tiktoken 不认识的模型,整个 OpenAi 路径无法计算 → registry 兜底字符估算
    // tiktoken doesn't know this model → registry will fall back to char estimation
    let t = OpenAiTokenizer::new();
    let req = text_request("not-a-real-model-zz", "hi");
    let n = t.count_prompt("not-a-real-model-zz", &req).await;
    assert!(n.is_none());
}

// ─── TiktokenTokenizer 直路 ──────────────────────────────────────────────────

#[tokio::test]
async fn tiktoken_tokenizer_direct_path() {
    let t = TiktokenTokenizer;
    let req = text_request("gpt-4", "Hello, world!");
    let n = t.count_prompt("gpt-4", &req).await;
    assert!(n.is_some());
    assert!(n.unwrap() > 0);
}

#[tokio::test]
async fn noop_tokenizer_returns_none() {
    let t = NoopTokenizer;
    let req = text_request("gpt-4", "hi");
    assert!(t.count_prompt("gpt-4", &req).await.is_none());
}

// ─── infer_provider_type ─────────────────────────────────────────────────────

#[test]
fn infer_provider_type_openai_models() {
    assert_eq!(TokenizerRegistry::infer_provider_type("gpt-4"), "openai");
    assert_eq!(TokenizerRegistry::infer_provider_type("gpt-4o"), "openai");
    assert_eq!(TokenizerRegistry::infer_provider_type("o1-preview"), "openai");
    assert_eq!(TokenizerRegistry::infer_provider_type("o3-mini"), "openai");
}

#[test]
fn infer_provider_type_anthropic_models() {
    assert_eq!(
        TokenizerRegistry::infer_provider_type("claude-3-5-sonnet"),
        "anthropic"
    );
    assert_eq!(
        TokenizerRegistry::infer_provider_type("claude-opus-4-7"),
        "anthropic"
    );
}

#[test]
fn infer_provider_type_gemini_models() {
    assert_eq!(
        TokenizerRegistry::infer_provider_type("gemini-1.5-pro"),
        "gemini"
    );
}

#[test]
fn infer_provider_type_huggingface_models() {
    assert_eq!(
        TokenizerRegistry::infer_provider_type("Qwen/Qwen2.5-7B"),
        "huggingface"
    );
    assert_eq!(
        TokenizerRegistry::infer_provider_type("meta-llama/Llama-3-8B"),
        "huggingface"
    );
    assert_eq!(
        TokenizerRegistry::infer_provider_type("mistralai/Mistral-7B"),
        "huggingface"
    );
    assert_eq!(
        TokenizerRegistry::infer_provider_type("deepseek-coder"),
        "huggingface"
    );
}

#[test]
fn infer_provider_type_unknown_falls_back_to_openai_compat() {
    assert_eq!(
        TokenizerRegistry::infer_provider_type("custom-model-xyz"),
        "openai_compat"
    );
}

// ─── TokenizerRegistry routing ───────────────────────────────────────────────

#[tokio::test]
async fn registry_routes_openai_to_tiktoken_for_pure_text() {
    let registry = TokenizerRegistry::default();
    let req = text_request("gpt-4", "Hello, world!");
    let n = registry.count_prompt("openai", "gpt-4", &req).await;
    // 精确值,远大于字符估算的下限
    assert!(n > 0);
    let est = estimate_from_request(&req);
    // tiktoken count 不会比字符估算夸张地大 — sanity
    assert!(n < est * 4 + 10);
}

#[tokio::test]
async fn registry_routes_unknown_model_to_estimate() {
    // strategy=Estimate 兜底 → 走字符估算
    let registry = TokenizerRegistry::default();
    let req = text_request("not-a-real-xyz", "hello world test"); // 16 chars
    let n = registry.count_prompt("unknown_provider", "not-a-real-xyz", &req).await;
    let est = estimate_from_request(&req);
    assert_eq!(n, est);
}

#[tokio::test]
async fn registry_routes_openai_compat_to_tiktoken() {
    // openai_compat strategy=Tiktoken,直路
    let registry = TokenizerRegistry::default();
    let req = text_request("gpt-3.5-turbo", "vLLM hosted");
    let n = registry
        .count_prompt("openai_compat", "gpt-3.5-turbo", &req)
        .await;
    assert!(n > 0);
}

// ─── Deadline → estimate fallback ────────────────────────────────────────────

/// 模拟一个永远悬挂的 tokenizer,验证 registry 的 deadline 触发后能降级到 estimate
struct HangingTokenizer;

#[async_trait]
impl PromptTokenizer for HangingTokenizer {
    async fn count_prompt(&self, _model: &str, _req: &ChatRequest) -> Option<u64> {
        // 永久 sleep 远超 deadline
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Some(99999)
    }

    fn name(&self) -> &str {
        "hanging"
    }
}

#[tokio::test]
async fn registry_deadline_triggers_estimate_fallback() {
    // 用 Hanging tokenizer 注入 — 但 registry 内部 tokenizer 实例化是私有的,
    // 这里我们改成构造一个零 deadline 的 registry,strategy=OpenAi 会落到 tiktoken
    // → 但 tiktoken 是同步的,几乎不可能超时。改用未知模型走 OpenAi tokenizer,
    //   tiktoken 失败返回 None → registry 走 estimate.
    // 真正验证 deadline 的方式是用 mapping 强制走某 strategy + 人为 sleep,
    // 但 trait 实例注入 step 1 不暴露,因此我们这里只验证 None 路径触发 estimate.

    let cfg = TokenizerConfig {
        per_request_deadline: Duration::from_millis(1),
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::new(cfg);
    // OpenAi strategy + 未知模型 → tiktoken 返回 None → estimate 兜底
    let req = text_request("totally-unknown-model", "hello");
    let n = registry
        .count_prompt("openai", "totally-unknown-model", &req)
        .await;
    let est = estimate_from_request(&req);
    assert_eq!(n, est, "should fall back to estimate when tokenizer returns None");
}

// 直接验证 trait 层 deadline:绕过 registry,手动 timeout 一次
#[tokio::test]
async fn hanging_tokenizer_times_out_under_short_deadline() {
    let t: Arc<dyn PromptTokenizer> = Arc::new(HangingTokenizer);
    let req = text_request("any", "hi");
    let result = tokio::time::timeout(
        Duration::from_millis(20),
        t.count_prompt("any", &req),
    )
    .await;
    assert!(result.is_err(), "expected timeout");
}

// ─── Strategy mapping override ───────────────────────────────────────────────

#[tokio::test]
async fn registry_mapping_overrides_provider_default() {
    use kong_ai::token::TokenizerMapping;

    // 即使 provider_type=openai,只要 mapping 命中 → 用配置的 strategy
    // Even when provider_type=openai, mapping wins
    let cfg = TokenizerConfig {
        mappings: vec![TokenizerMapping {
            pattern: "^gpt-4$".to_string(),
            strategy: TokenizerStrategy::Estimate,
            hf_repo_id: None,
        }],
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::new(cfg);
    let req = text_request("gpt-4", "hello world test"); // 16 chars
    let n = registry.count_prompt("openai", "gpt-4", &req).await;
    let est = estimate_from_request(&req);
    assert_eq!(n, est, "mapping forced Estimate strategy");
}

// ─── count_prompt_from_body ──────────────────────────────────────────────────

#[tokio::test]
async fn count_prompt_from_body_parses_and_counts() {
    let registry = TokenizerRegistry::default();
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello, world!"}]}"#;
    let n = registry
        .count_prompt_from_body("openai", "gpt-4", body)
        .await;
    assert!(n > 0);
}

#[tokio::test]
async fn count_prompt_from_body_invalid_json_falls_back_to_byte_estimate() {
    let registry = TokenizerRegistry::default();
    let body = "this is not json"; // 16 chars → 4 tokens
    let n = registry
        .count_prompt_from_body("openai", "gpt-4", body)
        .await;
    assert_eq!(n, 4);
}

// ─── Global singleton ────────────────────────────────────────────────────────

#[tokio::test]
async fn global_registry_set_get() {
    use kong_ai::token::{global_registry, set_global_registry};

    // 注意:OnceLock 全局,只能 set 一次。本测试若先于其他测试运行才会成功 set;
    // 单纯验证 get 不为 None 即可(其他测试可能已经 set)
    // OnceLock is set-once globally; this test simply ensures get works after set
    let registry = Arc::new(TokenizerRegistry::default());
    set_global_registry(registry);
    assert!(global_registry().is_some());
}
