use crate::provider::TokenUsage;

/// 计算 LLM 请求成本（USD）
/// input_cost / output_cost 单位：每百万 token 的价格（per-million pricing）
pub fn calculate_cost(
    usage: &TokenUsage,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
) -> f64 {
    let prompt = usage.prompt_tokens.unwrap_or(0) as f64;
    let completion = usage.completion_tokens.unwrap_or(0) as f64;
    let ic = input_cost.unwrap_or(0.0);
    let oc = output_cost.unwrap_or(0.0);
    (prompt * ic + completion * oc) / 1_000_000.0
}
