//! REQ-AI-003 MemoryRateLimitStore 容量与并发手工基准。
//!
//! 本测试默认 ignored，避免把机器调度差异引入常规测试时延。运行：
//! `cargo test -p kong-ai --test ratelimit_capacity_test -- --ignored --nocapture`

use std::num::NonZeroU64;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kong_ai::ratelimit::{
    AdmissionDecision, AdmitCommand, ExceededDimension, ManualRateLimitClock,
    MemoryRateLimitConfig, MemoryRateLimitStore, QuotaCharge, QuotaLimits, RateLimitKey,
    RateLimitStore, RateLimitStoreError, RateLimitStoreStatsSnapshot, RateLimitSubject, WindowSpec,
};
use tokio::task::JoinSet;

const HIGH_CARDINALITY_OPERATIONS: usize = 10_000;
const HOT_KEY_OPERATIONS: usize = 10_000;
const HOT_KEY_LIMIT: usize = 5_000;

struct PhaseResult {
    elapsed: Duration,
    latencies: Vec<Duration>,
    allowed: usize,
    rejected: usize,
}

fn quota(requests: u64) -> QuotaLimits {
    QuotaLimits {
        requests: Some(NonZeroU64::new(requests).expect("配额必须大于零")),
        tokens: None,
    }
}

fn key(name: u128) -> RateLimitKey {
    RateLimitKey::new(
        "capacity-benchmark",
        RateLimitSubject::VirtualKey(uuid::Uuid::from_u128(name)),
    )
}

fn command(request_id: String, key: RateLimitKey, requests: u64) -> AdmitCommand {
    AdmitCommand {
        request_id: Arc::from(request_id),
        key,
        window: WindowSpec::fixed(Duration::from_secs(60)),
        limits: quota(requests),
        reserve: QuotaCharge {
            requests: 1,
            tokens: 0,
        },
    }
}

fn capacity_store() -> Arc<MemoryRateLimitStore> {
    let config = MemoryRateLimitConfig {
        max_buckets: HIGH_CARDINALITY_OPERATIONS + 100,
        max_idempotency_records: HIGH_CARDINALITY_OPERATIONS + HOT_KEY_OPERATIONS + 1_000,
        max_records_per_bucket: HOT_KEY_OPERATIONS + 100,
        max_live_reservations: HIGH_CARDINALITY_OPERATIONS + HOT_KEY_LIMIT + 100,
        recovery_record_headroom: 500,
        max_request_lifetime: Duration::from_secs(15 * 60),
        settlement_retry_grace: Duration::from_secs(5 * 60),
        cleanup_interval: Duration::from_secs(30),
        cleanup_scan_batch: 4_096,
    };
    Arc::new(
        MemoryRateLimitStore::new(config, Arc::new(ManualRateLimitClock::default()))
            .expect("容量基准配置必须有效"),
    )
}

async fn run_high_cardinality(store: Arc<MemoryRateLimitStore>) -> PhaseResult {
    let phase_started = Instant::now();
    let mut tasks = JoinSet::new();
    for index in 0..HIGH_CARDINALITY_OPERATIONS {
        let store = Arc::clone(&store);
        tasks.spawn(async move {
            let started = Instant::now();
            let decision = store
                .admit(command(
                    format!("high-cardinality-{index}"),
                    key(1_000_000 + index as u128),
                    1,
                ))
                .await;
            (started.elapsed(), decision)
        });
    }

    collect_phase(&mut tasks, phase_started, false).await
}

async fn run_hot_key(store: Arc<MemoryRateLimitStore>) -> PhaseResult {
    let hot_key = key(2_000_000);
    let phase_started = Instant::now();
    let mut tasks = JoinSet::new();
    for index in 0..HOT_KEY_OPERATIONS {
        let store = Arc::clone(&store);
        let hot_key = hot_key.clone();
        tasks.spawn(async move {
            let started = Instant::now();
            let decision = store
                .admit(command(
                    format!("hot-key-{index}"),
                    hot_key,
                    HOT_KEY_LIMIT as u64,
                ))
                .await;
            (started.elapsed(), decision)
        });
    }

    collect_phase(&mut tasks, phase_started, true).await
}

