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

/// 健康分级。序列化为小写字符串（`"healthy"` / `"warn"` / ...）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    /// 正常服役，运营可以完全不看。
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

/// 评估一条凭据的健康状态。
///
/// TODO(agent): 实现。要求见本文件头部与 `HealthReason` 各分支的注释。
pub fn assess(_input: &HealthInput, _thresholds: &HealthThresholds) -> HealthAssessment {
    unimplemented!("fleet_health::assess")
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
/// TODO(agent): 实现。
pub fn summarize(_assessed: &[(HealthInput, HealthAssessment)]) -> FleetSummary {
    unimplemented!("fleet_health::summarize")
}
</content>
