//! Virtual key authentication — 虚拟密钥认证
//!
//! Shared authenticator used by the ai-key-auth plugin (lookup) and the Admin API
//! (cache invalidation on key mutations).
//! 由 ai-key-auth 插件（查找）与 Admin API（密钥变更时失效缓存）共用的认证器。

use std::sync::Arc;
use std::time::Duration;

use kong_core::error::Result;
use kong_core::traits::{Dao, PageParams};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::AiVirtualKey;

/// Cache TTL — bounds staleness after an out-of-band key change (multi-node / direct DB writes)
/// 缓存 TTL — 限制带外密钥变更（多节点 / 直接写库）的陈旧窗口
const CACHE_TTL: Duration = Duration::from_secs(1);

/// Cache capacity — bounds memory under random-key probing
/// 缓存容量上限 — 限制随机密钥探测下的内存占用
const CACHE_CAPACITY: u64 = 10_000;

/// Safety valve for paged hash lookup — 分页哈希查找的安全阀
const MAX_LOOKUP_PAGES: usize = 100;

/// Authentication failure reason — 认证失败原因
///
/// Deliberately coarse: callers must not distinguish "unknown key" from
/// "disabled" or "expired", otherwise key state becomes probeable.
/// 故意粗粒度：调用方不得区分「密钥不存在」与「已禁用 / 已过期」，否则密钥状态可被探测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// No credential presented — 未提供凭证
    MissingKey,
    /// Credential rejected (unknown / disabled / expired) — 凭证被拒（不存在 / 已禁用 / 已过期）
    InvalidKey,
}

/// Authentication context passed to downstream plugins via `ctx.extensions`
/// 通过 `ctx.extensions` 传递给下游插件的认证上下文
#[derive(Debug, Clone)]
pub struct AiAuthContext {
    /// Virtual key id — 虚拟密钥 ID
    pub virtual_key_id: Uuid,
    /// Virtual key name — 虚拟密钥名称
    pub key_name: String,
    /// Bound consumer, if any — 绑定的 Consumer（如有）
    pub consumer_id: Option<Uuid>,
}

/// Virtual key authenticator — 虚拟密钥认证器
///
/// Caches lookups by key hash (including negative results, so invalid keys do not
/// reach the database on every request).
/// 按密钥哈希缓存查找结果（含负缓存，使无效密钥不会每次请求都打到数据库）。
pub struct VirtualKeyAuthenticator {
    dao: Arc<dyn Dao<AiVirtualKey>>,
    cache: moka::sync::Cache<String, Option<AiVirtualKey>>,
}

impl VirtualKeyAuthenticator {
    pub fn new(dao: Arc<dyn Dao<AiVirtualKey>>) -> Self {
        Self {
            dao,
            cache: moka::sync::Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_live(CACHE_TTL)
                .build(),
        }
    }

    /// Hash a raw key — must match the Admin API's storage encoding (lowercase hex)
    /// 哈希原文密钥 — 必须与 Admin API 的存储编码一致（小写十六进制）
    pub fn hash_key(raw_key: &str) -> String {
        format!("{:x}", Sha256::digest(raw_key.as_bytes()))
    }

    /// Authenticate a raw key, validating `enabled` and `expires_at`
    /// 认证原文密钥，校验 `enabled` 与 `expires_at`
    pub async fn authenticate(
        &self,
        raw_key: &str,
    ) -> std::result::Result<AiVirtualKey, AuthError> {
        if raw_key.is_empty() {
            return Err(AuthError::MissingKey);
        }

        let hash = Self::hash_key(raw_key);
        let key = match self.lookup(&hash).await {
            Ok(Some(key)) => key,
            Ok(None) => return Err(AuthError::InvalidKey),
            Err(e) => {
                // Lookup failure must not authenticate — 查找失败不得放行
                tracing::warn!("virtual key lookup failed: {}", e);
                return Err(AuthError::InvalidKey);
            }
        };

        if !key.enabled {
            return Err(AuthError::InvalidKey);
        }

        if let Some(expires_at) = key.expires_at {
            if chrono::Utc::now().timestamp() >= expires_at {
                return Err(AuthError::InvalidKey);
            }
        }

        Ok(key)
    }

    /// Drop all cached lookups — called by the Admin API after key mutations
    /// 清空全部缓存 — Admin API 在密钥变更后调用
    pub fn invalidate_all(&self) {
        self.cache.invalidate_all();
    }

