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
    /// 无「缓存写入溢价」的价表：`cache_write = input`。
    ///
    /// GPT-5.6 **之前**的 OpenAI 型号（gpt-5.5 / 5.4 / 4o…）官方定价页的
    /// "cache writes" 列是破折号 —— 写缓存不额外收费。用 `standard()` 会派生出
    /// `input × 1.25`，把这些模型的缓存写虚高 25%。
    /// （1.25× 的写入溢价是 GPT-5.6 起才有的，对 5.6 系用 `standard()` 是对的。）
    const fn no_cache_write_premium(input: f64, output: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_write_per_mtok: input,
            cache_read_per_mtok: input * 0.1,
        }
    }

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
    // GPT-5.6 之前无写入溢价（官方定价页该列为破折号），不能用 standard()
    ("gpt-5-5", ModelPrice::no_cache_write_premium(5.0, 30.0)),
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
    // Sonnet 5 = $2/$10。
    //
    // ⚠️ 这里曾写着「introductory 价，有效期至 2026-08-31，促销结束后改回 3/15」。
    // 那句话现在是错的：$2/$10 已转为永久价。**不要照着改回 3/15** —— 按 2026-08
    // 的量，改回去会让四个客户的账单虚增约 $316（for_O 独占约 $267），而且这种
    // 虚增在界面上完全看不出来，只会表现为"折扣变好了"。
    // 改价前先核对厂商官方定价页，不要信这行注释里的历史说法。
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

    /// credit → USD 汇率。配置里显式写 0 或负数会让"对客单价上界"算成 0，
    /// 从而拒绝任何合法定价、把操作员锁死；成本也会算成 0 或负数。
    /// 这里兜底回默认值，不接受非正汇率。
    pub fn credit_usd_rate(&self) -> f64 {
        if self.credit_usd_rate.is_finite() && self.credit_usd_rate > 0.0 {
            self.credit_usd_rate
        } else {
            default_credit_usd_rate()
        }
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
pub fn normalize_model(model: &str) -> String {
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
    if is_date_snapshot(rest) {
        return true;
    }
    let head = rest.split('-').next().unwrap_or("");
    if head.chars().all(|c| c.is_ascii_digit()) {
        // 纯数字段一律当新版本号（不同款）。日期快照已在上面放行。
        return false;
    }
    // 非数字后缀只放行已知的同价变体。
    //
    // 从前这里是无条件 true，但那是单向不安全的：上游哪天出个
    // `claude-opus-4-5-pro`（更贵的同族档），会静默继承便宜价 → 漏收。
    // 收成白名单后，未知后缀落到"未配价"，而未配价现在会在月结页报警——
    // 宁可多响一次，不可静默少收。
    const SAME_PRICE_SUFFIXES: [&str; 3] = ["thinking", "latest", "preview"];
    let head_lower = head.to_ascii_lowercase();
    SAME_PRICE_SUFFIXES.contains(&head_lower.as_str())
}

/// 是不是日期快照后缀。各家约定不同，都得认，否则真实模型 id 会掉进未配价：
/// - Anthropic：`-20260514`（8 位连写）
/// - OpenAI / Qwen：`-2026-08-22`（ISO 分段）
/// - 智谱：`-0520`（MMDD，4 位且首位为 0）
fn is_date_snapshot(rest: &str) -> bool {
    let segs: Vec<&str> = rest.split('-').collect();
    let all_digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());

    // YYYY-MM-DD 整段结尾
    if segs.len() == 3
        && all_digits(segs[0])
        && all_digits(segs[1])
        && all_digits(segs[2])
        && segs[0].len() == 4
        && segs[1].len() == 2
        && segs[2].len() == 2
    {
        return true;
    }
    let head = segs[0];
    if !all_digits(head) {
        return false;
    }
    match head.len() {
        8 => true,                       // 20260514
        // MMDD：月份 01-12。只判首位为 0 的话，1015 / 1120 / 1231 这三个月的
        // 快照会被当成版本号踢掉，模型静默掉进"未配价"。
        4 => matches!(
            &head[..2],
            "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
        ),
        _ => false,
    }
}

