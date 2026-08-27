//! 上游对各模型缓存读的**实测**计费折扣。
//!
//! # 数据来源
//!
//! 2026-08-27 对照实验：每个模型用同一段 ~20K token 的 prompt（带 `cache_control`
//! 断点）连发 3 次，第 1 次写缓存、第 2/3 次读缓存，比较上游回报的 `credits`。
//! 判据是 `读 ÷ 写`，只依赖 credits（唯一的上游真值），不依赖任何本地估算的 token 字段。
//!
//! 18 个模型 × 3 次 = 54 次请求，共消耗 7.05 credits。
//!
//! # 为什么写死在代码里
//!
//! 上游不公开这套规则，也没有任何接口能查。只能实测。写死的代价是它会过期，
//! 所以每条都带测量日期；`MEASURED_AT` 距今太久时界面会提示重测。
//!
//! # 一条踩过的坑
//!
//! 首轮 `claude-opus-4.7` 测出 2.76 折，比同族便宜一倍。复测两轮后是 5.30/5.36 折
//! ——和其它 Claude 一样。那次大概率是某个「读」请求没命中缓存（缓存有 TTL，
//! 前缀哈希稍有差异就会 miss）。**单点数据不能信**，这张表里每个值都至少两轮一致。

/// 实测日期（用于判断数据是否过期）
pub const MEASURED_AT: &str = "2026-08-27";

/// 一个模型的上游缓存计费特征
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamCacheProfile {
    /// 缓存读 ÷ 缓存写。1.0 = 一分不打折
    pub cache_read_ratio: f64,
    /// 该值是否经过复测确认
    pub verified: bool,
}

/// 实测折扣表。**键用归一化后的模型名**（小写、点号转横线）。
///
/// 规律非常干净：Claude 系一律 ~0.53，非 Claude 系一律 1.00。
/// 界限干净到不像巧合——很可能 Kiro 只对 Anthropic 那条链路透传了缓存计费。
const MEASURED: &[(&str, f64, bool)] = &[
    // ── Claude 系：约 5.3 折 ──
    ("claude-opus-5", 0.529, true),
    ("claude-opus-4-8", 0.529, true),
    ("claude-opus-4-7", 0.533, true), // 复测两轮 5.30 / 5.36
    ("claude-opus-4-6", 0.531, true),
    ("claude-opus-4-5", 0.531, true),
    ("claude-sonnet-5", 0.529, true),
    ("claude-sonnet-4-6", 0.531, true),
    ("claude-sonnet-4-5", 0.531, true),
    ("claude-sonnet-4", 0.531, true),
    ("claude-haiku-4-5", 0.531, true),
    // ── 非 Claude：一分不打折 ──
    ("gpt-5-6-sol", 1.0, true),
    ("gpt-5-6-terra", 1.0, true),
    ("gpt-5-6-luna", 1.0, true),
    ("deepseek-3-2", 1.0, true),
    ("glm-5", 1.0, true),
    ("qwen3-coder-next", 1.0, true),
    ("minimax-m2-1", 1.0, true),
    ("minimax-m2-5", 1.0, true),
];

/// 查某个模型的缓存折扣。未实测过的返回 `None`——**不要猜**：
/// 猜错方向会直接误导选型，而"没测过"本身就是有用的信息。
pub fn profile_for(model: &str) -> Option<UpstreamCacheProfile> {
    let key = crate::common::pricing::normalize_model(model);
    MEASURED
        .iter()
        .find(|(m, _, _)| *m == key)
        .map(|(_, ratio, verified)| UpstreamCacheProfile {
            cache_read_ratio: *ratio,
            verified: *verified,
        })
}

/// 是不是「缓存读能打折」的模型。用于选型建议。
pub fn has_cache_discount(model: &str) -> Option<bool> {
    profile_for(model).map(|p| p.cache_read_ratio < 0.9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_family_gets_a_discount_and_others_do_not() {
        for m in ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4.5"] {
            let p = profile_for(m).unwrap_or_else(|| panic!("{} 应已实测", m));
            assert!(p.cache_read_ratio < 0.6, "{} 应有缓存折扣", m);
            assert_eq!(has_cache_discount(m), Some(true));
        }
        for m in ["gpt-5.6-sol", "deepseek-3.2", "glm-5", "minimax-m2.5"] {
            let p = profile_for(m).unwrap_or_else(|| panic!("{} 应已实测", m));
            assert!((p.cache_read_ratio - 1.0).abs() < 1e-9, "{} 不该有折扣", m);
            assert_eq!(has_cache_discount(m), Some(false));
        }
    }

    /// 模型名的点号/横线/大小写写法都要能查到同一条
    #[test]
    fn lookup_normalizes_the_model_name() {
        let a = profile_for("claude-opus-4.8").expect("点号写法");
        let b = profile_for("claude-opus-4-8").expect("横线写法");
        let c = profile_for("CLAUDE-OPUS-4.8").expect("大写");
        assert_eq!(a.cache_read_ratio, b.cache_read_ratio);
        assert_eq!(a.cache_read_ratio, c.cache_read_ratio);
    }

    /// 没测过的模型必须返回 None 而不是编一个值
    #[test]
    fn unmeasured_models_return_none_rather_than_a_guess() {
        assert!(profile_for("some-future-model").is_none());
        assert!(has_cache_discount("some-future-model").is_none());
    }
}
