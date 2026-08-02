//! 上下文压缩热路径指标。

use std::fmt::Write;
use std::sync::OnceLock;

use dashmap::DashMap;

const HOP_BUCKETS_MS: [u64; 10] = [5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RequestKey {
    provider: String,
    status: String,
    reason: String,
}

#[derive(Debug, Default)]
struct HopHistogram {
    count: u64,
    sum_ms: u128,
    cumulative_buckets: [u64; HOP_BUCKETS_MS.len()],
}

#[derive(Debug, Default)]
struct ContextCompressionMetrics {
    requests: DashMap<RequestKey, u64>,
    tokens_before: DashMap<String, u64>,
    tokens_after: DashMap<String, u64>,
    tokens_saved: DashMap<String, u64>,
    hop_latency: DashMap<String, HopHistogram>,
}

fn registry() -> &'static ContextCompressionMetrics {
    static METRICS: OnceLock<ContextCompressionMetrics> = OnceLock::new();
    METRICS.get_or_init(ContextCompressionMetrics::default)
}

/// 每个请求只在 usage finalize 阶段调用一次，避免重试或多阶段 filter 重复计数。
pub fn observe_context_compression(
    provider: &str,
    status: &str,
    reason: &str,
    tokens: Option<(u64, u64, u64)>,
    hop_latency_ms: Option<u64>,
) {
    let metrics = registry();
    let provider = if provider.is_empty() {
        "unknown"
    } else {
        provider
    };
    metrics
        .requests
        .entry(RequestKey {
            provider: provider.to_string(),
            status: status.to_string(),
            reason: reason.to_string(),
        })
        .and_modify(|value| *value = value.saturating_add(1))
        .or_insert(1);

    if let Some((before, after, saved)) = tokens {
        increment(&metrics.tokens_before, provider, before);
        increment(&metrics.tokens_after, provider, after);
        increment(&metrics.tokens_saved, provider, saved);
    }
    if let Some(latency_ms) = hop_latency_ms {
        metrics
            .hop_latency
            .entry(provider.to_string())
            .and_modify(|histogram| histogram.observe(latency_ms))
            .or_insert_with(|| {
                let mut histogram = HopHistogram::default();
                histogram.observe(latency_ms);
                histogram
            });
    }
}

fn increment(map: &DashMap<String, u64>, provider: &str, delta: u64) {
    map.entry(provider.to_string())
        .and_modify(|value| *value = value.saturating_add(delta))
        .or_insert(delta);
}

impl HopHistogram {
    fn observe(&mut self, latency_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.sum_ms = self.sum_ms.saturating_add(u128::from(latency_ms));
        for (index, upper_bound) in HOP_BUCKETS_MS.iter().enumerate() {
            if latency_ms <= *upper_bound {
                self.cumulative_buckets[index] = self.cumulative_buckets[index].saturating_add(1);
            }
        }
    }
}

/// 生成确定性 Prometheus exposition，标签只包含内部低基数字段。
pub fn context_compression_prometheus_metrics() -> String {
    let metrics = registry();
    let mut output = String::new();

    let mut requests = metrics
        .requests
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| {
        (&left.0.provider, &left.0.status, &left.0.reason).cmp(&(
            &right.0.provider,
            &right.0.status,
            &right.0.reason,
        ))
    });
    if !requests.is_empty() {
        output.push_str(
            "# HELP kong_ai_context_compression_requests_total Context compression requests.\n",
        );
        output.push_str("# TYPE kong_ai_context_compression_requests_total counter\n");
        for (key, value) in requests {
            let _ = writeln!(
                output,
                "kong_ai_context_compression_requests_total{{provider=\"{}\",status=\"{}\",reason=\"{}\"}} {}",
                escape_label(&key.provider),
                escape_label(&key.status),
                escape_label(&key.reason),
                value
            );
        }
    }

    render_token_counter(
        &mut output,
        "kong_ai_context_compression_tokens_before_total",
        "Context compression tokens before compression.",
        &metrics.tokens_before,
    );
    render_token_counter(
        &mut output,
        "kong_ai_context_compression_tokens_after_total",
        "Context compression tokens after compression.",
        &metrics.tokens_after,
    );
    render_token_counter(
        &mut output,
        "kong_ai_context_compression_tokens_saved_total",
        "Context compression saved input tokens.",
        &metrics.tokens_saved,
    );

    let mut histograms = metrics
        .hop_latency
        .iter()
        .map(|entry| {
            (
                entry.key().clone(),
                entry.value().count,
                entry.value().sum_ms,
                entry.value().cumulative_buckets,
            )
        })
        .collect::<Vec<_>>();
    histograms.sort_by(|left, right| left.0.cmp(&right.0));
    if !histograms.is_empty() {
        output.push_str(
            "# HELP kong_ai_context_compression_hop_latency_seconds Context compression hop latency.\n",
        );
        output.push_str("# TYPE kong_ai_context_compression_hop_latency_seconds histogram\n");
        for (provider, count, sum_ms, buckets) in histograms {
            let provider = escape_label(&provider);
            for (upper_bound, bucket_count) in HOP_BUCKETS_MS.iter().zip(buckets) {
                let _ = writeln!(
                    output,
                    "kong_ai_context_compression_hop_latency_seconds_bucket{{provider=\"{}\",le=\"{}\"}} {}",
                    provider,
                    seconds_bound(*upper_bound),
                    bucket_count
                );
            }
            let _ = writeln!(
                output,
                "kong_ai_context_compression_hop_latency_seconds_bucket{{provider=\"{}\",le=\"+Inf\"}} {}",
                provider, count
            );
            let _ = writeln!(
                output,
                "kong_ai_context_compression_hop_latency_seconds_sum{{provider=\"{}\"}} {}",
                provider,
                milliseconds_as_seconds(sum_ms)
            );
            let _ = writeln!(
                output,
                "kong_ai_context_compression_hop_latency_seconds_count{{provider=\"{}\"}} {}",
                provider, count
            );
        }
    }
    output
}

fn render_token_counter(
    output: &mut String,
    name: &str,
    help: &str,
    values: &DashMap<String, u64>,
) {
    let mut values = values
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    if values.is_empty() {
        return;
    }
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} counter");
    for (provider, value) in values {
        let _ = writeln!(
            output,
            "{name}{{provider=\"{}\"}} {value}",
            escape_label(&provider)
        );
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn seconds_bound(milliseconds: u64) -> String {
    milliseconds_as_seconds(u128::from(milliseconds))
}

fn milliseconds_as_seconds(milliseconds: u128) -> String {
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_low_cardinality_counters_and_histogram() {
        let provider = format!("test-provider-{}", uuid::Uuid::new_v4());
        observe_context_compression(
            &provider,
            "applied",
            "applied",
            Some((100, 40, 60)),
            Some(12),
        );

        let output = context_compression_prometheus_metrics();
        assert!(output.contains(&format!(
            "kong_ai_context_compression_requests_total{{provider=\"{provider}\",status=\"applied\",reason=\"applied\"}} 1"
        )));
        assert!(output.contains(&format!(
            "kong_ai_context_compression_tokens_saved_total{{provider=\"{provider}\"}} 60"
        )));
        assert!(output.contains(&format!(
            "kong_ai_context_compression_hop_latency_seconds_bucket{{provider=\"{provider}\",le=\"0.025\"}} 1"
        )));
        assert!(output.contains(&format!(
            "kong_ai_context_compression_hop_latency_seconds_sum{{provider=\"{provider}\"}} 0.012"
        )));
        assert!(!output.contains("request_id"));
    }
}
