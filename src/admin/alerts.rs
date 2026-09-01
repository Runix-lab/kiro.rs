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
use std::time::Duration;

/// 单次发送的超时。告警是旁路，不值得为它长时间占住连接。
const DISPATCH_TIMEOUT_SECS: u64 = 10;

/// 卡片单个字段的字符上限。
///
/// `reason` / `last_error` 是调用方透传的上游文本，本仓其它地方（`token_manager`）
/// 会把**整个未截断的上游响应体**塞进错误串（例如 `format!("... : {}", body_text)`），
/// 一张 CloudFront/ALB 的 HTML 错误页就有几 KB。飞书卡片超过体积上限会被整条丢弃，
/// 而此时去抖窗口已经占用 → 这条告警在整个窗口内彻底消失。
/// 由单测 `card_fields_are_clipped_so_the_card_stays_deliverable` 钉住。
const CARD_FIELD_MAX_CHARS: usize = 512;

/// 按字符边界裁剪，避免把 UTF-8 切碎（中文场景必需）。
fn clip(value: String) -> String {
    if value.chars().count() < CARD_FIELD_MAX_CHARS {
        return value;
    }
    let head: String = value.chars().take(CARD_FIELD_MAX_CHARS).collect();
    format!("{}…(已截断)", head)
}

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

impl AlertSeverity {
    /// 卡片正文与日志里显示的短名。
    pub fn as_str(self) -> &'static str {
        match self {
            AlertSeverity::P0 => "P0",
            AlertSeverity::P1 => "P1",
            AlertSeverity::P2 => "P2",
        }
    }

    /// 飞书卡片 header 的配色模板。
    pub fn card_template(self) -> &'static str {
        match self {
            AlertSeverity::P0 => "red",
            AlertSeverity::P1 => "orange",
            AlertSeverity::P2 => "blue",
        }
    }
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

/// 拼一个人能读的凭据标识。email 缺失时只留 id。
fn credential_label(id: u64, email: &Option<String>) -> String {
    match email.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => format!("#{} {}", id, e),
        None => format!("#{}", id),
    }
}

