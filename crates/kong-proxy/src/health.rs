//! Health checker — supports active and passive health checks — 健康检查器 — 支持主动和被动健康检查
//!
//! Consistent with Kong's health check behavior: — 与 Kong 的健康检查行为一致:
//! - Active: periodically send HTTP/TCP probe requests to targets — 主动: 定时向目标发送 HTTP/TCP 请求探测
//! - Passive: track response status codes from proxied requests — 被动: 根据代理请求的响应状态码统计

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Target health status — 目标健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
    /// DNS errors and similar cases — DNS 错误等情况
    #[allow(dead_code)]
    DnsError,
}

/// Health statistics for a single target — 单个目标的健康统计
#[derive(Debug, Clone)]
struct TargetHealth {
    /// Current status — 当前状态
    status: HealthStatus,
    /// Consecutive success count — 连续成功次数
    successes: i32,
    /// Consecutive TCP failure count — 连续 TCP 失败次数
    tcp_failures: i32,
    /// Consecutive timeout count — 连续超时次数
    timeouts: i32,
    /// Consecutive HTTP failure count — 连续 HTTP 失败次数
    http_failures: i32,
}

impl Default for TargetHealth {
    fn default() -> Self {
        Self {
            status: HealthStatus::Healthy,
            successes: 0,
            tcp_failures: 0,
            timeouts: 0,
            http_failures: 0,
        }
    }
}

/// Health check configuration — 健康检查配置
#[derive(Debug, Clone)]
pub struct HealthCheckerConfig {
    /// Active check interval (seconds), 0 means disabled — 主动检查间隔（秒），0 表示禁用
    pub active_interval: f64,
    /// Active check HTTP path — 主动检查路径
    pub active_http_path: String,
    /// Consecutive successes required to be deemed healthy — 健康判定所需连续成功次数
    pub healthy_successes: i32,
    /// Consecutive TCP failures required to be deemed unhealthy — 不健康判定所需连续 TCP 失败次数
    pub unhealthy_tcp_failures: i32,
    /// Consecutive timeouts required to be deemed unhealthy — 不健康判定所需连续超时次数
    pub unhealthy_timeouts: i32,
    /// Consecutive HTTP failures required to be deemed unhealthy — 不健康判定所需连续 HTTP 失败次数
    pub unhealthy_http_failures: i32,
    /// Passive check — healthy HTTP status codes — 被动检查 — 健康 HTTP 状态码
    pub passive_healthy_statuses: Vec<i32>,
    /// Passive check — unhealthy HTTP status codes — 被动检查 — 不健康 HTTP 状态码
    pub passive_unhealthy_statuses: Vec<i32>,
}

impl Default for HealthCheckerConfig {
    fn default() -> Self {
        Self {
            active_interval: 0.0,
            active_http_path: "/".to_string(),
            healthy_successes: 0,
            unhealthy_tcp_failures: 0,
            unhealthy_timeouts: 0,
            unhealthy_http_failures: 0,
            passive_healthy_statuses: vec![
                200, 201, 202, 203, 204, 205, 206, 207, 208, 226, 300, 301, 302, 303, 304, 305,
                306, 307, 308,
            ],
            passive_unhealthy_statuses: vec![429, 500, 503],
        }
    }
}

/// Health checker — 健康检查器
pub struct HealthChecker {
    /// upstream_name -> target address -> health status — upstream_name -> 目标地址 -> 健康状态
    targets: Arc<RwLock<HashMap<String, HashMap<String, TargetHealth>>>>,
    /// upstream_name -> configuration — upstream_name -> 配置
    configs: Arc<RwLock<HashMap<String, HealthCheckerConfig>>>,
}

