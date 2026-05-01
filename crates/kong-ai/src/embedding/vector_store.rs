//! Vector store — 余弦近邻检索 + LRU + TTL
//! Vector store with cosine k-NN search, LRU eviction, and TTL expiry.
//!
//! MVP InMemory 实现:暴力 O(N) 余弦计算。N < 10k 时单次查询 ~ms 级,够用。
//! Redis-backed implementation lives behind the same VectorStore trait — TODO(#19B).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::cosine_similarity;

/// 缓存条目 — one cache entry: vector + payload + lifecycle bookkeeping.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    /// embedding 向量 — embedding vector
    pub vector: Vec<f32>,
    /// 序列化的 payload(响应 body 等)— serialized payload (e.g. cached response body)
    pub payload: String,
    /// 写入时间 — insertion timestamp
    pub inserted_at: Instant,
    /// 过期时间 — expiry timestamp
    pub expires_at: Instant,
    /// 最近一次命中(LRU 淘汰)— last-hit timestamp for LRU eviction
    pub last_used: Instant,
}

impl VectorEntry {
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// 检索命中结果 — search result.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub similarity: f32,
    pub payload: String,
}

/// Vector store trait — 后端无关接口
/// Backend-agnostic vector store interface.
pub trait VectorStore: Send + Sync {
    /// 插入(query_text + vector + payload) — insert an entry; evicts LRU if over capacity.
    fn insert(&self, vector: Vec<f32>, payload: String, ttl: Duration);

    /// 查询最相似条目;阈值未达返回 None — top-1 search; returns None if below threshold.
    fn search_top1(&self, query: &[f32], threshold: f32) -> Option<SearchHit>;

    /// 当前条目数 — current entry count (post any lazy eviction the impl wants to do).
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// InMemory VectorStore — Mutex<Vec<VectorEntry>> 暴力实现
pub struct InMemoryVectorStore {
    inner: Mutex<Vec<VectorEntry>>,
    max_entries: usize,
}

impl InMemoryVectorStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            max_entries: max_entries.max(1),
        }
    }

    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// 清理过期条目 — drop expired entries; called inside insert/search to amortize cost.
    fn prune_expired(entries: &mut Vec<VectorEntry>, now: Instant) {
        entries.retain(|e| !e.is_expired(now));
    }

    /// LRU 淘汰 — drop oldest-by-last_used until size <= max_entries.
    fn evict_lru(entries: &mut Vec<VectorEntry>, max: usize) {
        while entries.len() > max {
            // 找到最旧的 last_used — find oldest last_used
            let (idx, _) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_used)
                .expect("entries non-empty");
            entries.swap_remove(idx);
        }
    }
}

impl VectorStore for InMemoryVectorStore {
    fn insert(&self, vector: Vec<f32>, payload: String, ttl: Duration) {
        let now = Instant::now();
        let entry = VectorEntry {
            vector,
            payload,
            inserted_at: now,
            expires_at: now + ttl,
            last_used: now,
        };
        let mut guard = self.inner.lock().expect("vector store mutex poisoned");
        Self::prune_expired(&mut guard, now);
        guard.push(entry);
        Self::evict_lru(&mut guard, self.max_entries);
    }

    fn search_top1(&self, query: &[f32], threshold: f32) -> Option<SearchHit> {
        let now = Instant::now();
        let mut guard = self.inner.lock().expect("vector store mutex poisoned");
        Self::prune_expired(&mut guard, now);

        let mut best: Option<(usize, f32)> = None;
        for (i, entry) in guard.iter().enumerate() {
            let sim = cosine_similarity(query, &entry.vector);
            match best {
                Some((_, b)) if sim <= b => {}
                _ => best = Some((i, sim)),
            }
        }

        let (idx, sim) = best?;
        if sim < threshold {
            return None;
        }

        // 更新 LRU(命中即刷新 last_used)— refresh last_used on hit
        guard[idx].last_used = now;
        Some(SearchHit {
            similarity: sim,
            payload: guard[idx].payload.clone(),
        })
    }

    fn len(&self) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.lock().expect("vector store mutex poisoned");
        Self::prune_expired(&mut guard, now);
        guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn vec_a() -> Vec<f32> {
        vec![1.0, 0.0, 0.0]
    }
    fn vec_a_close() -> Vec<f32> {
        vec![0.99, 0.01, 0.0]
    }
    fn vec_b() -> Vec<f32> {
        vec![0.0, 1.0, 0.0]
    }

    #[test]
    fn insert_then_exact_search_hits() {
        let store = InMemoryVectorStore::new(10);
        store.insert(vec_a(), "payload-A".to_string(), Duration::from_secs(60));
        let hit = store.search_top1(&vec_a(), 0.9).unwrap();
        assert_eq!(hit.payload, "payload-A");
        assert!(hit.similarity > 0.999);
    }

    #[test]
    fn search_below_threshold_returns_none() {
        let store = InMemoryVectorStore::new(10);
        store.insert(vec_a(), "payload-A".to_string(), Duration::from_secs(60));
        // 完全垂直 — cosine = 0 < threshold 0.9
        assert!(store.search_top1(&vec_b(), 0.9).is_none());
    }

    #[test]
    fn close_vectors_hit_above_threshold() {
        let store = InMemoryVectorStore::new(10);
        store.insert(vec_a(), "payload-A".to_string(), Duration::from_secs(60));
        let hit = store.search_top1(&vec_a_close(), 0.9).unwrap();
        assert_eq!(hit.payload, "payload-A");
    }

    #[test]
    fn ttl_expiry_drops_entry() {
        let store = InMemoryVectorStore::new(10);
        store.insert(vec_a(), "payload-A".to_string(), Duration::from_millis(50));
        sleep(Duration::from_millis(80));
        assert!(store.search_top1(&vec_a(), 0.5).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn lru_eviction_when_over_capacity() {
        let store = InMemoryVectorStore::new(2);
        store.insert(vec![1.0, 0.0], "A".to_string(), Duration::from_secs(60));
        sleep(Duration::from_millis(5));
        store.insert(vec![0.0, 1.0], "B".to_string(), Duration::from_secs(60));
        sleep(Duration::from_millis(5));
        // 命中 A → 刷新 A 的 last_used
        let _ = store.search_top1(&[1.0, 0.0], 0.5);
        sleep(Duration::from_millis(5));
        // 插入第三条 → 淘汰最久未用的 B
        store.insert(vec![0.5, 0.5], "C".to_string(), Duration::from_secs(60));
        assert_eq!(store.len(), 2);
        assert!(store.search_top1(&[1.0, 0.0], 0.9).is_some());
        assert!(store.search_top1(&[0.0, 1.0], 0.9).is_none());
    }

    #[test]
    fn search_picks_highest_similarity() {
        let store = InMemoryVectorStore::new(10);
        store.insert(vec_a(), "A".to_string(), Duration::from_secs(60));
        store.insert(vec_a_close(), "A-close".to_string(), Duration::from_secs(60));
        store.insert(vec_b(), "B".to_string(), Duration::from_secs(60));
        let hit = store.search_top1(&[1.0, 0.0, 0.0], 0.5).unwrap();
        // 完全相同的 A 优先于 A-close
        assert_eq!(hit.payload, "A");
    }
}
