//! AI Gateway 数据模型 — AI Provider、AI Model、AI Virtual Key

use kong_core::traits::Entity;
use rust_decimal::Decimal;
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::borrow::Cow;
use std::fmt;
use uuid::Uuid;

/// 单个 AI 模型允许配置的最大权重。
pub const MAX_MODEL_WEIGHT: i32 = 10_000;
/// `NUMERIC(28,12)` 可表示金额的绝对上界（不包含）。
const DECIMAL_28_12_UPPER_BOUND: i128 = 10_000_000_000_000_000;

/// 校验并规范为 `NUMERIC(28,12)` 可无损保存的 12 位小数。
fn normalize_model_cost(value: Decimal) -> Result<Decimal, String> {
    if value < Decimal::ZERO {
        return Err("模型价格不能为负数".to_string());
    }

    let normalized = value.normalize();
    if normalized.scale() > 12 {
        return Err("模型价格最多支持 12 位小数".to_string());
    }

    let upper_bound = Decimal::from_i128_with_scale(DECIMAL_28_12_UPPER_BOUND, 0);
    if normalized >= upper_bound {
        return Err("模型价格超出 NUMERIC(28,12) 范围".to_string());
    }

    let mut fixed = normalized;
    fixed.rescale(12);
    Ok(fixed)
}

fn trim_decimal_fraction_zeros(value: &str) -> Cow<'_, str> {
    let exponent_index = value.find(['e', 'E']).unwrap_or(value.len());
    let mantissa = &value[..exponent_index];
    if !mantissa.contains('.') {
        return Cow::Borrowed(value);
    }

    let trimmed_mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
    if trimmed_mantissa.len() == mantissa.len() {
        return Cow::Borrowed(value);
    }

    let mut normalized = String::with_capacity(value.len());
    if trimmed_mantissa.is_empty() || matches!(trimmed_mantissa, "+" | "-") {
        normalized.push_str(trimmed_mantissa);
        normalized.push('0');
    } else {
        normalized.push_str(trimmed_mantissa);
    }
    normalized.push_str(&value[exponent_index..]);

    Cow::Owned(normalized)
}

fn parse_model_cost(value: &str) -> Result<Decimal, String> {
    let normalized = trim_decimal_fraction_zeros(value);
    let parsed = if normalized.contains(['e', 'E']) {
        Decimal::from_scientific(&normalized)
    } else {
        Decimal::from_str_exact(&normalized)
    }
    .map_err(|error| format!("模型价格不是有效十进制数: {error}"))?;

    normalize_model_cost(parsed)
}

fn serialize_optional_model_cost<S>(
    value: &Option<Decimal>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => {
            let value = normalize_model_cost(*value).map_err(serde::ser::Error::custom)?;
            serializer.serialize_str(&value.to_string())
        }
        None => serializer.serialize_none(),
    }
}

struct ModelCostVisitor;

impl<'de> Visitor<'de> for ModelCostVisitor {
    type Value = Decimal;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NUMERIC(28,12) 范围内的非负十进制字符串或数字")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_model_cost(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_model_cost(Decimal::from_i128_with_scale(i128::from(value), 0)).map_err(E::custom)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_model_cost(Decimal::from_i128_with_scale(i128::from(value), 0)).map_err(E::custom)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() {
            return Err(E::custom("模型价格必须是有限数字"));
        }
        parse_model_cost(&value.to_string()).map_err(E::custom)
    }
}

fn deserialize_optional_model_cost<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalModelCostVisitor;

    impl<'de> Visitor<'de> for OptionalModelCostVisitor {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null、十进制字符串或兼容的旧数字")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(ModelCostVisitor).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalModelCostVisitor)
}

/// 校验并规范 Virtual Key 预算金额为可无损写入 `NUMERIC(28,12)` 的值。
pub fn normalize_budget_amount(value: Decimal) -> Result<Decimal, String> {
    if value < Decimal::ZERO {
        return Err("预算金额不能为负数".to_string());
    }

    let normalized = value.normalize();
    if normalized.scale() > 12 {
        return Err("预算金额最多支持 12 位小数".to_string());
    }

    let upper_bound = Decimal::from_i128_with_scale(DECIMAL_28_12_UPPER_BOUND, 0);
    if normalized >= upper_bound {
        return Err("预算金额超出 NUMERIC(28,12) 范围".to_string());
    }

    let mut fixed = normalized;
    fixed.rescale(12);
    Ok(fixed)
}