impl HealthChecker {
    pub fn new() -> Self {
        Self {
            targets: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register health checks for an upstream — 注册 upstream 的健康检查
    pub fn register_upstream(
        &self,
        upstream_name: &str,
        target_addrs: &[String],
        config: HealthCheckerConfig,
    ) {
        if let Ok(mut targets) = self.targets.write() {
            let target_map: HashMap<String, TargetHealth> = target_addrs
                .iter()
                .map(|addr| (addr.clone(), TargetHealth::default()))
                .collect();
            targets.insert(upstream_name.to_string(), target_map);
        }

        if let Ok(mut configs) = self.configs.write() {
            configs.insert(upstream_name.to_string(), config);
        }
    }

    /// Check if target is healthy — 检查目标是否健康
    pub fn is_healthy(&self, upstream_name: &str, target_addr: &str) -> bool {
        if let Ok(targets) = self.targets.read() {
            if let Some(target_map) = targets.get(upstream_name) {
                if let Some(health) = target_map.get(target_addr) {
                    return health.status == HealthStatus::Healthy;
                }
            }
        }
        // Default healthy (unregistered targets are considered healthy) — 默认健康（未注册的目标视为健康）
        true
    }

    /// Report passive health check event — success — 报告被动健康检查事件 — 成功
    pub fn report_success(&self, upstream_name: &str, target_addr: &str) {
        self.update_health(upstream_name, target_addr, |health, config| {
            health.successes += 1;
            health.tcp_failures = 0;
            health.timeouts = 0;
            health.http_failures = 0;

            if config.healthy_successes > 0 && health.successes >= config.healthy_successes {
                if health.status != HealthStatus::Healthy {
                    tracing::info!(
                        "目标 {} ({}) 恢复健康",
                        target_addr,
                        upstream_name
                    );
                }
                health.status = HealthStatus::Healthy;
            }
        });
    }

    /// Report passive health check event — HTTP response — 报告被动健康检查事件 — HTTP 响应
    pub fn report_http_status(
        &self,
        upstream_name: &str,
        target_addr: &str,
        status_code: u16,
    ) {
        self.update_health(upstream_name, target_addr, |health, config| {
            if config
                .passive_unhealthy_statuses
                .contains(&(status_code as i32))
            {
                health.http_failures += 1;
                health.successes = 0;

                if config.unhealthy_http_failures > 0
                    && health.http_failures >= config.unhealthy_http_failures
                {
                    if health.status != HealthStatus::Unhealthy {
                        tracing::warn!(
                            "目标 {} ({}) 标记为不健康 (HTTP {})",
                            target_addr,
                            upstream_name,
                            status_code
                        );
                    }
                    health.status = HealthStatus::Unhealthy;
                }
            } else if config
                .passive_healthy_statuses
                .contains(&(status_code as i32))
            {
                health.successes += 1;
                health.http_failures = 0;

                if config.healthy_successes > 0 && health.successes >= config.healthy_successes {
                    health.status = HealthStatus::Healthy;
                }
            }
        });
    }

    /// Report passive health check event — TCP failure — 报告被动健康检查事件 — TCP 失败
    pub fn report_tcp_failure(&self, upstream_name: &str, target_addr: &str) {
        self.update_health(upstream_name, target_addr, |health, config| {
            health.tcp_failures += 1;
            health.successes = 0;

            if config.unhealthy_tcp_failures > 0
                && health.tcp_failures >= config.unhealthy_tcp_failures
            {
                if health.status != HealthStatus::Unhealthy {
                    tracing::warn!(
                        "目标 {} ({}) 标记为不健康 (TCP failure)",
                        target_addr,
                        upstream_name
                    );
                }
                health.status = HealthStatus::Unhealthy;
            }
        });
    }

    /// Report passive health check event — timeout — 报告被动健康检查事件 — 超时
    pub fn report_timeout(&self, upstream_name: &str, target_addr: &str) {
        self.update_health(upstream_name, target_addr, |health, config| {
            health.timeouts += 1;
            health.successes = 0;

            if config.unhealthy_timeouts > 0 && health.timeouts >= config.unhealthy_timeouts {
                if health.status != HealthStatus::Unhealthy {
                    tracing::warn!(
                        "目标 {} ({}) 标记为不健康 (timeout)",
                        target_addr,
                        upstream_name
                    );
                }
                health.status = HealthStatus::Unhealthy;
            }
        });
    }

    /// Get health status of all targets under an upstream — 获取 upstream 下所有目标的健康状态
    pub fn get_upstream_health(
        &self,
        upstream_name: &str,
    ) -> HashMap<String, HealthStatus> {
        if let Ok(targets) = self.targets.read() {
            if let Some(target_map) = targets.get(upstream_name) {
                return target_map
                    .iter()
                    .map(|(addr, health)| (addr.clone(), health.status))
                    .collect();
            }
        }
        HashMap::new()
    }

    /// Internal — update health status — 内部 — 更新健康状态
    fn update_health<F>(&self, upstream_name: &str, target_addr: &str, f: F)
    where
        F: FnOnce(&mut TargetHealth, &HealthCheckerConfig),
    {
        let config = {
            let configs = match self.configs.read() {
                Ok(c) => c,
                Err(_) => return,
            };
            match configs.get(upstream_name) {
                Some(c) => c.clone(),
                None => return,
            }
        };

        if let Ok(mut targets) = self.targets.write() {
            if let Some(target_map) = targets.get_mut(upstream_name) {
                if let Some(health) = target_map.get_mut(target_addr) {
                    f(health, &config);
                }
            }
        }
    }

    /// Start active health check background task — 启动主动健康检查后台任务
    pub fn start_active_checks(self: Arc<Self>) {
        let checker = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                checker.run_active_checks().await;
            }
        });
    }

    /// Execute one round of active health checks — 执行一轮主动健康检查
    async fn run_active_checks(&self) {
        let check_tasks: Vec<(String, String, String)> = {
            let configs = match self.configs.read() {
                Ok(c) => c,
                Err(_) => return,
            };
            let targets = match self.targets.read() {
                Ok(t) => t,
                Err(_) => return,
            };

            let mut tasks = Vec::new();
            for (upstream_name, config) in configs.iter() {
                if config.active_interval <= 0.0 {
                    continue;
                }
                if let Some(target_map) = targets.get(upstream_name) {
                    for addr in target_map.keys() {
                        tasks.push((
                            upstream_name.clone(),
                            addr.clone(),
                            config.active_http_path.clone(),
                        ));
                    }
                }
            }
            tasks
        };

        for (upstream_name, addr, path) in check_tasks {
            let url = format!("http://{}{}", addr, path);
            match tokio::time::timeout(
                Duration::from_secs(5),
                do_http_check(&url),
            )
            .await
            {
                Ok(Ok(status)) => {
                    self.report_http_status(&upstream_name, &addr, status);
                }
                Ok(Err(_)) => {
                    self.report_tcp_failure(&upstream_name, &addr);
                }
                Err(_) => {
                    self.report_timeout(&upstream_name, &addr);
                }
            }
        }
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Send a real HTTP GET request for health checking — 发送真正的 HTTP GET 请求进行健康检查
async fn do_http_check(url: &str) -> std::result::Result<u16, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let url_without_scheme = url
        .strip_prefix("http://")
        .unwrap_or(url);

    // Parse address and path — 解析地址和路径
    let (addr, path) = match url_without_scheme.find('/') {
        Some(i) => (&url_without_scheme[..i], &url_without_scheme[i..]),
        None => (url_without_scheme, "/"),
    };

    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| e.to_string())?;

    // Send a simple HTTP/1.1 GET request — 发送简单的 HTTP/1.1 GET 请求
    let host = addr.split(':').next().unwrap_or(addr);
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: kong-rust-healthcheck/0.1\r\n\r\n",
        path, host
    );

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    // Read response status line — 读取响应状态行
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("空响应".to_string());
    }

    let response = String::from_utf8_lossy(&buf[..n]);
    // Parse status code from "HTTP/1.1 200 OK" — 解析 "HTTP/1.1 200 OK" 中的状态码
    let status_str = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "无法解析 HTTP 响应状态".to_string())?;

    status_str
        .parse::<u16>()
        .map_err(|e| format!("状态码解析失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_checker_basic() {
        let hc = HealthChecker::new();

        let config = HealthCheckerConfig {
            unhealthy_http_failures: 3,
            passive_unhealthy_statuses: vec![500, 503],
            healthy_successes: 2,
            ..Default::default()
        };

        hc.register_upstream(
            "test-upstream",
            &["10.0.0.1:80".to_string()],
            config,
        );

        // Initial state: healthy — 初始状态: 健康
        assert!(hc.is_healthy("test-upstream", "10.0.0.1:80"));

        // Report 3 HTTP 500 errors — 报告 3 次 500 错误
        hc.report_http_status("test-upstream", "10.0.0.1:80", 500);
        hc.report_http_status("test-upstream", "10.0.0.1:80", 500);
        assert!(hc.is_healthy("test-upstream", "10.0.0.1:80")); // Still only 2 — 还是2次
        hc.report_http_status("test-upstream", "10.0.0.1:80", 500);
        assert!(!hc.is_healthy("test-upstream", "10.0.0.1:80")); // Becomes unhealthy — 变不健康

        // Report 2 successes to recover — 报告 2 次成功恢复
        hc.report_http_status("test-upstream", "10.0.0.1:80", 200);
        assert!(!hc.is_healthy("test-upstream", "10.0.0.1:80")); // 1 is not enough — 1次不够
        hc.report_http_status("test-upstream", "10.0.0.1:80", 200);
        assert!(hc.is_healthy("test-upstream", "10.0.0.1:80")); // Recovered — 恢复
    }

    #[test]
    fn test_unknown_target_is_healthy() {
        let hc = HealthChecker::new();
        assert!(hc.is_healthy("unknown", "0.0.0.0:0"));
    }
}
