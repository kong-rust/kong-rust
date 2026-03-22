//! 认证场景测试 — 验证各 driver 的 auth_config 应用逻辑

use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::anthropic::AnthropicDriver;
use kong_ai::provider::gemini::GeminiDriver;
use kong_ai::provider::openai::OpenAiDriver;
use kong_ai::provider::AiDriver;

fn make_model(name: &str) -> AiModel {
    AiModel {
        model_name: name.to_string(),
        ..Default::default()
    }
}

fn make_provider(provider_type: &str, auth: AuthConfig) -> AiProviderConfig {
    AiProviderConfig {
        provider_type: provider_type.to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        ..Default::default()
    }
}

// ============ OpenAI Bearer Header ============

#[test]
fn test_openai_auth_bearer_header() {
    // header_value 不带 "Bearer " 前缀时，driver 应自动补全
    let driver = OpenAiDriver;
    let model = make_model("gpt-4");
    let auth = AuthConfig {
        header_value: Some("sk-xxx".to_string()),
        ..Default::default()
    };
    let config = make_provider("openai", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(auth_header.is_some(), "应包含 Authorization header");
    assert_eq!(
        auth_header.unwrap().1,
        "Bearer sk-xxx",
        "应自动添加 Bearer 前缀"
    );
}

#[test]
fn test_openai_auth_already_has_bearer_prefix() {
    // header_value 已带 "Bearer " 前缀时，不应重复添加
    let driver = OpenAiDriver;
    let model = make_model("gpt-4");
    let auth = AuthConfig {
        header_value: Some("Bearer sk-xxx".to_string()),
        ..Default::default()
    };
    let config = make_provider("openai", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(auth_header.is_some());
    assert_eq!(
        auth_header.unwrap().1,
        "Bearer sk-xxx",
        "不应产生 'Bearer Bearer sk-xxx'"
    );
}

// ============ Anthropic x-api-key ============

#[test]
fn test_anthropic_auth_x_api_key() {
    // Anthropic driver 默认使用 x-api-key（非 Authorization）
    let driver = AnthropicDriver;
    let model = make_model("claude-3-opus-20240229");
    let auth = AuthConfig {
        header_value: Some("sk-ant-key-123".to_string()),
        ..Default::default()
    };
    let config = make_provider("anthropic", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    // 验证 x-api-key header
    let api_key_header = upstream.headers.iter().find(|(k, _)| k == "x-api-key");
    assert!(
        api_key_header.is_some(),
        "Anthropic 应使用 x-api-key header"
    );
    assert_eq!(api_key_header.unwrap().1, "sk-ant-key-123");

    // 验证 anthropic-version header 始终存在
    let version_header = upstream
        .headers
        .iter()
        .find(|(k, _)| k == "anthropic-version");
    assert!(
        version_header.is_some(),
        "Anthropic 应始终设置 anthropic-version header"
    );
    assert_eq!(version_header.unwrap().1, "2023-06-01");

    // 不应有 Authorization header
    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(
        auth_header.is_none(),
        "Anthropic 不应使用 Authorization header"
    );
}

// ============ Gemini auth ============

#[test]
fn test_gemini_auth_bearer_token() {
    // Gemini 默认 endpoint 使用 Bearer token
    let driver = GeminiDriver;
    let model = make_model("gemini-pro");
    let auth = AuthConfig {
        header_value: Some("AIza-test-key".to_string()),
        ..Default::default()
    };
    let config = make_provider("gemini", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    // Gemini 默认 header_name 为 Authorization，header_value 会被加 Bearer 前缀
    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(auth_header.is_some(), "Gemini 应设置 Authorization header");
    assert_eq!(auth_header.unwrap().1, "Bearer AIza-test-key");
}

// ============ 自定义 header_name ============

#[test]
fn test_openai_auth_custom_header_name() {
    // 自定义 header_name 时，header_value 原样设置（不添加 Bearer 前缀）
    let driver = OpenAiDriver;
    let model = make_model("gpt-4");
    let auth = AuthConfig {
        header_name: Some("X-Custom-Key".to_string()),
        header_value: Some("my-key".to_string()),
        ..Default::default()
    };
    let config = make_provider("openai", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    let custom_header = upstream.headers.iter().find(|(k, _)| k == "X-Custom-Key");
    assert!(
        custom_header.is_some(),
        "应使用自定义 header 名 X-Custom-Key"
    );
    assert_eq!(
        custom_header.unwrap().1,
        "my-key",
        "自定义 header 时不应添加 Bearer 前缀"
    );

    // 不应有默认的 Authorization header
    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(
        auth_header.is_none(),
        "使用自定义 header 名时不应有 Authorization"
    );
}

// ============ 空 auth_config ============

#[test]
fn test_auth_config_empty_values() {
    // auth_config 为空时，不应添加任何认证头（适用于本地模型）
    let driver = OpenAiDriver;
    let model = make_model("gpt-4");
    let auth = AuthConfig::default(); // 所有字段为 None
    let config = make_provider("openai", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    // 不应包含任何认证相关的 header
    assert!(
        upstream.headers.is_empty(),
        "空 auth_config 不应添加认证 header，实际: {:?}",
        upstream.headers
    );
}

#[test]
fn test_anthropic_auth_custom_header_name() {
    // Anthropic driver 也支持自定义 header_name
    let driver = AnthropicDriver;
    let model = make_model("claude-3-opus-20240229");
    let auth = AuthConfig {
        header_name: Some("X-My-Auth".to_string()),
        header_value: Some("custom-token".to_string()),
        ..Default::default()
    };
    let config = make_provider("anthropic", auth);

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    // 应有 anthropic-version + 自定义 header
    let custom = upstream.headers.iter().find(|(k, _)| k == "X-My-Auth");
    assert!(custom.is_some(), "应使用自定义 header 名");
    assert_eq!(custom.unwrap().1, "custom-token");

    // anthropic-version 始终存在
    let version = upstream
        .headers
        .iter()
        .find(|(k, _)| k == "anthropic-version");
    assert!(version.is_some());
}
