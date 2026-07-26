//! AI 请求生命周期观察器：在热路径内组装不可变事实并非阻塞入队。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use kong_core::traits::{RequestCtx, RequestTerminationHint, RequestTransportSource};
use kong_plugin_system::{RequestLifecycleObserver, ResolvedPlugin};
use uuid::Uuid;

use crate::auth::AiAuthContext;
use crate::models::{AiModel, AiProviderConfig};
use crate::plugins::context::AiRequestState;

use super::cursor::normalize_millis;
use super::model::{
    AiUsageFact, AiUsageOutcome, CacheStatus, FrozenPricingSnapshot, PricingStatus,
};
use super::normalizer::{UsageAccumulator, UsageObservation};
use super::pricing::{
    calculate_cost, model_price_overrides, ModelPriceOverrides, PriceCatalog, PricingFeatures,
    ResolvedPricing,
};
use super::writer::AiUsageWriter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamTerminalState {
    #[default]
    NotStreaming,
    Pending,
    Complete,
    ProviderFailed,
}

#[derive(Debug, Clone)]
pub struct AiProxyUsageConfigSnapshot {
    pub log_statistics: bool,
}

#[derive(Debug, Clone)]
pub struct AiUsageContext {
    pub fact_id: Uuid,
    pub ai_proxy_config: AiProxyUsageConfigSnapshot,
    pub requested_model: Option<String>,
    pub model_group: Option<String>,
    pub model_id: Option<Uuid>,
    pub model_name: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_name: Option<String>,
    pub provider_type: Option<String>,
    pub stream: Option<bool>,
    pub valid_stream_event_seen: bool,
    pub stream_terminal: StreamTerminalState,
    pub first_stream_event_at: Option<Instant>,
    pub gateway_cache_status: CacheStatus,
    pub usage: UsageAccumulator,
    pub pricing_features: PricingFeatures,
    pub input_override: Option<rust_decimal::Decimal>,
    pub output_override: Option<rust_decimal::Decimal>,
    pub frozen_pricing: Option<FrozenPricingSnapshot>,
    pub finalized: bool,
}

pub struct AiUsageCollector {
    writer: AiUsageWriter,
    catalog: Arc<PriceCatalog>,
    node_id: Uuid,
    default_workspace_id: Uuid,
    active_contexts: AtomicU64,
    invariant_violations: AtomicU64,
}

impl AiUsageCollector {
    pub fn new(
        writer: AiUsageWriter,
        catalog: Arc<PriceCatalog>,
        node_id: Uuid,
        default_workspace_id: Uuid,
    ) -> Self {
        Self {
            writer,
            catalog,
            node_id,
            default_workspace_id,
            active_contexts: AtomicU64::new(0),
            invariant_violations: AtomicU64::new(0),
        }
    }

    pub fn active_contexts(&self) -> u64 {
        self.active_contexts.load(Ordering::Relaxed)
    }

    pub fn invariant_violations(&self) -> u64 {
        self.invariant_violations.load(Ordering::Relaxed)
    }

