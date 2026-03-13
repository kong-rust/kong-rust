//! Load balancer — implements round-robin, least-connections, consistent-hashing — 负载均衡器 — 实现 round-robin、least-connections、consistent-hashing
//!
//! Consistent with Kong's upstream load balancing behavior — 与 Kong 的 upstream 负载均衡行为一致

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kong_core::models::{HashOn, LbAlgorithm, Target, Upstream};

use crate::health::HealthChecker;

/// Load balancer target — 负载均衡目标
#[derive(Debug, Clone)]
pub struct BalancerTarget {
    /// Target address (host:port) — 目标地址（host:port）
    pub address: String,
    /// Weight — 权重
    pub weight: i32,
}

/// Consistent hash ring node — 一致性哈希环节点
#[derive(Debug, Clone)]
struct HashRingNode {
    /// Virtual node position on the ring — 虚拟节点在环上的位置
    hash: u64,
    /// Corresponding target index — 对应的目标索引
    target_index: usize,
}

/// Load balancer — 负载均衡器
pub struct LoadBalancer {
    /// Target list — 目标列表
    targets: Vec<BalancerTarget>,
    /// Algorithm — 算法
    algorithm: LbAlgorithm,
    /// Round-robin index — Round-robin 索引
    rr_index: AtomicUsize,
    /// Upstream's host_header configuration — Upstream 的 host_header 配置
    host_header: Option<String>,
    /// Upstream name (used for health check lookups) — Upstream 名称（用于健康检查查询）
    upstream_name: String,
    /// Health checker reference — 健康检查器引用
    health_checker: Option<Arc<HealthChecker>>,
    /// hash_on configuration — hash_on 配置
    hash_on: HashOn,
    /// hash_on_header configuration — hash_on_header 配置
    hash_on_header: Option<String>,
    /// Consistent hash ring (precomputed) — 一致性哈希环（预计算）
    hash_ring: Vec<HashRingNode>,
    /// Least-connections: active connection count per target — 每个 target 的活跃连接数
    connection_counts: Vec<AtomicUsize>,
}

impl LoadBalancer {
    /// Create load balancer from Upstream + Targets — 从 Upstream + Targets 创建负载均衡器
    pub fn new(upstream: &Upstream, targets: &[&Target]) -> Self {
        let mut bt = Vec::new();
        for target in targets {
            if target.weight > 0 {
                bt.push(BalancerTarget {
                    address: target.target.clone(),
                    weight: target.weight,
                });
            }
        }

        let connection_counts: Vec<AtomicUsize> =
            (0..bt.len()).map(|_| AtomicUsize::new(0)).collect();

        let hash_ring = if upstream.algorithm == LbAlgorithm::ConsistentHashing {
            build_hash_ring(&bt, upstream.slots as usize)
        } else {
            Vec::new()
        };

        Self {
            targets: bt,
            algorithm: upstream.algorithm.clone(),
            rr_index: AtomicUsize::new(0),
            host_header: upstream.host_header.clone(),
            upstream_name: upstream.name.clone(),
            health_checker: None,
            hash_on: upstream.hash_on.clone(),
            hash_on_header: upstream.hash_on_header.clone(),
            hash_ring,
            connection_counts,
        }
    }

    /// Set health checker — 设置健康检查器
    pub fn set_health_checker(&mut self, checker: Arc<HealthChecker>) {
        self.health_checker = checker.into();
    }

    /// Select an upstream target address (for non-hash algorithms) — 选择一个上游目标地址（用于非哈希算法）
    pub fn select(&self) -> Option<String> {
        self.select_with_key(None)
    }

    /// Select an upstream target address — 选择一个上游目标地址
    ///
    /// hash_key: key used for consistent hashing (consumer ID, IP, header value, etc.) — 一致性哈希使用的 key（consumer ID、IP、header 值等）
    pub fn select_with_key(&self, hash_key: Option<&str>) -> Option<String> {
        if self.targets.is_empty() {
            return None;
        }

        // Get list of healthy target indices — 获取健康的目标索引列表
        let healthy_indices = self.get_healthy_indices();
        if healthy_indices.is_empty() {
            // Fall back to all targets when all are unhealthy (avoid total unavailability) — 所有目标不健康时回退到全部目标（避免完全不可用）
            tracing::warn!(
                "upstream {} 所有目标不健康，回退到全部目标",
                self.upstream_name
            );
            return self.select_from_all();
        }

        match self.algorithm {
            LbAlgorithm::RoundRobin => self.weighted_round_robin(&healthy_indices),
            LbAlgorithm::ConsistentHashing => {
                self.consistent_hash_select(hash_key, &healthy_indices)
            }
            LbAlgorithm::LeastConnections => self.least_connections_select(&healthy_indices),
            LbAlgorithm::Latency => {
                // Latency algorithm temporarily uses round-robin as substitute — Latency 算法暂用 round-robin 代替
                self.weighted_round_robin(&healthy_indices)
            }
        }
    }