/// 解析十进制预算字符串；支持科学计数法，但绝不进行舍入。
pub fn parse_budget_amount(value: &str) -> Result<Decimal, String> {
    let normalized = trim_decimal_fraction_zeros(value);
    let parsed = if normalized.contains(['e', 'E']) {
        Decimal::from_scientific(&normalized)
    } else {
        Decimal::from_str_exact(&normalized)
    }
    .map_err(|error| format!("预算金额不是有效十进制数: {error}"))?;

    normalize_budget_amount(parsed)
}

fn serialize_budget_amount<S>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let value = normalize_budget_amount(*value).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&value.to_string())
}

fn serialize_optional_budget_amount<S>(
    value: &Option<Decimal>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize_budget_amount(value, serializer),
        None => serializer.serialize_none(),
    }
}

struct BudgetAmountVisitor;

impl<'de> Visitor<'de> for BudgetAmountVisitor {
    type Value = Decimal;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NUMERIC(28,12) 范围内的非负预算十进制字符串或数字")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_budget_amount(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_budget_amount(Decimal::from_i128_with_scale(i128::from(value), 0))
            .map_err(E::custom)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_budget_amount(Decimal::from_i128_with_scale(i128::from(value), 0))
            .map_err(E::custom)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() {
            return Err(E::custom("预算金额必须是有限数字"));
        }
        parse_budget_amount(&value.to_string()).map_err(E::custom)
    }
}

fn deserialize_budget_amount<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(BudgetAmountVisitor)
}

fn deserialize_optional_budget_amount<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalBudgetAmountVisitor;

    impl<'de> Visitor<'de> for OptionalBudgetAmountVisitor {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null、预算十进制字符串或兼容的旧数字")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(BudgetAmountVisitor).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalBudgetAmountVisitor)
}

/// AI Provider 配置（对应 ai_providers 表）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiProviderConfig {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    pub auth_config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub config: serde_json::Value,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// AI Model（同 name 组成 model group 用于 LB — models with the same name form a load-balancing group）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiModel {
    pub id: Uuid,
    pub name: String,
    pub provider_id: Uuid,
    pub model_name: String,
    pub priority: i32,
    pub weight: i32,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_model_cost",
        deserialize_with = "deserialize_optional_model_cost"
    )]
    pub input_cost: Option<Decimal>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_model_cost",
        deserialize_with = "deserialize_optional_model_cost"
    )]
    pub output_cost: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// 单次请求 prompt token 上限（balancer by_token_size 路由用）
    /// 配合 TokenizerRegistry 计算的 prompt_tokens,在 ModelGroupBalancer.select_for 里做候选过滤:
    /// `prompt_tokens <= max_input_tokens` 才能命中,超过则 fallback 到下一 priority
    /// Per-request prompt token cap used by ModelGroupBalancer.select_for (by_token_size routing).
    /// Candidates only match when `prompt_tokens <= max_input_tokens`; otherwise fallback to next priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i32>,
    pub config: serde_json::Value,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// AI Virtual Key（虚拟 API Key — virtual API key for rate limiting and budget control）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiVirtualKey {
    pub id: Uuid,
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_budget_amount",
        deserialize_with = "deserialize_optional_budget_amount"
    )]
    pub budget_limit: Option<Decimal>,
    #[serde(
        serialize_with = "serialize_budget_amount",
        deserialize_with = "deserialize_budget_amount"
    )]
    pub budget_used: Decimal,
    /// 服务端维护的未决 intent 数量。
    pub budget_pending_count: i64,
    /// 服务端维护的 unresolved intent/account issue 数量。
    pub budget_unresolved_count: i64,
    /// 单 key 的账务 CAS revision。
    pub budget_accounting_revision: i64,
    /// 最新 checkpoint 之后的保守事件计数。
    pub budget_checkpoint_tail_events: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_state_updated_at: Option<i64>,
    /// 数据库生成的 `clean` / `pending` / `unresolved` 状态。
    pub budget_accounting_state: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for AiVirtualKey {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            key_hash: String::new(),
            key_prefix: String::new(),
            consumer_id: None,
            allowed_models: None,
            tpm_limit: None,
            rpm_limit: None,
            budget_limit: None,
            budget_used: Decimal::ZERO,
            budget_pending_count: 0,
            budget_unresolved_count: 0,
            budget_accounting_revision: 0,
            budget_checkpoint_tail_events: 0,
            budget_state_updated_at: None,
            budget_accounting_state: "clean".to_string(),
            enabled: false,
            expires_at: None,
            ws_id: None,
            created_at: None,
            updated_at: None,
            tags: None,
        }
    }
}

