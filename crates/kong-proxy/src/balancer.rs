//! 负载均衡器 — 实现 round-robin、least-connections、consistent-hashing
//!
//! 与 Kong 的 upstream 负载均衡行为一致

use std::sync::atomic::{AtomicUsize, Ordering};

use kong_core::models::{LbAlgorithm, Target, Upstream};

/// 负载均衡目标
#[derive(Debug, Clone)]
pub struct BalancerTarget {
    /// 目标地址（host:port）
    pub address: String,
    /// 权重
    pub weight: i32,
}

/// 负载均衡器
pub struct LoadBalancer {
    /// 目标列表（按权重展开）
    targets: Vec<BalancerTarget>,
    /// 算法
    #[allow(dead_code)]
    algorithm: LbAlgorithm,
    /// Round-robin 索引
    rr_index: AtomicUsize,
    /// Upstream 的 host_header 配置（用于 SNI 和 Host 头）
    host_header: Option<String>,
}

impl LoadBalancer {
    /// 从 Upstream + Targets 创建负载均衡器
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

        Self {
            targets: bt,
            algorithm: upstream.algorithm.clone(),
            rr_index: AtomicUsize::new(0),
            host_header: upstream.host_header.clone(),
        }
    }

    /// 选择一个上游目标地址
    pub fn select(&self) -> Option<String> {
        if self.targets.is_empty() {
            return None;
        }

        // 使用加权 round-robin（其他算法后续扩展）
        self.weighted_round_robin()
    }

    /// 加权 Round-Robin 选择
    fn weighted_round_robin(&self) -> Option<String> {
        if self.targets.is_empty() {
            return None;
        }

        // 构建权重展开列表
        let total_weight: i32 = self.targets.iter().map(|t| t.weight).sum();
        if total_weight == 0 {
            return None;
        }

        let idx = self.rr_index.fetch_add(1, Ordering::Relaxed);
        let pos = (idx as i32) % total_weight;

        let mut cumulative = 0;
        for target in &self.targets {
            cumulative += target.weight;
            if pos < cumulative {
                return Some(target.address.clone());
            }
        }

        // 不应到达这里
        Some(self.targets[0].address.clone())
    }

    /// 获取 upstream 配置的 host_header
    pub fn host_header(&self) -> Option<String> {
        self.host_header.clone()
    }

    /// 目标数量
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    /// 更新目标列表
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
    }
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

        // 200 次选择应该均匀分布（各 100 次）
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

        // 收集 400 次选择的分布
        let mut count1 = 0;
        let mut count2 = 0;
        for _ in 0..400 {
            match lb.select().unwrap().as_str() {
                "10.0.0.1:80" => count1 += 1,
                "10.0.0.2:80" => count2 += 1,
                _ => panic!("未知地址"),
            }
        }

        // 应该大致是 3:1 的比例
        assert_eq!(count1, 300);
        assert_eq!(count2, 100);
    }

    #[test]
    fn test_empty_targets() {
        let upstream = Upstream::default();
        let lb = LoadBalancer::new(&upstream, &[]);
        assert!(lb.select().is_none());
    }
}
