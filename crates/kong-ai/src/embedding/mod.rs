//! Embedding client + 余弦相似度工具 — 给 ai-semantic-cache / semantic routing 使用
//! Embedding client + cosine similarity helpers for ai-semantic-cache and semantic routing.

pub mod openai;
pub mod vector_store;

use async_trait::async_trait;
use kong_core::error::Result;
use std::sync::Arc;

pub use openai::OpenAiEmbeddingClient;
pub use vector_store::{InMemoryVectorStore, VectorEntry, VectorStore};

/// Embedding client trait — provider 无关的文本向量化接口
/// Provider-agnostic interface for text → vector embeddings.
#[async_trait]
pub trait EmbeddingClient: Send + Sync {
    /// 向量化单条文本 — embed a single text into a vector
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// 标识(用于日志/缓存键)— identifier for logs and cache keys
    fn identifier(&self) -> &str;
}

/// 余弦相似度 — cosine similarity ∈ [-1, 1]; returns 0 when either vector is zero.
/// 不强制要求归一化(内部自己除模长)。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot: f32 = 0.0;
    let mut na: f32 = 0.0;
    let mut nb: f32 = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// 类型别名 — 便于在 plugin context 中传递
/// Type alias for ergonomic use in plugin context.
pub type EmbeddingClientArc = Arc<dyn EmbeddingClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_vectors_is_minus_one() {
        let a = vec![1.0, 2.0];
        let b = vec![-1.0, -2.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector_returns_zero() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 2.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_mismatched_dim_returns_zero() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_close_vectors_high_similarity() {
        // 近似但不完全相同 — 期望 > 0.99
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![1.01, 2.01, 3.0, 3.99];
        let sim = cosine_similarity(&a, &b);
        assert!(sim > 0.999, "expected high similarity, got {}", sim);
    }
}
