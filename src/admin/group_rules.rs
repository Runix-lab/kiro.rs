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
//!
//! # 实现要点（对应上面的约束，每条都有单测钉住）
//!
//! - 求解前先把凭据按 id 升序排一遍，所以传入 `facts` 的顺序不影响任何输出字段。
//! - 容量裁剪的 tie-break 写死为：pinned 先占位（按 id 升序），其余候选按
//!   `(健康好的优先, remaining 大的优先, id 升序)` 排序取前 N。`remaining` 为
//!   `None` 的排在有值的后面（拿不到余额不当成余额多）。
//! - 规则只管自己 `name` 的那个分组：算 diff 时，凭据身上那些不由任何**启用**
//!   规则管理的分组名会原样保留。
//! - 字符串判据（订阅档 / 标签 / 端点 / 认证方式 / 来源渠道 / 区域）统一按
//!   「去首尾空白 + 转小写」比较，运营手输的大小写差异不会导致漏命中。
//!   分组名本身仍按 `groups.rs` 的口径区分大小写。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::admin::fleet_health::HealthLevel;

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
#[derive(Debug, Clone, PartialEq)]
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

/// 手写而不是 `derive(Default)`：`HealthLevel` 没有 `Default` 实现，
/// 这里把默认值定为 `Healthy`（"没给健康信息"当成正常服役，与
/// `fleet_health` 里"拿不到数据 ≠ 有问题"的取舍一致）。
impl Default for CredentialFacts {
    fn default() -> Self {
        Self {
            id: 0,
            email: None,
            subscription_title: None,
            auth_method: None,
            endpoint: None,
            source_channel: None,
            region: None,
            tags: Vec::new(),
            health: HealthLevel::Healthy,
            usage_percentage: None,
            remaining: None,
            current_groups: Vec::new(),
        }
    }
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

// ===================== 内部：比较与文案 =====================

/// 健康级别的中文短名，只用于解释文案。
fn health_label(level: HealthLevel) -> &'static str {
    match level {
        HealthLevel::Healthy => "健康",
        HealthLevel::Warn => "预警",
        HealthLevel::Critical => "严重",
        HealthLevel::Dead => "失效",
    }
}

/// 字符串判据的归一化：去首尾空白 + 转小写。
fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

fn opt_eq(actual: Option<&String>, expected: &str) -> bool {
    match actual {
        Some(a) => norm(a) == norm(expected),
        None => false,
    }
}

fn shown(actual: Option<&String>) -> &str {
    match actual {
        Some(s) => s.as_str(),
        None => "未知",
    }
}

/// 容量裁剪用的排序键里 `remaining` 的取值：`None` / 非有限值一律当成最小，
/// 这样它们排在有余额的后面。
fn remaining_key(f: &CredentialFacts) -> f64 {
    match f.remaining {
        Some(v) if v.is_finite() => v,
        _ => f64::NEG_INFINITY,
    }
}

/// 候选排序：健康好的优先 -> remaining 大的优先 -> id 升序。
/// 三级全定序，所以同一批输入的取舍结果不随传入顺序变化。
fn cmp_candidates(a: &CredentialFacts, b: &CredentialFacts) -> Ordering {
    a.health
        .cmp(&b.health)
        .then_with(|| {
            remaining_key(b)
                .partial_cmp(&remaining_key(a))
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.id.cmp(&b.id))
}

/// 判据求值 + 中文原因。
fn pred_reason(pred: &Predicate, f: &CredentialFacts) -> (bool, String) {
    match pred {
        Predicate::Subscription { value } => {
            let ok = opt_eq(f.subscription_title.as_ref(), value);
            let why = if ok {
                format!("订阅档 =「{}」", value)
            } else {
                format!(
                    "订阅档「{}」≠「{}」",
                    shown(f.subscription_title.as_ref()),
                    value
                )
            };
            (ok, why)
        }
        Predicate::HealthAtMost { level } => {
            let ok = f.health <= *level;
            let why = if ok {
                format!(
                    "健康「{}」不差于「{}」",
                    health_label(f.health),
                    health_label(*level)
                )
            } else {
                format!(
                    "健康「{}」差于「{}」",
                    health_label(f.health),
                    health_label(*level)
                )
            };
            (ok, why)
        }
        Predicate::Tag { value } => {
            let ok = f.tags.iter().any(|t| norm(t) == norm(value));
            let why = if ok {
                format!("含标签「{}」", value)
            } else {
                format!("不含标签「{}」", value)
            };
            (ok, why)
        }
        Predicate::Endpoint { value } => {
            let ok = opt_eq(f.endpoint.as_ref(), value);
            let why = if ok {
                format!("端点 =「{}」", value)
            } else {
                format!("端点「{}」≠「{}」", shown(f.endpoint.as_ref()), value)
            };
            (ok, why)
        }
        Predicate::AuthMethod { value } => {
            let ok = opt_eq(f.auth_method.as_ref(), value);
            let why = if ok {
                format!("认证方式 =「{}」", value)
            } else {
                format!("认证方式「{}」≠「{}」", shown(f.auth_method.as_ref()), value)
            };
            (ok, why)
        }
        Predicate::SourceChannel { value } => {
            let ok = opt_eq(f.source_channel.as_ref(), value);
            let why = if ok {
                format!("来源渠道 =「{}」", value)
            } else {
                format!(
                    "来源渠道「{}」≠「{}」",
                    shown(f.source_channel.as_ref()),
                    value
                )
            };
            (ok, why)
        }
        Predicate::Region { value } => {
            let ok = opt_eq(f.region.as_ref(), value);
            let why = if ok {
                format!("区域 =「{}」", value)
            } else {
                format!("区域「{}」≠「{}」", shown(f.region.as_ref()), value)
            };
            (ok, why)
        }
        Predicate::UsageBelowPct { value } => match f.usage_percentage {
            Some(p) if p < *value => (true, format!("已用 {:.1}% < {:.1}%", p, value)),
            Some(p) => (false, format!("已用 {:.1}% ≥ {:.1}%", p, value)),
            None => (
                false,
                "余额未知，usageBelowPct 不做判断（按不命中处理）".to_string(),
            ),
        },
        Predicate::IdIn { ids } => {
            let ok = ids.contains(&f.id);
            let why = if ok {
                format!("id {} 在显式列表内", f.id)
            } else {
                format!("id {} 不在显式列表内", f.id)
            };
            (ok, why)
        }
        Predicate::Always => (true, "恒真判据".to_string()),
    }
}

/// 选择器求值 + 中文原因。`All`/`Any` 短路，所以未命中时给的是「第一条决定性
/// 判据」，命中时给的是全部（`All`）或第一条命中的（`Any`）。
fn selector_reason(sel: &Selector, f: &CredentialFacts) -> (bool, String) {
    match sel {
        Selector::Is { pred } => pred_reason(pred, f),
        Selector::Not { of } => {
            let (ok, why) = selector_reason(of, f);
            (!ok, format!("非（{}）", why))
        }
        Selector::All { of } => {
            if of.is_empty() {
                return (true, "空的 all 判据集（按命中处理）".to_string());
            }
            let mut reasons = Vec::with_capacity(of.len());
            for s in of {
                let (ok, why) = selector_reason(s, f);
                if !ok {
                    return (false, why);
                }
                reasons.push(why);
            }
            (true, reasons.join("、"))
        }
        Selector::Any { of } => {
            if of.is_empty() {
                return (false, "空的 any 判据集（按不命中处理）".to_string());
            }
            let mut reasons = Vec::with_capacity(of.len());
            for s in of {
                let (ok, why) = selector_reason(s, f);
                if ok {
                    return (true, why);
                }
                reasons.push(why);
            }
            (false, format!("任一判据均未命中（{}）", reasons.join("、")))
        }
    }
}

/// 收集选择器树里出现的空 `all` / `any` 判据集，返回它们的种类名。
fn empty_set_kinds(sel: &Selector, out: &mut Vec<&'static str>) {
    match sel {
        Selector::All { of } => {
            if of.is_empty() {
                out.push("all");
            }
            for s in of {
                empty_set_kinds(s, out);
            }
        }
        Selector::Any { of } => {
            if of.is_empty() {
                out.push("any");
            }
            for s in of {
                empty_set_kinds(s, out);
            }
        }
        Selector::Not { of } => empty_set_kinds(of, out),
        Selector::Is { .. } => {}
    }
}

/// 摊平选择器树里的全部判据，供 `validate` 逐条体检。
fn walk_predicates<'a>(sel: &'a Selector, out: &mut Vec<&'a Predicate>) {
    match sel {
        Selector::All { of } | Selector::Any { of } => {
            for s in of {
                walk_predicates(s, out);
            }
        }
        Selector::Not { of } => walk_predicates(of, out),
        Selector::Is { pred } => out.push(pred),
    }
}

