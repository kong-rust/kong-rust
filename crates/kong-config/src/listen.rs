use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Listen address — parses Kong's listen directive format — 监听地址 — 解析 Kong 的 listen 指令格式
/// Examples: "0.0.0.0:8000 reuseport backlog=16384" — 例如: "0.0.0.0:8000 reuseport backlog=16384"
///           "0.0.0.0:8443 http2 ssl reuseport backlog=16384"
///           "unix:/tmp/kong.sock"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenAddr {
    /// Listen address (ip:port or unix socket path) — 监听地址（ip:port 或 unix socket 路径）
    pub address: String,
    /// Whether SSL/TLS is enabled — 是否启用 SSL/TLS
    pub ssl: bool,
    /// Whether HTTP/2 is enabled — 是否启用 HTTP/2
    pub http2: bool,
    /// Whether SO_REUSEPORT is enabled — 是否启用 SO_REUSEPORT
    pub reuseport: bool,
    /// Whether proxy protocol is enabled — 是否为代理协议
    pub proxy_protocol: bool,
    /// Backlog size — backlog 大小
    pub backlog: Option<u32>,
    /// Whether deferred binding is enabled — 是否启用延迟绑定
    pub deferred: bool,
    /// Receive buffer size — 接收缓冲大小
    pub rcvbuf: Option<u32>,
    /// Send buffer size — 发送缓冲大小
    pub sndbuf: Option<u32>,
    /// Bind to specific network device — 绑定到指定网络设备
    pub bind: bool,
    /// IP address part — ip 地址部分
    pub ip: String,
    /// Port part — 端口部分
    pub port: u16,
}

impl Default for ListenAddr {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:8000".to_string(),
            ssl: false,
            http2: false,
            reuseport: false,
            proxy_protocol: false,
            backlog: None,
            deferred: false,
            rcvbuf: None,
            sndbuf: None,
            bind: false,
            ip: "0.0.0.0".to_string(),
            port: 8000,
        }
    }
}

impl FromStr for ListenAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Special value "off" means disabled — 特殊值 "off" 表示禁用
        if s == "off" {
            return Err("listener is disabled".to_string());
        }

        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.is_empty() {
            return Err("empty listen address".to_string());
        }

        let addr_part = parts[0];
        let mut listen = ListenAddr::default();
        listen.address = addr_part.to_string();

        // Parse ip:port — 解析 ip:port
        if addr_part.starts_with("unix:") {
            listen.ip = addr_part.to_string();
            listen.port = 0;
        } else if let Some(bracket_end) = addr_part.find(']') {
            // IPv6: [::1]:8000
            let ip_part = &addr_part[1..bracket_end];
            listen.ip = ip_part.to_string();
            if addr_part.len() > bracket_end + 2 {
                listen.port = addr_part[bracket_end + 2..]
                    .parse()
                    .map_err(|e| format!("invalid port: {}", e))?;
            }
        } else if let Some(colon_pos) = addr_part.rfind(':') {
            listen.ip = addr_part[..colon_pos].to_string();
            listen.port = addr_part[colon_pos + 1..]
                .parse()
                .map_err(|e| format!("invalid port: {}", e))?;
        } else {
            // Port only — 仅端口号
            listen.port = addr_part
                .parse()
                .map_err(|e| format!("invalid address: {}", e))?;
            listen.ip = "0.0.0.0".to_string();
        }

        listen.address = format!("{}:{}", listen.ip, listen.port);

        // Parse modifiers — 解析修饰符
        for part in &parts[1..] {
            match *part {
                "ssl" => listen.ssl = true,
                "http2" => listen.http2 = true,
                "reuseport" => listen.reuseport = true,
                "proxy_protocol" => listen.proxy_protocol = true,
                "deferred" => listen.deferred = true,
                "bind" => listen.bind = true,
                _ if part.starts_with("backlog=") => {
                    listen.backlog = Some(
                        part[8..]
                            .parse()
                            .map_err(|e| format!("invalid backlog: {}", e))?,
                    );
                }
                _ if part.starts_with("rcvbuf=") => {
                    listen.rcvbuf = Some(
                        part[7..]
                            .parse()
                            .map_err(|e| format!("invalid rcvbuf: {}", e))?,
                    );
                }
                _ if part.starts_with("sndbuf=") => {
                    listen.sndbuf = Some(
                        part[7..]
                            .parse()
                            .map_err(|e| format!("invalid sndbuf: {}", e))?,
                    );
                }
                _ => {
                    tracing::warn!("未知的 listen 修饰符: {}", part);
                }
            }
        }

        Ok(listen)
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.address)?;
        if self.ssl {
            write!(f, " ssl")?;
        }
        if self.http2 {
            write!(f, " http2")?;
        }
        if self.reuseport {
            write!(f, " reuseport")?;
        }
        if self.proxy_protocol {
            write!(f, " proxy_protocol")?;
        }
        if self.deferred {
            write!(f, " deferred")?;
        }
        if let Some(backlog) = self.backlog {
            write!(f, " backlog={}", backlog)?;
        }
        Ok(())
    }
}

/// Parse multiple comma-separated listen addresses — 解析逗号分隔的多个 listen 地址
/// Example: "0.0.0.0:8000 reuseport backlog=16384, 0.0.0.0:8443 http2 ssl reuseport backlog=16384" — 例如: "0.0.0.0:8000 reuseport backlog=16384, 0.0.0.0:8443 http2 ssl reuseport backlog=16384"
pub fn parse_listen_addresses(s: &str) -> Result<Vec<ListenAddr>, String> {
    let s = s.trim();
    if s == "off" || s.is_empty() {
        return Ok(vec![]);
    }

    let mut result = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            result.push(ListenAddr::from_str(part)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_addr() {
        let addr: ListenAddr = "0.0.0.0:8000".parse().unwrap();
        assert_eq!(addr.ip, "0.0.0.0");
        assert_eq!(addr.port, 8000);
        assert!(!addr.ssl);
    }

    #[test]
    fn test_parse_with_flags() {
        let addr: ListenAddr = "0.0.0.0:8443 http2 ssl reuseport backlog=16384"
            .parse()
            .unwrap();
        assert_eq!(addr.ip, "0.0.0.0");
        assert_eq!(addr.port, 8443);
        assert!(addr.ssl);
        assert!(addr.http2);
        assert!(addr.reuseport);
        assert_eq!(addr.backlog, Some(16384));
    }

    #[test]
    fn test_parse_localhost() {
        let addr: ListenAddr = "127.0.0.1:8001 reuseport backlog=16384"
            .parse()
            .unwrap();
        assert_eq!(addr.ip, "127.0.0.1");
        assert_eq!(addr.port, 8001);
        assert!(addr.reuseport);
    }

    #[test]
    fn test_parse_multiple() {
        let addrs = parse_listen_addresses(
            "0.0.0.0:8000 reuseport backlog=16384, 0.0.0.0:8443 http2 ssl reuseport backlog=16384",
        )
        .unwrap();
        assert_eq!(addrs.len(), 2);
        assert!(!addrs[0].ssl);
        assert!(addrs[1].ssl);
    }

    #[test]
    fn test_parse_off() {
        let addrs = parse_listen_addresses("off").unwrap();
        assert!(addrs.is_empty());
    }
}
