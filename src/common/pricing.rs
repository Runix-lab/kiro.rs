//! 模型计价表：把 token 用量换算成官方牌价美金，把 credit 换算成实付美金。
//!
//! 两套口径并存，缺一不可：
//! - **官方口径**（official USD）：按各模型官方 API 牌价计算「这些 token 直连官方要花多少钱」。
//! - **实付口径**（credit USD）：上游按 credit 计费，`credits × creditUsdRate` 就是实际成本。
//!
//! 两者的比值就是运营侧要看的「折扣」：实付 ÷ 官方 = 0.14 即 1.4 折。
//!
//! 数据来源分两层：
//! 1. 内置默认表：Claude 家族的公开牌价（$/M token），随代码更新。
//! 2. `config.json` 的 `pricing.models`：运营可补充/覆盖任意模型（比如上游的非 Claude
//!    模型），改后重启生效。
//!
//! 查不到价的模型返回 `None` 而不是 0——「未配价」和「免费」是两回事，前端据此显示
//! "—" 而不是把折扣算成无穷大。
//!
//! 模型名归一化：上游目录里同一模型会以点号名（`claude-sonnet-4.5`）和官方横线名
//! （`claude-sonnet-4-5`）两种写法出现（网关对外接受官方名，trace 里两种都有），
//! 统一小写并把点换成横线后再查表。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 单个模型的官方牌价，单位 $/M token。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// 缓存写（5 分钟 TTL 档）：官方定价为输入价 × 1.25。
    pub cache_write_per_mtok: f64,
    /// 缓存读：官方定价为输入价 × 0.1。
    pub cache_read_per_mtok: f64,
}

impl ModelPrice {
    /// 按官方缓存倍率（写 1.25×、读 0.1×）从输入/输出单价推出完整价。
    const fn standard(input: f64, output: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_write_per_mtok: input * 1.25,
            cache_read_per_mtok: input * 0.1,
        }
    }
}

/// `config.json` 里的 `pricing` 段。所有字段可缺省。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingConfig {
    /// 1 credit 折合多少美金。默认 0.02（$200 订阅 / 10000 credits）。
    #[serde(default = "default_credit_usd_rate")]
    pub credit_usd_rate: f64,
    /// 追加/覆盖的模型牌价，键为模型 id（写点号或横线都行，查表前会归一化）。
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

fn default_credit_usd_rate() -> f64 {
    0.02
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            credit_usd_rate: default_credit_usd_rate(),
            models: HashMap::new(),
        }
    }
}

/// 内置默认牌价（官方 API list 价，2026-08 口径）。
///
/// 前缀匹配：`claude-opus-4-8` 命中 `claude-opus-4-8`，带日期后缀的变体也能命中。
/// 匹配时取最长前缀，避免 `claude-sonnet-4` 抢走 `claude-sonnet-4-6` 的精确命中。
///
/// GPT-5.6 三档（sol/terra/luna 是 OpenAI 官方型号名，Kiro 目录直接沿用）取
/// developers.openai.com 标准上下文档牌价（2026-08-25 抓取，含 8/22 sol 降价；
/// 恰好同为 缓存写 1.25×、缓存读 0.1× 输入价）。sol 为促销价（至少到 2026-11-21），
/// 涨价后可用 `pricing.models` 配置覆盖，无需改代码。长上下文加价档不建模。
const BUILTIN_PRICES: &[(&str, ModelPrice)] = &[
    ("gpt-5-6-sol", ModelPrice::standard(4.0, 20.0)),
    ("gpt-5-6-terra", ModelPrice::standard(2.0, 12.0)),
    ("gpt-5-6-luna", ModelPrice::standard(0.2, 1.2)),
    ("gpt-5-5", ModelPrice::standard(5.0, 30.0)),
    // 以下非 Anthropic/OpenAI 家族的价格于 2026-08-25 从各家官方定价页抓取并二次复核。
    // MiniMax / Qwen 的缓存价是官方published（恰好等于 1.25×/0.1×）；GLM-5 只公布了
    // 缓存读（$0.2），缓存写按 1.25× 推导——它实际按小时计缓存存储费，此处不建模。
    ("minimax-m2-5", ModelPrice::standard(0.3, 1.2)),
    ("minimax-m2-1", ModelPrice::standard(0.3, 1.2)),
    ("glm-5", ModelPrice::standard(1.0, 3.2)),
    ("qwen3-coder-next", ModelPrice::standard(0.3, 1.5)),
    // deepseek-3.2 刻意不配价：官方已于 2026-07-24 下线该型号、不再公布价格，
    // 拿历史价当现价会得出一个看着合理但错误的折扣。宁可显示"—"。
    ("claude-fable-5", ModelPrice::standard(10.0, 50.0)),
    ("claude-opus-5", ModelPrice::standard(5.0, 25.0)),
    ("claude-opus-4-8", ModelPrice::standard(5.0, 25.0)),
    ("claude-opus-4-7", ModelPrice::standard(5.0, 25.0)),
    ("claude-opus-4-6", ModelPrice::standard(5.0, 25.0)),
    ("claude-opus-4-5", ModelPrice::standard(5.0, 25.0)),
    ("claude-opus-4-1", ModelPrice::standard(15.0, 75.0)),
    // Sonnet 5 现价是introductory $2/$10（有效期至 2026-08-31），list 价 $3/$15。
    // 用现价：官方价值按实际能拿到的价算，折扣才不会偏乐观。促销结束后改回 3/15，
    // 或用 config 的 pricing.models 覆盖。
    ("claude-sonnet-5", ModelPrice::standard(2.0, 10.0)),
    ("claude-sonnet-4-6", ModelPrice::standard(3.0, 15.0)),
    ("claude-sonnet-4-5", ModelPrice::standard(3.0, 15.0)),
    ("claude-sonnet-4", ModelPrice::standard(3.0, 15.0)),
    ("claude-haiku-4-5", ModelPrice::standard(1.0, 5.0)),
    ("claude-3-5-haiku", ModelPrice::standard(0.8, 4.0)),
];

