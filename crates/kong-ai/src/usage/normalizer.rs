//! Provider usage 的受检归一化与字段来源合并。

use std::collections::BTreeSet;

use serde_json::Value;

use super::model::{TokenField, TokenFieldSource, UsageSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObservationKind {
    #[default]
    Snapshot,
    PartialUpdate,
}

#[derive(Debug, Clone, Default)]
pub struct UsageObservation {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub kind: ObservationKind,
}

#[derive(Debug, Clone)]
pub struct NormalizedUsage {
    pub prompt_tokens: Option<TokenField>,
    pub completion_tokens: Option<TokenField>,
    pub total_tokens: Option<TokenField>,
    pub reasoning_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub source: UsageSource,
    pub unavailable_reasons: Vec<String>,
    pub invalid_provider_usage: bool,
}

#[derive(Debug, Clone, Default)]
pub struct UsageAccumulator {
    prompt_tokens: Option<TokenField>,
    completion_tokens: Option<TokenField>,
    total_tokens: Option<TokenField>,
    reasoning_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_write_input_tokens: Option<i64>,
    invalid: bool,
}

impl UsageAccumulator {
    pub fn observe_provider(&mut self, observation: UsageObservation) {
        self.observe(observation, TokenFieldSource::Provider);
    }

    pub fn observe_estimated(&mut self, observation: UsageObservation) {
        self.observe(observation, TokenFieldSource::Estimated);
    }

    fn observe(&mut self, observation: UsageObservation, source: TokenFieldSource) {
        if [
            observation.prompt_tokens,
            observation.completion_tokens,
            observation.total_tokens,
            observation.reasoning_tokens,
            observation.cache_read_input_tokens,
            observation.cache_write_input_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|value| value < 0)
        {
            self.invalid = true;
            return;
        }

        merge_field(&mut self.prompt_tokens, observation.prompt_tokens, source);
        merge_field(
            &mut self.completion_tokens,
            observation.completion_tokens,
            source,
        );
        merge_field(&mut self.total_tokens, observation.total_tokens, source);
        merge_breakdown(
            &mut self.reasoning_tokens,
            observation.reasoning_tokens,
            source,
        );
        merge_breakdown(
            &mut self.cache_read_input_tokens,
            observation.cache_read_input_tokens,
            source,
        );
        merge_breakdown(
            &mut self.cache_write_input_tokens,
            observation.cache_write_input_tokens,
            source,
        );
    }

    pub fn finish(mut self, upstream_attempted: bool, response_complete: bool) -> NormalizedUsage {
        if self.invalid {
            return NormalizedUsage {
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                reasoning_tokens: None,
                cache_read_input_tokens: None,
                cache_write_input_tokens: None,
                source: UsageSource::Unavailable,
                unavailable_reasons: vec!["invalid_token_value".to_string()],
                invalid_provider_usage: upstream_attempted,
            };
        }

        if !upstream_attempted {
            return NormalizedUsage {
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                reasoning_tokens: None,
                cache_read_input_tokens: None,
                cache_write_input_tokens: None,
                source: UsageSource::Unavailable,
                unavailable_reasons: vec!["not_attempted".to_string()],
                invalid_provider_usage: false,
            };
        }

        if self.total_tokens.is_none() {
            if let (Some(prompt), Some(completion)) = (self.prompt_tokens, self.completion_tokens) {
                match prompt.value.checked_add(completion.value) {
                    Some(value) => {
                        self.total_tokens = Some(TokenField {
                            value,
                            source: combine_source(prompt.source, completion.source),
                            derived: true,
                        });
                    }
                    None => {
                        return NormalizedUsage {
                            prompt_tokens: None,
                            completion_tokens: None,
                            total_tokens: None,
                            reasoning_tokens: None,
                            cache_read_input_tokens: None,
                            cache_write_input_tokens: None,
                            source: UsageSource::Unavailable,
                            unavailable_reasons: vec!["invalid_token_value".to_string()],
                            invalid_provider_usage: true,
                        };
                    }
                }
            }
        }

        let source = overall_source([
            self.prompt_tokens,
            self.completion_tokens,
            self.total_tokens,
        ]);
        let mut reasons = BTreeSet::new();
        if source == UsageSource::Unavailable {
            reasons.insert("provider_usage_missing".to_string());
            if !response_complete {
                reasons.insert("incomplete_response".to_string());
            }
            reasons.insert("estimation_unavailable".to_string());
        }

        NormalizedUsage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_write_input_tokens: self.cache_write_input_tokens,
            source,
            unavailable_reasons: reasons.into_iter().collect(),
            invalid_provider_usage: false,
        }
    }
}