    fn begin(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        if ctx.extensions.get::<AiUsageContext>().is_some() {
            return;
        }
        let Some(ai_proxy) = plugins
            .iter()
            .find(|plugin| plugin.config.name == "ai-proxy")
        else {
            return;
        };
        let log_statistics = ai_proxy
            .config
            .config
            .get("logging")
            .and_then(|value| value.get("log_statistics"))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| {
                ai_proxy
                    .config
                    .config
                    .get("log_statistics")
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(true);
        let cache_status = if plugins
            .iter()
            .any(|plugin| plugin.config.name == "ai-cache")
        {
            CacheStatus::Unavailable
        } else {
            CacheStatus::NotConfigured
        };
        let model_group = ai_proxy
            .config
            .config
            .get("model_group")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        ctx.extensions.insert(AiUsageContext {
            fact_id: Uuid::now_v7(),
            ai_proxy_config: AiProxyUsageConfigSnapshot { log_statistics },
            requested_model: None,
            model_group,
            model_id: None,
            model_name: None,
            provider_id: None,
            provider_name: None,
            provider_type: None,
            stream: None,
            valid_stream_event_seen: false,
            stream_terminal: StreamTerminalState::NotStreaming,
            first_stream_event_at: None,
            gateway_cache_status: cache_status,
            usage: UsageAccumulator::default(),
            pricing_features: PricingFeatures::default(),
            input_override: None,
            output_override: None,
            frozen_pricing: None,
            finalized: false,
        });
        self.active_contexts.fetch_add(1, Ordering::Relaxed);
    }

    fn finalize(&self, ctx: &mut RequestCtx) {
        let Some(draft) = ctx.extensions.get::<AiUsageContext>().cloned() else {
            return;
        };
        if draft.finalized {
            return;
        }
        if let Some(context) = ctx.extensions.get_mut::<AiUsageContext>() {
            context.finalized = true;
        }

        let ai_state = ctx.extensions.get::<AiRequestState>();
        let requested_model = draft.requested_model.clone();
        let model_group = draft.model_group.clone().or_else(|| {
            ai_state
                .map(|state| state.model.name.clone())
                .filter(|v| !v.is_empty())
        });
        let model_id = draft.model_id.or_else(|| {
            ai_state
                .map(|state| stable_id(state.model.id))
                .unwrap_or(None)
        });
        let actual_model = draft.model_name.clone().or_else(|| {
            ai_state
                .map(|state| state.model.model_name.clone())
                .filter(|value| !value.is_empty())
        });
        let provider_id = draft.provider_id.or_else(|| {
            ai_state
                .map(|state| stable_id(state.provider_config.id))
                .unwrap_or(None)
        });
        let provider_name = draft.provider_name.clone().or_else(|| {
            ai_state
                .map(|state| state.provider_config.name.clone())
                .filter(|value| !value.is_empty())
        });
        let provider_type = draft.provider_type.clone().or_else(|| {
            ai_state
                .map(|state| state.provider_config.provider_type.clone())
                .filter(|value| !value.is_empty())
        });
        let stream = draft
            .stream
            .or_else(|| ai_state.map(|state| state.stream_mode));
        let stream_terminal = ai_state
            .map(|state| state.stream_terminal)
            .filter(|terminal| *terminal != StreamTerminalState::NotStreaming)
            .unwrap_or_else(|| {
                if stream == Some(true)
                    && draft.stream_terminal == StreamTerminalState::NotStreaming
                {
                    StreamTerminalState::Pending
                } else {
                    draft.stream_terminal
                }
            });

        let mut accumulator = draft.usage.clone();
        if let Some(state) = ai_state {
            let observation = (!state.usage.invalid)
                .then(|| {
                    checked_observation(
                        state.usage.prompt_tokens,
                        state.usage.completion_tokens,
                        state.usage.total_tokens,
                        state.usage.reasoning_tokens,
                        state.usage.cache_read_input_tokens,
                        state.usage.cache_write_input_tokens,
                    )
                })
                .flatten();
            match observation {
                Some(observation) => accumulator.observe_provider(observation),
                None => accumulator.observe_provider(UsageObservation {
                    prompt_tokens: Some(-1),
                    ..Default::default()
                }),
            }
            if state.usage.prompt_tokens.is_none() {
                match i64::try_from(state.estimated_prompt_tokens) {
                    Ok(value) => accumulator.observe_estimated(UsageObservation {
                        prompt_tokens: Some(value),
                        ..Default::default()
                    }),
                    Err(_) => accumulator.observe_provider(UsageObservation {
                        prompt_tokens: Some(-1),
                        ..Default::default()
                    }),
                }
            }
            if state.usage.completion_tokens.is_none() {
                match state
                    .estimated_completion_tokens
                    .map(i64::try_from)
                    .transpose()
                {
                    Ok(Some(value)) => accumulator.observe_estimated(UsageObservation {
                        completion_tokens: Some(value),
                        ..Default::default()
                    }),
                    Ok(None) => {}
                    Err(_) => accumulator.observe_provider(UsageObservation {
                        completion_tokens: Some(-1),
                        ..Default::default()
                    }),
                }
            }
        }
        let response_complete = ctx.lifecycle.downstream_response_completed
            && (stream != Some(true) || stream_terminal == StreamTerminalState::Complete);
        let usage = accumulator.finish(ctx.lifecycle.upstream_attempted, response_complete);
        let mut features = draft.pricing_features.clone();
        features.provider_cache_tokens = usage.cache_read_input_tokens.unwrap_or(0) > 0
            || usage.cache_write_input_tokens.unwrap_or(0) > 0;

        let pricing = if let Some(frozen) = draft.frozen_pricing.as_ref() {
            if ctx.lifecycle.upstream_attempted {
                let prompt_exceeds_frozen_limit = frozen
                    .max_prompt_tokens
                    .zip(usage.prompt_tokens.map(|field| field.value))
                    .is_some_and(|(maximum, actual)| actual > maximum);
                ResolvedPricing {
                    input: frozen.input.clone(),
                    output: frozen.output.clone(),
                    status: if prompt_exceeds_frozen_limit {
                        PricingStatus::Unsupported
                    } else {
                        PricingStatus::Matched
                    },
                    unsupported_reasons: if prompt_exceeds_frozen_limit {
                        vec!["long_context_pricing".to_string()]
                    } else {
                        Vec::new()
                    },
                }
            } else {
                ResolvedPricing {
                    input: None,
                    output: None,
                    status: PricingStatus::NotApplicable,
                    unsupported_reasons: Vec::new(),
                }
            }
        } else {
            let model_overrides = ai_state
                .map(|state| {
                    model_price_overrides(
                        &state.model,
                        provider_type.as_deref().unwrap_or_default(),
                        actual_model.as_deref().unwrap_or_default(),
                        ctx.lifecycle.started_at,
                    )
                })
                .unwrap_or_else(|| ModelPriceOverrides {
                    input: draft.input_override,
                    output: draft.output_override,
                    ..Default::default()
                });
            self.catalog.resolve(
                provider_type.as_deref().unwrap_or_default(),
                actual_model.as_deref().unwrap_or_default(),
                ctx.lifecycle.started_at,
                usage.prompt_tokens.map(|field| field.value),
                &model_overrides,
                &features,
                ctx.lifecycle.upstream_attempted,
            )
        };
        let cost = calculate_cost(
            &pricing,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.source,
            ctx.lifecycle.upstream_attempted,
            usage.invalid_provider_usage,
        );
        let valid_stream_event_seen = draft.valid_stream_event_seen
            || ai_state.is_some_and(|state| state.valid_stream_event_seen);
        let outcome = classify_outcome(ctx, stream, stream_terminal, valid_stream_event_seen);
        let final_status = persisted_status(ctx.lifecycle.final_status);
        let upstream_status = persisted_status(ctx.lifecycle.upstream_status);
        let invalid_status = ctx.lifecycle.final_status.is_some() && final_status.is_none()
            || ctx.lifecycle.upstream_status.is_some() && upstream_status.is_none();
        let unexplained_gateway_error = outcome == AiUsageOutcome::GatewayError
            && ctx.lifecycle.termination_hint.is_none()
            && ctx.lifecycle.transport_error.is_none()
            && !matches!(
                ctx.lifecycle.final_status,
                Some(200..=299) if ctx.lifecycle.downstream_response_completed
            );
        if invalid_status || unexplained_gateway_error {
            self.invariant_violations.fetch_add(1, Ordering::Relaxed);
        }

        let started_at = normalize_millis(ctx.lifecycle.started_at);
        let raw_finished_at = ctx.lifecycle.finished_at.unwrap_or_else(Utc::now);
        let finished_at = normalize_millis(raw_finished_at).max(started_at);
        let auth = ctx.extensions.get::<AiAuthContext>();
        let ttft_ms = (stream == Some(true) && outcome == AiUsageOutcome::Success)
            .then(|| {
                draft
                    .first_stream_event_at
                    .or_else(|| ai_state.and_then(|state| state.first_stream_event_at))
                    .and_then(|first| first.checked_duration_since(ctx.lifecycle.started_mono))
                    .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            })
            .flatten();
        let fact = Arc::new(AiUsageFact {
            id: draft.fact_id,
            ingest_seq: None,
            request_id: ctx.lifecycle.request_id.clone(),
            node_id: self.node_id,
            started_at,
            finished_at,
            recorded_at: None,
            workspace_id: Some(ctx.workspace_id.unwrap_or(self.default_workspace_id)),
            route_id: ctx.route_id,
            route_name: ctx.route_name.clone(),
            service_id: ctx.service_id,
            service_name: ctx.service_name.clone(),
            provider_id,
            provider_name,
            provider_type,
            model_id,
            requested_model,
            model_group,
            actual_model,
            attempt_count: i16::from(ctx.lifecycle.upstream_attempted),
            virtual_key_id: auth.map(|auth| auth.virtual_key_id),
            virtual_key_name: auth.map(|auth| auth.key_name.clone()),
            virtual_key_prefix: auth.map(|auth| auth.key_prefix.clone()),
            consumer_id: auth.and_then(|auth| auth.consumer_id).or(ctx.consumer_id),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            usage_source: usage.source,
            usage_unavailable_reasons: usage.unavailable_reasons,
            input_price: pricing.input,
            output_price: pricing.output,
            pricing_fingerprint: draft
                .frozen_pricing
                .as_ref()
                .map(|snapshot| Arc::clone(&snapshot.fingerprint)),
            pricing_status: pricing.status,
            pricing_unsupported_reasons: pricing.unsupported_reasons,
            cost_usd: cost.cost_usd,
            cost_status: cost.status,
            cost_unavailable_reasons: cost.unavailable_reasons,
            status_code: final_status,
            upstream_status_code: upstream_status,
            outcome,
            e2e_ms: i64::try_from(ctx.lifecycle.started_mono.elapsed().as_millis())
                .unwrap_or(i64::MAX),
            ttft_ms,
            upstream_attempted: ctx.lifecycle.upstream_attempted,
            stream,
            cache_status: draft.gateway_cache_status,
        });
        ctx.extensions.insert(Arc::clone(&fact));
        self.writer.try_enqueue(fact);
        self.active_contexts.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 将预算 preflight 的计价快照绑定到 usage 上下文。
///
/// 重复写入同一快照是幂等的；任何不同快照都说明一个请求发生了二次计价，
/// 必须在派发上游前失败。
pub fn freeze_pricing_snapshot(
    ctx: &mut RequestCtx,
    snapshot: FrozenPricingSnapshot,
) -> Result<(), &'static str> {
    let context = ctx
        .extensions
        .get_mut::<AiUsageContext>()
        .ok_or("usage_context_missing")?;
    if context.finalized {
        return Err("usage_context_finalized");
    }
    if let Some(existing) = context.frozen_pricing.as_ref() {
        return if existing == &snapshot {
            Ok(())
        } else {
            Err("pricing_snapshot_already_frozen")
        };
    }
    context.frozen_pricing = Some(snapshot);
    Ok(())
}

impl RequestLifecycleObserver for AiUsageCollector {
    fn on_plugins_resolved(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        self.begin(plugins, ctx);
    }

    fn on_request_finalizing(&self, _plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        self.finalize(ctx);
    }
}

pub fn observe_request_metadata(config: &serde_json::Value, ctx: &mut RequestCtx) {
    let request = ctx
        .request_body
        .as_deref()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());
    let requested_model = request
        .as_ref()
        .and_then(|body| body.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let requested_stream = request
        .as_ref()
        .and_then(|body| body.get("stream"))
        .and_then(serde_json::Value::as_bool);
    let pricing_features = request
        .as_ref()
        .map(detect_pricing_features)
        .unwrap_or_default();
    if let Some(context) = ctx.extensions.get_mut::<AiUsageContext>() {
        context.requested_model = requested_model.or_else(|| {
            config
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
        context.model_group = context.model_group.clone().or_else(|| {
            config
                .get("model_group")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
        context.stream = requested_stream;
        context.stream_terminal = if requested_stream == Some(true) {
            StreamTerminalState::Pending
        } else {
            StreamTerminalState::NotStreaming
        };
        context.pricing_features = pricing_features;
    }
}

fn detect_pricing_features(request: &serde_json::Value) -> PricingFeatures {
    let non_standard_service_tier = request
        .get("service_tier")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|tier| !matches!(tier, "auto" | "default"));
    let built_in_tools = request
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|tool_type| tool_type != "function")
            })
        });
    let non_text_modality = request
        .get("modalities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str().is_some_and(|value| value != "text"))
        })
        || contains_non_text_content(request);
    let additional_pricing = [
        "web_search_options",
        "file_search_options",
        "code_interpreter_options",
        "image_generation_options",
    ]
    .iter()
    .any(|field| request.get(*field).is_some());
    PricingFeatures {
        non_standard_service_tier,
        built_in_tools,
        non_text_modality,
        additional_pricing,
        ..Default::default()
    }
}