/// 解析好的计价表：内置默认 + 配置覆盖，进程内只读。
#[derive(Debug, Clone)]
pub struct PricingTable {
    credit_usd_rate: f64,
    /// 归一化模型名 → 牌价。配置项覆盖同名内置项。
    exact: HashMap<String, ModelPrice>,
}

impl Default for PricingTable {
    fn default() -> Self {
        Self::from_config(&PricingConfig::default())
    }
}

impl PricingTable {
    pub fn from_config(cfg: &PricingConfig) -> Self {
        let mut exact: HashMap<String, ModelPrice> = BUILTIN_PRICES
            .iter()
            .map(|(m, p)| ((*m).to_string(), *p))
            .collect();
        for (m, p) in &cfg.models {
            exact.insert(normalize_model(m), *p);
        }
        Self {
            credit_usd_rate: cfg.credit_usd_rate,
            exact,
        }
    }

    pub fn credit_usd_rate(&self) -> f64 {
        self.credit_usd_rate
    }

    /// credit → 实付美金。
    pub fn credit_usd(&self, credits: f64) -> f64 {
        if credits.is_finite() && credits > 0.0 {
            credits * self.credit_usd_rate
        } else {
            0.0
        }
    }

    /// 查某模型的牌价：先精确匹配归一化名，再退到最长前缀匹配。
    pub fn price_for(&self, model: &str) -> Option<ModelPrice> {
        let norm = normalize_model(model);
        if let Some(p) = self.exact.get(&norm) {
            return Some(*p);
        }
        self.exact
            .iter()
            .filter(|(k, _)| is_variant_of(&norm, k))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, p)| *p)
    }

    /// 按官方牌价计算一笔用量的美金成本。查不到价返回 `None`（≠ 免费）。
    pub fn official_usd(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
    ) -> Option<f64> {
        let p = self.price_for(model)?;
        const M: f64 = 1_000_000.0;
        Some(
            input_tokens as f64 / M * p.input_per_mtok
                + output_tokens as f64 / M * p.output_per_mtok
                + cache_write_tokens as f64 / M * p.cache_write_per_mtok
                + cache_read_tokens as f64 / M * p.cache_read_per_mtok,
        )
    }
}

/// 模型名归一化：小写 + 点号转横线。
fn normalize_model(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace('.', "-")
}

/// `candidate` 是否是价表条目 `key` 的**同价变体**——只认日期快照后缀。
///
/// 裸前缀匹配会把「更高版本」误判成同一款：`glm-5-2`（官方 $1.4/$4.4）以
/// `glm-5`（$1.0/$3.2）开头，按前缀会拿错价并算出一个看着合理的错误折扣。
/// 版本号后缀（`-2`、`-6`）一律不接受，只接受 8 位日期快照（`-20260101`）
/// 与非数字开头的后缀（`-thinking` 这类同价变体）。
fn is_variant_of(candidate: &str, key: &str) -> bool {
    let Some(rest) = candidate.strip_prefix(key) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('-') else {
        return false; // 必须落在段边界上，避免 gpt-5-6 命中 gpt-5
    };
    let head = rest.split('-').next().unwrap_or("");
    if head.chars().all(|c| c.is_ascii_digit()) {
        // 纯数字段：8 位视为日期快照（同款），其余视为新版本号（不同款）
        head.len() == 8
    } else {
        true
    }
}

