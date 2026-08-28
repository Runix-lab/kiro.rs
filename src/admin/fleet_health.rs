//! 凭据健康分级 —— 舰队视图的单一判据来源。
//!
//! # 为什么需要它
//!
//! 7 条凭据可以逐张卡片扫过去；100 条不行。要让人只看需要处置的那几条，
//! 必须先有一个**单点定义**的分级函数：任何地方（前端徽章、告警、排序、
//! 容量规划）都读同一个结论，否则前后端各写一套判据必然漂移。
//!
//! # 设计约束
//!
//! - **纯函数**：`assess` 不做 IO、不读全局状态，全部输入显式传入。这样每条
//!   判据都能被单测钉住。
//! - **拿不到数据 ≠ 有问题**：余额未知不能判成"快用完了"。调度层已有同样的
//!   取舍（`admin::scheduling` 里 `usage_pct == None` 直接跳过降级），这里保持一致。
//! - **理由必须可枚举**：`HealthReason` 是枚举而不是字符串，前端才能稳定地
//!   本地化与筛选。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 健康分级。序列化为小写字符串（`"healthy"` / `"warn"` / ...）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    /// 正常服役，运营可以完全不看。
    ///
    /// 同时是 `Default`：只为让持有本类型的结构体能 `derive(Default)`
    /// 做测试构造，不代表"未知即健康"的业务判断。
    #[default]
    Healthy,
    /// 需要本周处理，但现在还能用。
    Warn,
    /// 今天必须处理，随时会不可用或已在降级服役。
    Critical,
    /// 已经不可用，必须人介入（重登 / 换号 / 解封）。
    Dead,
}

/// 分级理由。一条凭据可以同时命中多条。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HealthReason {
    // === Dead ===
    /// 被禁用。`recoverable` 表示自愈机制是否会自动救它
    /// （只有 `TooManyFailures` 参与自愈，见 `token_manager::try_self_heal`）。
    Disabled { reason: String, recoverable: bool },
    /// refresh token 已被上游作废（`InvalidRefreshToken`），必须重登。
    RefreshTokenInvalid,

    // === Critical ===
    /// 额度已耗尽（remaining <= 0）。
    QuotaExhausted,
    /// 额度逼近上限。
    QuotaCritical { usage_pct: f64 },
    /// token 刷新连续失败，尚未到禁用阈值。
    RefreshFailing { count: u32, threshold: u32 },
    /// access token 已过期且未能刷新。
    TokenExpired,

    // === Warn ===
    /// 额度进入预警区。
    QuotaWarn { usage_pct: f64 },
    /// 被额度守卫自动降级，正在后排服役。
    AutoDemoted { from: u32, to: u32 },
    /// 账号级 429 冷却中。
    Throttled { remaining_secs: u64 },
    /// 错误率异常高于全池。
    ErrorRateHigh { pct: f64, samples: u64 },
    /// 不属于任何分组 —— 只有未绑分组的 Key 能用到它，通常意味着"开了号但没挂给客户"。
    Unassigned,
    /// 订阅档位掉到 FREE（或缺失），能力受限（如不支持 opus）。
    SubscriptionDegraded { title: Option<String> },
    /// 余额数据已陈旧，当前的额度判断不可信。
    BalanceStale { age_secs: u64 },
    /// 长期没有被调度到，可能是分组配错或优先级排在最后。
    Idle { days: u64 },
}

/// `disabled_reason` 里表示"连续调用失败过多"的取值。
/// 字符串来自 `token_manager::DisabledReason::as_str`，是七类里唯一参与自愈的一条。
pub const DISABLED_REASON_TOO_MANY_FAILURES: &str = "TooManyFailures";

/// `disabled_reason` 里表示 refresh token 被上游永久作废的取值。
pub const DISABLED_REASON_INVALID_REFRESH_TOKEN: &str = "InvalidRefreshToken";

/// `disabled` 为真但 `disabled_reason` 缺失时的占位（旧数据 / 手工改过 credentials.json）。
const DISABLED_REASON_UNKNOWN: &str = "Unknown";

impl HealthReason {
    /// 这条理由本身对应的严重程度。一条凭据的最终 level 取所有理由的最大值。
    pub fn level(&self) -> HealthLevel {
        match self {
            Self::Disabled { .. } | Self::RefreshTokenInvalid => HealthLevel::Dead,
            Self::QuotaExhausted
            | Self::QuotaCritical { .. }
            | Self::RefreshFailing { .. }
            | Self::TokenExpired => HealthLevel::Critical,
            Self::QuotaWarn { .. }
            | Self::AutoDemoted { .. }
            | Self::Throttled { .. }
            | Self::ErrorRateHigh { .. }
            | Self::Unassigned
            | Self::SubscriptionDegraded { .. }
            | Self::BalanceStale { .. }
            | Self::Idle { .. } => HealthLevel::Warn,
        }
    }

    /// 一句话中文描述，直接进列表行。
    pub fn describe(&self) -> String {
        match self {
            Self::Disabled { reason, recoverable } => format!(
                "已禁用：{}（{}）",
                disabled_reason_label(reason),
                if *recoverable { "自愈会自动恢复" } else { "需人工处理" }
            ),
            Self::RefreshTokenInvalid => "refresh token 已被上游作废，必须重新登录".to_string(),
            Self::QuotaExhausted => "额度已耗尽".to_string(),
            Self::QuotaCritical { usage_pct } => format!("额度告急：已用 {:.1}%", usage_pct),
            Self::RefreshFailing { count, threshold } => {
                format!("token 刷新连续失败 {}/{} 次", count, threshold)
            }
            Self::TokenExpired => "access token 已过期".to_string(),
            Self::QuotaWarn { usage_pct } => format!("额度预警：已用 {:.1}%", usage_pct),
            Self::AutoDemoted { from, to } => {
                format!("已被额度守卫自动降级：优先级 {} → {}", from, to)
            }
            Self::Throttled { remaining_secs } => {
                format!("账号级限流冷却中，剩余 {} 秒", remaining_secs)
            }
            Self::ErrorRateHigh { pct, samples } => {
                format!("错误率偏高：{:.1}%（{} 次样本）", pct, samples)
            }
            Self::Unassigned => "未挂到任何分组，产能闲置".to_string(),
            Self::SubscriptionDegraded { title } => format!(
                "订阅档位受限：{}",
                title.as_deref().unwrap_or("未知")
            ),
            Self::BalanceStale { age_secs } => {
                format!("余额数据已陈旧（{} 秒未刷新），额度判断不可信", age_secs)
            }
            Self::Idle { days } => format!("已 {} 天未被调度到", days),
        }
    }
}