fn contains_non_text_content(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(contains_non_text_content),
        serde_json::Value::Object(object) => {
            if object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content_type| {
                    matches!(
                        content_type,
                        "image"
                            | "image_url"
                            | "input_image"
                            | "output_image"
                            | "audio"
                            | "input_audio"
                            | "output_audio"
                            | "video"
                            | "input_video"
                    )
                })
            {
                return true;
            }
            object.values().any(contains_non_text_content)
        }
        _ => false,
    }
}

pub fn observe_model_selection(
    ctx: &mut RequestCtx,
    model: &AiModel,
    provider: &AiProviderConfig,
    stream: bool,
) {
    if let Some(context) = ctx.extensions.get_mut::<AiUsageContext>() {
        context.model_id = stable_id(model.id);
        context.model_name = (!model.model_name.is_empty()).then(|| model.model_name.clone());
        context.model_group = context
            .model_group
            .clone()
            .or_else(|| (!model.name.is_empty()).then(|| model.name.clone()));
        context.provider_id = stable_id(provider.id);
        context.provider_name = (!provider.name.is_empty()).then(|| provider.name.clone());
        context.provider_type =
            (!provider.provider_type.is_empty()).then(|| provider.provider_type.clone());
        context.stream = Some(stream);
        context.stream_terminal = if stream {
            StreamTerminalState::Pending
        } else {
            StreamTerminalState::NotStreaming
        };
        context.input_override = model.input_cost;
        context.output_override = model.output_cost;
    }
}