/// 折扣比：实付 ÷ 官方。官方价缺失或为 0 时返回 `None`。
pub fn discount_ratio(credit_usd: f64, official_usd: Option<f64>) -> Option<f64> {
    match official_usd {
        Some(o) if o > 0.0 && credit_usd >= 0.0 => Some(credit_usd / o),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_and_dash_names_resolve_to_the_same_price() {
        let t = PricingTable::default();
        let dot = t.price_for("claude-sonnet-4.5").expect("点号名应命中");
        let dash = t.price_for("claude-sonnet-4-5").expect("横线名应命中");
        assert_eq!(dot, dash);
        assert_eq!(dot.input_per_mtok, 3.0);
    }

    #[test]
    fn longest_prefix_wins_over_shorter_family_entry() {
        let t = PricingTable::default();
        // claude-sonnet-4-6-20260101 应命中 4-6 的价，而不是 claude-sonnet-4 家族价
        let p = t.price_for("claude-sonnet-4-6-20260101").unwrap();
        assert_eq!(p.input_per_mtok, 3.0);
        // opus-4-1 是 15/75，不能被 opus-4 前缀（不存在）或其他项污染
        let p = t.price_for("claude-opus-4-1").unwrap();
        assert_eq!(p.input_per_mtok, 15.0);
    }

    #[test]
    fn a_newer_version_does_not_inherit_the_older_models_price() {
        let t = PricingTable::default();
        // glm-5-2 官方 $1.4/$4.4，与 glm-5（$1.0/$3.2）不同款：宁可未配价也不能错配
        assert!(t.price_for("glm-5.2").is_none());
        assert!(t.price_for("minimax-m2.7").is_none());
        // 但 8 位日期快照与非数字变体后缀仍视为同款
        assert_eq!(t.price_for("glm-5-20260101").unwrap().input_per_mtok, 1.0);
        assert_eq!(t.price_for("glm-5-thinking").unwrap().input_per_mtok, 1.0);
        // 段边界：gpt-5-6 不能命中 gpt-5-5
        assert!(t.price_for("gpt-5-6").is_none());
    }

    #[test]
    fn third_party_models_are_priced_from_their_own_vendors() {
        let t = PricingTable::default();
        assert_eq!(t.price_for("minimax-m2.5").unwrap().output_per_mtok, 1.2);
        assert_eq!(t.price_for("qwen3-coder-next").unwrap().output_per_mtok, 1.5);
        assert_eq!(t.price_for("gpt-5.5").unwrap().output_per_mtok, 30.0);
        // deepseek-3.2 官方已下线且不再公布价格，刻意留空
        assert!(t.price_for("deepseek-3.2").is_none());
    }

    #[test]
    fn unknown_model_is_unpriced_not_free() {
        let t = PricingTable::default();
        assert!(t.price_for("deepseek-3.2").is_none());
        assert!(t.official_usd("deepseek-3.2", 1000, 1000, 0, 0).is_none());
    }

    #[test]
    fn gpt56_family_is_priced_via_dot_name_normalization() {
        let t = PricingTable::default();
        // Kiro 目录用点号名 gpt-5.6-*，归一化后命中内置 gpt-5-6-* 条目
        assert_eq!(t.price_for("gpt-5.6-sol").unwrap().input_per_mtok, 4.0);
        assert_eq!(t.price_for("gpt-5.6-terra").unwrap().output_per_mtok, 12.0);
        let luna = t.price_for("gpt-5.6-luna").unwrap();
        assert!((luna.cache_write_per_mtok - 0.25).abs() < 1e-12);
        assert!((luna.cache_read_per_mtok - 0.02).abs() < 1e-12);
    }

    #[test]
    fn config_overrides_and_additions_apply_after_normalization() {
        let mut cfg = PricingConfig::default();
        cfg.models.insert(
            "GPT-5.6-Terra".to_string(),
            ModelPrice {
                // 故意偏离内置价（内置 $2），证明配置覆盖赢过内置表
                input_per_mtok: 9.9,
                output_per_mtok: 8.0,
                cache_write_per_mtok: 2.5,
                cache_read_per_mtok: 0.2,
            },
        );
        cfg.models.insert(
            "claude-opus-4-8".to_string(),
            ModelPrice::standard(4.0, 20.0),
        );
        let t = PricingTable::from_config(&cfg);
        assert_eq!(t.price_for("gpt-5.6-terra").unwrap().input_per_mtok, 9.9);
        assert_eq!(t.price_for("claude-opus-4.8").unwrap().input_per_mtok, 4.0);
    }

    #[test]
    fn official_usd_applies_per_class_rates() {
        let t = PricingTable::default();
        // opus-4-8: in $5, out $25, cache write $6.25, cache read $0.5 每 M
        let usd = t
            .official_usd("claude-opus-4-8", 1_000_000, 1_000_000, 1_000_000, 1_000_000)
            .unwrap();
        assert!((usd - (5.0 + 25.0 + 6.25 + 0.5)).abs() < 1e-9);
    }

    #[test]
    fn credit_usd_uses_configured_rate_and_rejects_junk() {
        let t = PricingTable::default();
        assert!((t.credit_usd(10.0) - 0.2).abs() < 1e-12);
        assert_eq!(t.credit_usd(f64::NAN), 0.0);
        assert_eq!(t.credit_usd(-3.0), 0.0);
    }

    #[test]
    fn discount_ratio_guards_missing_or_zero_official() {
        assert_eq!(discount_ratio(1.0, None), None);
        assert_eq!(discount_ratio(1.0, Some(0.0)), None);
        let r = discount_ratio(0.14, Some(1.0)).unwrap();
        assert!((r - 0.14).abs() < 1e-12);
    }

    #[test]
    fn pricing_config_parses_from_empty_json() {
        let cfg: PricingConfig = serde_json::from_str("{}").unwrap();
        assert!((cfg.credit_usd_rate - 0.02).abs() < 1e-12);
        assert!(cfg.models.is_empty());
    }
}
