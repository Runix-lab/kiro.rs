//! 主动告警 —— 100 个号时唯一能让人不盯面板的东西。
//!
//! # 现状（本模块要解掉的）
//!
//! 全仓此前**没有任何外发通知**。凭据被自动禁用、全池耗尽、余额刷新连续失败，
//! 都只落进 `tracing::error!` 的容器日志，没有人会去看。发现延迟 = 下次打开面板的时间。
//!
//! # 设计约束
//!
//! - 🔴 **webhook URL 不出现在日志、API 响应或任何回显里**：对外只暴露
//!   `PublicAlertConfig`（一个 `webhook_configured: bool`，无 URL 字段）。
//!   这条约束由单测 `public_config_omits_webhook_url` 钉住。
//! - **只在状态跨级时发，不在每轮轮询时发**。调用方持有上一轮的级别，本模块
//!   提供 `transitions()` 做差分。
//! - **去抖是必须的**：同一 (凭据, 事件类型) 在冷却窗口内只发一次，否则 100 个号
//!   在月末会同时刷屏，等于没有告警。
//! - **时钟可注入**：所有时间判断走显式传入的 `now`，便于单测。
//! - **发送失败不能影响主流程**：告警是旁路，失败只记日志。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 告警级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// 客户已经或即将受影响，立刻要看。
    P0,
    /// 今天要处理。
    P1,
    /// 知会即可。
    P2,
}

/// 告警事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum AlertEvent {
    /// 凭据被禁用（含自动）。
    CredentialDisabled {
        id: u64,
        email: Option<String>,
        reason: String,
        /// 自愈机制是否会自动救它。
        recoverable: bool,
    },
    /// token 刷新连续失败但尚未禁用。
    CredentialRefreshFailing { id: u64, email: Option<String>, count: u32 },
    /// 额度跨过 critical 线。
    CredentialQuotaCritical { id: u64, email: Option<String>, usage_pct: f64, remaining: f64 },
    /// 额度跨过 warn 线。
    CredentialQuotaWarn { id: u64, email: Option<String>, usage_pct: f64 },
    /// 被额度守卫自动降级。
    CredentialDemoted { id: u64, email: Option<String>, from: u32, to: u32 },
    /// 订阅档位发生变化（掉档尤其要报）。
    CredentialSubscriptionChanged { id: u64, email: Option<String>, from: Option<String>, to: Option<String> },
    /// 🔴 **某个分组下已经没有可用凭据** —— 该分组的客户下一个请求就会拿到 502。
    /// 这是全部事件里最该第一时间知道的一条。
    GroupStarved { group: String, total_in_group: usize },
    /// 全池健康凭据数低于阈值。
    PoolCapacityLow { healthy: usize, total: usize, threshold: usize },
    /// 余额刷新连续失败 —— 这条不报的话，额度守卫会静默失效。
    BalanceRefreshFailing { consecutive_rounds: u32, last_error: String },
}

impl AlertEvent {
    /// 去抖键：同一凭据同一类事件共用一个冷却窗口。
    ///
    /// TODO(agent): 实现。形如 `"credentialDisabled:7"` / `"groupStarved:for_O"`。
    pub fn dedupe_key(&self) -> String {
        unimplemented!("AlertEvent::dedupe_key")
    }

    /// TODO(agent): 实现。`GroupStarved` / `CredentialDisabled(不可自愈)` /
    /// `BalanceRefreshFailing` 属 P0。
    pub fn severity(&self) -> AlertSeverity {
        unimplemented!("AlertEvent::severity")
    }

    /// 一行中文标题，用于卡片主标题与日志。
    ///
    /// TODO(agent): 实现。**不得包含任何 token / webhook / key 明文。**
    pub fn title(&self) -> String {
        unimplemented!("AlertEvent::title")
    }
}

/// 告警配置。持久化在 `config.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 飞书群机器人 webhook。
    /// 🔴 **只写不读**：任何对外接口都不得回传这个字段，只回传 `configured: bool`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// 低于此级别的事件不发送。
    #[serde(default = "default_min_severity")]
    pub min_severity: AlertSeverity,
    /// 去抖窗口（秒）。P0 事件使用 `dedupe_secs_p0`。
    #[serde(default = "default_dedupe_secs")]
    pub dedupe_secs: u64,
    #[serde(default = "default_dedupe_secs_p0")]
    pub dedupe_secs_p0: u64,
    /// 全池健康数低于此值报 `PoolCapacityLow`。
    #[serde(default = "default_pool_low_threshold")]
    pub pool_low_threshold: usize,
}