/// 把 `token_manager::DisabledReason` 的英文取值翻成中文；未知取值原样回显。
fn disabled_reason_label(reason: &str) -> &str {
    match reason {
        "Manual" => "手动禁用",
        "TooManyFailures" => "连续调用失败过多",
        "Suspended" => "账号被上游封禁",
        "TooManyRefreshFailures" => "token 刷新连续失败过多",
        "QuotaExceeded" => "额度已用尽",
        "InvalidRefreshToken" => "refresh token 失效",
        "InvalidConfig" => "凭据配置无效",
        other => other,
    }
}

/// 阈值。集中放在这里，将来要做成可配置只改这一处。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthThresholds {
    pub quota_warn_pct: f64,
    pub quota_critical_pct: f64,
    /// 余额超过这个秒数未刷新即视为陈旧。
    pub balance_stale_secs: u64,
    /// 错误率预警线（百分比），且样本数需达到 `error_rate_min_samples`。
    pub error_rate_warn_pct: f64,
    pub error_rate_min_samples: u64,
    /// 多少天没被调度到算 idle。
    pub idle_days: u64,
    /// 与 `token_manager::MAX_FAILURES_PER_CREDENTIAL` 对齐。
    pub refresh_failure_threshold: u32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            quota_warn_pct: 85.0,
            quota_critical_pct: 95.0,
            balance_stale_secs: 900,
            error_rate_warn_pct: 2.0,
            error_rate_min_samples: 200,
            idle_days: 7,
            refresh_failure_threshold: 3,
        }
    }
}

/// `assess` 的全部输入。字段命名与 `CredentialEntrySnapshot` / `BalanceResponse` 对齐。
#[derive(Debug, Clone, Default)]
pub struct HealthInput {
    pub id: u64,
    pub disabled: bool,
    pub disabled_reason: Option<String>,
    pub auto_demoted_from: Option<u32>,
    pub priority: u32,
    pub throttled_remaining_secs: Option<u64>,
    pub refresh_failure_count: u32,
    pub success_count: u64,
    pub total_failure_count: u64,
    pub groups: Vec<String>,
    pub subscription_title: Option<String>,
    /// access token 过期时间（RFC3339）。
    pub expires_at: Option<String>,
    /// 余额百分比；`None` = 拿不到，**不得当成"快用完了"**。
    pub usage_percentage: Option<f64>,
    pub remaining: Option<f64>,
    /// 上游是否为该账号开启了超额（overage）。
    ///
    /// 开了之后 `usage_percentage` 会真的超过 100、`remaining` 会转负，
    /// 而账号**仍在正常服务**（只是多计费）—— `service.rs` 的 `fetch_balance`
    /// 明确保留这个状态。所以这一位为真时不能判成额度耗尽。
    pub overage_enabled: bool,
    /// 余额缓存时间（Unix 秒）。
    pub balance_cached_at: Option<f64>,
    /// 最后一次被调度到的时间（RFC3339）。
    pub last_used_at: Option<String>,
    /// 评估时刻（Unix 秒）。显式传入以便测试。
    pub now_unix: f64,
}

/// 分级结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthAssessment {
    pub level: HealthLevel,
    pub reasons: Vec<HealthReason>,
    /// 一句话摘要，给列表行直接显示（中文）。`Healthy` 时为空串。
    pub headline: String,
}

/// 解析 RFC3339 时间串为 Unix 秒。解析不了返回 `None`，调用方按"没这条信息"处理。
fn rfc3339_to_unix(value: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
}

/// 判断订阅档位是否受限：缺失，或标题里含 `FREE`（大小写不敏感）。
///
/// **只在余额查得到时才有意义**——`title` 为 `None` 既可能是"上游没给档位"，
/// 也可能是"这一轮压根没查到余额"，本函数分不出来，由调用方用
/// `usage_percentage.is_some()` 先把后者挡掉（见 `assess`）。
fn subscription_is_degraded(title: Option<&str>) -> bool {
    match title {
        None => true,
        Some(t) => {
            let upper = t.to_ascii_uppercase();
            t.trim().is_empty() || upper.contains("FREE")
        }
    }
}

/// 评估一条凭据的健康状态。
///
/// 理由按严重程度从高到低依次判定并追加，因此 `reasons` 天然有序；`level`
/// 取其中最严重的一条，`headline` 取该级别里最先命中的那条的描述。
///
/// 余额类理由（`QuotaExhausted` / `QuotaCritical` / `QuotaWarn` / `BalanceStale` /
/// `SubscriptionDegraded`）全部以 `usage_percentage.is_some()` 为前提：这几项的数据
/// 都出自同一份 `BalanceResponse`，拿不到余额时一条都不产出，与 `admin::scheduling`
/// 的额度守卫保持同一取舍。三档额度理由互斥，只出一条。
///
/// 各理由之间不做互相抑制（例如已禁用的凭据仍可能同时带 `Idle`），这样每条
/// 判据可以被单独钉住，不会因为组合顺序变化而漂移。
/// 额度是否真的用尽了。
///
/// 两条判据是 OR：`remaining <= 0`（上游给了剩余量）或 `usage_percentage >= 100`
/// （只给了百分比）。两条都留着是因为不同上游返回的字段不一定齐。
///
/// **开了 overage 的账号一律不算用尽** —— 那种账号的 `remaining` 本来就会转负、
/// 百分比会超过 100，但它仍在正常服务。
fn is_quota_exhausted(input: &HealthInput) -> bool {
    if input.overage_enabled {
        return false;
    }
    input.remaining.is_some_and(|r| r <= 0.0)
        || input.usage_percentage.is_some_and(|p| p >= 100.0)
}

