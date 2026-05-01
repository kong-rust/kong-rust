//! ai-proxy semantic routing 集成测试 — 通过预注入 SemanticRoutingIndex 跳过真实 embedding HTTP
//! Integration tests for ai-proxy semantic routing: pre-inject SemanticRoutingIndex
//! to bypass real embedding HTTP calls.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use kong_ai::embedding::EmbeddingClient;
use kong_ai::plugins::ai_proxy::{AiProxyPlugin, SemanticRoutingIndex};

// ============ Mock EmbeddingClient ============

/// 简单 embedding mock — 把字面文本里出现的关键词映射到一个固定向量。
/// 测试场景:三个领域(weather / code / image)各有一组锚定向量。
struct DomainEmbedder;

impl DomainEmbedder {
    fn vec_weather() -> Vec<f32> {
        vec![1.0, 0.0, 0.0]
    }
    fn vec_code() -> Vec<f32> {
        vec![0.0, 1.0, 0.0]
    }
    fn vec_image() -> Vec<f32> {
        vec![0.0, 0.0, 1.0]
    }
}

#[async_trait]
impl EmbeddingClient for DomainEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let lower = text.to_lowercase();
        if lower.contains("weather") || lower.contains("rain") || lower.contains("forecast") {
            Ok(Self::vec_weather())
        } else if lower.contains("code") || lower.contains("function") || lower.contains("rust") {
            Ok(Self::vec_code())
        } else if lower.contains("image") || lower.contains("photo") || lower.contains("picture") {
            Ok(Self::vec_image())
        } else {
            // 未知领域 → 接近零向量,与所有锚点都低相似度
            Ok(vec![0.01, 0.01, 0.01])
        }
    }

    fn identifier(&self) -> &str {
        "domain-mock"
    }
}

// ============ 失败 Embedder — 测试 fallback 路径 ============

struct FailingEmbedder;

#[async_trait]
impl EmbeddingClient for FailingEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(kong_core::error::KongError::UpstreamError(
            "deliberate test failure".into(),
        ))
    }
    fn identifier(&self) -> &str {
        "failing"
    }
}

// ============ Helpers ============

fn make_three_domain_config() -> serde_json::Value {
    // 三个 model 各自带 examples,通配符 ".*" 匹配任意 prompt 落到 model_routes
    serde_json::json!({
        "model": "smart-router",
        "model_source": "config",
        "enable_token_size_routing": false,
        "enable_semantic_routing": true,
        "semantic_routing_min_score": 0.5,
        "model_routes": [
            {
                "pattern": ".*",
                "targets": [
                    {
                        "provider_type": "openai",
                        "model_name": "weather-pro",
                        "auth_config": { "header_value": "sk-w" },
                        "semantic_routing_examples": [
                            "what is the weather today",
                            "will it rain tomorrow"
                        ]
                    },
                    {
                        "provider_type": "openai",
                        "model_name": "code-llama",
                        "auth_config": { "header_value": "sk-c" },
                        "semantic_routing_examples": [
                            "write a rust function",
                            "explain this code"
                        ]
                    },
                    {
                        "provider_type": "openai",
                        "model_name": "vision-gpt",
                        "auth_config": { "header_value": "sk-i" },
                        "semantic_routing_examples": [
                            "describe this image",
                            "caption the photo"
                        ]
                    }
                ]
            }
        ]
    })
}

fn install_index_with_embedder(plugin: &AiProxyPlugin, embedder: Arc<dyn EmbeddingClient>) {
    // 直接预注入 examples 向量(rule_idx, target_idx) → vectors,
    // 与 make_three_domain_config 的 examples 顺序对齐。
    let mut examples: HashMap<(usize, usize), Vec<Vec<f32>>> = HashMap::new();
    examples.insert((0, 0), vec![DomainEmbedder::vec_weather(), DomainEmbedder::vec_weather()]);
    examples.insert((0, 1), vec![DomainEmbedder::vec_code(), DomainEmbedder::vec_code()]);
    examples.insert((0, 2), vec![DomainEmbedder::vec_image(), DomainEmbedder::vec_image()]);

    let cfg: kong_ai::plugins::ai_proxy::AiProxyConfig =
        serde_json::from_value(make_three_domain_config()).unwrap();

    let index = Arc::new(SemanticRoutingIndex {
        client: embedder,
        examples,
        min_score: 0.5,
    });
    plugin.install_semantic_index_for_config(&cfg, index);
}

fn make_ctx(user_msg: &str) -> RequestCtx {
    let body = serde_json::json!({
        "model": "smart-router",
        "messages": [{"role": "user", "content": user_msg}]
    })
    .to_string();
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(body);
    ctx
}

fn selected_model(ctx: &RequestCtx) -> Option<String> {
    ctx.response_headers_to_set
        .iter()
        .find(|(k, _)| k == "X-Kong-AI-Selected-Model")
        .map(|(_, v)| v.clone())
}

// ============ 测试 ============

#[tokio::test]
async fn weather_prompt_routes_to_weather_model() {
    let plugin = AiProxyPlugin::new();
    install_index_with_embedder(&plugin, Arc::new(DomainEmbedder));
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: make_three_domain_config(),
    };

    let mut ctx = make_ctx("will it rain in Tokyo tomorrow?");
    plugin.access(&pc, &mut ctx).await.unwrap();
    assert_eq!(selected_model(&ctx).as_deref(), Some("weather-pro"));
}

#[tokio::test]
async fn code_prompt_routes_to_code_model() {
    let plugin = AiProxyPlugin::new();
    install_index_with_embedder(&plugin, Arc::new(DomainEmbedder));
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: make_three_domain_config(),
    };

    let mut ctx = make_ctx("write a rust function that reverses a string");
    plugin.access(&pc, &mut ctx).await.unwrap();
    assert_eq!(selected_model(&ctx).as_deref(), Some("code-llama"));
}

#[tokio::test]
async fn image_prompt_routes_to_vision_model() {
    let plugin = AiProxyPlugin::new();
    install_index_with_embedder(&plugin, Arc::new(DomainEmbedder));
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: make_three_domain_config(),
    };

    let mut ctx = make_ctx("describe the photo I just uploaded");
    plugin.access(&pc, &mut ctx).await.unwrap();
    assert_eq!(selected_model(&ctx).as_deref(), Some("vision-gpt"));
}

#[tokio::test]
async fn embedding_failure_falls_back_to_priority_routing() {
    let plugin = AiProxyPlugin::new();
    install_index_with_embedder(&plugin, Arc::new(FailingEmbedder));
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: make_three_domain_config(),
    };

    let mut ctx = make_ctx("anything at all");
    plugin.access(&pc, &mut ctx).await.unwrap();
    // fallback 路径走 weighted RR — 第一个 target(weather-pro)被选中
    let m = selected_model(&ctx).expect("must still resolve a model");
    assert!(
        ["weather-pro", "code-llama", "vision-gpt"].contains(&m.as_str()),
        "fallback should still pick one of the configured targets, got {}",
        m
    );
}

#[tokio::test]
async fn unknown_domain_below_threshold_falls_back() {
    // prompt 与所有领域 examples 都很低相似度 → semantic_routing_min_score 拒绝 → fallback
    let plugin = AiProxyPlugin::new();
    install_index_with_embedder(&plugin, Arc::new(DomainEmbedder));
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: make_three_domain_config(),
    };

    let mut ctx = make_ctx("xyzzy quux blarg");
    plugin.access(&pc, &mut ctx).await.unwrap();
    // 落到 fallback 路径 — 第一个 target
    assert_eq!(selected_model(&ctx).as_deref(), Some("weather-pro"));
}