async fn collect_phase(
    tasks: &mut JoinSet<(Duration, Result<AdmissionDecision, RateLimitStoreError>)>,
    phase_started: Instant,
    allow_rejections: bool,
) -> PhaseResult {
    let mut latencies = Vec::with_capacity(tasks.len());
    let mut allowed = 0;
    let mut rejected = 0;

    while let Some(result) = tasks.join_next().await {
        let (latency, decision) = result.expect("容量基准任务不应 panic");
        latencies.push(latency);
        match decision.expect("Store 容量上界内不应返回错误") {
            AdmissionDecision::Allowed { .. } => allowed += 1,
            AdmissionDecision::Rejected {
                reason: ExceededDimension::Requests,
                ..
            } if allow_rejections => rejected += 1,
            decision => panic!("收到非预期准入结果: {decision:?}"),
        }
    }

    PhaseResult {
        elapsed: phase_started.elapsed(),
        latencies,
        allowed,
        rejected,
    }
}

fn percentile(latencies: &[Duration], percentile: usize) -> Duration {
    assert!(!latencies.is_empty());
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() - 1) * percentile / 100;
    sorted[index]
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1_000.0)
}

fn print_phase(name: &str, result: &PhaseResult) {
    let throughput = result.latencies.len() as f64 / result.elapsed.as_secs_f64();
    eprintln!(
        "{name}: operations={}, allowed={}, rejected={}, elapsed={}, throughput={throughput:.0} ops/s, p50={}, p95={}, p99={}",
        result.latencies.len(),
        result.allowed,
        result.rejected,
        format_duration(result.elapsed),
        format_duration(percentile(&result.latencies, 50)),
        format_duration(percentile(&result.latencies, 95)),
        format_duration(percentile(&result.latencies, 99)),
    );
}

fn resident_set_bytes() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kibibytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1_024)
}

fn print_capacity_estimate(
    before_rss: Option<u64>,
    after_rss: Option<u64>,
    stats: RateLimitStoreStatsSnapshot,
) {
    let rss_estimate = before_rss.zip(after_rss).map(|(before, after)| {
        let delta = after.saturating_sub(before);
        let tracked_entries = stats.buckets.saturating_add(stats.idempotency_records);
        let bytes_per_tracked_entry = if tracked_entries == 0 {
            0
        } else {
            delta / tracked_entries as u64
        };
        format!(
            "process_rss_before={:.2} MiB, process_rss_after={:.2} MiB, process_rss_delta={:.2} MiB, approx_bytes_per_tracked_entry={bytes_per_tracked_entry}",
            before as f64 / 1_048_576.0,
            after as f64 / 1_048_576.0,
            delta as f64 / 1_048_576.0,
        )
    });
    eprintln!(
        "capacity: stats={stats:?}, {}",
        rss_estimate.as_deref().unwrap_or(
            "RSS unavailable; use stats.buckets + stats.idempotency_records as the capacity estimate"
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "手工容量基准，避免常规测试受机器调度与 RSS 采样影响"]
async fn ten_thousand_high_cardinality_and_hot_key_admissions_are_bounded() {
    let store = capacity_store();
    let before_rss = resident_set_bytes();

    let high_cardinality = run_high_cardinality(Arc::clone(&store)).await;
    assert_eq!(high_cardinality.allowed, HIGH_CARDINALITY_OPERATIONS);
    assert_eq!(high_cardinality.rejected, 0);

    let hot_key = run_hot_key(Arc::clone(&store)).await;
    assert_eq!(hot_key.allowed, HOT_KEY_LIMIT);
    assert_eq!(hot_key.rejected, HOT_KEY_OPERATIONS - HOT_KEY_LIMIT);

    let stats = store.stats();
    assert_eq!(stats.buckets, HIGH_CARDINALITY_OPERATIONS + 1);
    assert_eq!(
        stats.idempotency_records,
        HIGH_CARDINALITY_OPERATIONS + HOT_KEY_OPERATIONS
    );
    assert_eq!(
        stats.live_reservations,
        HIGH_CARDINALITY_OPERATIONS + HOT_KEY_LIMIT
    );
    assert_eq!(
        stats.admissions_allowed,
        (HIGH_CARDINALITY_OPERATIONS + HOT_KEY_LIMIT) as u64
    );
    assert_eq!(
        stats.admissions_rejected,
        (HOT_KEY_OPERATIONS - HOT_KEY_LIMIT) as u64
    );
    assert_eq!(stats.overloads, 0);

    print_phase("high-cardinality", &high_cardinality);
    print_phase("hot-key", &hot_key);
    print_capacity_estimate(before_rss, resident_set_bytes(), stats);
}