/// opus-5 上下文窗口修复上线的时刻（Unix 秒，2026-08-24T13:16:40Z）。
///
/// 这之前生产跑的 v0.7.4 的 `get_context_window_size` 漏配了 `claude-opus-5`，
/// 它掉进 200_000 兜底而不是 1_000_000。
const OPUS5_WINDOW_FIX_TS: i64 = 1_787_577_400;

/// 该时段 opus-5 token 计量被压小的倍数。
///
/// 理论值 1_000_000/200_000 = 5.0，三条独立实测吻合：
/// - 同会话跨切换点 262602/52398 = 5.012
/// - credits/Mtok 阶跃 4.49×（含其它同期变化的稀释）
/// - `converter.rs` 里修复者自己写的注释："会让该模型的 usage 上报缩小 5 倍"
const OPUS5_WINDOW_SCALE: f64 = 5.0;

/// 历史 token 计量的补偿系数。
///
/// # 为什么需要它
///
/// 上游只回报上下文占用百分比，token 数是本地按 `pct × window / 100` 还原的。
/// 窗口常量配小 5 倍，该模型的 input / 缓存写 / 缓存读**三项全部等比压小**
/// （`split_against_total` 按总量拆分）。官方牌价因此被算小，折扣看起来虚高
/// ——实测 opus-5 显示 5.42 折，真实是 ~1.3 折，全部 key 合计少算官方牌价
/// $1,917~$2,197。
///
/// # 边界
///
/// 只作用于**修复上线之前**的 `claude-opus-5` 记录。返回 1.0 表示不补偿。
///
/// # 日落条款
///
/// 这个函数是一次性的历史数据补偿，**不是通用机制**。以后再出窗口配置错误，
/// 正确做法是修 `get_context_window_size` 并让 `context_window_guard` 提前报警，
/// 不要往这里加分支——每加一条都是在给账单叠加一层不可审计的乘数。
pub fn historical_token_scale(model: &str, ts_epoch_secs: i64) -> f64 {
    if ts_epoch_secs >= OPUS5_WINDOW_FIX_TS {
        return 1.0;
    }
    if normalize_model(model) == "claude-opus-5" {
        OPUS5_WINDOW_SCALE
    } else {
        1.0
    }
}

/// websearch 缓存归零修复上线的时刻（Unix 秒，2026-08-25T11:04:56Z）。
const WEBSEARCH_CACHE_FIX_TS: i64 = 1_787_655_896;

/// 该 bug 期间被误记为"新鲜输入"的部分里，实际命中缓存的比例。
///
/// 我们从未记录过真实命中率（那两个字段被硬写成 0 了），所以这是估算。
/// 三条独立推导收敛在 0.78~0.90：低并发对照切片 0.78~0.88、放宽链估计
/// 0.87~0.90、用上游 credits 反推 0.79。取中值 0.85。
///
/// **过错在我方**（是我们的兜底代码写死了 0，不是上游没给数据——同样拿不到
/// 明细，正常路径会估算），所以取值偏向客户一侧是应该的，不是让步。
pub const WEBSEARCH_CACHED_FRACTION: f64 = 0.85;