impl AlertEvent {
    /// 事件类型名。与 serde 的 `event` tag 取值保持一致
    /// （由单测 `kind_matches_serde_event_tag` 钉住）。
    pub fn kind(&self) -> &'static str {
        match self {
            AlertEvent::CredentialDisabled { .. } => "credentialDisabled",
            AlertEvent::CredentialRefreshFailing { .. } => "credentialRefreshFailing",
            AlertEvent::CredentialQuotaCritical { .. } => "credentialQuotaCritical",
            AlertEvent::CredentialQuotaWarn { .. } => "credentialQuotaWarn",
            AlertEvent::CredentialDemoted { .. } => "credentialDemoted",
            AlertEvent::CredentialSubscriptionChanged { .. } => "credentialSubscriptionChanged",
            AlertEvent::GroupStarved { .. } => "groupStarved",
            AlertEvent::PoolCapacityLow { .. } => "poolCapacityLow",
            AlertEvent::BalanceRefreshFailing { .. } => "balanceRefreshFailing",
        }
    }

    /// 去抖作用域：凭据级事件用凭据 id，分组级用分组名，全池级用 `global`。
    fn dedupe_scope(&self) -> String {
        match self {
            AlertEvent::CredentialDisabled { id, .. }
            | AlertEvent::CredentialRefreshFailing { id, .. }
            | AlertEvent::CredentialQuotaCritical { id, .. }
            | AlertEvent::CredentialQuotaWarn { id, .. }
            | AlertEvent::CredentialDemoted { id, .. }
            | AlertEvent::CredentialSubscriptionChanged { id, .. } => id.to_string(),
            AlertEvent::GroupStarved { group, .. } => group.clone(),
            AlertEvent::PoolCapacityLow { .. } | AlertEvent::BalanceRefreshFailing { .. } => {
                "global".to_string()
            }
        }
    }

    /// 去抖键：同一凭据同一类事件共用一个冷却窗口。
    ///
    /// 形如 `"credentialDisabled:7"` / `"groupStarved:for_O"`。
    /// 换凭据或换事件类型就是另一个键，互不影响。
    pub fn dedupe_key(&self) -> String {
        format!("{}:{}", self.kind(), self.dedupe_scope())
    }

    /// 事件级别。`GroupStarved` / `CredentialDisabled(不可自愈)` /
    /// `BalanceRefreshFailing` 属 P0。
    pub fn severity(&self) -> AlertSeverity {
        match self {
            AlertEvent::GroupStarved { .. } | AlertEvent::BalanceRefreshFailing { .. } => {
                AlertSeverity::P0
            }
            // 自愈机制会自动救回来的禁用降一档，不值得半夜叫醒人
            AlertEvent::CredentialDisabled { recoverable, .. } => {
                if *recoverable { AlertSeverity::P2 } else { AlertSeverity::P0 }
            }
            AlertEvent::CredentialQuotaCritical { .. }
            | AlertEvent::CredentialRefreshFailing { .. }
            | AlertEvent::CredentialSubscriptionChanged { .. }
            | AlertEvent::PoolCapacityLow { .. } => AlertSeverity::P1,
            AlertEvent::CredentialQuotaWarn { .. } | AlertEvent::CredentialDemoted { .. } => {
                AlertSeverity::P2
            }
        }
    }

    /// 一行中文标题，用于卡片主标题与日志。
    ///
    /// 只取事件自身的元数据（id / email / 计数 / 百分比），不碰 token、webhook 或 key。
    pub fn title(&self) -> String {
        match self {
            AlertEvent::CredentialDisabled { id, email, recoverable, .. } => format!(
                "凭据 {} 已被禁用（{}）",
                credential_label(*id, email),
                if *recoverable { "等待自愈" } else { "需人工介入" }
            ),
            AlertEvent::CredentialRefreshFailing { id, email, count } => format!(
                "凭据 {} token 刷新连续失败 {} 次",
                credential_label(*id, email),
                count
            ),
            AlertEvent::CredentialQuotaCritical { id, email, usage_pct, .. } => format!(
                "凭据 {} 额度告急：已用 {:.1}%",
                credential_label(*id, email),
                usage_pct
            ),
            AlertEvent::CredentialQuotaWarn { id, email, usage_pct } => format!(
                "凭据 {} 额度预警：已用 {:.1}%",
                credential_label(*id, email),
                usage_pct
            ),
            AlertEvent::CredentialDemoted { id, email, from, to } => format!(
                "凭据 {} 已自动降级：优先级 {} → {}",
                credential_label(*id, email),
                from,
                to
            ),
            AlertEvent::CredentialSubscriptionChanged { id, email, from, to } => format!(
                "凭据 {} 订阅档位变化：{} → {}",
                credential_label(*id, email),
                from.as_deref().unwrap_or("未知"),
                to.as_deref().unwrap_or("未知")
            ),
            AlertEvent::GroupStarved { group, total_in_group } => {
                format!("分组 {} 已无可用凭据（组内共 {} 条）", group, total_in_group)
            }
            AlertEvent::PoolCapacityLow { healthy, total, threshold } => {
                format!("全池可用凭据不足：{}/{}（阈值 {}）", healthy, total, threshold)
            }
            AlertEvent::BalanceRefreshFailing { consecutive_rounds, .. } => {
                format!("余额刷新已连续失败 {} 轮", consecutive_rounds)
            }
        }
    }

    /// 卡片正文要列的关键字段。与 `title` 同源，同样只含事件元数据。
    ///
    /// 调用方透传的自由文本（`reason` / `last_error`）一律过 [`clip`]，
    /// 否则一张几 KB 的上游 HTML 错误页会把整条告警撑到发不出去。
    fn card_fields(&self) -> Vec<(&'static str, String)> {
        match self {
            AlertEvent::CredentialDisabled { id, email, reason, recoverable } => vec![
                ("凭据", credential_label(*id, email)),
                ("原因", clip(reason.clone())),
                ("可自愈", if *recoverable { "是".into() } else { "否".into() }),
            ],
            AlertEvent::CredentialRefreshFailing { id, email, count } => vec![
                ("凭据", credential_label(*id, email)),
                ("连续失败", format!("{} 次", count)),
            ],
            AlertEvent::CredentialQuotaCritical { id, email, usage_pct, remaining } => vec![
                ("凭据", credential_label(*id, email)),
                ("已用", format!("{:.1}%", usage_pct)),
                ("剩余", format!("{:.0}", remaining)),
            ],
            AlertEvent::CredentialQuotaWarn { id, email, usage_pct } => vec![
                ("凭据", credential_label(*id, email)),
                ("已用", format!("{:.1}%", usage_pct)),
            ],
            AlertEvent::CredentialDemoted { id, email, from, to } => vec![
                ("凭据", credential_label(*id, email)),
                ("优先级", format!("{} → {}", from, to)),
            ],
            AlertEvent::CredentialSubscriptionChanged { id, email, from, to } => vec![
                ("凭据", credential_label(*id, email)),
                (
                    "档位",
                    format!(
                        "{} → {}",
                        from.as_deref().unwrap_or("未知"),
                        to.as_deref().unwrap_or("未知")
                    ),
                ),
            ],
            AlertEvent::GroupStarved { group, total_in_group } => vec![
                ("分组", group.clone()),
                ("组内凭据", format!("{} 条", total_in_group)),
                ("影响", "该分组的请求会直接失败".into()),
            ],
            AlertEvent::PoolCapacityLow { healthy, total, threshold } => vec![
                ("可用/总数", format!("{}/{}", healthy, total)),
                ("阈值", threshold.to_string()),
            ],
            AlertEvent::BalanceRefreshFailing { consecutive_rounds, last_error } => vec![
                ("连续失败", format!("{} 轮", consecutive_rounds)),
                ("最近错误", clip(last_error.clone())),
            ],
        }
    }
}

/// 告警配置。持久化在 `config.json`。
///
/// `Debug` 是手写的：派生实现会把 `webhook_url` 原样打印，任何一处
/// `{:?}`（tracing 的 `?config`、panic 消息、`dbg!`）就会把它写进日志。
/// 手写实现只输出是否已配置，让这条路径从一开始就不存在。
#[derive(Clone, Serialize, Deserialize)]
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

impl std::fmt::Debug for AlertConfig {
    /// 只输出 webhook 是否已配置，不输出它的值。见类型上的注释。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlertConfig")
            .field("enabled", &self.enabled)
            .field("webhook_configured", &webhook_configured(self))
            .field("min_severity", &self.min_severity)
            .field("dedupe_secs", &self.dedupe_secs)
            .field("dedupe_secs_p0", &self.dedupe_secs_p0)
            .field("pool_low_threshold", &self.pool_low_threshold)
            .finish()
    }
}

/// webhook 是否已配置。空串与全空白都算没配。
fn webhook_configured(config: &AlertConfig) -> bool {
    config.webhook_url.as_deref().map(str::trim).is_some_and(|u| !u.is_empty())
}

/// 去抖 + 分发。
pub struct AlertDispatcher {
    /// dedupe_key -> 上次发送时间（Unix 秒）
    last_sent: parking_lot::Mutex<HashMap<String, f64>>,
    config: parking_lot::RwLock<AlertConfig>,
    client: reqwest::Client,
}

impl AlertDispatcher {
    pub fn new(config: AlertConfig, client: reqwest::Client) -> Self {
        Self {
            last_sent: parking_lot::Mutex::new(HashMap::new()),
            config: parking_lot::RwLock::new(config),
            client,
        }
    }

