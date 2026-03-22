/// Token 计数器 — 三级 fallback
/// 1. Provider 返回的真实 usage（精确）
/// 2. tiktoken-rs 本地计算（GPT 系列精确）
/// 3. 字符估算 len/4（粗略兜底）
pub struct TokenCounter;

impl TokenCounter {
    pub fn new() -> Self {
        Self
    }

    /// 三级 fallback 计数：provider usage > tiktoken > 字符估算
    pub fn count(&self, model: &str, text: &str, provider_usage: Option<u64>) -> u64 {
        // 第一级：provider 提供了精确值，直接返回
        if let Some(usage) = provider_usage {
            return usage;
        }
        // 第二级：尝试 tiktoken（GPT 系列模型）
        if let Some(count) = self.count_tiktoken(model, text) {
            return count;
        }
        // 第三级：字符估算兜底
        Self::count_estimate(text)
    }

    /// tiktoken-rs 精确计数 — 仅支持 tiktoken 认识的模型，否则返回 None
    fn count_tiktoken(&self, model: &str, text: &str) -> Option<u64> {
        let bpe = tiktoken_rs::get_bpe_from_model(model).ok()?;
        let count = bpe.encode_with_special_tokens(text).len() as u64;
        Some(count)
    }

    /// 字符估算（~4 字符 = 1 token）— 向上取整
    pub fn count_estimate(text: &str) -> u64 {
        ((text.len() as u64) + 3) / 4
    }
}

impl Default for TokenCounter {
    fn default() -> Self {
        Self::new()
    }
}