fn merge_field(target: &mut Option<TokenField>, value: Option<i64>, source: TokenFieldSource) {
    let Some(value) = value else {
        return;
    };
    if source == TokenFieldSource::Estimated
        && target.is_some_and(|current| current.source == TokenFieldSource::Provider)
    {
        return;
    }
    *target = Some(TokenField {
        value,
        source,
        derived: false,
    });
}

fn merge_breakdown(target: &mut Option<i64>, value: Option<i64>, source: TokenFieldSource) {
    if value.is_some() && (source == TokenFieldSource::Provider || target.is_none()) {
        *target = value;
    }
}

fn combine_source(left: TokenFieldSource, right: TokenFieldSource) -> TokenFieldSource {
    if left == right {
        left
    } else {
        TokenFieldSource::Mixed
    }
}

fn overall_source(fields: [Option<TokenField>; 3]) -> UsageSource {
    let sources: Vec<_> = fields
        .into_iter()
        .flatten()
        .map(|field| field.source)
        .collect();
    if sources.is_empty() {
        return UsageSource::Unavailable;
    }
    let has_provider = sources.contains(&TokenFieldSource::Provider);
    let has_estimated = sources.contains(&TokenFieldSource::Estimated);
    let has_mixed = sources.contains(&TokenFieldSource::Mixed);
    match (has_provider, has_estimated, has_mixed) {
        (true, false, false) => UsageSource::Provider,
        (false, true, false) => UsageSource::Estimated,
        _ => UsageSource::Mixed,
    }
}

/// 将 OpenAI Chat/Responses usage 映射为统一 observation。
pub fn openai_observation(usage: &Value) -> Result<UsageObservation, &'static str> {
    if !usage.is_object() {
        return Err("invalid_token_value");
    }
    let prompt = optional_i64(usage, &["prompt_tokens", "input_tokens"])?;
    let completion = optional_i64(usage, &["completion_tokens", "output_tokens"])?;
    let total = optional_i64(usage, &["total_tokens"])?;
    let cached = nested_optional_i64(
        usage,
        &[
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
        ],
    )?;
    let reasoning = nested_optional_i64(
        usage,
        &[
            &["completion_tokens_details", "reasoning_tokens"],
            &["output_tokens_details", "reasoning_tokens"],
        ],
    )?;
    let cache_write = optional_i64(
        usage,
        &["cache_write_input_tokens", "cache_creation_input_tokens"],
    )?;
    Ok(UsageObservation {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        reasoning_tokens: reasoning,
        cache_read_input_tokens: cached,
        cache_write_input_tokens: cache_write,
        kind: ObservationKind::Snapshot,
    })
}

/// 将 Anthropic usage 映射为统一 observation。
pub fn anthropic_observation(usage: &Value) -> Result<UsageObservation, &'static str> {
    if !usage.is_object() {
        return Err("invalid_token_value");
    }
    let input = optional_i64(usage, &["input_tokens"])?;
    let cache_write = optional_i64(usage, &["cache_creation_input_tokens"])?;
    let cache_read = optional_i64(usage, &["cache_read_input_tokens"])?;
    let prompt = match input {
        Some(value) => Some(
            value
                .checked_add(cache_write.unwrap_or(0))
                .and_then(|value| value.checked_add(cache_read.unwrap_or(0)))
                .ok_or("invalid_token_value")?,
        ),
        None => None,
    };
    Ok(UsageObservation {
        prompt_tokens: prompt,
        completion_tokens: optional_i64(usage, &["output_tokens"])?,
        total_tokens: None,
        reasoning_tokens: None,
        cache_read_input_tokens: cache_read,
        cache_write_input_tokens: cache_write,
        kind: ObservationKind::Snapshot,
    })
}