    /// 判断该事件此刻是否应发送，**并在返回 true 时记录发送时间**。
    /// 纯内存判断，可单测（`now` 显式传入）。
    ///
    /// 被配置挡掉（未启用 / 未配 webhook / 级别不够）时不记时间戳，
    /// 这样改完配置立刻就能发出第一条。
    pub fn should_emit(&self, event: &AlertEvent, now: f64) -> bool {
        let severity = event.severity();
        let window = {
            let config = self.config.read();
            if !config.enabled {
                return false;
            }
            if !webhook_configured(&config) {
                return false;
            }
            // AlertSeverity 的序是 P0 < P1 < P2，序更大即重要性更低
            if severity > config.min_severity {
                return false;
            }
            let secs = if severity == AlertSeverity::P0 {
                config.dedupe_secs_p0
            } else {
                config.dedupe_secs
            };
            secs as f64
        };

        let key = event.dedupe_key();
        let mut last_sent = self.last_sent.lock();
        if let Some(previous) = last_sent.get(&key) {
            let elapsed = now - *previous;
            // elapsed 为负说明时钟被回拨，这时按"该发"处理并重新记时
            if (0.0..window).contains(&elapsed) {
                return false;
            }
        }
        last_sent.insert(key, now);
        true
    }

    /// 实际发送。失败只记日志，不向上传播。
    /// 🔴 日志里不得出现 webhook URL。
    ///
    /// 内部先过 `should_emit`，所以调用方每轮直接调它即可，去抖在这里生效。
    /// 注意：窗口在决定发送时就已占用，发送失败也不会立刻重试，避免上游抖动时刷屏。
    pub async fn dispatch(&self, event: &AlertEvent, now: f64) {
        if !self.should_emit(event, now) {
            return;
        }
        // 标题与级别在这里取好：post_card 只管发，不认识 AlertEvent。
        self.post_card(&feishu_payload(event), &event.title(), event.severity().as_str())
            .await;
    }

    /// 手动测试卡。**绕过去抖与级别过滤**。
    ///
    /// 绕过是刻意的：连点两次"测试"就该收到两张。若它也走去抖，第二次静默不发，
    /// 人会以为 webhook 坏了 —— 而这个功能存在的唯一目的就是排除这种怀疑。
    ///
    /// 复用 `post_card` 而不是自己写一遍发送：两套代码的话，测试通了并不能说明
    /// 真告警能通，这个自检就失去意义了。
    pub async fn send_test(&self) -> bool {
        let payload = serde_json::json!({
            "msg_type": "interactive",
            "card": {
                "config": {"wide_screen_mode": true},
                "header": {
                    "title": {"tag": "plain_text", "content": "🔔 Kiro 号池告警自检"},
                    "template": "blue"
                },
                "elements": [{
                    "tag": "div",
                    "text": {
                        "tag": "lark_md",
                        "content": "收到这张卡说明 webhook 与签名都是通的。\n这是手动触发的自检，不代表当前有异常。"
                    }
                }]
            }
        });
        self.post_card(&payload, "告警自检", "test").await
    }

