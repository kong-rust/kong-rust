//! Rust 原生插件配置的共享校验。
//!
//! Admin API 与数据面执行器必须调用同一份规则，避免数据库配置和声明式配置
//! 出现不同的校验语义。

use std::fmt;

use serde_json::{Map, Value};

/// AI 配额字段允许的最大值。
pub const AI_RATE_LIMIT_MAX_LIMIT: u64 = i32::MAX as u64;

/// 单个插件配置校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfigValidationError {
    field: String,
    message: String,
}

impl PluginConfigValidationError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }

    /// 相对于 `config` 的字段路径；实体级错误使用 `@entity`。
    pub fn field(&self) -> &str {
        &self.field
    }

    /// 字段级错误描述。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PluginConfigValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field == "@entity" {
            write!(f, "config: {}", self.message)
        } else {
            write!(f, "config.{}: {}", self.field, self.message)
        }
    }
}

impl std::error::Error for PluginConfigValidationError {}

/// 为 AI 限流插件应用 schema 默认值。
pub fn apply_ai_rate_limit_config_defaults(config: &mut Map<String, Value>) {
    config
        .entry("limit_by".to_string())
        .or_insert_with(|| Value::String("consumer".to_string()));
    config.entry("tpm_limit".to_string()).or_insert(Value::Null);
    config.entry("rpm_limit".to_string()).or_insert(Value::Null);
    config
        .entry("header_name".to_string())
        .or_insert_with(|| Value::String("X-AI-Key".to_string()));
    config
        .entry("error_code".to_string())
        .or_insert_with(|| Value::from(429));
    config
        .entry("error_message".to_string())
        .or_insert_with(|| Value::String("AI rate limit exceeded".to_string()));
}

/// 校验已知 Rust 原生插件配置。
///
/// 未知或尚无专用规则的插件保持原有行为。
pub fn validate_plugin_config(
    plugin_name: &str,
    config: &Value,
) -> Result<(), PluginConfigValidationError> {
    match plugin_name {
        "ai-rate-limit" => validate_ai_rate_limit_config(config),
        _ => Ok(()),
    }
}

/// 校验 `ai-rate-limit` 的完整配置。
///
/// 缺失字段按 schema 默认值解释；调用方可选择先调用
/// [`apply_ai_rate_limit_config_defaults`] 将默认值写回实体。
pub fn validate_ai_rate_limit_config(config: &Value) -> Result<(), PluginConfigValidationError> {
    let config = config
        .as_object()
        .ok_or_else(|| PluginConfigValidationError::new("", "expected a record"))?;

    const KNOWN_FIELDS: &[&str] = &[
        "limit_by",
        "tpm_limit",
        "rpm_limit",
        "header_name",
        "error_code",
        "error_message",
    ];
    if let Some(field) = config
        .keys()
        .find(|field| !KNOWN_FIELDS.contains(&field.as_str()))
    {
        return Err(PluginConfigValidationError::new(field, "unknown field"));
    }

    let limit_by = match config.get("limit_by") {
        None => "consumer",
        Some(Value::String(value))
            if matches!(
                value.as_str(),
                "global" | "route" | "consumer" | "virtual_key"
            ) =>
        {
            value
        }
        Some(Value::String(_)) => {
            return Err(PluginConfigValidationError::new(
                "limit_by",
                "expected one of: global, route, consumer, virtual_key",
            ));
        }
        Some(_) => {
            return Err(PluginConfigValidationError::new(
                "limit_by",
                "expected a string",
            ));
        }
    };

    validate_optional_limit(config, "tpm_limit")?;
    validate_optional_limit(config, "rpm_limit")?;

    if let Some(value) = config.get("header_name") {
        if !value.is_string() {
            return Err(PluginConfigValidationError::new(
                "header_name",
                "expected a string",
            ));
        }
    }

    if let Some(value) = config.get("error_code") {
        let valid = value
            .as_u64()
            .map(|code| code <= u16::MAX as u64)
            .unwrap_or(false);
        if !valid {
            return Err(PluginConfigValidationError::new(
                "error_code",
                "expected an integer between 0 and 65535",
            ));
        }
    }

    if let Some(value) = config.get("error_message") {
        if !value.is_string() {
            return Err(PluginConfigValidationError::new(
                "error_message",
                "expected a string",
            ));
        }
    }

    let tpm_configured = config
        .get("tpm_limit")
        .map(|value| !value.is_null())
        .unwrap_or(false);
    let rpm_configured = config
        .get("rpm_limit")
        .map(|value| !value.is_null())
        .unwrap_or(false);

    if limit_by == "virtual_key" {
        if tpm_configured || rpm_configured {
            return Err(PluginConfigValidationError::new(
                "@entity",
                "tpm_limit and rpm_limit must be null when limit_by is virtual_key",
            ));
        }
    } else if !tpm_configured && !rpm_configured {
        return Err(PluginConfigValidationError::new(
            "@entity",
            "at least one of tpm_limit or rpm_limit must be configured",
        ));
    }

    Ok(())
}

