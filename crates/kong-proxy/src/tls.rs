//! TLS 证书管理 — 基于 SNI 的动态证书选择
//!
//! 实现与 Kong 一致的证书匹配逻辑：
//! 1. 精确匹配 SNI
//! 2. 前缀通配符匹配（*.example.com）
//! 3. 后缀通配符匹配（example.*）
//! 4. 回退到默认证书

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use uuid::Uuid;

use kong_core::models::{Certificate, Sni};

/// 证书和密钥对
#[derive(Debug, Clone)]
pub struct CertKeyPair {
    /// PEM 格式证书
    pub cert: String,
    /// PEM 格式私钥
    pub key: String,
    /// 备选证书（RSA + ECDSA 双证书场景）
    pub cert_alt: Option<String>,
    /// 备选私钥
    pub key_alt: Option<String>,
}

/// TLS 证书管理器
pub struct CertificateManager {
    /// SNI 名称 -> 证书对（包含通配符 SNI）
    sni_map: Arc<RwLock<HashMap<String, CertKeyPair>>>,
    /// 证书 ID -> 证书对
    cert_cache: Arc<RwLock<HashMap<Uuid, CertKeyPair>>>,
    /// 默认证书（当 SNI 无匹配时使用）
    default_cert: Arc<RwLock<Option<CertKeyPair>>>,
}

impl CertificateManager {
    pub fn new() -> Self {
        Self {
            sni_map: Arc::new(RwLock::new(HashMap::new())),
            cert_cache: Arc::new(RwLock::new(HashMap::new())),
            default_cert: Arc::new(RwLock::new(None)),
        }
    }

    /// 设置默认证书（从 kong.conf 的 ssl_cert/ssl_cert_key 加载）
    pub fn set_default_cert(&self, cert: String, key: String) {
        if let Ok(mut default) = self.default_cert.write() {
            *default = Some(CertKeyPair {
                cert,
                key,
                cert_alt: None,
                key_alt: None,
            });
        }
    }

    /// 从数据库加载所有证书和 SNI 映射
    pub fn load_certificates(&self, certificates: &[Certificate], snis: &[Sni]) {
        // 构建证书缓存
        let mut cert_map = HashMap::new();
        for cert in certificates {
            let pair = CertKeyPair {
                cert: cert.cert.clone(),
                key: cert.key.clone(),
                cert_alt: cert.cert_alt.clone(),
                key_alt: cert.key_alt.clone(),
            };
            cert_map.insert(cert.id, pair);
        }

        // 构建 SNI -> 证书映射
        let mut sni_map = HashMap::new();
        for sni in snis {
            if let Some(pair) = cert_map.get(&sni.certificate.id) {
                sni_map.insert(sni.name.clone(), pair.clone());
            }
        }

        // 原子更新
        if let Ok(mut cache) = self.cert_cache.write() {
            *cache = cert_map;
        }
        if let Ok(mut map) = self.sni_map.write() {
            *map = sni_map;
        }
    }

    /// 根据 SNI 查找证书
    ///
    /// 匹配优先级（与 Kong 一致）：
    /// 1. 精确匹配
    /// 2. 前缀通配符（*.example.com）
    /// 3. 后缀通配符（example.*）
    /// 4. 默认 SNI（"*"）
    /// 5. 默认证书
    pub fn find_certificate(&self, sni: Option<&str>) -> Option<CertKeyPair> {
        let sni = match sni {
            Some(s) if !s.is_empty() => s,
            _ => {
                // 无 SNI，返回默认证书
                return self.get_default_cert();
            }
        };

        let sni_lower = sni.to_lowercase();

        if let Ok(map) = self.sni_map.read() {
            // 1. 精确匹配
            if let Some(pair) = map.get(&sni_lower) {
                return Some(pair.clone());
            }

            // 2. 前缀通配符匹配（*.example.com）
            if let Some(wild_prefix) = produce_wild_prefix(&sni_lower) {
                if let Some(pair) = map.get(&wild_prefix) {
                    return Some(pair.clone());
                }
            }

            // 3. 后缀通配符匹配（example.*）
            if let Some(wild_suffix) = produce_wild_suffix(&sni_lower) {
                if let Some(pair) = map.get(&wild_suffix) {
                    return Some(pair.clone());
                }
            }

            // 4. 默认 SNI（通配符 "*"）
            if let Some(pair) = map.get("*") {
                return Some(pair.clone());
            }
        }

        // 5. 回退到默认证书
        self.get_default_cert()
    }

    /// 根据证书 ID 查找证书（用于 Service 的 client_certificate）
    pub fn get_certificate_by_id(&self, id: &Uuid) -> Option<CertKeyPair> {
        if let Ok(cache) = self.cert_cache.read() {
            cache.get(id).cloned()
        } else {
            None
        }
    }