    /// 实际发送。返回是否**确认送达**（HTTP 2xx 且业务 code == 0）。
    async fn post_card(&self, payload: &serde_json::Value, title: &str, severity: &str) -> bool {
        let url = {
            let config = self.config.read();
            match config.webhook_url.as_deref().map(str::trim) {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => {
                    tracing::warn!(alert = %title, "未配置 webhook，跳过发送");
                    return false;
                }
            }
        };

        let result = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(DISPATCH_TIMEOUT_SECS))
            .json(payload)
            .send()
            .await;

        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                // 🔴 只看状态码会把失败记成成功：飞书自定义机器人对 webhook 不存在、
                // 签名不符、触发限流（100 条/分钟）这类业务失败会回 HTTP 200，
                // 真正的成败在响应体的 `code` 字段（0 = 成功）。
                // 记一条假的"告警已发送"比不记更糟——它会让人以为告警链路是通的。
                //
                // 响应体只用来取 `code`，**不进日志**（它可能回显我方发出的内容）。
                let body = tokio::time::timeout(
                    Duration::from_secs(DISPATCH_TIMEOUT_SECS),
                    response.text(),
                )
                .await
                .unwrap_or_else(|_| Ok(String::new()))
                .unwrap_or_default();
                let biz_code = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("code").and_then(serde_json::Value::as_i64));

                if (200..300).contains(&status) && biz_code == Some(0) {
                    tracing::info!(severity, status, alert = %title, "告警已发送");
                    true
                } else {
                    tracing::warn!(
                        severity,
                        status,
                        code = biz_code,
                        alert = %title,
                        "告警发送被拒绝"
                    );
                    false
                }
            }
            Err(e) => {
                // reqwest 的错误默认会把请求 URL 打进 Display，without_url() 把它摘掉
                tracing::warn!(
                    severity,
                    alert = %title,
                    error = %e.without_url(),
                    "告警发送失败"
                );
                false
            }
        }
    }

    /// 返回不含 `webhook_url` 的安全视图。
    pub fn public_config(&self) -> PublicAlertConfig {
        let config = self.config.read();
        PublicAlertConfig {
            enabled: config.enabled,
            webhook_configured: webhook_configured(&config),
            min_severity: config.min_severity,
            dedupe_secs: config.dedupe_secs,
            dedupe_secs_p0: config.dedupe_secs_p0,
            pool_low_threshold: config.pool_low_threshold,
        }
    }

    /// 取一份完整配置（含 webhook）供持久化使用。
    /// 🔴 调用方只能把它写进 `config.json`，不得回传给 API 或写进日志。
    pub fn config_snapshot(&self) -> AlertConfig {
        self.config.read().clone()
    }

    /// 应用配置补丁。`webhook_url: None` 表示不改，`Some("")` 表示清除。
    pub fn update_config(&self, patch: AlertConfigPatch) {
        let mut config = self.config.write();
        if let Some(v) = patch.enabled {
            config.enabled = v;
        }
        if let Some(url) = patch.webhook_url {
            let trimmed = url.trim();
            config.webhook_url = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
        }
        if let Some(v) = patch.min_severity {
            config.min_severity = v;
        }
        if let Some(v) = patch.dedupe_secs {
            config.dedupe_secs = v;
        }
        if let Some(v) = patch.dedupe_secs_p0 {
            config.dedupe_secs_p0 = v;
        }
        if let Some(v) = patch.pool_low_threshold {
            config.pool_low_threshold = v;
        }
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
/// 入参只有事件本身，所以卡片里出不了 webhook / token —— 由单测
/// `feishu_payload_carries_no_secret` 钉住。
pub fn feishu_payload(event: &AlertEvent) -> serde_json::Value {
    let severity = event.severity();

    let mut lines = vec![
        format!("**级别**：{}", severity.as_str()),
        format!("**事件**：{}", event.kind()),
    ];
    for (name, value) in event.card_fields() {
        lines.push(format!("**{}**：{}", name, value));
    }

    serde_json::json!({
        "msg_type": "interactive",
        "card": {
            "config": { "wide_screen_mode": true },
            "header": {
                "template": severity.card_template(),
                "title": {
                    "tag": "plain_text",
                    "content": format!("[{}] {}", severity.as_str(), event.title()),
                }
            },
            "elements": [
                {
                    "tag": "div",
                    "text": { "tag": "lark_md", "content": lines.join("\n") }
                }
            ]
        }
    })
}

/// 状态差分：给定上一轮与本轮的分级，产出需要发送的事件。
///
/// 调用方持有上一轮快照；本函数只做纯计算。
///
/// 规则：
/// - 级别变差（`HealthLevel` 的序更大）才产出，变好或不变都不产出；
/// - 本轮新出现的凭据，非 `Healthy` 就产出一次；
/// - 本轮已消失的凭据不产出（删号不是告警）。
///
/// 返回值按 id 升序，方便调用方与单测比对。
pub fn transitions(
    prev: &HashMap<u64, crate::admin::fleet_health::HealthLevel>,
    current: &HashMap<u64, crate::admin::fleet_health::HealthLevel>,
) -> Vec<u64> {
    use crate::admin::fleet_health::HealthLevel;

    let mut worsened: Vec<u64> = current
        .iter()
        .filter(|(id, level)| match prev.get(id) {
            Some(before) => *level > before,
            None => **level != HealthLevel::Healthy,
        })
        .map(|(id, _)| *id)
        .collect();
    worsened.sort_unstable();
    worsened
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::fleet_health::HealthLevel;

    /// 测试用 webhook。`SECRET_FRAGMENT` 是其中最敏感的那段（飞书的 hook token 在 path 上）。
    const TEST_WEBHOOK: &str = "https://open.feishu.cn/open-apis/bot/v2/hook/zz-secret-hook-token";
    const SECRET_FRAGMENT: &str = "zz-secret-hook-token";

    fn dispatcher(config: AlertConfig) -> AlertDispatcher {
        AlertDispatcher::new(config, reqwest::Client::new())
    }

    fn armed_config() -> AlertConfig {
        AlertConfig {
            enabled: true,
            webhook_url: Some(TEST_WEBHOOK.to_string()),
            ..Default::default()
        }
    }

    fn p0_event() -> AlertEvent {
        AlertEvent::GroupStarved { group: "for_O".to_string(), total_in_group: 3 }
    }
    fn p1_event() -> AlertEvent {
        AlertEvent::CredentialRefreshFailing { id: 3, email: None, count: 2 }
    }
    fn p2_event() -> AlertEvent {
        AlertEvent::CredentialQuotaWarn { id: 7, email: None, usage_pct: 88.0 }
    }

    /// 九个变体全覆盖（`CredentialDisabled` 两种自愈取值都在）。
    fn all_events() -> Vec<AlertEvent> {
        vec![
            AlertEvent::CredentialDisabled {
                id: 7,
                email: Some("ops@example.com".into()),
                reason: "TooManyFailures".into(),
                recoverable: true,
            },
            AlertEvent::CredentialDisabled {
                id: 8,
                email: None,
                reason: "InvalidRefreshToken".into(),
                recoverable: false,
            },
            AlertEvent::CredentialRefreshFailing { id: 9, email: Some("a@example.com".into()), count: 2 },
            AlertEvent::CredentialQuotaCritical {
                id: 10,
                email: None,
                usage_pct: 96.5,
                remaining: 340.0,
            },
            AlertEvent::CredentialQuotaWarn { id: 11, email: None, usage_pct: 86.2 },
            AlertEvent::CredentialDemoted { id: 12, email: None, from: 10, to: 90 },
            AlertEvent::CredentialSubscriptionChanged {
                id: 13,
                email: None,
                from: Some("PRO".into()),
                to: Some("FREE".into()),
            },
            AlertEvent::GroupStarved { group: "for_O".into(), total_in_group: 2 },
            AlertEvent::PoolCapacityLow { healthy: 1, total: 7, threshold: 2 },
            AlertEvent::BalanceRefreshFailing {
                consecutive_rounds: 5,
                last_error: "timeout".into(),
            },
        ]
    }

    // === 🔴 webhook 不外泄 ===

    #[test]
    fn public_config_omits_webhook_url() {
        let d = dispatcher(armed_config());
        let public = d.public_config();
        assert!(public.webhook_configured, "配了 webhook 就该显示已配置");

        let json = serde_json::to_string(&public).unwrap();
        assert!(!json.contains(SECRET_FRAGMENT), "public_config 泄露了 webhook token：{}", json);
        assert!(!json.contains(TEST_WEBHOOK), "public_config 泄露了完整 webhook：{}", json);
        assert!(!json.contains("open.feishu.cn"), "public_config 泄露了 webhook 主机：{}", json);
        assert!(!json.contains("http"), "public_config 里不该出现任何 URL：{}", json);
        assert!(!json.contains("webhookUrl"), "public_config 不该有 webhookUrl 字段：{}", json);
        assert!(json.contains("webhookConfigured"));
    }

    #[test]
    fn public_config_reports_unconfigured_for_blank_webhook() {
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some("   ".to_string()),
            ..Default::default()
        });
        assert!(!d.public_config().webhook_configured, "全空白的 webhook 应视为没配");
    }

    #[test]
    fn feishu_payload_carries_no_secret() {
        for event in all_events() {
            let payload = feishu_payload(&event);
            let text = serde_json::to_string(&payload).unwrap();
            assert!(!text.contains(SECRET_FRAGMENT), "卡片泄露了 token：{}", text);
            assert!(!text.contains("http://"), "卡片里出现了 URL：{}", text);
            assert!(!text.contains("https://"), "卡片里出现了 URL：{}", text);
            assert!(!text.contains("Bearer"), "卡片里出现了 Authorization：{}", text);
        }
    }

    // === 卡片结构 ===

    #[test]
    fn feishu_payload_is_interactive_card_with_severity_template() {
        for event in all_events() {
            let payload = feishu_payload(&event);
            assert_eq!(payload["msg_type"], "interactive", "{:?}", event);

            let expected_template = match event.severity() {
                AlertSeverity::P0 => "red",
                AlertSeverity::P1 => "orange",
                AlertSeverity::P2 => "blue",
            };
            assert_eq!(
                payload["card"]["header"]["template"], expected_template,
                "{:?} 的 header 配色不对",
                event
            );

            let header_title = payload["card"]["header"]["title"]["content"].as_str().unwrap();
            assert!(header_title.contains(&event.title()), "header 标题应包含事件标题：{}", header_title);
            assert!(header_title.contains(event.severity().as_str()));
            assert_eq!(payload["card"]["header"]["title"]["tag"], "plain_text");

            let element = &payload["card"]["elements"][0];
            assert_eq!(element["tag"], "div", "{:?}", event);
            assert_eq!(element["text"]["tag"], "lark_md", "{:?}", event);
            let content = element["text"]["content"].as_str().unwrap();
            assert!(content.contains(event.severity().as_str()), "正文缺级别：{}", content);
            assert!(content.contains(event.kind()), "正文缺事件类型：{}", content);
            for (name, _) in event.card_fields() {
                assert!(content.contains(name), "正文缺字段 {}：{}", name, content);
            }
        }
    }

    #[test]
    fn titles_are_non_empty_and_single_line() {
        for event in all_events() {
            let title = event.title();
            assert!(!title.trim().is_empty(), "{:?} 没有标题", event);
            assert!(!title.contains('\n'), "标题应是一行：{}", title);
        }
    }

    // === 事件分类 ===

    #[test]
    fn kind_matches_serde_event_tag() {
        for event in all_events() {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["event"], event.kind(), "{:?} 的 kind 与 serde tag 不一致", event);
        }
    }

    #[test]
    fn severity_matrix() {
        let cases: Vec<(AlertEvent, AlertSeverity)> = vec![
            (p0_event(), AlertSeverity::P0),
            (
                AlertEvent::BalanceRefreshFailing {
                    consecutive_rounds: 3,
                    last_error: "timeout".into(),
                },
                AlertSeverity::P0,
            ),
            (
                AlertEvent::CredentialDisabled {
                    id: 1,
                    email: None,
                    reason: "InvalidRefreshToken".into(),
                    recoverable: false,
                },
                AlertSeverity::P0,
            ),
            (
                AlertEvent::CredentialDisabled {
                    id: 1,
                    email: None,
                    reason: "TooManyFailures".into(),
                    recoverable: true,
                },
                AlertSeverity::P2,
            ),
            (
                AlertEvent::CredentialQuotaCritical {
                    id: 1,
                    email: None,
                    usage_pct: 96.0,
                    remaining: 10.0,
                },
                AlertSeverity::P1,
            ),
            (p1_event(), AlertSeverity::P1),
            (
                AlertEvent::CredentialSubscriptionChanged {
                    id: 1,
                    email: None,
                    from: Some("PRO".into()),
                    to: None,
                },
                AlertSeverity::P1,
            ),
            (
                AlertEvent::PoolCapacityLow { healthy: 1, total: 7, threshold: 2 },
                AlertSeverity::P1,
            ),
            (p2_event(), AlertSeverity::P2),
            (
                AlertEvent::CredentialDemoted { id: 1, email: None, from: 10, to: 90 },
                AlertSeverity::P2,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.severity(), expected, "{:?} 级别不对", event);
        }
    }

    #[test]
    fn severity_order_puts_p0_first() {
        assert!(AlertSeverity::P0 < AlertSeverity::P1);
        assert!(AlertSeverity::P1 < AlertSeverity::P2);
    }

    #[test]
    fn dedupe_key_is_per_credential_and_per_kind() {
        assert_eq!(
            AlertEvent::CredentialDisabled {
                id: 7,
                email: Some("a@example.com".into()),
                reason: "x".into(),
                recoverable: false,
            }
            .dedupe_key(),
            "credentialDisabled:7"
        );
        // 同一凭据、同一类事件 → 同键（细节字段变化不影响）
        assert_eq!(
            AlertEvent::CredentialDisabled {
                id: 7,
                email: None,
                reason: "别的原因".into(),
                recoverable: true,
            }
            .dedupe_key(),
            "credentialDisabled:7"
        );
        // 换凭据 → 换键
        assert_ne!(
            AlertEvent::CredentialQuotaWarn { id: 7, email: None, usage_pct: 90.0 }.dedupe_key(),
            AlertEvent::CredentialQuotaWarn { id: 8, email: None, usage_pct: 90.0 }.dedupe_key()
        );
        // 同凭据换事件类型 → 换键
        assert_ne!(
            AlertEvent::CredentialQuotaWarn { id: 7, email: None, usage_pct: 90.0 }.dedupe_key(),
            AlertEvent::CredentialQuotaCritical {
                id: 7,
                email: None,
                usage_pct: 96.0,
                remaining: 1.0,
            }
            .dedupe_key()
        );
        assert_eq!(p0_event().dedupe_key(), "groupStarved:for_O");
        assert_eq!(
            AlertEvent::PoolCapacityLow { healthy: 1, total: 7, threshold: 2 }.dedupe_key(),
            "poolCapacityLow:global"
        );
        assert_eq!(
            AlertEvent::BalanceRefreshFailing {
                consecutive_rounds: 2,
                last_error: "x".into()
            }
            .dedupe_key(),
            "balanceRefreshFailing:global"
        );
    }

    // === should_emit 的四道闸 ===

    #[test]
    fn disabled_config_never_emits() {
        let d = dispatcher(AlertConfig { enabled: false, ..armed_config() });
        assert!(!d.should_emit(&p0_event(), 1000.0));
    }

    #[test]
    fn unconfigured_webhook_never_emits() {
        let no_url = dispatcher(AlertConfig { enabled: true, webhook_url: None, ..Default::default() });
        assert!(!no_url.should_emit(&p0_event(), 1000.0));

        let blank = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some("  ".into()),
            ..Default::default()
        });
        assert!(!blank.should_emit(&p0_event(), 1000.0));
    }

    #[test]
    fn min_severity_filters_less_important_events() {
        let d = dispatcher(AlertConfig { min_severity: AlertSeverity::P1, ..armed_config() });
        assert!(d.should_emit(&p0_event(), 1000.0), "P0 应通过 P1 门槛");
        assert!(d.should_emit(&p1_event(), 1000.0), "P1 应通过 P1 门槛");
        assert!(!d.should_emit(&p2_event(), 1000.0), "P2 应被 P1 门槛挡下");

        let only_p0 = dispatcher(AlertConfig { min_severity: AlertSeverity::P0, ..armed_config() });
        assert!(only_p0.should_emit(&p0_event(), 1000.0));
        assert!(!only_p0.should_emit(&p1_event(), 1000.0));
    }

    #[test]
    fn config_gated_events_do_not_consume_dedupe_window() {
        let d = dispatcher(AlertConfig { enabled: false, ..armed_config() });
        assert!(!d.should_emit(&p2_event(), 1000.0));
        d.update_config(AlertConfigPatch { enabled: Some(true), ..Default::default() });
        assert!(d.should_emit(&p2_event(), 1000.0), "开启后第一条应立刻发得出去");
    }

    // === 去抖窗口 ===

    #[test]
    fn dedupe_window_suppresses_then_expires() {
        let d = dispatcher(AlertConfig { dedupe_secs: 1800, ..armed_config() });
        let event = p2_event();

        assert!(d.should_emit(&event, 1000.0), "第一条应发出");
        assert!(!d.should_emit(&event, 1000.0), "同一时刻的重复事件应被抑制");
        assert!(!d.should_emit(&event, 2799.9), "窗口内应被抑制");
        assert!(d.should_emit(&event, 2800.0), "窗口一到就该放行");
        // 放行后窗口从新的时间点重新起算
        assert!(!d.should_emit(&event, 2801.0));
    }

    #[test]
    fn dedupe_window_is_not_shared_across_credentials() {
        let d = dispatcher(armed_config());
        let a = AlertEvent::CredentialQuotaWarn { id: 1, email: None, usage_pct: 88.0 };
        let b = AlertEvent::CredentialQuotaWarn { id: 2, email: None, usage_pct: 88.0 };

        assert!(d.should_emit(&a, 0.0));
        assert!(d.should_emit(&b, 0.0), "另一条凭据不该被 a 的窗口挡住");
        assert!(!d.should_emit(&a, 10.0));
        assert!(!d.should_emit(&b, 10.0));
    }

    #[test]
    fn p0_uses_the_short_dedupe_window() {
        let d = dispatcher(AlertConfig { dedupe_secs: 1800, dedupe_secs_p0: 600, ..armed_config() });
        let p0 = p0_event();
        let p1 = p1_event();

        assert!(d.should_emit(&p0, 0.0));
        assert!(d.should_emit(&p1, 0.0));

        assert!(!d.should_emit(&p0, 599.0), "P0 在 600s 内仍应抑制");
        assert!(d.should_emit(&p0, 700.0), "P0 过了 600s 就该再报");
        assert!(!d.should_emit(&p1, 700.0), "非 P0 用的是 1800s 窗口，此刻还该抑制");
        assert!(d.should_emit(&p1, 1800.0));
    }

    #[test]
    fn clock_rollback_does_not_mute_alerts_forever() {
        let d = dispatcher(armed_config());
        let event = p2_event();
        assert!(d.should_emit(&event, 10_000.0));
        // 时钟被回拨到窗口之前：按"该发"处理，否则会静默到追平为止
        assert!(d.should_emit(&event, 5_000.0));
    }

    // === 配置更新 ===

    #[test]
    fn update_config_applies_patch_fields() {
        let d = dispatcher(AlertConfig::default());
        d.update_config(AlertConfigPatch {
            enabled: Some(true),
            webhook_url: Some(TEST_WEBHOOK.to_string()),
            min_severity: Some(AlertSeverity::P1),
            dedupe_secs: Some(60),
            dedupe_secs_p0: Some(30),
            pool_low_threshold: Some(4),
        });

        let public = d.public_config();
        assert_eq!(
            public,
            PublicAlertConfig {
                enabled: true,
                webhook_configured: true,
                min_severity: AlertSeverity::P1,
                dedupe_secs: 60,
                dedupe_secs_p0: 30,
                pool_low_threshold: 4,
            }
        );
        assert_eq!(d.config_snapshot().webhook_url.as_deref(), Some(TEST_WEBHOOK));
    }

    #[test]
    fn update_config_none_keeps_webhook_and_empty_clears_it() {
        let d = dispatcher(armed_config());

        // None = 不改
        d.update_config(AlertConfigPatch { enabled: Some(true), ..Default::default() });
        assert!(d.public_config().webhook_configured, "webhook_url: None 不应清掉已有配置");

        // Some("") = 清除
        d.update_config(AlertConfigPatch {
            webhook_url: Some(String::new()),
            ..Default::default()
        });
        assert!(!d.public_config().webhook_configured, "空串应清除 webhook");
        assert!(d.config_snapshot().webhook_url.is_none());
        assert!(!d.should_emit(&p0_event(), 1000.0), "清掉 webhook 后不该再发");
    }

    // === 状态差分 ===

    #[test]
    fn transitions_only_reports_worsening() {
        let prev = HashMap::from([
            (1, HealthLevel::Healthy),
            (2, HealthLevel::Warn),
            (3, HealthLevel::Critical),
            (9, HealthLevel::Dead),
        ]);
        let current = HashMap::from([
            (1, HealthLevel::Warn),     // 变差 → 报
            (2, HealthLevel::Healthy),  // 变好 → 不报
            (3, HealthLevel::Critical), // 不变 → 不报
            (4, HealthLevel::Dead),     // 新出现且不健康 → 报
            (5, HealthLevel::Healthy),  // 新出现但健康 → 不报
            // 9 本轮消失 → 不报
        ]);
        assert_eq!(transitions(&prev, &current), vec![1, 4]);
    }

    #[test]
    fn transitions_reports_multi_level_jump() {
        let prev = HashMap::from([(1, HealthLevel::Healthy)]);
        let current = HashMap::from([(1, HealthLevel::Dead)]);
        assert_eq!(transitions(&prev, &current), vec![1]);
    }

    #[test]
    fn transitions_of_empty_prev_reports_every_unhealthy() {
        let prev = HashMap::new();
        let current = HashMap::from([
            (3, HealthLevel::Critical),
            (1, HealthLevel::Healthy),
            (2, HealthLevel::Warn),
        ]);
        // 顺序固定按 id 升序，调用方不用再排
        assert_eq!(transitions(&prev, &current), vec![2, 3]);
    }

    #[test]
    fn transitions_recovery_produces_nothing() {
        let prev = HashMap::from([(1, HealthLevel::Dead), (2, HealthLevel::Critical)]);
        let current = HashMap::from([(1, HealthLevel::Healthy), (2, HealthLevel::Warn)]);
        assert!(transitions(&prev, &current).is_empty());
    }

    // === dispatch（起本地 socket，不出网） ===

    /// 从累积的字节里切出完整的 HTTP 请求体；头没收全或 body 不够长就返回 None。
    fn http_body(raw: &[u8]) -> Option<Vec<u8>> {
        let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        let headers = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
        let len: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let body = &raw[head_end + 4..];
        (body.len() >= len).then(|| body[..len].to_vec())
    }

    #[tokio::test]
    async fn dispatch_posts_feishu_card_and_tolerates_error_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let body = loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break Vec::new();
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(body) = http_body(&raw) {
                    break body;
                }
            };
            // 故意回 500：dispatch 必须能吞下失败
            socket
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
                .unwrap();
            socket.flush().await.unwrap();
            body
        });

        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/open-apis/bot/v2/hook/local", addr)),
            ..Default::default()
        });
        let event = p0_event();

        tokio::time::timeout(Duration::from_secs(5), d.dispatch(&event, 1000.0))
            .await
            .expect("dispatch 不该卡住");

        let body = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server 不该卡住")
            .unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&body).expect("请求体应是 JSON");
        assert_eq!(sent, feishu_payload(&event), "发出去的就是这张卡片");
        assert_eq!(sent["msg_type"], "interactive");
    }

    #[tokio::test]
    async fn dispatch_skips_network_when_alerting_is_off() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let d = dispatcher(AlertConfig {
            enabled: false,
            webhook_url: Some(format!("http://{}/hook", addr)),
            ..Default::default()
        });
        d.dispatch(&p0_event(), 1000.0).await;

        let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(accepted.is_err(), "关闭告警时不该发起任何连接");
    }

    #[tokio::test]
    async fn dispatch_within_dedupe_window_sends_once() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
                .unwrap();
            socket.flush().await.unwrap();
            // 第二次连接不该来
            tokio::time::timeout(Duration::from_millis(300), listener.accept()).await.is_err()
        });

        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook", addr)),
            dedupe_secs_p0: 600,
            ..Default::default()
        });
        let event = p0_event();
        tokio::time::timeout(Duration::from_secs(5), d.dispatch(&event, 1000.0)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), d.dispatch(&event, 1100.0)).await.unwrap();

        let only_once = tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap();
        assert!(only_once, "窗口内的第二条不该真的发出去");
    }

    // === 🔴 日志脱敏：直接打 dispatch 的真实日志输出 ===
    //
    // 下面那条 `reqwest_error_text_without_url_hides_the_hook_token` 测的是 **reqwest 的行为**，
    // 不是 `dispatch` 的行为——把 `dispatch` 里的 `.without_url()` 删掉，它照样绿。
    // （实测：删掉后原有 27 条测试全过，而 WARN 日志里出现了完整 webhook 与 hook token。）
    // 所以这里必须捕获 `dispatch` 自己吐出来的日志。

    /// 把 tracing 输出收进内存，供断言检查。
    #[derive(Clone, Default)]
    struct LogSink(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for LogSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
        type Writer = LogSink;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl LogSink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock()).to_string()
        }
    }

    /// 在捕获 tracing 输出的前提下跑一次 `dispatch`，返回日志文本。
    async fn dispatch_capturing_logs(d: &AlertDispatcher, event: &AlertEvent) -> String {
        let sink = LogSink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            tokio::time::timeout(Duration::from_secs(15), d.dispatch(event, 1000.0))
                .await
                .expect("dispatch 不该卡住");
        }
        sink.text()
    }

    /// 起一个只回一次固定响应的本地 HTTP server。
    async fn oneshot_server(response: &'static [u8]) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response).await;
                let _ = socket.flush().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn dispatch_failure_log_never_contains_the_webhook_url() {
        // 先占端口再释放，保证 connection refused 走到 Err 分支
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let url = format!("http://{}/open-apis/bot/v2/hook/{}", addr, SECRET_FRAGMENT);
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(url.clone()),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(logs.contains("告警发送失败"), "应该记下这次失败：{}", logs);
        assert!(!logs.contains(SECRET_FRAGMENT), "🔴 日志泄露了 webhook token：{}", logs);
        assert!(!logs.contains(&url), "🔴 日志泄露了完整 webhook：{}", logs);
    }

    #[tokio::test]
    async fn dispatch_does_not_claim_success_on_feishu_business_error() {
        // 飞书对 webhook 不存在 / 限流这类业务失败会回 HTTP 200 + code != 0
        let addr = oneshot_server(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"code\":19024,\"msg\":\"webhook is not exist\",\"data\":{}}",
        )
        .await;
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook/{}", addr, SECRET_FRAGMENT)),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(
            !logs.contains("告警已发送"),
            "🔴 飞书回了 code=19024（没发出去）却记成「已发送」：{}",
            logs
        );
        assert!(logs.contains("告警发送被拒绝"), "业务失败应记为被拒绝：{}", logs);
        assert!(!logs.contains(SECRET_FRAGMENT), "🔴 日志泄露了 webhook token：{}", logs);
    }

    #[tokio::test]
    async fn dispatch_treats_code_zero_as_success() {
        let addr = oneshot_server(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"code\":0,\"msg\":\"success\"}",
        )
        .await;
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook", addr)),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(logs.contains("告警已发送"), "code=0 是成功：{}", logs);
        assert!(!logs.contains("告警发送被拒绝"), "code=0 不该记失败：{}", logs);
    }

    #[tokio::test]
    async fn dispatch_reports_non_2xx_as_rejected() {
        let addr =
            oneshot_server(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook/{}", addr, SECRET_FRAGMENT)),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(logs.contains("告警发送被拒绝"), "403 应记为被拒绝：{}", logs);
        assert!(!logs.contains("告警已发送"), "403 不该记成功：{}", logs);
        assert!(!logs.contains(SECRET_FRAGMENT), "🔴 日志泄露了 webhook token：{}", logs);
    }

    // === 卡片体积 ===

    #[test]
    fn card_fields_are_clipped_so_the_card_stays_deliverable() {
        // 上游 HTML 错误页原样进 last_error 是本仓的既有写法
        // （token_manager 里 `format!("... : {}", body_text)` 不截断）
        let huge = "上游返回了一整页 HTML 错误 ".repeat(4000);
        let events = [
            AlertEvent::BalanceRefreshFailing {
                consecutive_rounds: 3,
                last_error: huge.clone(),
            },
            AlertEvent::CredentialDisabled {
                id: 1,
                email: None,
                reason: huge,
                recoverable: false,
            },
        ];
        for event in events {
            let bytes = serde_json::to_vec(&feishu_payload(&event)).unwrap().len();
            assert!(bytes < 30_000, "卡片 {} bytes，会被飞书按超限丢弃：{:?}", bytes, event.kind());
        }
        // 正常长度的文本必须原样保留
        let normal = AlertEvent::CredentialDisabled {
            id: 1,
            email: None,
            reason: "InvalidRefreshToken".into(),
            recoverable: false,
        };
        assert!(
            normal.card_fields().iter().any(|(_, v)| v == "InvalidRefreshToken"),
            "短文本不该被动"
        );
    }

    /// 钉住 `dispatch` 记错误日志时用的那条路径：reqwest 错误默认带 URL，
    /// `without_url()` 之后不能再含 webhook 的 token 段。
    #[tokio::test]
    async fn reqwest_error_text_without_url_hides_the_hook_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // 端口随即空出，连接会被拒

        let url = format!("http://{}/open-apis/bot/v2/hook/{}", addr, SECRET_FRAGMENT);
        let err = reqwest::Client::new()
            .post(&url)
            .timeout(Duration::from_secs(2))
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect_err("端口已关，请求应当失败");

        let text = err.without_url().to_string();
        assert!(!text.contains(SECRET_FRAGMENT), "错误文本泄露了 webhook token：{}", text);
    }

    #[tokio::test]
    async fn attack_ambiguous_body_is_not_recorded_as_success() {
        let addr = oneshot_server(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 13\r\nconnection: close\r\n\r\nnot json body",
        )
        .await;
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook/{}", addr, SECRET_FRAGMENT)),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(
            !logs.contains("告警已发送"),
            "🔴 body 不是合法 JSON（拿不到 code）却被记成「已发送」：{}",
            logs
        );
    }

    #[tokio::test]
    async fn attack_empty_body_200_is_not_recorded_as_success() {
        let addr =
            oneshot_server(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        let d = dispatcher(AlertConfig {
            enabled: true,
            webhook_url: Some(format!("http://{}/hook/{}", addr, SECRET_FRAGMENT)),
            ..Default::default()
        });

        let logs = dispatch_capturing_logs(&d, &p0_event()).await;
        assert!(
            !logs.contains("告警已发送"),
            "🔴 空 body（拿不到 code）却被记成「已发送」：{}",
            logs
        );
    }
}