    async fn lookup(&self, hash: &str) -> Result<Option<AiVirtualKey>> {
        if let Some(cached) = self.cache.get(hash) {
            return Ok(cached);
        }
        let found = self.find_by_hash(hash).await?;
        self.cache.insert(hash.to_string(), found.clone());
        Ok(found)
    }

    /// Look up a key by hash through the generic DAO (PostgreSQL and DB-less alike)
    /// 通过通用 DAO 按哈希查找密钥（PostgreSQL 与 DB-less 通用）
    ///
    /// The returned hash is re-checked locally: a DAO that silently drops an
    /// unsupported filter must never yield an unrelated key.
    /// 返回结果会在本地重新校验哈希：若某 DAO 静默丢弃了不支持的过滤条件，绝不能放行无关密钥。
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AiVirtualKey>> {
        let mut params = PageParams {
            filters: vec![("key_hash".to_string(), hash.to_string())],
            ..Default::default()
        };

        for _ in 0..MAX_LOOKUP_PAGES {
            let page = self.dao.page(&params).await?;
            if let Some(found) = page.data.into_iter().find(|k| k.key_hash == hash) {
                return Ok(Some(found));
            }
            match page.offset {
                Some(offset) => params.offset = Some(offset),
                None => return Ok(None),
            }
        }

        Ok(None)
    }
}

/// Check a model name against an allow list — 校验模型名是否在白名单内
///
/// An empty list means unrestricted. Entries ending in `*` match by prefix
/// (`gpt-4*` matches `gpt-4o`); all other entries must match exactly. The list is
/// OR-combined — any hit allows the request.
/// 空列表表示不限制。以 `*` 结尾的项按前缀匹配（`gpt-4*` 匹配 `gpt-4o`）；其余项须精确匹配。
/// 白名单为 OR 语义 — 任一命中即放行。
pub fn model_allowed(patterns: &[String], model: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns
        .iter()
        .any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => model.starts_with(prefix),
            None => pattern == model,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allow_list_is_unrestricted() {
        assert!(model_allowed(&[], "gpt-4o"));
    }

    #[test]
    fn exact_match_only_without_wildcard() {
        let patterns = vec!["gpt-4".to_string()];
        assert!(model_allowed(&patterns, "gpt-4"));
        assert!(!model_allowed(&patterns, "gpt-4o"));
        assert!(!model_allowed(&patterns, "gpt-"));
    }

    #[test]
    fn trailing_wildcard_matches_prefix() {
        let patterns = vec!["gpt-4*".to_string()];
        assert!(model_allowed(&patterns, "gpt-4"));
        assert!(model_allowed(&patterns, "gpt-4o"));
        assert!(model_allowed(&patterns, "gpt-4-turbo"));
        assert!(!model_allowed(&patterns, "gpt-3.5-turbo"));
    }

    #[test]
    fn bare_wildcard_allows_everything() {
        let patterns = vec!["*".to_string()];
        assert!(model_allowed(&patterns, "anything"));
        assert!(model_allowed(&patterns, ""));
    }

    #[test]
    fn allow_list_is_or_combined() {
        let patterns = vec!["gpt-4*".to_string(), "claude-sonnet-5".to_string()];
        assert!(model_allowed(&patterns, "gpt-4o"));
        assert!(model_allowed(&patterns, "claude-sonnet-5"));
        assert!(!model_allowed(&patterns, "gemini-2.5-pro"));
    }

    #[test]
    fn matching_is_case_sensitive() {
        let patterns = vec!["gpt-4".to_string()];
        assert!(!model_allowed(&patterns, "GPT-4"));
    }

    #[test]
    fn wildcard_only_applies_at_the_end() {
        // A leading '*' is not a wildcard — 前导 '*' 不是通配符
        let patterns = vec!["*-turbo".to_string()];
        assert!(!model_allowed(&patterns, "gpt-4-turbo"));
        assert!(model_allowed(&patterns, "*-turbo"));
    }

    #[test]
    fn hash_matches_admin_encoding() {
        // Same algorithm/encoding as the Admin API's generate_key()
        // 与 Admin API 的 generate_key() 使用相同算法与编码
        let hash = VirtualKeyAuthenticator::hash_key("sk-kr-test");
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash.to_lowercase());
        assert_eq!(
            VirtualKeyAuthenticator::hash_key("sk-kr-test"),
            hash,
            "hashing must be deterministic"
        );
        assert_ne!(VirtualKeyAuthenticator::hash_key("sk-kr-other"), hash);
    }
}