    /// Get currently healthy target indices — 获取当前健康的目标索引
    fn get_healthy_indices(&self) -> Vec<usize> {
        let checker = match &self.health_checker {
            Some(c) => c,
            None => return (0..self.targets.len()).collect(), // No health checker, all considered healthy — 无健康检查器，全部视为健康
        };

        (0..self.targets.len())
            .filter(|&i| checker.is_healthy(&self.upstream_name, &self.targets[i].address))
            .collect()
    }

    /// No health check fallback: select from all targets — 无健康检查回退：从所有目标中选择
    fn select_from_all(&self) -> Option<String> {
        if self.targets.is_empty() {
            return None;
        }
        let indices: Vec<usize> = (0..self.targets.len()).collect();
        self.weighted_round_robin(&indices)
    }

    /// Weighted round-robin selection (among healthy targets only) — 加权 Round-Robin 选择（仅在健康目标中选择）
    fn weighted_round_robin(&self, healthy_indices: &[usize]) -> Option<String> {
        if healthy_indices.is_empty() {
            return None;
        }

        let total_weight: i32 = healthy_indices
            .iter()
            .map(|&i| self.targets[i].weight)
            .sum();
        if total_weight == 0 {
            return None;
        }

        let idx = self.rr_index.fetch_add(1, Ordering::Relaxed);
        let pos = (idx as i32) % total_weight;

        let mut cumulative = 0;
        for &target_idx in healthy_indices {
            cumulative += self.targets[target_idx].weight;
            if pos < cumulative {
                return Some(self.targets[target_idx].address.clone());
            }
        }

        Some(self.targets[healthy_indices[0]].address.clone())
    }

    /// Consistent hash selection — 一致性哈希选择
    fn consistent_hash_select(
        &self,
        hash_key: Option<&str>,
        healthy_indices: &[usize],
    ) -> Option<String> {
        if healthy_indices.is_empty() {
            return None;
        }

        let key = hash_key.unwrap_or("default");
        let hash = compute_hash(key);

        // Search on hash ring (binary search) — 在哈希环上查找（二分搜索）
        let ring = &self.hash_ring;
        if ring.is_empty() {
            return self.weighted_round_robin(healthy_indices);
        }

        let pos = match ring.binary_search_by_key(&hash, |node| node.hash) {
            Ok(i) => i,
            Err(i) => i % ring.len(),
        };

        // Find a healthy target starting from pos — 从 pos 开始寻找健康的目标
        for offset in 0..ring.len() {
            let idx = (pos + offset) % ring.len();
            let target_idx = ring[idx].target_index;
            if healthy_indices.contains(&target_idx) {
                return Some(self.targets[target_idx].address.clone());
            }
        }

        // Fall back to first healthy target — 回退到第一个健康目标
        Some(self.targets[healthy_indices[0]].address.clone())
    }

    /// Least connections selection — 最少连接选择
    fn least_connections_select(&self, healthy_indices: &[usize]) -> Option<String> {
        if healthy_indices.is_empty() {
            return None;
        }

        let mut min_conns = usize::MAX;
        let mut min_idx = healthy_indices[0];

        for &i in healthy_indices {
            let conns = self.connection_counts[i].load(Ordering::Relaxed);
            // Factor in weight: effective connections = actual connections / weight — 考虑权重：有效连接数 = 实际连接数 / 权重
            // Higher weight means lower effective count, more likely to be selected — 权重越高，等效连接数越低，越容易被选中
            let effective = if self.targets[i].weight > 0 {
                conns * 100 / self.targets[i].weight as usize
            } else {
                usize::MAX
            };
            if effective < min_conns {
                min_conns = effective;
                min_idx = i;
            }
        }

        Some(self.targets[min_idx].address.clone())
    }