fn default_min_severity() -> AlertSeverity {
    AlertSeverity::P2
}
fn default_dedupe_secs() -> u64 {
    1800
}
fn default_dedupe_secs_p0() -> u64 {
    600
}
fn default_pool_low_threshold() -> usize {
    2
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: None,
            min_severity: default_min_severity(),
            dedupe_secs: default_dedupe_secs(),
            dedupe_secs_p0: default_dedupe_secs_p0(),
            pool_low_threshold: default_pool_low_threshold(),
        }
    }
}

/// 去抖 + 分发。
pub struct AlertDispatcher {
    /// dedupe_key -> 上次发送时间（Unix 秒）
    last_sent: parking_lot::Mutex<HashMap<String, f64>>,
    config: parking_lot::RwLock<AlertConfig>,
    client: reqwest::Client,
}

impl AlertDispatcher {
    /// TODO(agent): 实现。
    pub fn new(_config: AlertConfig, _client: reqwest::Client) -> Self {
        unimplemented!("AlertDispatcher::new")
    }

    /// 判断该事件此刻是否应发送，**并在返回 true 时记录发送时间**。
    /// 纯内存判断，可单测（`now` 显式传入）。
    ///
    /// TODO(agent): 实现。要点：`enabled` 关闭 → false；级别低于 `min_severity` → false；
    /// 未配置 webhook → false；冷却窗口内 → false。
    pub fn should_emit(&self, _event: &AlertEvent, _now: f64) -> bool {
        unimplemented!("AlertDispatcher::should_emit")
    }

    /// 实际发送。失败只记日志，不向上传播。
    /// 🔴 日志里不得出现 webhook URL。
    ///
    /// TODO(agent): 实现。
    pub async fn dispatch(&self, _event: &AlertEvent, _now: f64) {
        unimplemented!("AlertDispatcher::dispatch")
    }

    /// TODO(agent): 实现。返回不含 `webhook_url` 的安全视图。
    pub fn public_config(&self) -> PublicAlertConfig {
        unimplemented!("AlertDispatcher::public_config")
    }

    /// TODO(agent): 实现。`webhook_url: None` 表示不改，`Some("")` 表示清除。
    pub fn update_config(&self, _patch: AlertConfigPatch) {
        unimplemented!("AlertDispatcher::update_config")
    }
}

/// 对外暴露的配置视图 —— **没有 webhook_url 字段**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAlertConfig {
    pub enabled: bool,
    /// 是否已配置 webhook（不回传内容）。
    pub webhook_configured: bool,
    pub min_severity: AlertSeverity,
    pub dedupe_secs: u64,
    pub dedupe_secs_p0: u64,
    pub pool_low_threshold: usize,
}

/// 配置补丁。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertConfigPatch {
    pub enabled: Option<bool>,
    pub webhook_url: Option<String>,
    pub min_severity: Option<AlertSeverity>,
    pub dedupe_secs: Option<u64>,
    pub dedupe_secs_p0: Option<u64>,
    pub pool_low_threshold: Option<usize>,
}

/// 构造飞书群机器人的交互式卡片消息体。
///
/// TODO(agent): 实现。飞书 `msg_type: "interactive"` + `card`。
/// 标题带级别色（P0 红 / P1 橙 / P2 蓝），正文列关键字段，**不含任何密钥**。
pub fn feishu_payload(_event: &AlertEvent) -> serde_json::Value {
    unimplemented!("alerts::feishu_payload")
}

/// 状态差分：给定上一轮与本轮的分级，产出需要发送的事件。
///
/// 调用方持有上一轮快照；本函数只做纯计算。
///
/// TODO(agent): 实现。只在**跨级变差**时产生事件（healthy→warn 报，warn→healthy 不报）。
pub fn transitions(
    _prev: &HashMap<u64, crate::admin::fleet_health::HealthLevel>,
    _now: &HashMap<u64, crate::admin::fleet_health::HealthLevel>,
) -> Vec<u64> {
    unimplemented!("alerts::transitions")
}
</content>
