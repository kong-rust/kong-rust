//! 版本化静态价表、Model 覆盖价与 Decimal 成本计算。

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::models::AiModel;

use super::model::{CostStatus, PriceSnapshot, PricingStatus, TokenField, UsageSource};

const BUILTIN_CATALOG: &str = include_str!("data/model_prices.json");
const MAX_NUMERIC_28_12: &str = "9999999999999999.999999999999";
const SONNET_5_PRICE_SWITCH: &str = "2026-09-01T00:00:00Z";

#[derive(Debug, Clone, Copy)]
struct RequiredPriceSpec {
    provider_type: &'static str,
    model_id: &'static str,
    aliases: &'static [&'static str],
    input: &'static str,
    output: &'static str,
    effective_from: &'static str,
    effective_to: Option<&'static str>,
    max_prompt_tokens: Option<i64>,
    source_url: &'static str,
}

const REQUIRED_PRICE_SPECS: &[RequiredPriceSpec] = &[
    RequiredPriceSpec {
        provider_type: "openai",
        model_id: "gpt-5.6-sol",
        aliases: &["gpt-5.6"],
        input: "5.000000000000",
        output: "30.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: Some(272_000),
        source_url: "https://developers.openai.com/api/docs/models",
    },
    RequiredPriceSpec {
        provider_type: "openai",
        model_id: "gpt-5.6-terra",
        aliases: &[],
        input: "2.500000000000",
        output: "15.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: Some(272_000),
        source_url: "https://developers.openai.com/api/docs/models",
    },
    RequiredPriceSpec {
        provider_type: "openai",
        model_id: "gpt-5.6-luna",
        aliases: &[],
        input: "1.000000000000",
        output: "6.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: Some(272_000),
        source_url: "https://developers.openai.com/api/docs/models",
    },
    RequiredPriceSpec {
        provider_type: "anthropic",
        model_id: "claude-fable-5",
        aliases: &[],
        input: "10.000000000000",
        output: "50.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    RequiredPriceSpec {
        provider_type: "anthropic",
        model_id: "claude-opus-4-8",
        aliases: &[],
        input: "5.000000000000",
        output: "25.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    RequiredPriceSpec {
        provider_type: "anthropic",
        model_id: "claude-sonnet-5",
        aliases: &[],
        input: "2.000000000000",
        output: "10.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: Some(SONNET_5_PRICE_SWITCH),
        max_prompt_tokens: None,
        source_url: "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    RequiredPriceSpec {
        provider_type: "anthropic",
        model_id: "claude-sonnet-5",
        aliases: &[],
        input: "3.000000000000",
        output: "15.000000000000",
        effective_from: SONNET_5_PRICE_SWITCH,
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    RequiredPriceSpec {
        provider_type: "anthropic",
        model_id: "claude-haiku-4-5-20251001",
        aliases: &["claude-haiku-4-5"],
        input: "1.000000000000",
        output: "5.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://platform.claude.com/docs/en/about-claude/pricing",
    },
    RequiredPriceSpec {
        provider_type: "gemini",
        model_id: "gemini-3.6-flash",
        aliases: &[],
        input: "1.500000000000",
        output: "7.500000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://ai.google.dev/gemini-api/docs/pricing",
    },
    RequiredPriceSpec {
        provider_type: "gemini",
        model_id: "gemini-3.5-flash",
        aliases: &[],
        input: "1.500000000000",
        output: "9.000000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://ai.google.dev/gemini-api/docs/pricing",
    },
    RequiredPriceSpec {
        provider_type: "gemini",
        model_id: "gemini-3.5-flash-lite",
        aliases: &[],
        input: "0.300000000000",
        output: "2.500000000000",
        effective_from: "2026-07-26T00:00:00Z",
        effective_to: None,
        max_prompt_tokens: None,
        source_url: "https://ai.google.dev/gemini-api/docs/pricing",
    },
];

#[derive(Debug, Clone)]
pub struct PriceCatalog {
    schema_version: u32,
    catalog_version: String,
    snapshot_date: NaiveDate,
    entries: Vec<PriceEntry>,
}

#[derive(Debug, Clone)]
struct PriceEntry {
    provider_type: String,
    model_ids: Vec<String>,
    aliases: Vec<String>,
    prefixes: Vec<String>,
    input: Decimal,
    output: Decimal,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
    conditions: PriceConditions,
    source_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PriceConditions {
    max_prompt_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    schema_version: u32,
    catalog_version: String,
    snapshot_date: String,
    entries: Vec<RawPriceEntry>,
}

#[derive(Debug, Deserialize)]
struct RawPriceEntry {
    provider_type: String,
    model_ids: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    prefixes: Vec<String>,
    input_usd_per_million: String,
    output_usd_per_million: String,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
    #[serde(default)]
    conditions: PriceConditions,
    source_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct ModelPriceOverrides {
    pub input: Option<Decimal>,
    pub output: Option<Decimal>,
    pub input_version: Option<String>,
    pub output_version: Option<String>,
    /// 兼容调用方的一体版本；方向版本未提供时才使用。
    pub version: Option<String>,
    pub effective_from: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub enum PriceDirection {
    Input,
    Output,
}

impl PriceDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// 生成按方向稳定、可追溯的 Model 覆盖价版本。
pub fn model_override_version(
    model_id: Option<&str>,
    updated_at: Option<DateTime<Utc>>,
    provider_type: &str,
    model: &str,
    direction: PriceDirection,
    rate: Decimal,
) -> String {
    let direction = direction.as_str();
    let canonical = format!(
        "{}\n{}\n{}\n{rate:.12}",
        provider_type.trim().to_ascii_lowercase(),
        model.trim(),
        direction,
    );
    let price_hash = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    let model_id = model_id.unwrap_or("inline");
    let revision = updated_at
        .map(|value| value.timestamp_millis().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!("model:{model_id}:{revision}:{direction}:{price_hash}")
}

/// 从实际选中的 Model 冻结覆盖价及其稳定版本。
pub fn model_price_overrides(
    model: &AiModel,
    provider_type: &str,
    actual_model: &str,
    fallback_effective_from: DateTime<Utc>,
) -> ModelPriceOverrides {
    let model_id = (!model.id.is_nil()).then(|| model.id.to_string());
    let effective_from = model
        .updated_at
        .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        .or_else(|| {
            model
                .created_at
                .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        })
        .unwrap_or(fallback_effective_from);
    ModelPriceOverrides {
        input: model.input_cost,
        output: model.output_cost,
        input_version: model.input_cost.map(|rate| {
            model_override_version(
                model_id.as_deref(),
                Some(effective_from),
                provider_type,
                actual_model,
                PriceDirection::Input,
                rate,
            )
        }),
        output_version: model.output_cost.map(|rate| {
            model_override_version(
                model_id.as_deref(),
                Some(effective_from),
                provider_type,
                actual_model,
                PriceDirection::Output,
                rate,
            )
        }),
        version: None,
        effective_from: Some(effective_from),
    }
}

#[derive(Debug, Clone, Default)]
pub struct PricingFeatures {
    pub provider_cache_tokens: bool,
    pub non_standard_service_tier: bool,
    pub built_in_tools: bool,
    pub non_text_modality: bool,
    pub additional_pricing: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedPricing {
    pub input: Option<PriceSnapshot>,
    pub output: Option<PriceSnapshot>,
    pub status: PricingStatus,
    pub unsupported_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CostComputation {
    pub cost_usd: Option<Decimal>,
    pub status: CostStatus,
    pub unavailable_reasons: Vec<String>,
}

impl PriceCatalog {
    pub fn builtin() -> Result<Self, String> {
        Self::from_json(BUILTIN_CATALOG)
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        let raw: RawCatalog =
            serde_json::from_str(json).map_err(|error| format!("价表 JSON 无效: {error}"))?;
        if raw.schema_version != 1 {
            return Err(format!(
                "不支持的价表 schema_version: {}",
                raw.schema_version
            ));
        }
        if raw.catalog_version.trim().is_empty() {
            return Err("catalog_version 不能为空".to_string());
        }
        let snapshot_date = NaiveDate::parse_from_str(&raw.snapshot_date, "%Y-%m-%d")
            .map_err(|error| format!("snapshot_date 无效: {error}"))?;
        if raw.entries.is_empty() {
            return Err("价表 entries 不能为空".to_string());
        }

        let mut entries = Vec::with_capacity(raw.entries.len());
        for raw_entry in raw.entries {
            validate_matchers(&raw_entry)?;
            if raw_entry.source_url.trim().is_empty() {
                return Err("价目 source_url 不能为空".to_string());
            }
            if raw_entry
                .effective_to
                .is_some_and(|end| end <= raw_entry.effective_from)
            {
                return Err("价目 effective_to 必须晚于 effective_from".to_string());
            }
            let input = parse_rate(&raw_entry.input_usd_per_million)?;
            let output = parse_rate(&raw_entry.output_usd_per_million)?;
            if raw_entry
                .conditions
                .max_prompt_tokens
                .is_some_and(|value| value < 0)
            {
                return Err("max_prompt_tokens 不能为负数".to_string());
            }
            entries.push(PriceEntry {
                provider_type: raw_entry.provider_type,
                model_ids: raw_entry.model_ids,
                aliases: raw_entry.aliases,
                prefixes: raw_entry.prefixes,
                input,
                output,
                effective_from: raw_entry.effective_from,
                effective_to: raw_entry.effective_to,
                conditions: raw_entry.conditions,
                source_url: raw_entry.source_url,
            });
        }

        validate_conflicts(&entries)?;
        validate_required_entries(&entries)?;

        Ok(Self {
            schema_version: raw.schema_version,
            catalog_version: raw.catalog_version,
            snapshot_date,
            entries,
        })
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn catalog_version(&self) -> &str {
        &self.catalog_version
    }

    pub fn snapshot_date(&self) -> NaiveDate {
        self.snapshot_date
    }

    // 这些参数共同描述一次请求的计价快照，保持展开可避免调用方遗漏价格口径。
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &self,
        provider_type: &str,
        model: &str,
        started_at: DateTime<Utc>,
        prompt_tokens: Option<i64>,
        overrides: &ModelPriceOverrides,
        features: &PricingFeatures,
        upstream_attempted: bool,
    ) -> ResolvedPricing {
        if !upstream_attempted {
            return ResolvedPricing {
                input: None,
                output: None,
                status: PricingStatus::NotApplicable,
                unsupported_reasons: Vec::new(),
            };
        }

        self.resolve_snapshot(
            provider_type,
            model,
            started_at,
            prompt_tokens,
            overrides,
            features,
        )
    }

    /// 在真正尝试上游之前冻结计价快照。
    ///
    /// 是否已经发生上游调用只影响最终成本状态，不应阻止预算 preflight
    /// 验证当前模型是否具备可安全结算的价格。
    pub fn resolve_snapshot(
        &self,
        provider_type: &str,
        model: &str,
        started_at: DateTime<Utc>,
        prompt_tokens: Option<i64>,
        overrides: &ModelPriceOverrides,
        features: &PricingFeatures,
    ) -> ResolvedPricing {
        let entry = self.find_entry(provider_type, model, started_at);
        let input_override_version = overrides
            .input_version
            .clone()
            .or_else(|| overrides.version.clone())
            .unwrap_or_else(|| "model_override:input".to_string());
        let output_override_version = overrides
            .output_version
            .clone()
            .or_else(|| overrides.version.clone())
            .unwrap_or_else(|| "model_override:output".to_string());
        let override_effective_from = overrides.effective_from.unwrap_or(started_at);
        let input = overrides
            .input
            .map(|rate| PriceSnapshot {
                usd_per_million: rate,
                source: "model_override".to_string(),
                version: input_override_version,
                snapshot_date: override_effective_from.date_naive(),
                effective_from: override_effective_from,
                effective_to: None,
            })
            .or_else(|| entry.map(|entry| self.snapshot(entry.input, entry)));
        let output = overrides
            .output
            .map(|rate| PriceSnapshot {
                usd_per_million: rate,
                source: "model_override".to_string(),
                version: output_override_version,
                snapshot_date: override_effective_from.date_naive(),
                effective_from: override_effective_from,
                effective_to: None,
            })
            .or_else(|| entry.map(|entry| self.snapshot(entry.output, entry)));

        let mut unsupported = BTreeSet::new();
        if features.provider_cache_tokens {
            unsupported.insert("provider_cache_pricing".to_string());
        }
        if features.non_standard_service_tier {
            unsupported.insert("service_tier_pricing".to_string());
        }
        if features.built_in_tools {
            unsupported.insert("built_in_tool_pricing".to_string());
        }
        if features.non_text_modality {
            unsupported.insert("non_text_modality_pricing".to_string());
        }
        if features.additional_pricing {
            unsupported.insert("additional_pricing".to_string());
        }
        let has_full_override = overrides.input.is_some() && overrides.output.is_some();
        if !has_full_override
            && entry
                .and_then(|entry| entry.conditions.max_prompt_tokens)
                .zip(prompt_tokens)
                .is_some_and(|(maximum, actual)| actual > maximum)
        {
            unsupported.insert("long_context_pricing".to_string());
        }

        let status = if !unsupported.is_empty() {
            PricingStatus::Unsupported
        } else if input.is_some() && output.is_some() {
            PricingStatus::Matched
        } else {
            PricingStatus::Unmatched
        };
        ResolvedPricing {
            input,
            output,
            status,
            unsupported_reasons: unsupported.into_iter().collect(),
        }
    }

    fn find_entry(
        &self,
        provider_type: &str,
        model: &str,
        started_at: DateTime<Utc>,
    ) -> Option<&PriceEntry> {
        let active_entries = || {
            self.entries.iter().filter(|entry| {
                entry.provider_type == provider_type
                    && started_at >= entry.effective_from
                    && entry
                        .effective_to
                        .is_none_or(|effective_to| started_at < effective_to)
            })
        };
        if let Some(entry) = active_entries()
            .find(|entry| entry.model_ids.iter().any(|candidate| candidate == model))
        {
            return Some(entry);
        }
        if let Some(entry) =
            active_entries().find(|entry| entry.aliases.iter().any(|candidate| candidate == model))
        {
            return Some(entry);
        }
        let mut prefix_match: Option<(&PriceEntry, usize)> = None;
        for entry in active_entries() {
            for prefix in &entry.prefixes {
                if model.starts_with(prefix)
                    && prefix_match
                        .as_ref()
                        .is_none_or(|(_, current)| prefix.len() > *current)
                {
                    prefix_match = Some((entry, prefix.len()));
                }
            }
        }
        prefix_match.map(|(entry, _)| entry)
    }

    fn snapshot(&self, rate: Decimal, entry: &PriceEntry) -> PriceSnapshot {
        PriceSnapshot {
            usd_per_million: rate,
            source: "builtin".to_string(),
            version: self.catalog_version.clone(),
            snapshot_date: self.snapshot_date,
            effective_from: entry.effective_from,
            effective_to: entry.effective_to,
        }
    }

    pub fn source_urls(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.source_url.as_str())
    }

    pub fn max_prompt_tokens(
        &self,
        provider_type: &str,
        model: &str,
        started_at: DateTime<Utc>,
    ) -> Option<i64> {
        self.find_entry(provider_type, model, started_at)
            .and_then(|entry| entry.conditions.max_prompt_tokens)
    }
}

pub fn calculate_cost(
    pricing: &ResolvedPricing,
    prompt: Option<TokenField>,
    completion: Option<TokenField>,
    usage_source: UsageSource,
    upstream_attempted: bool,
    invalid_provider_usage: bool,
) -> CostComputation {
    if !upstream_attempted {
        return CostComputation {
            cost_usd: Some(Decimal::ZERO),
            status: CostStatus::NotIncurred,
            unavailable_reasons: Vec::new(),
        };
    }

    let mut reasons = BTreeSet::new();
    if invalid_provider_usage {
        reasons.insert("invalid_provider_usage".to_string());
    }
    if pricing.input.is_none() {
        reasons.insert("unmatched_input_price".to_string());
    }
    if pricing.output.is_none() {
        reasons.insert("unmatched_output_price".to_string());
    }
    if pricing.status == PricingStatus::Unsupported {
        reasons.insert("unsupported_pricing".to_string());
    }
    if prompt.is_none() {
        reasons.insert("missing_prompt_usage".to_string());
    }
    if completion.is_none() {
        reasons.insert("missing_completion_usage".to_string());
    }
    if !reasons.is_empty() {
        return CostComputation {
            cost_usd: None,
            status: CostStatus::Unavailable,
            unavailable_reasons: reasons.into_iter().collect(),
        };
    }

    let input_rate = pricing.input.as_ref().unwrap().usd_per_million;
    let output_rate = pricing.output.as_ref().unwrap().usd_per_million;
    let prompt = Decimal::from(prompt.unwrap().value);
    let completion = Decimal::from(completion.unwrap().value);
    let cost = prompt
        .checked_mul(input_rate)
        .and_then(|input| {
            completion
                .checked_mul(output_rate)
                .and_then(|output| input.checked_add(output))
        })
        .and_then(|total| total.checked_div(Decimal::from(1_000_000u64)))
        .map(|value| value.round_dp_with_strategy(12, RoundingStrategy::MidpointAwayFromZero))
        .filter(|value| fits_numeric_28_12(*value));

    let Some(cost_usd) = cost else {
        return CostComputation {
            cost_usd: None,
            status: CostStatus::Unavailable,
            unavailable_reasons: vec!["arithmetic_overflow".to_string()],
        };
    };

    CostComputation {
        cost_usd: Some(cost_usd),
        status: if usage_source == UsageSource::Provider {
            CostStatus::Calculated
        } else {
            CostStatus::Estimated
        },
        unavailable_reasons: Vec::new(),
    }
}

fn parse_rate(value: &str) -> Result<Decimal, String> {
    let parsed = Decimal::from_str(value)
        .map_err(|error| format!("无效的 Decimal 价格 {value}: {error}"))?;
    if !fits_numeric_28_12(parsed) {
        return Err(format!("价格不在 NUMERIC(28,12) 范围内: {value}"));
    }
    Ok(parsed)
}

pub fn fits_numeric_28_12(value: Decimal) -> bool {
    value >= Decimal::ZERO
        && value.scale() <= 12
        && Decimal::from_str(MAX_NUMERIC_28_12).is_ok_and(|maximum| value <= maximum)
}

fn validate_matchers(entry: &RawPriceEntry) -> Result<(), String> {
    if entry.provider_type.trim().is_empty() {
        return Err("provider_type 不能为空".to_string());
    }
    if entry.model_ids.is_empty() && entry.aliases.is_empty() && entry.prefixes.is_empty() {
        return Err("每条价目至少需要一个模型匹配器".to_string());
    }
    for alias in &entry.aliases {
        if alias.contains('*') || alias.eq_ignore_ascii_case("latest") || alias.ends_with("-latest")
        {
            return Err(format!("不允许动态或 wildcard alias: {alias}"));
        }
    }
    if entry.prefixes.iter().any(|prefix| prefix.is_empty()) {
        return Err("prefix 不能为空".to_string());
    }
    Ok(())
}

fn validate_conflicts(entries: &[PriceEntry]) -> Result<(), String> {
    let mut matchers: BTreeMap<(String, String), Vec<(&'static str, &PriceEntry)>> =
        BTreeMap::new();
    for entry in entries {
        for model in &entry.model_ids {
            matchers
                .entry((entry.provider_type.clone(), model.clone()))
                .or_default()
                .push(("id", entry));
        }
        for alias in &entry.aliases {
            matchers
                .entry((entry.provider_type.clone(), alias.clone()))
                .or_default()
                .push(("alias", entry));
        }
        for prefix in &entry.prefixes {
            matchers
                .entry((entry.provider_type.clone(), prefix.clone()))
                .or_default()
                .push(("prefix", entry));
        }
    }
    for ((provider, matcher), candidates) in matchers {
        for (index, (left_kind, left)) in candidates.iter().enumerate() {
            for (right_kind, right) in candidates.iter().skip(index + 1) {
                if periods_overlap(left, right) {
                    return Err(format!(
                        "{provider} 的 {left_kind}/{right_kind} 匹配器 {matcher} 在有效期内冲突"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn periods_overlap(left: &PriceEntry, right: &PriceEntry) -> bool {
    left.effective_from < right.effective_to.unwrap_or(DateTime::<Utc>::MAX_UTC)
        && right.effective_from < left.effective_to.unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn validate_required_entries(entries: &[PriceEntry]) -> Result<(), String> {
    for required in REQUIRED_PRICE_SPECS {
        let effective_from = parse_required_timestamp(required.effective_from)?;
        let effective_to = required
            .effective_to
            .map(parse_required_timestamp)
            .transpose()?;
        let input = parse_rate(required.input)?;
        let output = parse_rate(required.output)?;
        let entry = entries
            .iter()
            .find(|entry| {
                entry.provider_type == required.provider_type
                    && entry
                        .model_ids
                        .iter()
                        .any(|model| model == required.model_id)
                    && entry.effective_from == effective_from
            })
            .ok_or_else(|| {
                format!(
                    "内置价表缺少必需价目 {}/{}@{}",
                    required.provider_type, required.model_id, required.effective_from
                )
            })?;

        if entry.input != input || entry.output != output {
            return Err(format!(
                "内置价目 {}/{}@{} 单价必须为 input={}、output={}",
                required.provider_type,
                required.model_id,
                required.effective_from,
                required.input,
                required.output
            ));
        }
        if entry.effective_to != effective_to {
            return Err(format!(
                "内置价目 {}/{}@{} 的 effective_to 必须为 {}",
                required.provider_type,
                required.model_id,
                required.effective_from,
                required.effective_to.unwrap_or("null")
            ));
        }
        if entry.conditions.max_prompt_tokens != required.max_prompt_tokens {
            return Err(format!(
                "内置价目 {}/{}@{} 的 max_prompt_tokens 必须为 {:?}",
                required.provider_type,
                required.model_id,
                required.effective_from,
                required.max_prompt_tokens
            ));
        }
        for alias in required.aliases {
            if !entry.aliases.iter().any(|value| value == alias) {
                return Err(format!(
                    "内置价目 {}/{} 缺少必需 alias {}",
                    required.provider_type, required.model_id, alias
                ));
            }
        }
        if !entry.prefixes.is_empty() {
            return Err(format!(
                "首版内置价目 {}/{} 不允许 prefix",
                required.provider_type, required.model_id
            ));
        }
        if entry.source_url != required.source_url {
            return Err(format!(
                "内置价目 {}/{} 的 source_url 必须为 {}",
                required.provider_type, required.model_id, required.source_url
            ));
        }
    }

    let mut sonnet = entries
        .iter()
        .filter(|entry| {
            entry.provider_type == "anthropic"
                && entry
                    .model_ids
                    .iter()
                    .any(|model| model == "claude-sonnet-5")
        })
        .collect::<Vec<_>>();
    if sonnet.len() != 2 {
        return Err("claude-sonnet-5 必须包含切换边界前后两条价目".to_string());
    }
    sonnet.sort_by_key(|entry| entry.effective_from);
    let switch = parse_required_timestamp(SONNET_5_PRICE_SWITCH)?;
    if sonnet[0].effective_to != Some(switch)
        || sonnet[1].effective_from != switch
        || sonnet[0].effective_to != Some(sonnet[1].effective_from)
    {
        return Err(format!(
            "claude-sonnet-5 必须在 {SONNET_5_PRICE_SWITCH} 无缝切换"
        ));
    }
    Ok(())
}

fn parse_required_timestamp(value: &str) -> Result<DateTime<Utc>, String> {
    value
        .parse()
        .map_err(|error| format!("内置价表校验时间 {value} 无效: {error}"))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::{Map, Value};

    use super::*;
    use crate::usage::model::{TokenFieldSource, UsageSource};

    fn started_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap()
    }

    fn builtin_catalog_value() -> Value {
        serde_json::from_str(BUILTIN_CATALOG).unwrap()
    }

    fn required_entry_mut<'a>(
        catalog: &'a mut Value,
        required: &RequiredPriceSpec,
    ) -> &'a mut Map<String, Value> {
        catalog["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| {
                entry["provider_type"] == required.provider_type
                    && entry["model_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|id| id == required.model_id))
                    && entry["effective_from"] == required.effective_from
            })
            .unwrap()
            .as_object_mut()
            .unwrap()
    }

    fn assert_tampered_catalog_fails(catalog: Value, case: &str) {
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(
            PriceCatalog::from_json(&json).is_err(),
            "篡改后的价表必须启动失败: {case}"
        );
    }

    #[test]
    fn builtin_catalog_has_required_versioned_entries() {
        let catalog = PriceCatalog::builtin().unwrap();
        assert_eq!(catalog.schema_version(), 1);
        assert_eq!(catalog.catalog_version(), "2026-07-26.1");
        assert!(catalog.source_urls().all(|url| url.starts_with("https://")));
    }

    #[test]
    fn required_catalog_rates_are_fail_fast_validated() {
        for required in REQUIRED_PRICE_SPECS {
            for field in ["input_usd_per_million", "output_usd_per_million"] {
                let mut catalog = builtin_catalog_value();
                required_entry_mut(&mut catalog, required).insert(
                    field.to_string(),
                    Value::String("999.000000000000".to_string()),
                );
                assert_tampered_catalog_fails(
                    catalog,
                    &format!(
                        "{}/{}@{} {field}",
                        required.provider_type, required.model_id, required.effective_from
                    ),
                );
            }
        }
    }

    #[test]
    fn required_catalog_aliases_and_sources_are_fail_fast_validated() {
        for required in REQUIRED_PRICE_SPECS
            .iter()
            .filter(|required| !required.aliases.is_empty())
        {
            let mut catalog = builtin_catalog_value();
            required_entry_mut(&mut catalog, required)
                .insert("aliases".to_string(), Value::Array(Vec::new()));
            assert_tampered_catalog_fails(
                catalog,
                &format!("{}/{} aliases", required.provider_type, required.model_id),
            );
        }

        let required = &REQUIRED_PRICE_SPECS[0];
        let mut catalog = builtin_catalog_value();
        required_entry_mut(&mut catalog, required).insert(
            "source_url".to_string(),
            Value::String("https://example.invalid/pricing".to_string()),
        );
        assert_tampered_catalog_fails(catalog, "source_url");
    }

    #[test]
    fn all_gpt_56_prices_require_the_272000_prompt_limit() {
        for required in REQUIRED_PRICE_SPECS
            .iter()
            .filter(|required| required.provider_type == "openai")
        {
            let mut catalog = builtin_catalog_value();
            required_entry_mut(&mut catalog, required)
                .get_mut("conditions")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    "max_prompt_tokens".to_string(),
                    Value::Number(272_001.into()),
                );
            assert_tampered_catalog_fails(
                catalog,
                &format!(
                    "{}/{} max_prompt_tokens",
                    required.provider_type, required.model_id
                ),
            );
        }
    }

    #[test]
    fn sonnet_5_requires_the_exact_seamless_switch_boundary() {
        let before_switch = REQUIRED_PRICE_SPECS
            .iter()
            .find(|required| {
                required.model_id == "claude-sonnet-5"
                    && required.effective_to == Some(SONNET_5_PRICE_SWITCH)
            })
            .unwrap();
        let mut catalog = builtin_catalog_value();
        required_entry_mut(&mut catalog, before_switch).insert(
            "effective_to".to_string(),
            Value::String("2026-08-31T00:00:00Z".to_string()),
        );
        assert_tampered_catalog_fails(catalog, "claude-sonnet-5 switch gap");

        let after_switch = REQUIRED_PRICE_SPECS
            .iter()
            .find(|required| {
                required.model_id == "claude-sonnet-5"
                    && required.effective_from == SONNET_5_PRICE_SWITCH
            })
            .unwrap();
        let mut catalog = builtin_catalog_value();
        required_entry_mut(&mut catalog, after_switch).insert(
            "effective_from".to_string(),
            Value::String("2026-09-02T00:00:00Z".to_string()),
        );
        assert_tampered_catalog_fails(catalog, "claude-sonnet-5 switch start");
    }

    #[test]
    fn sonnet_5_resolves_the_correct_price_on_both_sides_of_the_switch() {
        let catalog = PriceCatalog::builtin().unwrap();
        let switch = parse_required_timestamp(SONNET_5_PRICE_SWITCH).unwrap();
        let before = catalog.resolve(
            "anthropic",
            "claude-sonnet-5",
            switch - chrono::Duration::nanoseconds(1),
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
            true,
        );
        let after = catalog.resolve(
            "anthropic",
            "claude-sonnet-5",
            switch,
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
            true,
        );

        assert_eq!(before.input.unwrap().usd_per_million, Decimal::new(2, 0));
        assert_eq!(before.output.unwrap().usd_per_million, Decimal::new(10, 0));
        assert_eq!(after.input.unwrap().usd_per_million, Decimal::new(3, 0));
        assert_eq!(after.output.unwrap().usd_per_million, Decimal::new(15, 0));
    }

    #[test]
    fn all_gpt_56_prices_switch_to_unsupported_after_272000_tokens() {
        let catalog = PriceCatalog::builtin().unwrap();
        for required in REQUIRED_PRICE_SPECS
            .iter()
            .filter(|required| required.provider_type == "openai")
        {
            let supported = catalog.resolve(
                required.provider_type,
                required.model_id,
                started_at(),
                Some(272_000),
                &ModelPriceOverrides::default(),
                &PricingFeatures::default(),
                true,
            );
            let unsupported = catalog.resolve(
                required.provider_type,
                required.model_id,
                started_at(),
                Some(272_001),
                &ModelPriceOverrides::default(),
                &PricingFeatures::default(),
                true,
            );

            assert_eq!(supported.status, PricingStatus::Matched);
            assert_eq!(unsupported.status, PricingStatus::Unsupported);
            assert_eq!(unsupported.unsupported_reasons, ["long_context_pricing"]);
        }
    }

    #[test]
    fn openai_compat_does_not_inherit_openai_price() {
        let catalog = PriceCatalog::builtin().unwrap();
        let pricing = catalog.resolve(
            "openai_compat",
            "gpt-5.6-sol",
            started_at(),
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
            true,
        );
        assert_eq!(pricing.status, PricingStatus::Unmatched);
        assert!(pricing.input.is_none());
    }

    #[test]
    fn pre_dispatch_snapshot_resolves_without_claiming_cost_was_incurred() {
        let catalog = PriceCatalog::builtin().unwrap();
        let snapshot = catalog.resolve_snapshot(
            "openai",
            "gpt-5.6-sol",
            started_at(),
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
        );
        let final_pricing = catalog.resolve(
            "openai",
            "gpt-5.6-sol",
            started_at(),
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
            false,
        );

        assert_eq!(snapshot.status, PricingStatus::Matched);
        assert!(snapshot.input.is_some());
        assert!(snapshot.output.is_some());
        assert_eq!(final_pricing.status, PricingStatus::NotApplicable);
        assert!(final_pricing.input.is_none());
    }

    #[test]
    fn model_override_zero_is_a_real_price() {
        let catalog = PriceCatalog::builtin().unwrap();
        let pricing = catalog.resolve(
            "openai_compat",
            "private-model",
            started_at(),
            Some(100),
            &ModelPriceOverrides {
                input: Some(Decimal::ZERO),
                output: Some(Decimal::ZERO),
                ..Default::default()
            },
            &PricingFeatures::default(),
            true,
        );
        assert_eq!(pricing.status, PricingStatus::Matched);
        assert_eq!(pricing.input.unwrap().usd_per_million, Decimal::ZERO);
    }

    #[test]
    fn model_override_versions_are_stable_and_direction_specific() {
        let updated_at = started_at();
        let input = model_override_version(
            Some("model-id"),
            Some(updated_at),
            "OpenAI",
            " gpt-5.6-sol ",
            PriceDirection::Input,
            Decimal::new(125, 2),
        );
        let same_input = model_override_version(
            Some("model-id"),
            Some(updated_at),
            "openai",
            "gpt-5.6-sol",
            PriceDirection::Input,
            Decimal::new(1250, 3),
        );
        let output = model_override_version(
            Some("model-id"),
            Some(updated_at),
            "openai",
            "gpt-5.6-sol",
            PriceDirection::Output,
            Decimal::new(125, 2),
        );
        assert_eq!(input, same_input);
        assert_ne!(input, output);
        assert!(input.starts_with("model:model-id:"));
    }

    #[test]
    fn gpt_long_context_and_cache_usage_are_unsupported() {
        let catalog = PriceCatalog::builtin().unwrap();
        let pricing = catalog.resolve(
            "openai",
            "gpt-5.6-sol",
            started_at(),
            Some(272_001),
            &ModelPriceOverrides::default(),
            &PricingFeatures {
                provider_cache_tokens: true,
                ..Default::default()
            },
            true,
        );
        assert_eq!(pricing.status, PricingStatus::Unsupported);
        assert_eq!(
            pricing.unsupported_reasons,
            ["long_context_pricing", "provider_cache_pricing"]
        );
    }

    #[test]
    fn cost_is_exact_and_uses_usage_lineage() {
        let catalog = PriceCatalog::builtin().unwrap();
        let pricing = catalog.resolve(
            "openai",
            "gpt-5.6-sol",
            started_at(),
            Some(100),
            &ModelPriceOverrides::default(),
            &PricingFeatures::default(),
            true,
        );
        let cost = calculate_cost(
            &pricing,
            Some(TokenField {
                value: 100,
                source: TokenFieldSource::Provider,
                derived: false,
            }),
            Some(TokenField {
                value: 20,
                source: TokenFieldSource::Provider,
                derived: false,
            }),
            UsageSource::Provider,
            true,
            false,
        );
        assert_eq!(cost.status, CostStatus::Calculated);
        assert_eq!(cost.cost_usd.unwrap().to_string(), "0.001100000000");
    }
}
