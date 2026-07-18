//! AI 模型的交错加权轮转选择。

/// 按交错加权轮转序列返回候选下标。
///
/// 权重先用最大公约数约简，再按轮次交错。例如 `4:1` 的一个周期为
/// `A, B, A, A, A`，避免低流量时连续多次命中同一模型。零权重不会被选中；
/// 全部权重为零时返回 `None`，由调用方决定降级策略。
pub(super) fn weighted_round_robin_index(weights: &[u32], sequence: u64) -> Option<usize> {
    let divisor = weights.iter().copied().fold(0, gcd);
    if divisor == 0 {
        return None;
    }

    let reduced: Vec<u32> = weights.iter().map(|weight| weight / divisor).collect();
    let total_weight: u64 = reduced.iter().map(|weight| u64::from(*weight)).sum();
    let slot = sequence % total_weight;
    let max_weight = reduced.iter().copied().max()?;

    // 找到 slot 所在的交错轮次。前 r 轮的槽位数为 sum(min(weight, r))。
    let mut low = 1;
    let mut high = max_weight;
    while low < high {
        let middle = low + (high - low) / 2;
        if slots_through_round(&reduced, middle) > slot {
            high = middle;
        } else {
            low = middle + 1;
        }
    }

    let round = low;
    let offset = slot - slots_through_round(&reduced, round - 1);
    reduced
        .iter()
        .enumerate()
        .filter(|(_, weight)| **weight >= round)
        .nth(offset as usize)
        .map(|(index, _)| index)
}

fn slots_through_round(weights: &[u32], round: u32) -> u64 {
    weights
        .iter()
        .map(|weight| u64::from((*weight).min(round)))
        .sum()
}

fn gcd(left: u32, right: u32) -> u32 {
    if right == 0 {
        left
    } else {
        gcd(right, left % right)
    }
}

#[cfg(test)]
mod tests {
    use super::weighted_round_robin_index;

    #[test]
    fn equal_weights_alternate() {
        let selected: Vec<_> = (0..6)
            .map(|sequence| weighted_round_robin_index(&[50, 50], sequence).unwrap())
            .collect();

        assert_eq!(selected, vec![0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn arbitrary_total_keeps_exact_ratio() {
        let selected: Vec<_> = (0..6)
            .map(|sequence| weighted_round_robin_index(&[10_000, 5_000], sequence).unwrap())
            .collect();

        assert_eq!(selected, vec![0, 1, 0, 0, 1, 0]);
    }

    #[test]
    fn zero_weight_is_skipped() {
        assert_eq!(weighted_round_robin_index(&[0, 7], 0), Some(1));
        assert_eq!(weighted_round_robin_index(&[0, 0], 0), None);
    }
}
