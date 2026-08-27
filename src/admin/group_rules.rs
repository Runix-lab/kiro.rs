//! 规则式分组 —— 把「哪个号给哪个客户」从手工数组变成一条规则。
//!
//! # 为什么
//!
//! 现状是每条凭据带一个 `groups: Vec<String>` 手工维护。10 个客户 × 100 个号
//! 最多 1000 条绑定关系，加一个客户就要重新决定 100 个号里哪些给他 —— 这条
//! 成本曲线是加多少批量按钮都压不平的。
//!
//! 规则式：凭据只带**事实标签**（订阅档 / 健康 / 批次 / 端点 / 来源，多数系统
//! 自己就知道），分组是一条**选择器 + 容量**。加客户 = 写一条规则，与号数无关。
//!
//! # 设计约束
//!
//! - **纯函数**：`resolve` 无 IO，输入输出都是值，便于单测与"预览再应用"。
//! - **结果必须稳定**：同样的输入必须给出同样的分配。容量筛选要有确定的
//!   tie-break（否则每轮调度都在换号，凭据的 groups 会被反复重写、反复全量写盘）。
//! - **必须可解释**：运营会问"为什么这个号没进这个组"。`ResolveOutcome`
//!   要能回答，否则规则是黑盒，比手工数组更难用。
//! - **手工优先**：`pinned` 强制包含、`excluded` 强制排除，是规则不够用时的逃生舱。
//! - **只产出建议，不直接写盘**：调用方拿到 diff 后决定是否应用。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 单条判据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Predicate {
    /// 订阅档位精确匹配（大小写不敏感），如 `"KIRO POWER"`。
    Subscription { value: String },
    /// 健康级别不差于给定值（`Warn` 表示 healthy 或 warn 都算）。
    HealthAtMost { level: crate::admin::fleet_health::HealthLevel },
    /// 凭据标签精确匹配。
    Tag { value: String },
    /// 端点名匹配。
    Endpoint { value: String },
    /// 认证方式匹配（`social` / `idc` / `api_key` / `external_idp`）。
    AuthMethod { value: String },
    /// 来源渠道匹配。
    SourceChannel { value: String },
    /// SSO / API region 匹配。
    Region { value: String },
    /// 已用额度百分比低于给定值。余额未知的凭据**不匹配**
    /// （与调度层「拿不到余额不做判断」保持一致）。
    UsageBelowPct { value: f64 },
    /// 显式 id 列表。
    IdIn { ids: Vec<u64> },
    /// 恒真。
    Always,
}

/// 判据组合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Selector {
    All { of: Vec<Selector> },
    Any { of: Vec<Selector> },
    Not { of: Box<Selector> },
    Is { pred: Predicate },
}

/// 一条分组规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupRule {
    /// 目标分组名（须已在 groups.json 注册）。
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    /// 候选筛选器。
    pub selector: Selector,
    /// 容量上限。`None` = 取全部命中的。
    /// 有值时表示「从命中的候选里取 N 个」，挂了会自动补位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    /// 强制包含（不受 selector 与 capacity 约束）。
    #[serde(default)]
    pub pinned: Vec<u64>,
    /// 强制排除（优先级高于 pinned）。
    #[serde(default)]
    pub excluded: Vec<u64>,
}

/// `resolve` 需要知道的每条凭据的事实。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CredentialFacts {
    pub id: u64,
    pub email: Option<String>,
    pub subscription_title: Option<String>,
    pub auth_method: Option<String>,
    pub endpoint: Option<String>,
    pub source_channel: Option<String>,
    pub region: Option<String>,
    pub tags: Vec<String>,
    pub health: crate::admin::fleet_health::HealthLevel,
    pub usage_percentage: Option<f64>,
    pub remaining: Option<f64>,
    /// 当前实际挂着的分组（用于算 diff）。
    pub current_groups: Vec<String>,
}

/// 某条凭据在某条规则下的裁定，用于回答"为什么它没进这个组"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Explanation {
    pub credential_id: u64,
    pub group: String,
    pub included: bool,
    /// 人类可读的原因，如「selector 未命中: subscription != KIRO POWER」
    /// 或「命中但超出容量 20，排在第 23 位」。
    pub reason: String,
}

/// 应用一条规则集后，某条凭据的分组会怎么变。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDiff {
    pub credential_id: u64,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub resulting_groups: Vec<String>,
}

/// 求解结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveOutcome {
    /// 分组名 -> 该组最终包含的凭据 id（升序）。
    pub assignments: BTreeMap<String, Vec<u64>>,
    /// 相对当前状态的差异，只列真正有变化的凭据。
    pub diffs: Vec<GroupDiff>,
    pub explanations: Vec<Explanation>,
    /// 需要人看的问题，如「规则 for_O 要 20 个但只凑到 6 个」。
    pub warnings: Vec<String>,
}

/// 求解。
///
/// 只做计算，不写任何状态。
///
/// TODO(agent): 实现。容量筛选的 tie-break 顺序必须写死并单测：
/// pinned 优先，其余按 `(health, remaining desc, id asc)` 排序取前 N。
/// 这个顺序保证「输入不变则输出不变」，避免每轮调度都在换号。
pub fn resolve(_rules: &[GroupRule], _facts: &[CredentialFacts]) -> ResolveOutcome {
    unimplemented!("group_rules::resolve")
}

/// 单条规则的候选命中判断（不含容量裁剪）。
///
/// TODO(agent): 实现。
pub fn matches(_selector: &Selector, _facts: &CredentialFacts) -> bool {
    unimplemented!("group_rules::matches")
}

/// 规则集自检：引用了未注册的分组名、容量为 0、selector 为空 `All`/`Any` 等。
///
/// TODO(agent): 实现。返回人类可读的问题列表，空表示通过。
pub fn validate(_rules: &[GroupRule], _registered_groups: &[String]) -> Vec<String> {
    unimplemented!("group_rules::validate")
}
</content>