/// 走 websearch 兜底、且被误记成全新鲜输入的记录，其真实的 (input, cache_read) 拆分。
///
/// # 判据
///
/// 只在**同时满足**三个条件时才修正，任一不满足就原样返回：
/// 1. 时间早于修复上线（之后的数据落盘时就是对的）
/// 2. 模型是走 websearch 那条路的 gpt-5.6 系
/// 3. 缓存两项**都是 0** —— 这正是 bug 的指纹
///
/// 第 3 条让判据自我筛选：正常路径的 gpt 记录零缓存率只有 0~1%，几乎不会被误伤；
/// 而受影响的切片是 100% 命中。宁可漏掉几条真·零缓存请求，也不要把修正扩大到
/// 判不准的记录上——方向上漏掉 = 少修正 = 对客户不利那一侧更保守。
///
/// # 日落
///
/// 与 [`historical_token_scale`] 同理，这是一次性历史补偿。根因已修
/// （`websearch_loop.rs` 的兜底改为调 `split_against_total`），
/// 不要再往这里加分支。
pub fn websearch_cache_correction(
    model: &str,
    ts_epoch_secs: i64,
    input_tokens: u64,
    cache_write: u64,
    cache_read: u64,
) -> (u64, u64) {
    if ts_epoch_secs >= WEBSEARCH_CACHE_FIX_TS
        || cache_write != 0
        || cache_read != 0
        || input_tokens == 0
    {
        return (input_tokens, cache_read);
    }
    if !normalize_model(model).starts_with("gpt-5-6") {
        return (input_tokens, cache_read);
    }
    let cached = (input_tokens as f64 * WEBSEARCH_CACHED_FRACTION).round() as u64;
    let cached = cached.min(input_tokens);
    (input_tokens - cached, cached)
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

#[cfg(test)]
mod variant_tests {
    use super::*;

    /// 各家的日期快照约定都得认，否则真实模型 id 会掉进"未配价"→ 静默漏收。
    #[test]
    fn date_snapshots_of_every_vendor_match_their_family() {
        assert!(is_variant_of("claude-opus-4-5-20260514", "claude-opus-4-5"));
        assert!(is_variant_of("gpt-5-6-sol-2026-08-22", "gpt-5-6-sol"));
        assert!(is_variant_of("qwen3-coder-next-2025-07-22", "qwen3-coder-next"));
        assert!(is_variant_of("glm-5-0520", "glm-5"), "智谱用 MMDD");
    }

    /// 版本号不是日期，不能当同款——glm-5-2 的官方价比 glm-5 贵。
    #[test]
    fn version_bumps_do_not_inherit_the_cheaper_price() {
        assert!(!is_variant_of("glm-5-2", "glm-5"));
        assert!(!is_variant_of("gpt-5-6", "gpt-5-5"));
        assert!(!is_variant_of("claude-opus-4-5", "claude-opus-4"));
        assert!(!is_variant_of("glm-5-2026", "glm-5"), "2026 是年份不是 MMDD");
    }

    /// 未知后缀必须落到"未配价"而不是静默继承便宜价。
    /// 上游出个 -pro / -max 档时，宁可在月结页报警，也不要少收钱。
    #[test]
    fn unknown_suffixes_fall_through_to_unpriced() {
        assert!(is_variant_of("claude-opus-4-5-thinking", "claude-opus-4-5"));
        assert!(!is_variant_of("claude-opus-4-5-pro", "claude-opus-4-5"));
        assert!(!is_variant_of("claude-opus-4-5-max", "claude-opus-4-5"));
    }
}

#[cfg(test)]
mod historical_scale_tests {
    use super::*;

    /// 补偿只作用于修复之前的 opus-5，其它模型任何时间都不动。
    #[test]
    fn only_opus5_before_the_fix_is_scaled() {
        let before = OPUS5_WINDOW_FIX_TS - 1;
        let after = OPUS5_WINDOW_FIX_TS;
        assert_eq!(historical_token_scale("claude-opus-5", before), 5.0);
        assert_eq!(historical_token_scale("claude-opus-5", after), 1.0, "修复时刻起不再补偿");
        // 其它模型窗口配置一直是对的，补偿会变成凭空多收
        for m in ["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4.5", "gpt-5.6-sol"] {
            assert_eq!(historical_token_scale(m, before), 1.0, "{} 不该被补偿", m);
            assert_eq!(historical_token_scale(m, after), 1.0);
        }
    }

    /// 模型名归一化后再判定：点号/横线/日期快照写法都要命中同一档。
    #[test]
    fn model_name_variants_resolve_to_the_same_scale() {
        let before = OPUS5_WINDOW_FIX_TS - 1;
        assert_eq!(historical_token_scale("claude-opus-5", before), 5.0);
        assert_eq!(historical_token_scale("CLAUDE-OPUS-5", before), 5.0);
        // 变体不做补偿：窗口 bug 只影响目录里那个确切的 model id
        assert_eq!(historical_token_scale("claude-opus-5-thinking", before), 1.0);
    }

    /// 边界前后各挪一秒都不该改变系数表的形态（区间内无记录，纯防回归）。
    #[test]
    fn the_boundary_is_a_hard_cut_not_a_ramp() {
        for delta in [-2i64, -1, 0, 1, 2] {
            let ts = OPUS5_WINDOW_FIX_TS + delta;
            let got = historical_token_scale("claude-opus-5", ts);
            let want = if delta < 0 { 5.0 } else { 1.0 };
            assert_eq!(got, want, "ts 偏移 {} 秒时系数应为 {}", delta, want);
        }
    }
}

#[cfg(test)]
mod websearch_correction_tests {
    use super::*;

    const BEFORE: i64 = WEBSEARCH_CACHE_FIX_TS - 1;
    const AFTER: i64 = WEBSEARCH_CACHE_FIX_TS;

    /// bug 指纹命中：修复前 + gpt-5.6 + 缓存两项全 0 → 按 85% 重新归类
    #[test]
    fn zero_cache_gpt_before_fix_is_reclassified() {
        let (input, read) = websearch_cache_correction("gpt-5.6-sol", BEFORE, 10_000, 0, 0);
        assert_eq!(read, 8_500, "85% 应归为缓存读取");
        assert_eq!(input, 1_500);
        assert_eq!(input + read, 10_000, "总量必须守恒");
    }

    /// 修复之后的数据落盘时就是对的，绝不能再修一次（那会变成少收）
    #[test]
    fn records_after_the_fix_are_left_alone() {
        assert_eq!(
            websearch_cache_correction("gpt-5.6-sol", AFTER, 10_000, 0, 0),
            (10_000, 0)
        );
    }

    /// 已经带缓存的记录说明它没走那条兜底，动它就是凭空少收
    #[test]
    fn records_that_already_carry_cache_are_untouched() {
        assert_eq!(
            websearch_cache_correction("gpt-5.6-sol", BEFORE, 10_000, 0, 500),
            (10_000, 500)
        );
        assert_eq!(
            websearch_cache_correction("gpt-5.6-sol", BEFORE, 10_000, 200, 0),
            (10_000, 0)
        );
    }

    /// Claude 系不走 websearch 兜底，修正它等于凭空少收
    #[test]
    fn non_gpt_models_are_never_corrected() {
        for m in ["claude-opus-5", "claude-sonnet-5", "claude-opus-4-8"] {
            assert_eq!(
                websearch_cache_correction(m, BEFORE, 10_000, 0, 0),
                (10_000, 0),
                "{} 不该被修正",
                m
            );
        }
    }
}

#[cfg(test)]
mod cache_write_premium_tests {
    use super::*;

    /// GPT-5.6 起有 1.25× 写入溢价，5.6 之前没有。混用会让老型号虚高 25%。
    #[test]
    fn only_gpt_5_6_and_later_carry_a_cache_write_premium() {
        let t = PricingTable::from_config(&PricingConfig::default());
        let sol = t.price_for("gpt-5.6-sol").expect("sol 应已配价");
        assert!(
            (sol.cache_write_per_mtok - sol.input_per_mtok * 1.25).abs() < 1e-9,
            "gpt-5.6 应有 1.25x 写入溢价"
        );
        let g55 = t.price_for("gpt-5.5").expect("gpt-5.5 应已配价");
        assert!(
            (g55.cache_write_per_mtok - g55.input_per_mtok).abs() < 1e-9,
            "gpt-5.5 不该有写入溢价，实得 {} vs input {}",
            g55.cache_write_per_mtok,
            g55.input_per_mtok
        );
        // 缓存读两者都是 0.1x（官方定价页逐档核对过）
        assert!((sol.cache_read_per_mtok - sol.input_per_mtok * 0.1).abs() < 1e-9);
        assert!((g55.cache_read_per_mtok - g55.input_per_mtok * 0.1).abs() < 1e-9);
    }
}