pub fn observe_usage(ctx: &mut RequestCtx, observation: UsageObservation) {
    if let Some(context) = ctx.extensions.get_mut::<AiUsageContext>() {
        context.usage.observe_provider(observation);
    }
}

pub fn observe_stream_event(
    ctx: &mut RequestCtx,
    event_type: Option<&str>,
    data: &str,
    is_done: bool,
) {
    let Some(context) = ctx.extensions.get_mut::<AiUsageContext>() else {
        return;
    };
    if is_done {
        context.stream_terminal = StreamTerminalState::Complete;
        return;
    }
    if data.trim().is_empty() || data.trim_start().starts_with(':') {
        return;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(data).ok();
    let event_name = event_type
        .or_else(|| parsed.as_ref()?.get("type")?.as_str())
        .unwrap_or_default();
    if matches!(
        event_name,
        "ping" | "heartbeat" | "keepalive" | "response.keepalive"
    ) {
        return;
    }
    if event_name == "error"
        || event_name == "response.failed"
        || parsed
            .as_ref()
            .is_some_and(|value| value.get("error").is_some())
    {
        context.stream_terminal = StreamTerminalState::ProviderFailed;
    } else if matches!(
        event_name,
        "response.completed" | "response.incomplete" | "message_stop"
    ) {
        context.stream_terminal = StreamTerminalState::Complete;
    } else {
        context.valid_stream_event_seen = true;
        context
            .first_stream_event_at
            .get_or_insert_with(Instant::now);
    }
}

fn checked_observation(
    prompt: Option<u64>,
    completion: Option<u64>,
    total: Option<u64>,
    reasoning: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
) -> Option<UsageObservation> {
    Some(UsageObservation {
        prompt_tokens: prompt.map(i64::try_from).transpose().ok()?,
        completion_tokens: completion.map(i64::try_from).transpose().ok()?,
        total_tokens: total.map(i64::try_from).transpose().ok()?,
        reasoning_tokens: reasoning.map(i64::try_from).transpose().ok()?,
        cache_read_input_tokens: cache_read.map(i64::try_from).transpose().ok()?,
        cache_write_input_tokens: cache_write.map(i64::try_from).transpose().ok()?,
        ..Default::default()
    })
}

fn stable_id(id: Uuid) -> Option<Uuid> {
    (!id.is_nil()).then_some(id)
}

fn classify_outcome(
    ctx: &RequestCtx,
    stream: Option<bool>,
    terminal: StreamTerminalState,
    valid_stream_event_seen: bool,
) -> AiUsageOutcome {
    if ctx
        .lifecycle
        .transport_error
        .is_some_and(|error| error.source == RequestTransportSource::Downstream)
        && !ctx.lifecycle.downstream_response_completed
    {
        return AiUsageOutcome::ClientDisconnected;
    }
    let upstream_success = matches!(ctx.lifecycle.upstream_status, Some(200..=299));
    let clean_empty_stream_eof = ctx.lifecycle.downstream_response_completed
        && ctx.lifecycle.transport_error.is_none()
        && ctx.lifecycle.termination_hint.is_none();
    if stream == Some(true)
        && terminal != StreamTerminalState::Complete
        && (valid_stream_event_seen || upstream_success && clean_empty_stream_eof)
    {
        return AiUsageOutcome::StreamInterrupted;
    }
    match &ctx.lifecycle.termination_hint {
        Some(RequestTerminationHint::PolicyRejected { .. })
            if !ctx.lifecycle.upstream_attempted =>
        {
            return AiUsageOutcome::GatewayRejected;
        }
        Some(RequestTerminationHint::GatewayError { .. }) => {
            return AiUsageOutcome::GatewayError;
        }
        _ => {}
    }
    let upstream_transport_error = ctx
        .lifecycle
        .transport_error
        .is_some_and(|error| error.source == RequestTransportSource::Upstream);
    let upstream_semantic_error = matches!(
        ctx.lifecycle.termination_hint,
        Some(RequestTerminationHint::UpstreamSemanticError { .. })
    );
    if ctx.lifecycle.upstream_attempted
        && (!upstream_success || upstream_transport_error || upstream_semantic_error)
    {
        return AiUsageOutcome::UpstreamError;
    }
    if matches!(ctx.lifecycle.final_status, Some(200..=299))
        && ctx.lifecycle.downstream_response_completed
        && (stream != Some(true) || terminal == StreamTerminalState::Complete)
    {
        return AiUsageOutcome::Success;
    }
    AiUsageOutcome::GatewayError
}

fn persisted_status(value: Option<u16>) -> Option<i16> {
    value
        .filter(|status| matches!(status, 100..=599))
        .and_then(|status| i16::try_from(status).ok())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kong_core::traits::{Phase, RequestTransportError, RequestTransportErrorKind};
    use rust_decimal::Decimal;

    use super::*;
    use crate::usage::memory::MemoryAiUsageStore;
    use crate::usage::model::{CostStatus, PriceSnapshot, TokenFieldSource};
    use crate::usage::store::AiUsageStore;

    #[test]
    fn outcome_priority_is_deterministic() {
        let mut ctx = RequestCtx::new();
        ctx.lifecycle.mark_upstream_status(200);
        ctx.lifecycle.final_status = Some(200);
        ctx.lifecycle.downstream_response_completed = true;
        assert_eq!(
            classify_outcome(&ctx, Some(true), StreamTerminalState::Pending, true,),
            AiUsageOutcome::StreamInterrupted
        );

        ctx.lifecycle
            .mark_transport_error(RequestTransportError::downstream(
                RequestTransportErrorKind::ConnectionClosed,
            ));
        ctx.lifecycle.downstream_response_completed = false;
        assert_eq!(
            classify_outcome(&ctx, Some(true), StreamTerminalState::Pending, true,),
            AiUsageOutcome::ClientDisconnected
        );

        let mut rejected = RequestCtx::new();
        rejected
            .lifecycle
            .mark_policy_rejected(Phase::Access, "ai-key-auth");
        assert_eq!(
            classify_outcome(&rejected, None, StreamTerminalState::NotStreaming, false,),
            AiUsageOutcome::GatewayRejected
        );

        let mut provider_failed = RequestCtx::new();
        provider_failed.lifecycle.mark_upstream_status(200);
        provider_failed
            .lifecycle
            .mark_upstream_semantic_error(Some("openai".to_string()));
        assert_eq!(
            classify_outcome(
                &provider_failed,
                Some(true),
                StreamTerminalState::ProviderFailed,
                true,
            ),
            AiUsageOutcome::StreamInterrupted
        );
        assert_eq!(
            classify_outcome(
                &provider_failed,
                Some(true),
                StreamTerminalState::ProviderFailed,
                false,
            ),
            AiUsageOutcome::UpstreamError
        );

        let mut empty_stream = RequestCtx::new();
        empty_stream.lifecycle.mark_upstream_status(200);
        empty_stream.lifecycle.final_status = Some(200);
        empty_stream.lifecycle.downstream_response_completed = true;
        assert_eq!(
            classify_outcome(
                &empty_stream,
                Some(true),
                StreamTerminalState::Pending,
                false,
            ),
            AiUsageOutcome::StreamInterrupted
        );

        assert_eq!(persisted_status(Some(200)), Some(200));
        assert_eq!(persisted_status(Some(600)), None);
        assert_eq!(persisted_status(Some(999)), None);
    }

    #[test]
    fn request_pricing_features_detect_non_standard_charges() {
        let features = detect_pricing_features(&serde_json::json!({
            "service_tier": "priority",
            "modalities": ["text", "audio"],
            "tools": [
                {"type": "function", "function": {"name": "local"}},
                {"type": "web_search_preview"}
            ],
            "web_search_options": {"search_context_size": "high"}
        }));
        assert!(features.non_standard_service_tier);
        assert!(features.built_in_tools);
        assert!(features.non_text_modality);
        assert!(features.additional_pricing);

        let standard = detect_pricing_features(&serde_json::json!({
            "service_tier": "auto",
            "modalities": ["text"],
            "tools": [{"type": "function", "function": {"name": "local"}}],
            "messages": [{"role": "user", "content": "hello"}]
        }));
        assert!(!standard.non_standard_service_tier);
        assert!(!standard.built_in_tools);
        assert!(!standard.non_text_modality);
        assert!(!standard.additional_pricing);

        let explicit_null = detect_pricing_features(&serde_json::json!({
            "service_tier": null
        }));
        assert!(!explicit_null.non_standard_service_tier);
    }

    #[test]
    fn frozen_pricing_survives_catalog_and_actual_model_changes() {
        let workspace_id = Uuid::new_v4();
        let store: Arc<dyn AiUsageStore> =
            Arc::new(MemoryAiUsageStore::new(workspace_id, 8).unwrap());
        let (writer, _runner) =
            AiUsageWriter::channel(store, 8, 8, Duration::from_secs(1)).unwrap();
        let collector = AiUsageCollector::new(
            writer,
            Arc::new(PriceCatalog::builtin().unwrap()),
            Uuid::new_v4(),
            workspace_id,
        );
        let mut ctx = RequestCtx::new();
        let started_at = ctx.lifecycle.started_at;
        let frozen_input = PriceSnapshot {
            usd_per_million: Decimal::from(100),
            source: "frozen-test".to_string(),
            version: "catalog-before-refresh".to_string(),
            snapshot_date: started_at.date_naive(),
            effective_from: started_at,
            effective_to: None,
        };
        let frozen_output = PriceSnapshot {
            usd_per_million: Decimal::from(200),
            source: "frozen-test".to_string(),
            version: "catalog-before-refresh".to_string(),
            snapshot_date: started_at.date_naive(),
            effective_from: started_at,
            effective_to: None,
        };
        let frozen_fingerprint: Arc<str> = Arc::from("a".repeat(64));
        let mut usage = UsageAccumulator::default();
        usage.observe_provider(UsageObservation {
            prompt_tokens: Some(100),
            completion_tokens: Some(20),
            total_tokens: Some(120),
            ..Default::default()
        });
        ctx.extensions.insert(AiUsageContext {
            fact_id: Uuid::now_v7(),
            ai_proxy_config: AiProxyUsageConfigSnapshot {
                log_statistics: true,
            },
            requested_model: Some("requested-alias".to_string()),
            model_group: Some("model-group".to_string()),
            model_id: None,
            model_name: Some("actual-model-returned-by-provider".to_string()),
            provider_id: None,
            provider_name: Some("provider".to_string()),
            provider_type: Some("provider-after-refresh".to_string()),
            stream: Some(false),
            valid_stream_event_seen: false,
            stream_terminal: StreamTerminalState::NotStreaming,
            first_stream_event_at: None,
            gateway_cache_status: CacheStatus::NotConfigured,
            usage,
            pricing_features: PricingFeatures::default(),
            input_override: None,
            output_override: None,
            frozen_pricing: Some(FrozenPricingSnapshot {
                fingerprint: Arc::clone(&frozen_fingerprint),
                provider_type: "provider-before-refresh".to_string(),
                model: "model-before-refresh".to_string(),
                input: Some(frozen_input.clone()),
                output: Some(frozen_output.clone()),
                max_prompt_tokens: Some(1_000),
            }),
            finalized: false,
        });
        ctx.lifecycle.mark_upstream_attempted();
        ctx.lifecycle.mark_upstream_status(200);
        ctx.lifecycle.final_status = Some(200);
        ctx.lifecycle.downstream_response_completed = true;

        collector.finalize(&mut ctx);

        let fact = ctx.extensions.get::<Arc<AiUsageFact>>().unwrap();
        assert_eq!(
            fact.actual_model.as_deref(),
            Some("actual-model-returned-by-provider")
        );
        assert_eq!(fact.input_price.as_ref(), Some(&frozen_input));
        assert_eq!(fact.output_price.as_ref(), Some(&frozen_output));
        assert_eq!(
            fact.pricing_fingerprint.as_deref(),
            Some(frozen_fingerprint.as_ref())
        );
        assert_eq!(fact.cost_status, CostStatus::Calculated);
        assert_eq!(fact.cost_usd, Some(Decimal::new(14, 3)));
        assert_eq!(
            fact.prompt_tokens.map(|field| field.source),
            Some(TokenFieldSource::Provider)
        );
    }
}