fn join_ids(ids: &[u64]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("、")
}

// ===================== 对外 API =====================

/// 求解。
///
/// 只做计算，不写任何状态。
///
/// 输出的每个字段都与传入 `facts` 的顺序无关：内部先按 id 升序重排，容量裁剪走
/// `cmp_candidates` 的全定序 tie-break，`assignments` 走 `BTreeMap` + 升序 id。
///
/// `explanations` 覆盖「每条凭据 × 每条启用规则」，included 与否都给原因。
/// `diffs` 只列 added/removed 非空的凭据；`resulting_groups` 会保留该凭据身上
/// 那些不由任何启用规则管理的分组名。
///
/// 传入的 `facts` 里出现重复 id 时不做去重（去重会让结果依赖传入顺序），
/// 只在 `warnings` 里记一条。
pub fn resolve(rules: &[GroupRule], facts: &[CredentialFacts]) -> ResolveOutcome {
    let mut out = ResolveOutcome::default();

    // 1) 输入归一：按 id 升序 + 按 id 去重，后续所有遍历都用这一份。
    //
    // 去重不是洁癖。不去重的话同一个 id 的每条记录都会各占一个容量名额，
    // 于是 capacity=N 实际覆盖到的**不同 id 数**可能少于 N；更糟的是它们会各出
    // 一条 explanation 和 GroupDiff，彼此可以互相矛盾（一条说"超容量未入选"，
    // 另一条却真的被选中）。运营看到两条相反的解释，这个功能就废了。
    //
    // 保留先出现的那条（排序是稳定的，所以等价于保留调用方给的第一条）。
    let mut ordered: Vec<&CredentialFacts> = facts.iter().collect();
    ordered.sort_by_key(|f| f.id);

    let mut dup: BTreeSet<u64> = BTreeSet::new();
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    ordered.retain(|f| {
        if seen.insert(f.id) {
            true
        } else {
            dup.insert(f.id);
            false
        }
    });
    if !dup.is_empty() {
        out.warnings.push(format!(
            "凭据 id 重复：{}，已只保留首次出现的那条",
            join_ids(&dup.iter().copied().collect::<Vec<_>>())
        ));
    }

    let known_ids: BTreeSet<u64> = ordered.iter().map(|f| f.id).collect();
    let enabled: Vec<&GroupRule> = rules.iter().filter(|r| r.enabled).collect();

    // 2) 规则集层面的问题。
    let mut name_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &enabled {
        *name_counts.entry(r.name.as_str()).or_insert(0) += 1;
    }
    for (name, n) in &name_counts {
        if *n > 1 {
            out.warnings.push(format!(
                "分组「{}」有 {} 条启用规则，最终成员取并集",
                name, n
            ));
        }
    }
    for r in &enabled {
        let mut kinds = Vec::new();
        empty_set_kinds(&r.selector, &mut kinds);
        for kind in kinds {
            out.warnings.push(format!(
                "规则「{}」的 selector 含空的 {} 判据集，多半是配错了",
                r.name, kind
            ));
        }
    }

    // 3) 逐条规则求解。
    let mut assignments: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for r in &enabled {
        assignments.entry(r.name.clone()).or_default();
    }

    for rule in &enabled {
        let excluded: BTreeSet<u64> = rule.excluded.iter().copied().collect();
        // excluded 优先级高于 pinned：先把被排除的 id 从 pinned 里剔掉。
        let pinned: BTreeSet<u64> = rule
            .pinned
            .iter()
            .copied()
            .filter(|id| !excluded.contains(id))
            .collect();

        let missing: Vec<u64> = pinned
            .iter()
            .copied()
            .filter(|id| !known_ids.contains(id))
            .collect();
        if !missing.is_empty() {
            out.warnings.push(format!(
                "规则「{}」的 pinned 里有 {} 个 id 在凭据里找不到：{}",
                rule.name,
                missing.len(),
                join_ids(&missing)
            ));
        }

        // ordered 已按 id 升序，过滤保序 -> pinned 占位顺序就是 id 升序。
        let pinned_present: Vec<&CredentialFacts> = ordered
            .iter()
            .copied()
            .filter(|f| pinned.contains(&f.id))
            .collect();

        let mut candidates: Vec<&CredentialFacts> = ordered
            .iter()
            .copied()
            .filter(|f| {
                !excluded.contains(&f.id) && !pinned.contains(&f.id) && matches(&rule.selector, f)
            })
            .collect();
        candidates.sort_by(|a, b| cmp_candidates(a, b));

        let pinned_count = pinned_present.len();
        let take = match rule.capacity {
            Some(c) => c.saturating_sub(pinned_count).min(candidates.len()),
            None => candidates.len(),
        };

        // 全局排名：pinned 先占位，其余按裁剪顺序接在后面（1-based）。
        let mut rank_of: BTreeMap<u64, usize> = BTreeMap::new();
        for (i, c) in candidates.iter().enumerate() {
            rank_of.insert(c.id, pinned_count + i + 1);
        }

        let selected: BTreeSet<u64> = pinned_present
            .iter()
            .map(|f| f.id)
            .chain(candidates.iter().take(take).map(|f| f.id))
            .collect();

        if let Some(c) = rule.capacity {
            if pinned_count > c {
                out.warnings.push(format!(
                    "规则「{}」的 pinned 有 {} 条，超过容量 {}，selector 命中的候选一条都进不来",
                    rule.name, pinned_count, c
                ));
            }
            if selected.len() < c {
                out.warnings.push(format!(
                    "规则「{}」容量 {} 没凑满，只到 {} 条",
                    rule.name,
                    c,
                    selected.len()
                ));
            }
        }

        // 4) 解释：每条凭据 × 这条规则，都要有一条。
        for f in &ordered {
            let (included, reason) = if excluded.contains(&f.id) {
                (
                    false,
                    "已被 excluded 强制排除（优先级高于 pinned）".to_string(),
                )
            } else if pinned.contains(&f.id) {
                (
                    true,
                    "已被 pinned 强制包含，不受 selector 与容量约束".to_string(),
                )
            } else {
                let (ok, why) = selector_reason(&rule.selector, f);
                if !ok {
                    (false, format!("selector 未命中：{}", why))
                } else {
                    let rank = rank_of.get(&f.id).copied().unwrap_or(0);
                    match rule.capacity {
                        None => (true, format!("selector 命中：{}；无容量上限", why)),
                        Some(c) if rank <= c => (
                            true,
                            format!("selector 命中：{}；容量 {} 内排第 {} 位", why, c, rank),
                        ),
                        Some(c) => (
                            false,
                            format!("命中但排在第 {} 位，超出容量 {}", rank, c),
                        ),
                    }
                }
            };
            out.explanations.push(Explanation {
                credential_id: f.id,
                group: rule.name.clone(),
                included,
                reason,
            });
        }

        assignments
            .entry(rule.name.clone())
            .or_default()
            .extend(selected);
    }

    // 5) diff：只动由启用规则管理的分组名，其余原样保留。
    let managed: BTreeSet<&str> = enabled.iter().map(|r| r.name.as_str()).collect();
    for f in &ordered {
        let current: BTreeSet<String> = f.current_groups.iter().cloned().collect();
        let mut next: BTreeSet<String> = current
            .iter()
            .filter(|g| !managed.contains(g.as_str()))
            .cloned()
            .collect();
        for (name, members) in &assignments {
            if members.contains(&f.id) {
                next.insert(name.clone());
            }
        }

        let added: Vec<String> = next.difference(&current).cloned().collect();
        let removed: Vec<String> = current.difference(&next).cloned().collect();
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        out.diffs.push(GroupDiff {
            credential_id: f.id,
            added,
            removed,
            resulting_groups: next.into_iter().collect(),
        });
    }

    out.assignments = assignments
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    out
}