    /// 获取默认证书
    fn get_default_cert(&self) -> Option<CertKeyPair> {
        if let Ok(default) = self.default_cert.read() {
            default.clone()
        } else {
            None
        }
    }

    /// 热更新单个证书
    pub fn update_certificate(&self, cert: &Certificate) {
        let pair = CertKeyPair {
            cert: cert.cert.clone(),
            key: cert.key.clone(),
            cert_alt: cert.cert_alt.clone(),
            key_alt: cert.key_alt.clone(),
        };

        if let Ok(mut cache) = self.cert_cache.write() {
            cache.insert(cert.id, pair);
        }
    }

    /// 热更新 SNI 映射
    pub fn update_sni(&self, sni: &Sni) {
        if let Ok(cert_cache) = self.cert_cache.read() {
            if let Some(pair) = cert_cache.get(&sni.certificate.id) {
                if let Ok(mut map) = self.sni_map.write() {
                    map.insert(sni.name.clone(), pair.clone());
                }
            }
        }
    }

    /// 删除 SNI 映射
    pub fn remove_sni(&self, sni_name: &str) {
        if let Ok(mut map) = self.sni_map.write() {
            map.remove(sni_name);
        }
    }
}

/// 生成前缀通配符变体
/// 例如：api.example.com → *.example.com
///       sub.api.example.com → *.api.example.com
fn produce_wild_prefix(sni: &str) -> Option<String> {
    // 如果已经是通配符，直接返回
    if sni.starts_with('*') {
        return Some(sni.to_string());
    }

    // 找第一个点的位置，替换第一段为 *
    let dot_pos = sni.find('.')?;
    let remainder = &sni[dot_pos..];

    // 至少需要 *.x.y 格式（余下部分需要包含至少一个点）
    if remainder[1..].contains('.') {
        Some(format!("*{}", remainder))
    } else {
        None
    }
}

/// 生成后缀通配符变体
/// 例如：example.com → example.*
///       api.example.com → api.example.*
fn produce_wild_suffix(sni: &str) -> Option<String> {
    // 如果已经是通配符，直接返回
    if sni.ends_with('*') {
        return Some(sni.to_string());
    }

    // 找最后一个点的位置，替换最后一段为 *
    let dot_pos = sni.rfind('.')?;
    if dot_pos == 0 {
        return None;
    }

    Some(format!("{}.*", &sni[..dot_pos]))
}

/// 上游 TLS 配置 — 用于 Service 级别的 TLS 设置
#[derive(Debug, Clone)]
pub struct UpstreamTlsConfig {
    /// 客户端证书（用于 mTLS）
    pub client_cert: Option<CertKeyPair>,
    /// 是否验证上游证书
    pub tls_verify: bool,
    /// 验证深度
    pub tls_verify_depth: i32,
    /// CA 证书列表（PEM 格式）
    pub ca_certs: Vec<String>,
}