    /// Increment active connection count for a target (called after upstream_peer returns) — 增加目标的活跃连接数（在 upstream_peer 返回后调用）
    pub fn increment_connections(&self, addr: &str) {
        if let Some(i) = self.targets.iter().position(|t| t.address == addr) {
            if i < self.connection_counts.len() {
                self.connection_counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Decrement active connection count for a target (called during logging phase) — 减少目标的活跃连接数（在 logging 阶段调用）
    pub fn decrement_connections(&self, addr: &str) {
        if let Some(i) = self.targets.iter().position(|t| t.address == addr) {
            if i < self.connection_counts.len() {
                // Prevent underflow — 防止下溢
                let _ = self.connection_counts[i].fetch_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |v| if v > 0 { Some(v - 1) } else { Some(0) },
                );
            }
        }
    }

    /// Extract hash key from request context based on hash_on configuration — 根据 hash_on 配置从请求上下文中提取 hash key
    pub fn extract_hash_key(&self, ctx: &kong_core::traits::RequestCtx) -> Option<String> {
        match self.hash_on {
            HashOn::None => None,
            HashOn::Consumer => ctx.consumer_id.map(|id| id.to_string()),
            HashOn::Ip => {
                let ip = &ctx.client_ip;
                if ip.is_empty() {
                    None
                } else {
                    Some(ip.clone())
                }
            }
            HashOn::Header => {
                if let Some(ref header_name) = self.hash_on_header {
                    ctx.request_headers
                        .get(&header_name.to_lowercase())
                        .cloned()
                } else {
                    None
                }
            }
            HashOn::Path => {
                let path = &ctx.request_path;
                if path.is_empty() {
                    None
                } else {
                    Some(path.clone())
                }
            }
            HashOn::Cookie | HashOn::QueryArg | HashOn::UriCapture => {
                // Simplified implementation: these advanced hash modes are not yet supported — 简化实现：暂不支持这些高级哈希方式
                None
            }
        }
    }

    /// Get the upstream's configured host_header — 获取 upstream 配置的 host_header
    pub fn host_header(&self) -> Option<String> {
        self.host_header.clone()
    }

    /// Get upstream name — 获取 upstream 名称
    pub fn upstream_name(&self) -> &str {
        &self.upstream_name
    }

    /// Target count — 目标数量
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    /// Get algorithm — 获取算法
    pub fn algorithm(&self) -> &LbAlgorithm {
        &self.algorithm
    }

    /// Update target list — 更新目标列表
    pub fn update_targets(&mut self, targets: &[&Target]) {
        self.targets.clear();
        for target in targets {
            if target.weight > 0 {
                self.targets.push(BalancerTarget {
                    address: target.target.clone(),
                    weight: target.weight,
                });
            }
        }
        self.connection_counts = (0..self.targets.len())
            .map(|_| AtomicUsize::new(0))
            .collect();

        if self.algorithm == LbAlgorithm::ConsistentHashing {
            self.hash_ring = build_hash_ring(&self.targets, 10000);
        }
    }
}

/// Compute ketama-style hash value — 计算 ketama 风格的哈希值
fn compute_hash(key: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// Build consistent hash ring — 构建一致性哈希环
fn build_hash_ring(targets: &[BalancerTarget], total_slots: usize) -> Vec<HashRingNode> {
    if targets.is_empty() {
        return Vec::new();
    }

    let total_weight: i32 = targets.iter().map(|t| t.weight).sum();
    if total_weight == 0 {
        return Vec::new();
    }

    let mut ring = Vec::with_capacity(total_slots);

    for (idx, target) in targets.iter().enumerate() {
        // Allocate virtual nodes proportionally by weight — 按权重比例分配虚拟节点数
        let vnodes = (target.weight as usize * total_slots) / total_weight as usize;
        let vnodes = vnodes.max(1);

        for i in 0..vnodes {
            let key = format!("{}-{}", target.address, i);
            let hash = compute_hash(&key);
            ring.push(HashRingNode {
                hash,
                target_index: idx,
            });
        }
    }

    ring.sort_by_key(|node| node.hash);
    ring
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin() {
        let upstream = Upstream::default();
        let t1 = Target {
            target: "10.0.0.1:80".to_string(),
            weight: 100,
            ..Target::default()
        };
        let t2 = Target {
            target: "10.0.0.2:80".to_string(),
            weight: 100,
            ..Target::default()
        };

        let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

        let mut count1 = 0;
        let mut count2 = 0;
        for _ in 0..200 {
            match lb.select().unwrap().as_str() {
                "10.0.0.1:80" => count1 += 1,
                "10.0.0.2:80" => count2 += 1,
                _ => panic!("未知地址"),
            }
        }
        assert_eq!(count1, 100);
        assert_eq!(count2, 100);
    }

    #[test]
    fn test_weighted_selection() {
        let upstream = Upstream::default();
        let t1 = Target {
            target: "10.0.0.1:80".to_string(),
            weight: 300,
            ..Target::default()
        };
        let t2 = Target {
            target: "10.0.0.2:80".to_string(),
            weight: 100,
            ..Target::default()
        };

        let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

        let mut count1 = 0;
        let mut count2 = 0;
        for _ in 0..400 {
            match lb.select().unwrap().as_str() {
                "10.0.0.1:80" => count1 += 1,
                "10.0.0.2:80" => count2 += 1,
                _ => panic!("未知地址"),
            }
        }
        assert_eq!(count1, 300);
        assert_eq!(count2, 100);
    }

    #[test]
    fn test_empty_targets() {
        let upstream = Upstream::default();
        let lb = LoadBalancer::new(&upstream, &[]);
        assert!(lb.select().is_none());
    }

    #[test]
    fn test_consistent_hashing_same_key_same_target() {
        let mut upstream = Upstream::default();
        upstream.algorithm = LbAlgorithm::ConsistentHashing;
        upstream.hash_on = HashOn::Ip;

        let t1 = Target {
            target: "10.0.0.1:80".to_string(),
            weight: 100,
            ..Target::default()
        };
        let t2 = Target {
            target: "10.0.0.2:80".to_string(),
            weight: 100,
            ..Target::default()
        };
        let t3 = Target {
            target: "10.0.0.3:80".to_string(),
            weight: 100,
            ..Target::default()
        };

        let lb = LoadBalancer::new(&upstream, &[&t1, &t2, &t3]);

        // Same key should always select the same target — 相同 key 应始终选择相同 target
        let first = lb.select_with_key(Some("192.168.1.100")).unwrap();
        for _ in 0..100 {
            let result = lb.select_with_key(Some("192.168.1.100")).unwrap();
            assert_eq!(result, first, "相同 key 应选择相同 target");
        }

        // Different keys may select different targets (not enforced, just verify no panic) — 不同 key 可能选择不同 target（不强制要求，只验证不 panic）
        let _other = lb.select_with_key(Some("10.0.0.99")).unwrap();
    }

    #[test]
    fn test_least_connections() {
        let mut upstream = Upstream::default();
        upstream.algorithm = LbAlgorithm::LeastConnections;

        let t1 = Target {
            target: "10.0.0.1:80".to_string(),
            weight: 100,
            ..Target::default()
        };
        let t2 = Target {
            target: "10.0.0.2:80".to_string(),
            weight: 100,
            ..Target::default()
        };

        let lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

        // Initial state: both have same connection count, select first — 初始状态：两者连接数相同，选择第一个
        let first = lb.select().unwrap();
        assert_eq!(first, "10.0.0.1:80");

        // Increase connections for target1 — 给 target1 增加连接
        lb.increment_connections("10.0.0.1:80");
        lb.increment_connections("10.0.0.1:80");

        // Should now select target2 with fewer connections — 现在应该选择连接数更少的 target2
        let second = lb.select().unwrap();
        assert_eq!(second, "10.0.0.2:80");

        // Decrease target1 connections — 减少 target1 的连接
        lb.decrement_connections("10.0.0.1:80");
        lb.decrement_connections("10.0.0.1:80");

        // Both equal again, select first — 两者又相等，选择第一个
        let third = lb.select().unwrap();
        assert_eq!(third, "10.0.0.1:80");
    }

    #[test]
    fn test_health_check_integration() {
        let mut upstream = Upstream::default();
        upstream.name = "test-upstream".to_string();

        let t1 = Target {
            target: "10.0.0.1:80".to_string(),
            weight: 100,
            ..Target::default()
        };
        let t2 = Target {
            target: "10.0.0.2:80".to_string(),
            weight: 100,
            ..Target::default()
        };

        let mut lb = LoadBalancer::new(&upstream, &[&t1, &t2]);

        // Set up health checker — 设置健康检查器
        let checker = Arc::new(HealthChecker::new());
        let config = crate::health::HealthCheckerConfig {
            unhealthy_tcp_failures: 1,
            healthy_successes: 1,
            ..Default::default()
        };
        checker.register_upstream(
            "test-upstream",
            &["10.0.0.1:80".to_string(), "10.0.0.2:80".to_string()],
            config,
        );

        lb.set_health_checker(checker.clone());

        // Initial state: both healthy — 初始状态：两者都健康
        // Make target1 unhealthy — 使 target1 不健康
        checker.report_tcp_failure("test-upstream", "10.0.0.1:80");

        // Should now only select target2 — 现在只应该选择 target2
        for _ in 0..10 {
            let addr = lb.select().unwrap();
            assert_eq!(addr, "10.0.0.2:80", "不健康的目标应被跳过");
        }

        // Recover target1 — 恢复 target1
        checker.report_success("test-upstream", "10.0.0.1:80");

        // Both should be selected (increase sample count to cover round-robin cycle) — 两者都应该被选择（增大采样数确保覆盖 round-robin 周期）
        let mut got_t1 = false;
        let mut got_t2 = false;
        for _ in 0..200 {
            match lb.select().unwrap().as_str() {
                "10.0.0.1:80" => got_t1 = true,
                "10.0.0.2:80" => got_t2 = true,
                _ => panic!("未知地址"),
            }
            if got_t1 && got_t2 {
                break;
            }
        }
        assert!(got_t1 && got_t2, "恢复健康后两者都应被选择");
    }
}