/// 单条规则的候选命中判断（不含容量裁剪）。
///
/// 与 `resolve` 生成解释用的是同一套求值逻辑，不会出现「解释说命中、实际没进」。
/// `All { of: [] }` 视为命中，`Any { of: [] }` 视为不命中。
pub fn matches(selector: &Selector, facts: &CredentialFacts) -> bool {
    selector_reason(selector, facts).0
}

/// 规则集自检：引用了未注册的分组名、容量为 0、selector 为空 `All`/`Any` 等。
///
/// 返回中文问题列表，空表示通过。禁用的规则也检查（它们迟早会被打开）。
pub fn validate(rules: &[GroupRule], registered_groups: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let registered: BTreeSet<&str> = registered_groups.iter().map(|s| s.as_str()).collect();
    let mut enabled_names: BTreeMap<&str, usize> = BTreeMap::new();

    for (i, r) in rules.iter().enumerate() {
        let label = format!("规则 #{}「{}」", i + 1, r.name);

        if r.name.trim().is_empty() {
            problems.push(format!("{}：分组名为空", label));
        } else {
            if r.name.trim() != r.name {
                problems.push(format!("{}：分组名首尾有空白", label));
            }
            if !registered.contains(r.name.as_str()) {
                problems.push(format!("{}：引用了未注册的分组名", label));
            }
        }

        match r.capacity {
            Some(c) => {
                let pinned_n = r.pinned.iter().copied().collect::<BTreeSet<_>>().len();
                if c == 0 && pinned_n == 0 {
                    // 只有 pinned 也是空的，容量 0 才真的等于把规则关掉。
                    // pinned 不受 capacity 约束（见 resolve 里的 pinned_present），
                    // 非空时它们仍会被强制塞进分组，不能说规则被关掉了。
                    problems.push(format!("{}：容量为 0，等于把这条规则关掉", label));
                } else if pinned_n > c {
                    problems.push(format!(
                        "{}：pinned 有 {} 条，超过容量 {}，selector 命中的候选一条都进不来",
                        label, pinned_n, c
                    ));
                }
            }
            None => {}
        }

        let mut kinds = Vec::new();
        empty_set_kinds(&r.selector, &mut kinds);
        for kind in kinds {
            let effect = if kind == "all" {
                "按命中处理"
            } else {
                "按不命中处理"
            };
            problems.push(format!(
                "{}：selector 含空的 {} 判据集（{}），多半是配错了",
                label, kind, effect
            ));
        }

        let mut preds = Vec::new();
        walk_predicates(&r.selector, &mut preds);
        for p in preds {
            match p {
                Predicate::Subscription { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：subscription 判据的值为空", label));
                }
                Predicate::Tag { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：tag 判据的值为空", label));
                }
                Predicate::Endpoint { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：endpoint 判据的值为空", label));
                }
                Predicate::AuthMethod { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：authMethod 判据的值为空", label));
                }
                Predicate::SourceChannel { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：sourceChannel 判据的值为空", label));
                }
                Predicate::Region { value } if value.trim().is_empty() => {
                    problems.push(format!("{}：region 判据的值为空", label));
                }
                Predicate::UsageBelowPct { value } => {
                    if !value.is_finite() {
                        problems.push(format!("{}：usageBelowPct 的值不是有效数字", label));
                    } else if *value <= 0.0 {
                        problems.push(format!(
                            "{}：usageBelowPct = {}，任何凭据都不会命中",
                            label, value
                        ));
                    } else if *value > 100.0 {
                        problems.push(format!(
                            "{}：usageBelowPct = {} 超过 100，余额已知的凭据全会命中",
                            label, value
                        ));
                    }
                }
                Predicate::IdIn { ids } if ids.is_empty() => {
                    problems.push(format!("{}：idIn 的 id 列表为空，任何凭据都不会命中", label));
                }
                _ => {}
            }
        }

        let ex: BTreeSet<u64> = r.excluded.iter().copied().collect();
        let pin: BTreeSet<u64> = r.pinned.iter().copied().collect();
        let both: Vec<u64> = pin.intersection(&ex).copied().collect();
        if !both.is_empty() {
            problems.push(format!(
                "{}：id {} 同时在 pinned 与 excluded 里，按 excluded 处理",
                label,
                join_ids(&both)
            ));
        }

        if r.enabled {
            *enabled_names.entry(r.name.as_str()).or_insert(0) += 1;
        }
    }

    for (name, n) in enabled_names {
        if n > 1 {
            problems.push(format!(
                "分组「{}」有 {} 条启用规则，成员会取并集",
                name, n
            ));
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 构造辅助 ----------

    fn fact(id: u64) -> CredentialFacts {
        CredentialFacts {
            id,
            ..Default::default()
        }
    }

    fn is(pred: Predicate) -> Selector {
        Selector::Is { pred }
    }

    fn rule(name: &str, selector: Selector) -> GroupRule {
        GroupRule {
            name: name.to_string(),
            enabled: true,
            selector,
            capacity: None,
            pinned: Vec::new(),
            excluded: Vec::new(),
        }
    }

    fn groups(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// 把 resolve 的 diff 应用回 facts，供幂等性测试用。
    fn apply(facts: &mut [CredentialFacts], out: &ResolveOutcome) {
        for d in &out.diffs {
            for f in facts.iter_mut() {
                if f.id == d.credential_id {
                    f.current_groups = d.resulting_groups.clone();
                }
            }
        }
    }

    fn explanation<'a>(out: &'a ResolveOutcome, id: u64, group: &str) -> &'a Explanation {
        out.explanations
            .iter()
            .find(|e| e.credential_id == id && e.group == group)
            .expect("缺少该凭据在该分组下的裁定")
    }

    fn assigned(out: &ResolveOutcome, group: &str) -> Vec<u64> {
        out.assignments.get(group).cloned().unwrap_or_default()
    }

    // ---------- Predicate 逐条 ----------

    #[test]
    fn pred_always_matches_everything() {
        assert!(matches(&is(Predicate::Always), &fact(1)));
    }

    #[test]
    fn pred_subscription_is_case_and_space_insensitive() {
        let mut f = fact(1);
        f.subscription_title = Some("  Kiro Power ".to_string());
        assert!(matches(
            &is(Predicate::Subscription {
                value: "KIRO POWER".to_string()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::Subscription {
                value: "KIRO FREE".to_string()
            }),
            &f
        ));

        // 缺失订阅档不命中
        let g = fact(2);
        assert!(!matches(
            &is(Predicate::Subscription {
                value: "KIRO POWER".to_string()
            }),
            &g
        ));
    }

    #[test]
    fn pred_health_at_most_uses_healthy_lt_warn_lt_critical_lt_dead() {
        // 级别序本身
        assert!(HealthLevel::Healthy < HealthLevel::Warn);
        assert!(HealthLevel::Warn < HealthLevel::Critical);
        assert!(HealthLevel::Critical < HealthLevel::Dead);

        let sel = is(Predicate::HealthAtMost {
            level: HealthLevel::Warn,
        });
        for (level, expect) in [
            (HealthLevel::Healthy, true),
            (HealthLevel::Warn, true),
            (HealthLevel::Critical, false),
            (HealthLevel::Dead, false),
        ] {
            let mut f = fact(1);
            f.health = level;
            assert_eq!(matches(&sel, &f), expect, "level={:?}", level);
        }
    }

    #[test]
    fn pred_tag_matches_any_tag() {
        let mut f = fact(1);
        f.tags = vec!["batch-a".to_string(), "VIP".to_string()];
        assert!(matches(
            &is(Predicate::Tag {
                value: "vip".to_string()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::Tag {
                value: "batch-b".to_string()
            }),
            &f
        ));
    }

    #[test]
    fn pred_endpoint_auth_source_region() {
        let mut f = fact(1);
        f.endpoint = Some("us-east-1".to_string());
        f.auth_method = Some("social".to_string());
        f.source_channel = Some("vendor-x".to_string());
        f.region = Some("us-east-1".to_string());

        assert!(matches(
            &is(Predicate::Endpoint {
                value: "us-east-1".into()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::Endpoint {
                value: "eu-west-1".into()
            }),
            &f
        ));
        assert!(matches(
            &is(Predicate::AuthMethod {
                value: "social".into()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::AuthMethod {
                value: "idc".into()
            }),
            &f
        ));
        assert!(matches(
            &is(Predicate::SourceChannel {
                value: "vendor-x".into()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::SourceChannel {
                value: "vendor-y".into()
            }),
            &f
        ));
        assert!(matches(
            &is(Predicate::Region {
                value: "US-EAST-1".into()
            }),
            &f
        ));
        assert!(!matches(
            &is(Predicate::Region {
                value: "ap-northeast-1".into()
            }),
            &f
        ));

        // 字段缺失一律不命中
        let empty = fact(2);
        assert!(!matches(
            &is(Predicate::Endpoint {
                value: "us-east-1".into()
            }),
            &empty
        ));
        assert!(!matches(
            &is(Predicate::AuthMethod {
                value: "social".into()
            }),
            &empty
        ));
        assert!(!matches(
            &is(Predicate::SourceChannel {
                value: "vendor-x".into()
            }),
            &empty
        ));
        assert!(!matches(
            &is(Predicate::Region {
                value: "us-east-1".into()
            }),
            &empty
        ));
    }

    #[test]
    fn pred_usage_below_pct_skips_unknown_balance() {
        let sel = is(Predicate::UsageBelowPct { value: 80.0 });

        let mut low = fact(1);
        low.usage_percentage = Some(79.9);
        assert!(matches(&sel, &low));

        let mut edge = fact(2);
        edge.usage_percentage = Some(80.0);
        assert!(!matches(&sel, &edge), "严格小于，等于不算命中");

        let mut high = fact(3);
        high.usage_percentage = Some(99.0);
        assert!(!matches(&sel, &high));

        // 余额未知：不匹配，且原因写清是"没做判断"
        let unknown = fact(4);
        assert!(!matches(&sel, &unknown));
        let r = rule("g", sel.clone());
        let out = resolve(&[r], &[unknown]);
        assert!(
            explanation(&out, 4, "g").reason.contains("余额未知"),
            "实际原因: {}",
            explanation(&out, 4, "g").reason
        );
    }

    #[test]
    fn pred_id_in_matches_explicit_list() {
        let sel = is(Predicate::IdIn { ids: vec![2, 5] });
        assert!(matches(&sel, &fact(2)));
        assert!(matches(&sel, &fact(5)));
        assert!(!matches(&sel, &fact(3)));
        assert!(!matches(&is(Predicate::IdIn { ids: vec![] }), &fact(1)));
    }

    // ---------- All / Any / Not 组合 ----------

    #[test]
    fn selector_all_any_not_combination() {
        let mut f = fact(1);
        f.subscription_title = Some("KIRO POWER".to_string());
        f.health = HealthLevel::Warn;
        f.tags = vec!["batch-a".to_string()];

        // all: 全中才中
        let all_ok = Selector::All {
            of: vec![
                is(Predicate::Subscription {
                    value: "KIRO POWER".into(),
                }),
                is(Predicate::HealthAtMost {
                    level: HealthLevel::Warn,
                }),
            ],
        };
        assert!(matches(&all_ok, &f));

        let all_bad = Selector::All {
            of: vec![
                is(Predicate::Subscription {
                    value: "KIRO POWER".into(),
                }),
                is(Predicate::HealthAtMost {
                    level: HealthLevel::Healthy,
                }),
            ],
        };
        assert!(!matches(&all_bad, &f));

        // any: 中一个即可
        let any_ok = Selector::Any {
            of: vec![
                is(Predicate::Tag {
                    value: "batch-z".into(),
                }),
                is(Predicate::Tag {
                    value: "batch-a".into(),
                }),
            ],
        };
        assert!(matches(&any_ok, &f));

        let any_bad = Selector::Any {
            of: vec![
                is(Predicate::Tag {
                    value: "batch-y".into(),
                }),
                is(Predicate::Tag {
                    value: "batch-z".into(),
                }),
            ],
        };
        assert!(!matches(&any_bad, &f));

        // not 取反
        assert!(matches(
            &Selector::Not {
                of: Box::new(any_bad.clone())
            },
            &f
        ));
        assert!(!matches(
            &Selector::Not {
                of: Box::new(any_ok.clone())
            },
            &f
        ));

        // 嵌套：all(any(...), not(...))
        let nested = Selector::All {
            of: vec![
                any_ok,
                Selector::Not {
                    of: Box::new(is(Predicate::HealthAtMost {
                        level: HealthLevel::Healthy,
                    })),
                },
            ],
        };
        assert!(matches(&nested, &f), "warn 级别不满足 healthy，取反后命中");
    }

    #[test]
    fn empty_all_is_true_empty_any_is_false() {
        let f = fact(1);
        assert!(matches(&Selector::All { of: vec![] }, &f));
        assert!(!matches(&Selector::Any { of: vec![] }, &f));
    }

    #[test]
    fn empty_selector_sets_are_reported() {
        // validate 报 warning
        let r_all = rule("g", Selector::All { of: vec![] });
        let problems = validate(&[r_all.clone()], &groups(&["g"]));
        assert!(
            problems.iter().any(|p| p.contains("空的 all 判据集")),
            "{:?}",
            problems
        );

        let r_any = rule("g", Selector::Any { of: vec![] });
        let problems = validate(&[r_any.clone()], &groups(&["g"]));
        assert!(
            problems.iter().any(|p| p.contains("空的 any 判据集")),
            "{:?}",
            problems
        );

        // resolve 也把它记进 warnings
        let out = resolve(&[r_all], &[fact(1)]);
        assert!(
            out.warnings.iter().any(|w| w.contains("空的 all 判据集")),
            "{:?}",
            out.warnings
        );
    }

    // ---------- pinned / excluded ----------

    #[test]
    fn excluded_beats_pinned() {
        let mut r = rule("g", is(Predicate::Always));
        r.pinned = vec![1, 2];
        r.excluded = vec![2];

        let out = resolve(&[r], &[fact(1), fact(2), fact(3)]);
        assert_eq!(assigned(&out, "g"), vec![1, 3]);

        let e = explanation(&out, 2, "g");
        assert!(!e.included);
        assert!(e.reason.contains("excluded"), "实际原因: {}", e.reason);
        assert!(e.reason.contains("pinned"), "要说清它压过了 pinned");
    }

    #[test]
    fn pinned_bypasses_selector_and_capacity() {
        let mut r = rule(
            "g",
            is(Predicate::Tag {
                value: "batch-a".into(),
            }),
        );
        r.capacity = Some(1);
        r.pinned = vec![9]; // 9 不带 batch-a 标签

        let mut a = fact(1);
        a.tags = vec!["batch-a".to_string()];
        let mut b = fact(2);
        b.tags = vec!["batch-a".to_string()];
        let nine = fact(9);

        let out = resolve(&[r], &[a, b, nine]);
        // pinned 占掉容量 1，其余候选一条都进不来
        assert_eq!(assigned(&out, "g"), vec![9]);
        assert!(explanation(&out, 9, "g").included);
        assert!(!explanation(&out, 1, "g").included);
        assert!(
            explanation(&out, 1, "g").reason.contains("超出容量 1"),
            "实际原因: {}",
            explanation(&out, 1, "g").reason
        );
    }

    #[test]
    fn pinned_over_capacity_warns() {
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(1);
        r.pinned = vec![1, 2];

        let out = resolve(&[r], &[fact(1), fact(2), fact(3)]);
        assert_eq!(assigned(&out, "g"), vec![1, 2]);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("pinned 有 2 条，超过容量 1")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn pinned_id_not_found_warns() {
        let mut r = rule("g", is(Predicate::Always));
        r.pinned = vec![42];
        let out = resolve(&[r], &[fact(1)]);
        assert!(
            out.warnings.iter().any(|w| w.contains("42")),
            "{:?}",
            out.warnings
        );
    }

    // ---------- 容量与 tie-break ----------

    fn cand(id: u64, health: HealthLevel, remaining: Option<f64>) -> CredentialFacts {
        CredentialFacts {
            id,
            health,
            remaining,
            ..Default::default()
        }
    }

    #[test]
    fn capacity_tie_break_is_health_then_remaining_then_id() {
        let facts = vec![
            cand(1, HealthLevel::Warn, Some(9000.0)),
            cand(2, HealthLevel::Healthy, Some(100.0)),
            cand(3, HealthLevel::Healthy, Some(500.0)),
            cand(4, HealthLevel::Healthy, Some(500.0)),
            cand(5, HealthLevel::Healthy, None),
            cand(6, HealthLevel::Critical, Some(9999.0)),
        ];
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(4);

        let out = resolve(&[r], &facts);
        // healthy 优先；healthy 内 remaining 降序；500 打平按 id 升序；
        // remaining 未知排在有值的后面；warn/critical 最后。
        assert_eq!(assigned(&out, "g"), vec![2, 3, 4, 5]);

        // 被裁掉的写清排名与容量
        let e1 = explanation(&out, 1, "g");
        assert!(!e1.included);
        assert_eq!(e1.reason, "命中但排在第 5 位，超出容量 4");
        let e6 = explanation(&out, 6, "g");
        assert_eq!(e6.reason, "命中但排在第 6 位，超出容量 4");
    }

    // 上面那个用例 capacity=4 而恰好有 4 条 healthy，healthy 内部怎么排都不影响
    // 取舍，所以它只钉住了「健康优先」那一级。下面三条各自把一级判据单独隔离出来
    // ——反向变异（把排序改成升序 / 把 None 排到最前 / 把 id 改成降序）必须能被打挂，
    // 否则 tie-break 就是没被覆盖的。
    #[test]
    fn capacity_tie_break_prefers_larger_remaining() {
        let facts = vec![
            cand(1, HealthLevel::Healthy, Some(100.0)),
            cand(2, HealthLevel::Healthy, Some(99.0)),
            cand(3, HealthLevel::Healthy, Some(98.0)),
        ];
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(2);
        let out = resolve(&[r], &facts);
        assert_eq!(
            assigned(&out, "g"),
            vec![1, 2],
            "健康打平时 remaining 大的优先，余额最少的该被裁掉"
        );
    }

    #[test]
    fn capacity_tie_break_puts_unknown_remaining_last() {
        let facts = vec![
            cand(1, HealthLevel::Healthy, None),
            cand(2, HealthLevel::Healthy, Some(0.01)),
        ];
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(1);
        let out = resolve(&[r], &facts);
        assert_eq!(
            assigned(&out, "g"),
            vec![2],
            "拿不到余额不当成余额多：即便对方只剩 0.01 也排在未知的前面"
        );
    }

    #[test]
    fn capacity_tie_break_falls_back_to_smaller_id() {
        let facts = vec![
            cand(7, HealthLevel::Healthy, Some(500.0)),
            cand(3, HealthLevel::Healthy, Some(500.0)),
        ];
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(1);
        let out = resolve(&[r], &facts);
        assert_eq!(assigned(&out, "g"), vec![3], "健康与余额都打平时取 id 小的");
    }

    #[test]
    fn capacity_shortfall_warns() {
        let mut r = rule("for_O", is(Predicate::Always));
        r.capacity = Some(20);
        let facts: Vec<CredentialFacts> = (1..=6).map(fact).collect();

        let out = resolve(&[r], &facts);
        assert_eq!(assigned(&out, "for_O").len(), 6);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("for_O") && w.contains("容量 20 没凑满") && w.contains("6")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn no_capacity_takes_all_matches() {
        let r = rule("g", is(Predicate::Always));
        let facts: Vec<CredentialFacts> = (1..=5).map(fact).collect();
        let out = resolve(&[r], &facts);
        assert_eq!(assigned(&out, "g"), vec![1, 2, 3, 4, 5]);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(explanation(&out, 3, "g").reason.contains("无容量上限"));
    }

    // ---------- 解释的完整性 ----------

    #[test]
    fn every_credential_times_every_enabled_rule_has_a_verdict() {
        let mut disabled = rule("off", is(Predicate::Always));
        disabled.enabled = false;

        let rules = vec![
            rule("a", is(Predicate::Always)),
            rule(
                "b",
                is(Predicate::Tag {
                    value: "x".to_string(),
                }),
            ),
            disabled,
        ];
        let facts: Vec<CredentialFacts> = (1..=4).map(fact).collect();

        let out = resolve(&rules, &facts);
        assert_eq!(out.explanations.len(), 4 * 2, "禁用规则不产生裁定");
        for id in 1..=4u64 {
            for g in ["a", "b"] {
                let e = explanation(&out, id, g);
                assert!(!e.reason.trim().is_empty(), "included 与否都要给原因");
            }
        }
        assert!(
            !out.explanations.iter().any(|e| e.group == "off"),
            "禁用规则不该出现在解释里"
        );
    }

    // ---------- diff ----------

    #[test]
    fn diff_keeps_groups_not_managed_by_any_rule() {
        let mut f = fact(1);
        // manual_only 不由任何规则管理，rule_group 由规则管理
        f.current_groups = vec!["manual_only".to_string(), "stale_group".to_string()];

        let r = rule("rule_group", is(Predicate::Always));
        let out = resolve(&[r], &[f]);

        assert_eq!(out.diffs.len(), 1);
        let d = &out.diffs[0];
        assert_eq!(d.credential_id, 1);
        assert_eq!(d.added, vec!["rule_group".to_string()]);
        assert!(d.removed.is_empty(), "不由规则管理的分组不许被顺手抹掉");
        assert_eq!(
            d.resulting_groups,
            vec![
                "manual_only".to_string(),
                "rule_group".to_string(),
                "stale_group".to_string()
            ]
        );
    }

    #[test]
    fn diff_removes_only_managed_group_when_no_longer_matching() {
        let mut f = fact(1);
        f.current_groups = vec!["managed".to_string(), "manual".to_string()];

        // 规则管理 managed，但这条凭据不再命中
        let r = rule("managed", is(Predicate::IdIn { ids: vec![2] }));
        let out = resolve(&[r], &[f]);

        assert_eq!(out.diffs.len(), 1);
        let d = &out.diffs[0];
        assert_eq!(d.removed, vec!["managed".to_string()]);
        assert!(d.added.is_empty());
        assert_eq!(d.resulting_groups, vec!["manual".to_string()]);
    }

    #[test]
    fn diff_skips_credentials_without_change() {
        let mut f1 = fact(1);
        f1.current_groups = vec!["g".to_string()];
        let f2 = fact(2);

        let r = rule("g", is(Predicate::IdIn { ids: vec![1] }));
        let out = resolve(&[r], &[f1, f2]);
        assert!(out.diffs.is_empty(), "没有变化的凭据不该出现在 diffs 里");
    }

    #[test]
    fn disabled_rule_does_not_touch_its_group() {
        let mut f = fact(1);
        f.current_groups = vec!["off".to_string()];
        let mut r = rule("off", is(Predicate::IdIn { ids: vec![999] }));
        r.enabled = false;

        let out = resolve(&[r], &[f]);
        assert!(out.diffs.is_empty(), "禁用的规则不管它那个分组");
        assert!(out.assignments.is_empty());
    }

    // ---------- 确定性 / 稳定性 ----------

    fn sample_rules() -> Vec<GroupRule> {
        let mut r1 = rule(
            "for_O",
            Selector::All {
                of: vec![
                    is(Predicate::Subscription {
                        value: "KIRO POWER".into(),
                    }),
                    is(Predicate::HealthAtMost {
                        level: HealthLevel::Warn,
                    }),
                ],
            },
        );
        r1.capacity = Some(3);
        r1.pinned = vec![7];
        r1.excluded = vec![2];

        let mut r2 = rule(
            "internal",
            Selector::Any {
                of: vec![
                    is(Predicate::Tag {
                        value: "internal".into(),
                    }),
                    is(Predicate::IdIn { ids: vec![1] }),
                ],
            },
        );
        r2.capacity = Some(5);

        vec![r1, r2]
    }

    fn sample_facts() -> Vec<CredentialFacts> {
        let mk = |id: u64,
                  sub: &str,
                  health: HealthLevel,
                  remaining: f64,
                  tags: &[&str],
                  current: &[&str]| CredentialFacts {
            id,
            subscription_title: Some(sub.to_string()),
            health,
            remaining: Some(remaining),
            usage_percentage: Some(10.0),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            current_groups: current.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };

        vec![
            mk(1, "KIRO POWER", HealthLevel::Healthy, 500.0, &[], &["legacy"]),
            mk(2, "KIRO POWER", HealthLevel::Healthy, 900.0, &["internal"], &[]),
            mk(3, "KIRO POWER", HealthLevel::Warn, 900.0, &[], &[]),
            mk(4, "KIRO FREE", HealthLevel::Healthy, 900.0, &["internal"], &[]),
            mk(5, "KIRO POWER", HealthLevel::Healthy, 500.0, &[], &["for_O"]),
            mk(6, "KIRO POWER", HealthLevel::Dead, 900.0, &[], &[]),
            mk(7, "KIRO FREE", HealthLevel::Dead, 0.0, &[], &[]),
        ]
    }

    #[test]
    fn resolve_is_independent_of_input_order() {
        let rules = sample_rules();
        let facts = sample_facts();

        let baseline = resolve(&rules, &facts);
        let baseline_json = serde_json::to_string(&baseline).unwrap();

        // 三种不同的打乱方式，结果必须逐字节相同
        let mut reversed = facts.clone();
        reversed.reverse();

        let mut rotated = facts.clone();
        rotated.rotate_left(3);

        let mut interleaved: Vec<CredentialFacts> = Vec::new();
        let (head, tail) = facts.split_at(facts.len() / 2);
        for i in 0..head.len().max(tail.len()) {
            if let Some(f) = tail.get(i) {
                interleaved.push(f.clone());
            }
            if let Some(f) = head.get(i) {
                interleaved.push(f.clone());
            }
        }

        for shuffled in [reversed, rotated, interleaved] {
            let got = resolve(&rules, &shuffled);
            assert_eq!(
                serde_json::to_string(&got).unwrap(),
                baseline_json,
                "打乱输入顺序后结果变了"
            );
        }
    }

    #[test]
    fn resolve_is_deterministic_across_repeated_calls() {
        let rules = sample_rules();
        let facts = sample_facts();
        let a = serde_json::to_string(&resolve(&rules, &facts)).unwrap();
        let b = serde_json::to_string(&resolve(&rules, &facts)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn applying_diffs_then_resolving_again_yields_no_diff() {
        let rules = sample_rules();
        let mut facts = sample_facts();

        let first = resolve(&rules, &facts);
        assert!(!first.diffs.is_empty(), "第一轮应当有变更，否则测不出稳定性");

        apply(&mut facts, &first);
        let second = resolve(&rules, &facts);

        assert!(
            second.diffs.is_empty(),
            "第二轮不该再有变更，否则每轮调度都在换号：{:?}",
            second.diffs
        );
        assert_eq!(
            first.assignments, second.assignments,
            "分配结果必须保持不变"
        );
        assert_eq!(first.explanations, second.explanations);
    }

    #[test]
    fn duplicate_rule_names_union_and_warn() {
        let mut r1 = rule("g", is(Predicate::IdIn { ids: vec![1] }));
        r1.capacity = Some(1);
        let r2 = rule("g", is(Predicate::IdIn { ids: vec![2] }));

        let out = resolve(&[r1, r2], &[fact(1), fact(2), fact(3)]);
        assert_eq!(assigned(&out, "g"), vec![1, 2], "同名规则取并集");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("有 2 条启用规则")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn duplicate_credential_ids_warn() {
        let out = resolve(&[rule("g", is(Predicate::Always))], &[fact(1), fact(1)]);
        assert!(
            out.warnings.iter().any(|w| w.contains("id 重复")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn duplicate_credential_ids_are_deduped_not_double_counted() {
        // 重复 id 不去重的话会各占一个容量名额：capacity=2 喂三条记录（id 1,1,2）
        // 会让 assignments 只覆盖到 id=1，id=2 被"挤掉"。
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(2);
        let out = resolve(std::slice::from_ref(&r), &[fact(1), fact(1), fact(2)]);
        assert_eq!(
            out.assignments.get("g").map(Vec::as_slice),
            Some([1u64, 2].as_slice()),
            "去重后两个名额应给到两个不同 id"
        );
    }

    #[test]
    fn duplicate_credential_ids_yield_one_explanation_each() {
        // 同一个 id 出两条解释时，运营可能读到互相矛盾的两句话。
        let out = resolve(&[rule("g", is(Predicate::Always))], &[fact(1), fact(1)]);
        let n = out.explanations.iter().filter(|e| e.credential_id == 1).count();
        assert_eq!(n, 1, "同一 id 只应有一条解释，实际 {n} 条");
        let diffs = out.diffs.iter().filter(|d| d.credential_id == 1).count();
        assert!(diffs <= 1, "同一 id 只应有一条 diff，实际 {diffs} 条");
    }

    // ---------- validate ----------

    #[test]
    fn validate_passes_clean_ruleset() {
        let mut r = rule("for_O", is(Predicate::Always));
        r.capacity = Some(3);
        assert!(validate(&[r], &groups(&["for_O"])).is_empty());
    }

    #[test]
    fn validate_flags_unregistered_group() {
        let r = rule("ghost", is(Predicate::Always));
        let problems = validate(&[r], &groups(&["for_O"]));
        assert!(
            problems.iter().any(|p| p.contains("未注册的分组名")),
            "{:?}",
            problems
        );
    }

    #[test]
    fn validate_flags_zero_capacity_and_blank_name() {
        let mut r = rule("  ", is(Predicate::Always));
        r.capacity = Some(0);
        let problems = validate(&[r], &groups(&["for_O"]));
        assert!(problems.iter().any(|p| p.contains("分组名为空")), "{:?}", problems);
        assert!(problems.iter().any(|p| p.contains("容量为 0")), "{:?}", problems);
    }

    #[test]
    fn validate_flags_padded_name() {
        let r = rule(" for_O ", is(Predicate::Always));
        let problems = validate(&[r], &groups(&["for_O"]));
        assert!(
            problems.iter().any(|p| p.contains("首尾有空白")),
            "{:?}",
            problems
        );
    }

    #[test]
    fn validate_flags_suspicious_predicates() {
        let r = rule(
            "g",
            Selector::All {
                of: vec![
                    is(Predicate::Tag { value: " ".into() }),
                    is(Predicate::UsageBelowPct { value: 0.0 }),
                    is(Predicate::UsageBelowPct { value: 150.0 }),
                    is(Predicate::IdIn { ids: vec![] }),
                    Selector::Not {
                        of: Box::new(is(Predicate::Region { value: "".into() })),
                    },
                ],
            },
        );
        let problems = validate(&[r], &groups(&["g"]));
        assert!(problems.iter().any(|p| p.contains("tag 判据的值为空")), "{:?}", problems);
        assert!(
            problems.iter().any(|p| p.contains("都不会命中")),
            "{:?}",
            problems
        );
        assert!(
            problems.iter().any(|p| p.contains("超过 100")),
            "{:?}",
            problems
        );
        assert!(
            problems.iter().any(|p| p.contains("idIn 的 id 列表为空")),
            "{:?}",
            problems
        );
        assert!(
            problems.iter().any(|p| p.contains("region 判据的值为空")),
            "{:?}",
            problems
        );
    }

    #[test]
    fn validate_flags_pinned_and_excluded_overlap_and_over_capacity() {
        let mut r = rule("g", is(Predicate::Always));
        r.capacity = Some(1);
        r.pinned = vec![1, 2];
        r.excluded = vec![2];
        let problems = validate(&[r], &groups(&["g"]));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("同时在 pinned 与 excluded")),
            "{:?}",
            problems
        );
        assert!(
            problems.iter().any(|p| p.contains("超过容量 1")),
            "{:?}",
            problems
        );
    }

    #[test]
    fn validate_flags_duplicate_enabled_rule_names_only() {
        let r1 = rule("g", is(Predicate::Always));
        let r2 = rule("g", is(Predicate::Always));
        let problems = validate(&[r1.clone(), r2], &groups(&["g"]));
        assert!(
            problems.iter().any(|p| p.contains("有 2 条启用规则")),
            "{:?}",
            problems
        );

        // 其中一条禁用就不算冲突
        let mut off = rule("g", is(Predicate::Always));
        off.enabled = false;
        let problems = validate(&[r1, off], &groups(&["g"]));
        assert!(
            !problems.iter().any(|p| p.contains("条启用规则")),
            "{:?}",
            problems
        );
    }

    #[test]
    fn validate_zero_capacity_only_claims_rule_is_off_when_pinned_is_also_empty() {
        // 回归测试：修复前 validate() 的 match 是 Some(0) => 独立分支（只给
        // "等于把这条规则关掉"）、Some(c) => 才检查 pinned_n>c，两个分支互斥。
        // 但 resolve() 里 pinned 完全绕过 capacity 检查（pinned_present 直接
        // chain 进 selected，不受 take 限制），所以 capacity=0 但 pinned 非空时，
        // 规则并没有真的被关掉——旧文案与 resolve() 的实际行为矛盾，
        // 且 pinned 超过容量 0 这条问题会被"关掉"这句盖住、不会被单独指出。
        let mut r_pinned = rule("g", is(Predicate::Always));
        r_pinned.capacity = Some(0);
        r_pinned.pinned = vec![1, 2];
        let problems_pinned = validate(&[r_pinned.clone()], &groups(&["g"]));
        assert!(
            !problems_pinned.iter().any(|p| p.contains("等于把这条规则关掉")),
            "pinned 非空时规则没有真的被关掉：{:?}",
            problems_pinned
        );
        assert!(
            problems_pinned.iter().any(|p| p.contains("超过容量 0")),
            "{:?}",
            problems_pinned
        );

        let out = resolve(&[r_pinned], &[fact(1), fact(2)]);
        assert_eq!(
            assigned(&out, "g"),
            vec![1, 2],
            "resolve() 侧行为不变：pinned 仍强制包含，这是设计如此，不是本次修复对象"
        );

        // 容量0 且 pinned 为空：这才是真的"等于关掉规则"。
        let mut r_off = rule("g", is(Predicate::Always));
        r_off.capacity = Some(0);
        let problems_off = validate(&[r_off], &groups(&["g"]));
        assert!(
            problems_off.iter().any(|p| p.contains("等于把这条规则关掉")),
            "{:?}",
            problems_off
        );
    }

    #[test]
    fn validate_is_deterministic() {
        let rules = sample_rules();
        let registered = groups(&["for_O", "internal"]);
        assert_eq!(
            validate(&rules, &registered),
            validate(&rules, &registered)
        );
    }
}