/// 认证配置（嵌入在 AiProviderConfig.auth_config JSONB 中 — embedded in auth_config JSONB column）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_access_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_secret_access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gcp_service_account_json: Option<String>,
}

// ============ Entity trait 实现 — Entity trait implementations ============

impl Entity for AiProviderConfig {
    fn table_name() -> &'static str {
        "ai_providers"
    }
    fn id(&self) -> Uuid {
        self.id
    }
    fn endpoint_key() -> Option<&'static str> {
        Some("name")
    }
    fn endpoint_key_value(&self) -> Option<String> {
        Some(self.name.clone())
    }
    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

impl Entity for AiModel {
    fn table_name() -> &'static str {
        "ai_models"
    }
    fn id(&self) -> Uuid {
        self.id
    }
    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

impl Entity for AiVirtualKey {
    fn table_name() -> &'static str {
        "ai_virtual_keys"
    }
    fn id(&self) -> Uuid {
        self.id
    }
    fn endpoint_key() -> Option<&'static str> {
        Some("name")
    }
    fn endpoint_key_value(&self) -> Option<String> {
        Some(self.name.clone())
    }
    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{AiModel, AiProviderConfig, AiVirtualKey};
    use kong_core::traits::{Dao, PrimaryKey};
    use kong_db::{DblessDao, DblessStore};
    use rust_decimal::Decimal;
    use std::sync::Arc;
    use uuid::Uuid;

    fn assert_integer_timestamps(value: serde_json::Value) {
        assert!(value["created_at"].as_i64().is_some());
        assert!(value["updated_at"].as_i64().is_some());
    }

    #[test]
    fn ai_entity_timestamps_serialize_as_integer_epoch_seconds() {
        let created_at = 1_700_000_000;
        let updated_at = 1_700_000_001;

        assert_integer_timestamps(
            serde_json::to_value(AiProviderConfig {
                created_at: Some(created_at),
                updated_at: Some(updated_at),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_integer_timestamps(
            serde_json::to_value(AiModel {
                created_at: Some(created_at),
                updated_at: Some(updated_at),
                ..Default::default()
            })
            .unwrap(),
        );
        let virtual_key = serde_json::to_value(AiVirtualKey {
            expires_at: Some(updated_at),
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            ..Default::default()
        })
        .unwrap();
        assert_integer_timestamps(virtual_key.clone());
        assert!(virtual_key["expires_at"].as_i64().is_some());
    }

    #[test]
    fn ai_model_costs_accept_strings_and_legacy_numbers() {
        let model: AiModel = serde_json::from_value(serde_json::json!({
            "input_cost": "1.250000000000",
            "output_cost": 2.5
        }))
        .unwrap();

        assert_eq!(
            model.input_cost,
            Some(Decimal::from_i128_with_scale(125, 2))
        );
        assert_eq!(
            model.output_cost,
            Some(Decimal::from_i128_with_scale(25, 1))
        );
    }

    #[test]
    fn ai_model_costs_serialize_as_fixed_scale_strings() {
        let value = serde_json::to_value(AiModel {
            input_cost: Some(Decimal::from_i128_with_scale(125, 2)),
            output_cost: Some(Decimal::ZERO),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(value["input_cost"], "1.250000000000");
        assert_eq!(value["output_cost"], "0.000000000000");
    }

    #[test]
    fn ai_model_costs_reject_lossy_negative_and_out_of_range_values() {
        for input_cost in [
            serde_json::json!("0.0000000000001"),
            serde_json::json!(-1),
            serde_json::json!("10000000000000000"),
        ] {
            let error = serde_json::from_value::<AiModel>(serde_json::json!({
                "input_cost": input_cost
            }))
            .unwrap_err();
            assert!(error.to_string().contains("模型价格"));
        }
    }

    #[test]
    fn ai_model_costs_allow_extra_trailing_zeroes_without_rounding() {
        let model: AiModel = serde_json::from_value(serde_json::json!({
            "input_cost": "1.2300000000000",
            "output_cost": "9999999999999999.9999999999990"
        }))
        .unwrap();
        let value = serde_json::to_value(model).unwrap();

        assert_eq!(value["input_cost"], "1.230000000000");
        assert_eq!(value["output_cost"], "9999999999999999.999999999999");
    }

    #[test]
    fn ai_virtual_key_budget_accepts_strings_and_legacy_numbers() {
        let key: AiVirtualKey = serde_json::from_value(serde_json::json!({
            "budget_limit": "100.500000000000",
            "budget_used": 83.25,
            "budget_pending_count": 2,
            "budget_unresolved_count": 1,
            "budget_accounting_revision": 7,
            "budget_checkpoint_tail_events": 3,
            "budget_accounting_state": "unresolved"
        }))
        .unwrap();

        assert_eq!(
            key.budget_limit,
            Some(Decimal::from_i128_with_scale(1005, 1))
        );
        assert_eq!(key.budget_used, Decimal::from_i128_with_scale(8325, 2));
        assert_eq!(key.budget_pending_count, 2);
        assert_eq!(key.budget_unresolved_count, 1);
        assert_eq!(key.budget_accounting_revision, 7);
        assert_eq!(key.budget_checkpoint_tail_events, 3);
        assert_eq!(key.budget_accounting_state, "unresolved");
    }

    #[test]
    fn ai_virtual_key_missing_accounting_fields_default_to_clean_zero_state() {
        let key: AiVirtualKey = serde_json::from_value(serde_json::json!({
            "name": "db-less-key"
        }))
        .unwrap();

        assert_eq!(key.budget_used, Decimal::ZERO);
        assert_eq!(key.budget_pending_count, 0);
        assert_eq!(key.budget_unresolved_count, 0);
        assert_eq!(key.budget_accounting_state, "clean");
    }

    #[test]
    fn ai_virtual_key_budget_serializes_as_fixed_scale_strings() {
        let value = serde_json::to_value(AiVirtualKey {
            budget_limit: Some(Decimal::from_i128_with_scale(1005, 1)),
            budget_used: Decimal::from_i128_with_scale(8325, 2),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(value["budget_limit"], "100.500000000000");
        assert_eq!(value["budget_used"], "83.250000000000");
    }

    #[test]
    fn ai_virtual_key_budget_rejects_lossy_negative_and_out_of_range_values() {
        for budget_limit in [
            serde_json::json!("0.0000000000001"),
            serde_json::json!(-1),
            serde_json::json!("10000000000000000"),
        ] {
            let error = serde_json::from_value::<AiVirtualKey>(serde_json::json!({
                "budget_limit": budget_limit
            }))
            .unwrap_err();
            assert!(error.to_string().contains("预算金额"));
        }
    }

    #[test]
    fn ai_virtual_key_budget_allows_extra_trailing_zeroes_without_rounding() {
        let key: AiVirtualKey = serde_json::from_value(serde_json::json!({
            "budget_limit": "1.2300000000000",
            "budget_used": "9999999999999999.9999999999990"
        }))
        .unwrap();
        let value = serde_json::to_value(key).unwrap();

        assert_eq!(value["budget_limit"], "1.230000000000");
        assert_eq!(value["budget_used"], "9999999999999999.999999999999");
    }

    #[test]
    fn dbless_models_accept_string_and_legacy_number_costs() {
        let string_id = Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap();
        let number_id = Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap();
        let store = Arc::new(DblessStore::new());
        store
            .load_from_json(&serde_json::json!({
                "_format_version": "3.0",
                "ai_models": [
                    {
                        "id": string_id,
                        "input_cost": "1.250000000000"
                    },
                    {
                        "id": number_id,
                        "input_cost": 2.5
                    }
                ]
            }))
            .unwrap();
        let dao = DblessDao::<AiModel>::new(store);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let string_model = dao
                .select(&PrimaryKey::Id(string_id))
                .await
                .unwrap()
                .expect("string cost model must load");
            let number_model = dao
                .select(&PrimaryKey::Id(number_id))
                .await
                .unwrap()
                .expect("legacy number cost model must load");

            assert_eq!(
                string_model.input_cost,
                Some(Decimal::from_i128_with_scale(125, 2))
            );
            assert_eq!(
                number_model.input_cost,
                Some(Decimal::from_i128_with_scale(25, 1))
            );
        });
    }
}