impl UpstreamTlsConfig {
    /// 从 Service 配置构建上游 TLS 配置
    pub fn from_service(
        service: &kong_core::models::Service,
        cert_manager: &CertificateManager,
        ca_certificates: &[kong_core::models::CaCertificate],
    ) -> Self {
        // 获取客户端证书
        let client_cert = service
            .client_certificate
            .as_ref()
            .and_then(|fk| cert_manager.get_certificate_by_id(&fk.id));

        // 收集 CA 证书
        let ca_certs = service
            .ca_certificates
            .as_ref()
            .map(|ca_ids| {
                ca_ids
                    .iter()
                    .filter_map(|id| {
                        ca_certificates
                            .iter()
                            .find(|ca| ca.id == *id)
                            .map(|ca| ca.cert.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self {
            client_cert,
            tls_verify: service.tls_verify.unwrap_or(false),
            tls_verify_depth: service.tls_verify_depth.unwrap_or(1),
            ca_certs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kong_core::models::ForeignKey;

    fn make_cert(id: Uuid, cert: &str, key: &str) -> Certificate {
        Certificate {
            id,
            cert: cert.to_string(),
            key: key.to_string(),
            ..Certificate::default()
        }
    }

    fn make_sni(name: &str, cert_id: Uuid) -> Sni {
        Sni {
            name: name.to_string(),
            certificate: ForeignKey::new(cert_id),
            ..Sni::default()
        }
    }

    #[test]
    fn test_exact_sni_match() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-exact", "key-exact")];
        let snis = vec![make_sni("api.example.com", cert_id)];
        manager.load_certificates(&certs, &snis);

        let result = manager.find_certificate(Some("api.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-exact");
    }

    #[test]
    fn test_wildcard_prefix_match() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-wild", "key-wild")];
        let snis = vec![make_sni("*.example.com", cert_id)];
        manager.load_certificates(&certs, &snis);

        // api.example.com 应该匹配 *.example.com
        let result = manager.find_certificate(Some("api.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-wild");

        // sub.api.example.com 不应该匹配 *.example.com
        // （因为生成的通配符是 *.api.example.com）
        let result = manager.find_certificate(Some("sub.api.example.com"));
        assert!(result.is_none());
    }

    #[test]
    fn test_wildcard_suffix_match() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-suffix", "key-suffix")];
        let snis = vec![make_sni("example.*", cert_id)];
        manager.load_certificates(&certs, &snis);

        // example.com 应该匹配 example.*
        let result = manager.find_certificate(Some("example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-suffix");

        // example.org 也应该匹配
        let result = manager.find_certificate(Some("example.org"));
        assert!(result.is_some());
    }

    #[test]
    fn test_exact_over_wildcard_priority() {
        let manager = CertificateManager::new();
        let cert_id1 = Uuid::new_v4();
        let cert_id2 = Uuid::new_v4();

        let certs = vec![
            make_cert(cert_id1, "cert-exact", "key-exact"),
            make_cert(cert_id2, "cert-wild", "key-wild"),
        ];
        let snis = vec![
            make_sni("api.example.com", cert_id1),
            make_sni("*.example.com", cert_id2),
        ];
        manager.load_certificates(&certs, &snis);

        // 精确匹配优先
        let result = manager.find_certificate(Some("api.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-exact");

        // 其他子域名走通配符
        let result = manager.find_certificate(Some("other.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-wild");
    }

    #[test]
    fn test_default_cert_fallback() {
        let manager = CertificateManager::new();
        manager.set_default_cert("default-cert".to_string(), "default-key".to_string());

        // 无 SNI 返回默认
        let result = manager.find_certificate(None);
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "default-cert");

        // 无匹配也返回默认
        let result = manager.find_certificate(Some("unknown.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "default-cert");
    }

    #[test]
    fn test_default_sni_star() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-default-sni", "key-default-sni")];
        let snis = vec![make_sni("*", cert_id)];
        manager.load_certificates(&certs, &snis);

        // 任何 SNI 在无精确/通配符匹配时应该回退到 "*"
        let result = manager.find_certificate(Some("anything.random.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-default-sni");
    }

    #[test]
    fn test_no_cert_no_default() {
        let manager = CertificateManager::new();

        let result = manager.find_certificate(Some("example.com"));
        assert!(result.is_none());

        let result = manager.find_certificate(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_case_insensitive_sni() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-case", "key-case")];
        let snis = vec![make_sni("api.example.com", cert_id)];
        manager.load_certificates(&certs, &snis);

        // SNI 匹配应该不区分大小写
        let result = manager.find_certificate(Some("API.Example.COM"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-case");
    }

    #[test]
    fn test_produce_wild_prefix() {
        assert_eq!(
            produce_wild_prefix("api.example.com"),
            Some("*.example.com".to_string())
        );
        assert_eq!(
            produce_wild_prefix("sub.api.example.com"),
            Some("*.api.example.com".to_string())
        );
        // 不能从顶级域名生成通配符
        assert_eq!(produce_wild_prefix("example.com"), None);
        // 已经是通配符
        assert_eq!(
            produce_wild_prefix("*.example.com"),
            Some("*.example.com".to_string())
        );
    }

    #[test]
    fn test_produce_wild_suffix() {
        assert_eq!(
            produce_wild_suffix("example.com"),
            Some("example.*".to_string())
        );
        assert_eq!(
            produce_wild_suffix("api.example.com"),
            Some("api.example.*".to_string())
        );
        assert_eq!(
            produce_wild_suffix("example.*"),
            Some("example.*".to_string())
        );
    }

    #[test]
    fn test_hot_update_sni() {
        let manager = CertificateManager::new();
        let cert_id = Uuid::new_v4();

        let certs = vec![make_cert(cert_id, "cert-1", "key-1")];
        let snis = vec![make_sni("api.example.com", cert_id)];
        manager.load_certificates(&certs, &snis);

        // 添加新 SNI
        let new_sni = make_sni("new.example.com", cert_id);
        manager.update_sni(&new_sni);

        let result = manager.find_certificate(Some("new.example.com"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().cert, "cert-1");

        // 删除 SNI
        manager.remove_sni("api.example.com");
        let result = manager.find_certificate(Some("api.example.com"));
        assert!(result.is_none());
    }
}