/// 将 Gemini usageMetadata 映射为统一 observation。
pub fn gemini_observation(usage: &Value) -> Result<UsageObservation, &'static str> {
    if !usage.is_object() {
        return Err("invalid_token_value");
    }
    let candidates = optional_i64(usage, &["candidatesTokenCount"])?;
    let thoughts = optional_i64(usage, &["thoughtsTokenCount"])?;
    let completion = match candidates {
        Some(value) => Some(
            value
                .checked_add(thoughts.unwrap_or(0))
                .ok_or("invalid_token_value")?,
        ),
        None => None,
    };
    Ok(UsageObservation {
        prompt_tokens: optional_i64(usage, &["promptTokenCount"])?,
        completion_tokens: completion,
        total_tokens: optional_i64(usage, &["totalTokenCount"])?,
        reasoning_tokens: thoughts,
        cache_read_input_tokens: optional_i64(usage, &["cachedContentTokenCount"])?,
        cache_write_input_tokens: None,
        kind: ObservationKind::Snapshot,
    })
}

fn optional_i64(value: &Value, keys: &[&str]) -> Result<Option<i64>, &'static str> {
    for key in keys {
        if let Some(candidate) = value.get(key) {
            if candidate.is_null() {
                continue;
            }
            return json_i64(candidate).map(Some);
        }
    }
    Ok(None)
}

fn nested_optional_i64(value: &Value, paths: &[&[&str]]) -> Result<Option<i64>, &'static str> {
    for path in paths {
        let mut current = value;
        let mut found = true;
        for key in *path {
            let Some(next) = current.get(*key) else {
                found = false;
                break;
            };
            current = next;
        }
        if found && !current.is_null() {
            return json_i64(current).map(Some);
        }
    }
    Ok(None)
}

fn json_i64(value: &Value) -> Result<i64, &'static str> {
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).map_err(|_| "invalid_token_value");
    }
    if let Some(value) = value.as_i64() {
        return (value >= 0).then_some(value).ok_or("invalid_token_value");
    }
    Err("invalid_token_value")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn official_total_is_not_rederived() {
        let mut accumulator = UsageAccumulator::default();
        accumulator.observe_provider(UsageObservation {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(99),
            ..Default::default()
        });
        let result = accumulator.finish(true, true);
        assert_eq!(result.total_tokens.unwrap().value, 99);
        assert!(!result.total_tokens.unwrap().derived);
    }

    #[test]
    fn derived_total_preserves_mixed_lineage() {
        let mut accumulator = UsageAccumulator::default();
        accumulator.observe_provider(UsageObservation {
            prompt_tokens: Some(10),
            ..Default::default()
        });
        accumulator.observe_estimated(UsageObservation {
            completion_tokens: Some(5),
            ..Default::default()
        });
        let result = accumulator.finish(true, true);
        assert_eq!(result.source, UsageSource::Mixed);
        assert_eq!(result.total_tokens.unwrap().source, TokenFieldSource::Mixed);
    }

    #[test]
    fn invalid_value_invalidates_the_whole_usage_fact() {
        let mut accumulator = UsageAccumulator::default();
        accumulator.observe_provider(UsageObservation {
            prompt_tokens: Some(-1),
            ..Default::default()
        });
        let result = accumulator.finish(true, true);
        assert_eq!(result.source, UsageSource::Unavailable);
        assert_eq!(result.unavailable_reasons, ["invalid_token_value"]);
        assert!(result.invalid_provider_usage);
    }

    #[test]
    fn provider_specific_breakdowns_are_normalized() {
        let anthropic = anthropic_observation(&json!({
            "input_tokens": 10,
            "cache_creation_input_tokens": 2,
            "cache_read_input_tokens": 3,
            "output_tokens": 4
        }))
        .unwrap();
        assert_eq!(anthropic.prompt_tokens, Some(15));
        assert_eq!(anthropic.completion_tokens, Some(4));

        let gemini = gemini_observation(&json!({
            "promptTokenCount": 10,
            "candidatesTokenCount": 4,
            "thoughtsTokenCount": 2,
            "totalTokenCount": 16
        }))
        .unwrap();
        assert_eq!(gemini.completion_tokens, Some(6));
        assert_eq!(gemini.reasoning_tokens, Some(2));
    }
}