fn validate_optional_limit(
    config: &Map<String, Value>,
    field: &'static str,
) -> Result<(), PluginConfigValidationError> {
    let Some(value) = config.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }

    let valid = value
        .as_u64()
        .map(|limit| (1..=AI_RATE_LIMIT_MAX_LIMIT).contains(&limit))
        .unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err(PluginConfigValidationError::new(
            field,
            format!(
                "expected an integer between 1 and {} or null",
                AI_RATE_LIMIT_MAX_LIMIT
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_supported_limit_modes() {
        for limit_by in ["global", "route", "consumer"] {
            validate_ai_rate_limit_config(&json!({
                "limit_by": limit_by,
                "rpm_limit": 1,
                "tpm_limit": null
            }))
            .unwrap();
        }
        validate_ai_rate_limit_config(&json!({
            "limit_by": "virtual_key",
            "rpm_limit": null,
            "tpm_limit": null
        }))
        .unwrap();
    }

    #[test]
    fn rejects_unsupported_limit_mode() {
        let error = validate_ai_rate_limit_config(&json!({
            "limit_by": "service",
            "rpm_limit": 1
        }))
        .unwrap_err();
        assert_eq!(error.field(), "limit_by");
    }

    #[test]
    fn limit_must_be_json_integer_in_range() {
        for invalid in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(2_147_483_648_u64),
        ] {
            let error = validate_ai_rate_limit_config(&json!({
                "limit_by": "consumer",
                "rpm_limit": invalid
            }))
            .unwrap_err();
            assert_eq!(error.field(), "rpm_limit");
        }

        validate_ai_rate_limit_config(&json!({
            "limit_by": "consumer",
            "rpm_limit": 2_147_483_647_u64
        }))
        .unwrap();
    }

    #[test]
    fn validates_mode_specific_limit_rules() {
        let virtual_key_error = validate_ai_rate_limit_config(&json!({
            "limit_by": "virtual_key",
            "rpm_limit": 10
        }))
        .unwrap_err();
        assert_eq!(virtual_key_error.field(), "@entity");

        let legacy_error = validate_ai_rate_limit_config(&json!({
            "limit_by": "route",
            "rpm_limit": null,
            "tpm_limit": null
        }))
        .unwrap_err();
        assert_eq!(legacy_error.field(), "@entity");
    }

    #[test]
    fn preserves_deprecated_header_name_but_rejects_unknown_fields() {
        validate_plugin_config(
            "ai-rate-limit",
            &json!({
                "limit_by": "global",
                "rpm_limit": 1,
                "header_name": "X-Legacy-AI-Key"
            }),
        )
        .unwrap();

        let error = validate_ai_rate_limit_config(&json!({
            "limit_by": "global",
            "rpm_limit": 1,
            "unexpected": true
        }))
        .unwrap_err();
        assert_eq!(error.field(), "unexpected");
    }

    #[test]
    fn applies_schema_defaults_without_overwriting_values() {
        let mut config = json!({"rpm_limit": 100}).as_object().unwrap().clone();
        apply_ai_rate_limit_config_defaults(&mut config);

        assert_eq!(config["limit_by"], "consumer");
        assert_eq!(config["rpm_limit"], 100);
        assert!(config["tpm_limit"].is_null());
        assert_eq!(config["header_name"], "X-AI-Key");
        validate_ai_rate_limit_config(&Value::Object(config)).unwrap();
    }
}