pub fn assess(input: &HealthInput, thresholds: &HealthThresholds) -> HealthAssessment {
    let mut reasons: Vec<HealthReason> = Vec::new();

    // ---- Dead ----
    if input.disabled {
        let reason = input
            .disabled_reason
            .clone()
            .unwrap_or_else(|| DISABLED_REASON_UNKNOWN.to_string());
        // 只有 TooManyFailures 会被 token_manager::try_self_heal 捞回来，其余都要人介入。
        let recoverable = reason == DISABLED_REASON_TOO_MANY_FAILURES;
        reasons.push(HealthReason::Disabled { reason, recoverable });
    }
    if input.disabled_reason.as_deref() == Some(DISABLED_REASON_INVALID_REFRESH_TOKEN) {
        reasons.push(HealthReason::RefreshTokenInvalid);
    }

    // ---- Critical ----
    // 额度三档互斥，且只在拿得到余额时才判。
    // 判据抽成函数是因为 Critical 与 Warn 两处都要用它，写两遍必然漂。
    // 开了 overage 的账号整块跳过：它的百分比可以正常地超过 100，
    // 「快用完了」对它不成立 —— 它不会用完，只会多计费。
    if let Some(pct) = input.usage_percentage
        && !input.overage_enabled
    {
        if is_quota_exhausted(input) {
            reasons.push(HealthReason::QuotaExhausted);
        } else if pct >= thresholds.quota_critical_pct {
            reasons.push(HealthReason::QuotaCritical { usage_pct: pct });
        }
    }

    // 刷新已经在失败但还没撞到禁用阈值 —— 撞到之后由 Disabled 接手。
    if input.refresh_failure_count > 0
        && input.refresh_failure_count < thresholds.refresh_failure_threshold
    {
        reasons.push(HealthReason::RefreshFailing {
            count: input.refresh_failure_count,
            threshold: thresholds.refresh_failure_threshold,
        });
    }

    // 过期判断不留提前量：token_manager 自己带 5 分钟余量做刷新，这里只报"确实已过期"。
    // expires_at 缺失或解析不了 = 没这条信息，不报。
    if let Some(expires_unix) = input.expires_at.as_deref().and_then(rfc3339_to_unix)
        && expires_unix <= input.now_unix
    {
        reasons.push(HealthReason::TokenExpired);
    }

    // ---- Warn ----
    if let Some(pct) = input.usage_percentage
        && !input.overage_enabled
        && !is_quota_exhausted(input)
        && pct < thresholds.quota_critical_pct
        && pct >= thresholds.quota_warn_pct
    {
        reasons.push(HealthReason::QuotaWarn { usage_pct: pct });
    }

    if let Some(from) = input.auto_demoted_from {
        reasons.push(HealthReason::AutoDemoted { from, to: input.priority });
    }

    if let Some(secs) = input.throttled_remaining_secs
        && secs > 0
    {
        reasons.push(HealthReason::Throttled { remaining_secs: secs });
    }

    let samples = input.success_count.saturating_add(input.total_failure_count);
    if samples >= thresholds.error_rate_min_samples && samples > 0 {
        let pct = input.total_failure_count as f64 / samples as f64 * 100.0;
        if pct > thresholds.error_rate_warn_pct {
            reasons.push(HealthReason::ErrorRateHigh { pct, samples });
        }
    }

    if input.groups.is_empty() {
        reasons.push(HealthReason::Unassigned);
    }

    // 订阅档位与 `usage_percentage` 同源（都来自 `BalanceResponse`，见 service.rs
    // `fetch_balance`；`CredentialEntrySnapshot` 里根本没有 subscription_title）。
    // 余额缓存 miss 时两者一起是 None，此时 `title == None` 表示"没查到"而不是
    // "档位掉了"——不加这道闸，一轮余额没刷上就能把整片凭据染成 Warn。
    if input.usage_percentage.is_some()
        && subscription_is_degraded(input.subscription_title.as_deref())
    {
        reasons.push(HealthReason::SubscriptionDegraded {
            title: input.subscription_title.clone(),
        });
    }

    // 余额陈旧只在"有余额数据"时才有意义：没有余额就没有需要被质疑的额度判断。
    if input.usage_percentage.is_some()
        && let Some(cached_at) = input.balance_cached_at
    {
        let age = input.now_unix - cached_at;
        if age > thresholds.balance_stale_secs as f64 {
            reasons.push(HealthReason::BalanceStale { age_secs: age.max(0.0) as u64 });
        }
    }

    // last_used_at 缺失 = 没有可比对的时间点，不判 idle（新导入的号交给 Unassigned 去说）。
    if let Some(last_unix) = input.last_used_at.as_deref().and_then(rfc3339_to_unix) {
        let idle_secs = input.now_unix - last_unix;
        if idle_secs > 0.0 {
            let days = (idle_secs / 86_400.0).floor() as u64;
            if days >= thresholds.idle_days {
                reasons.push(HealthReason::Idle { days });
            }
        }
    }

    let level = reasons
        .iter()
        .map(HealthReason::level)
        .max()
        .unwrap_or(HealthLevel::Healthy);
    let headline = reasons
        .iter()
        .find(|r| r.level() == level)
        .map(HealthReason::describe)
        .unwrap_or_default();

    HealthAssessment { level, reasons, headline }
}

/// 全池汇总，供舰队视图顶部的"折叠成一个数字"使用。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSummary {
    pub total: usize,
    pub healthy: usize,
    pub warn: usize,
    pub critical: usize,
    pub dead: usize,
    /// 不属于任何分组的凭据数（浪费中的产能）。
    pub unassigned: usize,
    /// 每个分组当前有多少条**可用**（level <= Warn 且未禁用）凭据。
    /// 值为 0 的分组是客户即将报错的前兆。
    pub available_by_group: std::collections::BTreeMap<String, usize>,
}

/// 汇总一批评估结果。
///
/// `available_by_group` 先把**出现过的每个分组名**都登记成 0，再累加可用条数，
/// 这样"这个组一条都不剩了"会以 `0` 的形式出现在结果里，而不是整个键消失 ——
/// 键消失时调用方看不出区别，正好漏掉最该报的那种。
pub fn summarize(assessed: &[(HealthInput, HealthAssessment)]) -> FleetSummary {
    let mut summary = FleetSummary {
        total: assessed.len(),
        ..Default::default()
    };
    let mut by_group: BTreeMap<String, usize> = BTreeMap::new();

    for (input, assessment) in assessed {
        match assessment.level {
            HealthLevel::Healthy => summary.healthy += 1,
            HealthLevel::Warn => summary.warn += 1,
            HealthLevel::Critical => summary.critical += 1,
            HealthLevel::Dead => summary.dead += 1,
        }

        if input.groups.is_empty() {
            summary.unassigned += 1;
            continue;
        }

        // 同一条凭据里重复写了同一个组名只算一次
        let groups: BTreeSet<&str> = input.groups.iter().map(String::as_str).collect();
        let available = assessment.level <= HealthLevel::Warn && !input.disabled;
        for group in groups {
            let slot = by_group.entry(group.to_string()).or_insert(0);
            if available {
                *slot += 1;
            }
        }
    }

    summary.available_by_group = by_group;
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_756_000_000.0;

    fn ts(unix: f64) -> String {
        chrono::DateTime::from_timestamp(unix as i64, 0)
            .expect("时间戳可转换")
            .to_rfc3339()
    }

    /// 一条各项都正常的凭据：有分组、有余额且很富余、余额新鲜、token 没过期、刚被用过。
    fn healthy_input() -> HealthInput {
        HealthInput {
            id: 1,
            disabled: false,
            disabled_reason: None,
            auto_demoted_from: None,
            priority: 10,
            throttled_remaining_secs: None,
            refresh_failure_count: 0,
            success_count: 1000,
            total_failure_count: 1,
            groups: vec!["for_O".to_string()],
            subscription_title: Some("KIRO POWER".to_string()),
            expires_at: Some(ts(NOW + 3600.0)),
            usage_percentage: Some(12.5),
            remaining: Some(8750.0),
            overage_enabled: false,
            balance_cached_at: Some(NOW - 30.0),
            last_used_at: Some(ts(NOW - 120.0)),
            now_unix: NOW,
        }
    }

    fn assess_default(input: &HealthInput) -> HealthAssessment {
        assess(input, &HealthThresholds::default())
    }

    // ============ 四个级别各一例 ============

    #[test]
    fn healthy_has_no_reason_and_empty_headline() {
        let a = assess_default(&healthy_input());
        assert_eq!(a.level, HealthLevel::Healthy);
        assert!(a.reasons.is_empty(), "正常凭据不应产出任何理由: {:?}", a.reasons);
        assert_eq!(a.headline, "", "Healthy 的 headline 必须是空串");
    }

    #[test]
    fn quota_warn_band_is_warn() {
        let mut input = healthy_input();
        input.usage_percentage = Some(88.0);
        input.remaining = Some(1200.0);
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Warn);
        assert_eq!(a.reasons, vec![HealthReason::QuotaWarn { usage_pct: 88.0 }]);
        assert!(a.headline.contains("额度预警"), "headline={}", a.headline);
    }

    #[test]
    fn quota_critical_band_is_critical() {
        let mut input = healthy_input();
        input.usage_percentage = Some(96.0);
        input.remaining = Some(400.0);
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Critical);
        assert_eq!(a.reasons, vec![HealthReason::QuotaCritical { usage_pct: 96.0 }]);
        assert!(a.headline.contains("额度告急"), "headline={}", a.headline);
    }

    #[test]
    fn disabled_is_dead() {
        let mut input = healthy_input();
        input.disabled = true;
        input.disabled_reason = Some("Suspended".to_string());
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Dead);
        assert_eq!(
            a.reasons,
            vec![HealthReason::Disabled {
                reason: "Suspended".to_string(),
                recoverable: false,
            }]
        );
        assert!(a.headline.contains("已禁用"), "headline={}", a.headline);
    }

    // ============ 拿不到余额 ≠ 有问题 ============

    #[test]
    fn unknown_balance_produces_no_quota_reason() {
        let mut input = healthy_input();
        input.usage_percentage = None;
        input.remaining = None;
        // 故意把余额缓存时间设成远古：即便"陈旧"，没有额度数据也不该报额度类问题
        input.balance_cached_at = Some(NOW - 86_400.0);
        let a = assess_default(&input);

        assert_eq!(a.level, HealthLevel::Healthy, "余额未知不得被判成有问题");
        assert!(
            a.reasons.is_empty(),
            "余额未知时不得产生任何额度类理由，实际: {:?}",
            a.reasons
        );
    }

    /// 真实的「余额缓存 miss」形态：`usage_percentage` / `remaining` /
    /// `balance_cached_at` / `subscription_title` 出自同一个 `BalanceResponse`，
    /// miss 的时候是**一起**变 None 的（`CredentialEntrySnapshot` 里没有
    /// subscription_title 这个字段，只能从余额来）。
    ///
    /// 只把前两个置空的测法测不出问题：那种输入现实中不存在。
    /// 100 条凭据串行刷余额（每条 sleep 400ms）本来就可能刷不完一轮，
    /// 这条一破，一次没刷上就能把半个舰队染成 Warn。
    #[test]
    fn balance_cache_miss_shape_is_fully_healthy() {
        let mut input = healthy_input();
        input.usage_percentage = None;
        input.remaining = None;
        input.balance_cached_at = None;
        input.subscription_title = None;
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Healthy, "余额整份拿不到不得判成有问题");
        assert!(
            a.reasons.is_empty(),
            "余额缓存 miss 不得产出任何余额派生理由（含 SubscriptionDegraded），实际: {:?}",
            a.reasons
        );
    }

    /// 余额查到了、但上游没给档位 —— 这才是真的"档位缺失"，仍要报。
    #[test]
    fn missing_title_with_balance_present_is_still_degraded() {
        let mut input = healthy_input();
        input.subscription_title = None;
        assert!(input.usage_percentage.is_some());
        let a = assess_default(&input);
        assert_eq!(
            a.reasons,
            vec![HealthReason::SubscriptionDegraded { title: None }],
            "有余额数据时档位缺失仍要报"
        );
    }

    #[test]
    fn unknown_balance_with_zero_remaining_still_produces_no_quota_reason() {
        // remaining 有值但 usage_percentage 没有：仍然不做额度判断
        let mut input = healthy_input();
        input.usage_percentage = None;
        input.remaining = Some(0.0);
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Healthy);
        assert!(a.reasons.is_empty(), "实际: {:?}", a.reasons);
    }

    // ============ 多理由取最严 ============

    #[test]
    fn level_takes_the_most_severe_reason() {
        let mut input = healthy_input();
        // Warn：未分组 + 被降级 + 限流冷却
        input.groups = vec![];
        input.auto_demoted_from = Some(5);
        input.priority = 60;
        input.throttled_remaining_secs = Some(30);
        // Critical：额度告急 + 刷新失败 1/3
        input.usage_percentage = Some(97.0);
        input.remaining = Some(300.0);
        input.refresh_failure_count = 1;
        // Dead：被禁用
        input.disabled = true;
        input.disabled_reason = Some("TooManyFailures".to_string());

        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Dead, "命中多级时必须取最严的 Dead");
        assert!(
            a.reasons.contains(&HealthReason::QuotaCritical { usage_pct: 97.0 }),
            "更轻的理由不应被丢掉: {:?}",
            a.reasons
        );
        assert!(a.reasons.contains(&HealthReason::Unassigned));
        assert!(a.reasons.contains(&HealthReason::AutoDemoted { from: 5, to: 60 }));
        assert!(a.reasons.contains(&HealthReason::Throttled { remaining_secs: 30 }));
        assert!(a.reasons.contains(&HealthReason::RefreshFailing { count: 1, threshold: 3 }));
        assert_eq!(
            a.headline,
            HealthReason::Disabled {
                reason: "TooManyFailures".to_string(),
                recoverable: true,
            }
            .describe(),
            "headline 取最严重那条的描述"
        );
    }

    #[test]
    fn warn_plus_critical_yields_critical() {
        let mut input = healthy_input();
        input.groups = vec![]; // Warn
        input.refresh_failure_count = 2; // Critical
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Critical);
        assert!(a.headline.contains("token 刷新连续失败"), "headline={}", a.headline);
    }

    // ============ 禁用原因映射 ============

    #[test]
    fn only_too_many_failures_is_recoverable() {
        // 字符串取值来自 token_manager::DisabledReason::as_str
        let cases: [(&str, bool); 7] = [
            ("Manual", false),
            ("TooManyFailures", true),
            ("Suspended", false),
            ("TooManyRefreshFailures", false),
            ("QuotaExceeded", false),
            ("InvalidRefreshToken", false),
            ("InvalidConfig", false),
        ];
        for (reason, expect_recoverable) in cases {
            let mut input = healthy_input();
            input.disabled = true;
            input.disabled_reason = Some(reason.to_string());
            let a = assess_default(&input);
            assert_eq!(a.level, HealthLevel::Dead, "{} 应判 Dead", reason);
            let found = a
                .reasons
                .iter()
                .find_map(|r| match r {
                    HealthReason::Disabled { reason: got, recoverable } if got == reason => {
                        Some(*recoverable)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{} 未产出 Disabled 理由: {:?}", reason, a.reasons));
            assert_eq!(found, expect_recoverable, "{} 的 recoverable 判断错了", reason);
        }
    }

    #[test]
    fn invalid_refresh_token_adds_dedicated_reason() {
        let mut input = healthy_input();
        input.disabled = true;
        input.disabled_reason = Some("InvalidRefreshToken".to_string());
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Dead);
        assert!(
            a.reasons.contains(&HealthReason::RefreshTokenInvalid),
            "InvalidRefreshToken 必须额外产出 RefreshTokenInvalid: {:?}",
            a.reasons
        );
        assert!(
            a.reasons.contains(&HealthReason::Disabled {
                reason: "InvalidRefreshToken".to_string(),
                recoverable: false,
            }),
            "同时保留 Disabled 理由: {:?}",
            a.reasons
        );
    }

    #[test]
    fn disabled_without_reason_falls_back_to_unknown_and_not_recoverable() {
        let mut input = healthy_input();
        input.disabled = true;
        input.disabled_reason = None;
        let a = assess_default(&input);
        assert_eq!(
            a.reasons,
            vec![HealthReason::Disabled {
                reason: "Unknown".to_string(),
                recoverable: false,
            }]
        );
    }

    // ============ 各条 Warn 判据 ============

    #[test]
    fn unassigned_when_groups_empty() {
        let mut input = healthy_input();
        input.groups = vec![];
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Warn);
        assert_eq!(a.reasons, vec![HealthReason::Unassigned]);
        assert!(a.headline.contains("未挂到任何分组"), "headline={}", a.headline);
    }

    #[test]
    fn assigned_credential_is_not_unassigned() {
        let input = healthy_input();
        assert!(!input.groups.is_empty());
        let a = assess_default(&input);
        assert!(!a.reasons.contains(&HealthReason::Unassigned));
    }

    #[test]
    fn balance_stale_beyond_threshold() {
        let mut input = healthy_input();
        input.balance_cached_at = Some(NOW - 1000.0); // 阈值 900s
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Warn);
        assert_eq!(a.reasons, vec![HealthReason::BalanceStale { age_secs: 1000 }]);
        assert!(a.headline.contains("陈旧"), "headline={}", a.headline);
    }

    #[test]
    fn balance_fresh_within_threshold_is_not_stale() {
        let mut input = healthy_input();
        input.balance_cached_at = Some(NOW - 899.0);
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(r, HealthReason::BalanceStale { .. })),
            "899s < 900s 阈值不该判陈旧: {:?}",
            a.reasons
        );
    }

    #[test]
    fn idle_when_not_scheduled_for_threshold_days() {
        let mut input = healthy_input();
        input.last_used_at = Some(ts(NOW - 9.0 * 86_400.0));
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Warn);
        assert_eq!(a.reasons, vec![HealthReason::Idle { days: 9 }]);
        assert!(a.headline.contains("未被调度"), "headline={}", a.headline);
    }

    #[test]
    fn recently_used_is_not_idle() {
        let mut input = healthy_input();
        input.last_used_at = Some(ts(NOW - 6.9 * 86_400.0));
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(r, HealthReason::Idle { .. })),
            "不足 7 天不该判 idle: {:?}",
            a.reasons
        );
    }

    #[test]
    fn missing_last_used_at_is_not_idle() {
        let mut input = healthy_input();
        input.last_used_at = None;
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(r, HealthReason::Idle { .. })),
            "没有 last_used_at 就没有可比对的时间点，不判 idle: {:?}",
            a.reasons
        );
    }

    #[test]
    fn throttled_reports_remaining_secs() {
        let mut input = healthy_input();
        input.throttled_remaining_secs = Some(45);
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::Throttled { remaining_secs: 45 }]);
        assert_eq!(a.level, HealthLevel::Warn);
    }

    #[test]
    fn zero_throttle_remaining_is_not_a_reason() {
        let mut input = healthy_input();
        input.throttled_remaining_secs = Some(0);
        let a = assess_default(&input);
        assert!(a.reasons.is_empty(), "冷却已到期不该继续报: {:?}", a.reasons);
    }

    #[test]
    fn auto_demoted_records_from_and_to() {
        let mut input = healthy_input();
        input.auto_demoted_from = Some(20);
        input.priority = 60;
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::AutoDemoted { from: 20, to: 60 }]);
    }

    #[test]
    fn error_rate_high_needs_enough_samples() {
        let mut input = healthy_input();
        // 样本不足：50 次里错 25 次，错误率 50% 但样本 < 200
        input.success_count = 25;
        input.total_failure_count = 25;
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(r, HealthReason::ErrorRateHigh { .. })),
            "样本不足不该报错误率: {:?}",
            a.reasons
        );

        // 样本足够：1000 次里错 50 次 = 5% > 2%
        input.success_count = 950;
        input.total_failure_count = 50;
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::ErrorRateHigh { pct: 5.0, samples: 1000 }]);
        assert_eq!(a.level, HealthLevel::Warn);
    }

    #[test]
    fn subscription_free_or_missing_is_degraded() {
        for title in [None, Some("FREE".to_string()), Some("Kiro Free Tier".to_string())] {
            let mut input = healthy_input();
            input.subscription_title = title.clone();
            let a = assess_default(&input);
            assert!(
                a.reasons.contains(&HealthReason::SubscriptionDegraded { title: title.clone() }),
                "{:?} 应判订阅受限: {:?}",
                title,
                a.reasons
            );
            assert_eq!(a.level, HealthLevel::Warn);
        }
    }

    // ============ Critical 判据 ============

    #[test]
    fn quota_exhausted_when_remaining_not_positive() {
        let mut input = healthy_input();
        input.usage_percentage = Some(100.0);
        input.remaining = Some(0.0);
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Critical);
        assert_eq!(a.reasons, vec![HealthReason::QuotaExhausted]);
        assert!(a.headline.contains("耗尽"), "headline={}", a.headline);
    }

    // 下面四个测试是对抗审查用变异测试挖出来的覆盖缺口：原来所有构造
    // QuotaExhausted 的用例都同时让 `remaining<=0` 和 `pct>=100` 成立，
    // 于是删掉任意一支、或把 100.0 改成 100_000.0，全套测试仍然全绿。
    // 每一支单独钉一个。

    #[test]
    fn exhausted_by_remaining_alone_even_when_pct_below_100() {
        let mut input = healthy_input();
        input.usage_percentage = Some(60.0); // 远低于任何额度阈值
        input.remaining = Some(0.0);
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::QuotaExhausted]);
    }

    #[test]
    fn exhausted_by_pct_alone_when_remaining_unknown() {
        let mut input = healthy_input();
        input.usage_percentage = Some(100.0);
        input.remaining = None; // 上游只给了百分比
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::QuotaExhausted]);
    }

    #[test]
    fn overage_account_over_100_pct_is_not_exhausted() {
        // 开了 overage 的账号百分比会真的超过 100、remaining 会转负，
        // 但它仍在正常服务（只是多计费），不能判成用尽。
        let mut input = healthy_input();
        input.overage_enabled = true;
        input.usage_percentage = Some(137.0);
        input.remaining = Some(-3_700.0);
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(
                r,
                HealthReason::QuotaExhausted
                    | HealthReason::QuotaCritical { .. }
                    | HealthReason::QuotaWarn { .. }
            )),
            "开了 overage 不该出额度类理由，实际 ={:?}",
            a.reasons
        );
        assert_eq!(a.level, HealthLevel::Healthy);
    }

    #[test]
    fn overage_disabled_over_100_pct_is_still_exhausted() {
        // 与上一条配对：把 overage 关掉，同一份数据必须翻回耗尽。
        // 没有这条配对，上面那个测试无法区分"overage 生效"与"这段代码没跑"。
        let mut input = healthy_input();
        input.overage_enabled = false;
        input.usage_percentage = Some(137.0);
        input.remaining = Some(-3_700.0);
        assert_eq!(assess_default(&input).reasons, vec![HealthReason::QuotaExhausted]);
    }

    // 边界值：原来的用例都用 88.0/96.0/9天 这种明显偏离阈值的值，
    // 于是把 `>=` 变异成 `>`（或反向）全套仍绿。下面精确落在阈值上。

    #[test]
    fn quota_critical_fires_exactly_at_threshold() {
        let t = HealthThresholds::default();
        let mut input = healthy_input();
        input.usage_percentage = Some(t.quota_critical_pct); // 恰好 95.0
        input.remaining = Some(500.0);
        assert_eq!(
            assess(&input, &t).reasons,
            vec![HealthReason::QuotaCritical { usage_pct: t.quota_critical_pct }],
            "阈值上应判 critical（判据是 >=）"
        );
    }

    #[test]
    fn quota_warn_fires_exactly_at_threshold() {
        let t = HealthThresholds::default();
        let mut input = healthy_input();
        input.usage_percentage = Some(t.quota_warn_pct); // 恰好 85.0
        input.remaining = Some(1_500.0);
        assert_eq!(
            assess(&input, &t).reasons,
            vec![HealthReason::QuotaWarn { usage_pct: t.quota_warn_pct }]
        );
    }

    #[test]
    fn just_below_warn_threshold_is_healthy() {
        let t = HealthThresholds::default();
        let mut input = healthy_input();
        input.usage_percentage = Some(t.quota_warn_pct - 0.1);
        input.remaining = Some(1_500.0);
        assert_eq!(assess(&input, &t).level, HealthLevel::Healthy);
    }

    #[test]
    fn token_expired_fires_exactly_at_now() {
        // 判据是 `expires_at <= now`，等号那一刻必须算过期。
        let mut input = healthy_input();
        let now = input.now_unix;
        input.expires_at = Some(ts(now));
        assert!(
            assess_default(&input).reasons.contains(&HealthReason::TokenExpired),
            "过期时刻恰好等于 now 时应判过期"
        );
    }

    #[test]
    fn token_one_second_from_expiry_is_not_expired() {
        let mut input = healthy_input();
        let now = input.now_unix;
        input.expires_at = Some(ts(now + 1.0));
        assert!(!assess_default(&input).reasons.contains(&HealthReason::TokenExpired));
    }

    #[test]
    fn idle_fires_exactly_at_threshold_days() {
        let t = HealthThresholds::default();
        let mut input = healthy_input();
        let secs = t.idle_days as f64 * 86_400.0;
        input.last_used_at = Some(ts(input.now_unix - secs));
        assert!(
            assess(&input, &t)
                .reasons
                .iter()
                .any(|r| matches!(r, HealthReason::Idle { .. })),
            "恰好满 idle_days 应判 idle（判据是 >=）"
        );
    }

    #[test]
    fn error_rate_fires_just_above_threshold_not_at_it() {
        // 判据是严格 `>`：恰好等于阈值不报，略高才报。
        let t = HealthThresholds::default();
        let samples = t.error_rate_min_samples;
        let at = (samples as f64 * t.error_rate_warn_pct / 100.0).round() as u64;
        let mut input = healthy_input();
        input.total_failure_count = at;
        input.success_count = samples - at;
        assert!(
            !assess(&input, &t)
                .reasons
                .iter()
                .any(|r| matches!(r, HealthReason::ErrorRateHigh { .. })),
            "恰好等于阈值不应报（判据是严格 >）"
        );

        input.total_failure_count = at + 1;
        input.success_count = samples - at - 1;
        assert!(
            assess(&input, &t)
                .reasons
                .iter()
                .any(|r| matches!(r, HealthReason::ErrorRateHigh { .. })),
            "略高于阈值应报"
        );
    }

    #[test]
    fn quota_bands_are_mutually_exclusive() {
        let mut input = healthy_input();
        input.usage_percentage = Some(99.0);
        input.remaining = Some(100.0);
        let a = assess_default(&input);
        let quota_reasons: Vec<_> = a
            .reasons
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    HealthReason::QuotaExhausted
                        | HealthReason::QuotaCritical { .. }
                        | HealthReason::QuotaWarn { .. }
                )
            })
            .collect();
        assert_eq!(quota_reasons.len(), 1, "额度三档必须互斥: {:?}", a.reasons);
        assert_eq!(quota_reasons[0], &HealthReason::QuotaCritical { usage_pct: 99.0 });
    }

    #[test]
    fn refresh_failing_only_below_threshold() {
        let mut input = healthy_input();
        input.refresh_failure_count = 2;
        let a = assess_default(&input);
        assert_eq!(a.reasons, vec![HealthReason::RefreshFailing { count: 2, threshold: 3 }]);
        assert_eq!(a.level, HealthLevel::Critical);

        // 撞到阈值后由 token_manager 禁用，这里不再重复报
        input.refresh_failure_count = 3;
        let a = assess_default(&input);
        assert!(
            !a.reasons.iter().any(|r| matches!(r, HealthReason::RefreshFailing { .. })),
            "到阈值后交给 Disabled 表达: {:?}",
            a.reasons
        );
    }

    #[test]
    fn token_expired_uses_passed_in_now() {
        let mut input = healthy_input();
        input.expires_at = Some(ts(NOW - 1.0));
        let a = assess_default(&input);
        assert_eq!(a.level, HealthLevel::Critical);
        assert_eq!(a.reasons, vec![HealthReason::TokenExpired]);

        // 同一份数据换一个更早的 now_unix，就不该判过期 —— 证明没有偷用 Utc::now()
        input.now_unix = NOW - 3600.0;
        let a = assess_default(&input);
        assert!(
            !a.reasons.contains(&HealthReason::TokenExpired),
            "过期判断必须用传入的 now_unix: {:?}",
            a.reasons
        );
    }

    #[test]
    fn unparseable_timestamps_are_ignored() {
        let mut input = healthy_input();
        input.expires_at = Some("not-a-time".to_string());
        input.last_used_at = Some("2026/08/27".to_string());
        let a = assess_default(&input);
        assert!(a.reasons.is_empty(), "解析不了的时间串按缺失处理: {:?}", a.reasons);
    }

    #[test]
    fn thresholds_are_configurable() {
        let mut input = healthy_input();
        input.usage_percentage = Some(50.0);
        input.remaining = Some(5000.0);
        let strict = HealthThresholds {
            quota_warn_pct: 40.0,
            quota_critical_pct: 45.0,
            ..HealthThresholds::default()
        };
        let a = assess(&input, &strict);
        assert_eq!(a.reasons, vec![HealthReason::QuotaCritical { usage_pct: 50.0 }]);
    }

    // ============ summarize ============

    fn pair(input: HealthInput) -> (HealthInput, HealthAssessment) {
        let a = assess_default(&input);
        (input, a)
    }

    #[test]
    fn summarize_counts_each_level() {
        let mut warn = healthy_input();
        warn.id = 2;
        warn.groups = vec![];

        let mut critical = healthy_input();
        critical.id = 3;
        critical.usage_percentage = Some(97.0);
        critical.remaining = Some(300.0);

        let mut dead = healthy_input();
        dead.id = 4;
        dead.disabled = true;
        dead.disabled_reason = Some("Suspended".to_string());

        let s = summarize(&[pair(healthy_input()), pair(warn), pair(critical), pair(dead)]);
        assert_eq!(s.total, 4);
        assert_eq!(s.healthy, 1);
        assert_eq!(s.warn, 1);
        assert_eq!(s.critical, 1);
        assert_eq!(s.dead, 1);
        assert_eq!(s.unassigned, 1);
    }

    #[test]
    fn available_by_group_keeps_zero_valued_groups() {
        // for_O 下两条都不可用（一条 Dead、一条 Critical），for_tianxiao 有一条健康
        let mut dead = healthy_input();
        dead.id = 1;
        dead.groups = vec!["for_O".to_string()];
        dead.disabled = true;
        dead.disabled_reason = Some("Suspended".to_string());

        let mut critical = healthy_input();
        critical.id = 2;
        critical.groups = vec!["for_O".to_string()];
        critical.usage_percentage = Some(96.0);
        critical.remaining = Some(400.0);

        let mut ok = healthy_input();
        ok.id = 3;
        ok.groups = vec!["for_tianxiao".to_string()];

        let s = summarize(&[pair(dead), pair(critical), pair(ok)]);
        assert_eq!(
            s.available_by_group.get("for_O"),
            Some(&0),
            "一条都不剩的分组必须以 0 出现，而不是键消失: {:?}",
            s.available_by_group
        );
        assert_eq!(s.available_by_group.get("for_tianxiao"), Some(&1));
        assert_eq!(s.available_by_group.len(), 2);
    }

    #[test]
    fn available_counts_warn_but_not_disabled() {
        // Warn 且未禁用 → 算可用
        let mut warn = healthy_input();
        warn.id = 1;
        warn.groups = vec!["g".to_string()];
        warn.throttled_remaining_secs = Some(10);

        // 禁用（Dead）→ 不算
        let mut off = healthy_input();
        off.id = 2;
        off.groups = vec!["g".to_string()];
        off.disabled = true;
        off.disabled_reason = Some("Manual".to_string());

        let s = summarize(&[pair(warn), pair(off)]);
        assert_eq!(s.available_by_group.get("g"), Some(&1));
    }

    #[test]
    fn unassigned_credentials_do_not_create_group_keys() {
        let mut lonely = healthy_input();
        lonely.groups = vec![];
        let s = summarize(&[pair(lonely)]);
        assert_eq!(s.unassigned, 1);
        assert!(s.available_by_group.is_empty(), "无分组的凭据不产生分组键");
    }

    #[test]
    fn duplicate_group_names_count_once() {
        let mut dup = healthy_input();
        dup.groups = vec!["g".to_string(), "g".to_string()];
        let s = summarize(&[pair(dup)]);
        assert_eq!(s.available_by_group.get("g"), Some(&1), "同一条凭据重复写组名只算一次");
    }

    #[test]
    fn summarize_of_empty_input_is_all_zero() {
        let s = summarize(&[]);
        assert_eq!(s, FleetSummary::default());
    }

    #[test]
    fn level_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&HealthLevel::Warn).unwrap(), "\"warn\"");
        assert_eq!(serde_json::to_string(&HealthLevel::Dead).unwrap(), "\"dead\"");
    }

    #[test]
    fn level_ordering_is_healthy_to_dead() {
        assert!(HealthLevel::Healthy < HealthLevel::Warn);
        assert!(HealthLevel::Warn < HealthLevel::Critical);
        assert!(HealthLevel::Critical < HealthLevel::Dead);
    }
}
