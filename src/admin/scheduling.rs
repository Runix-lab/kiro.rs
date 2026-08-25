//! 凭据调度自动化：按余额自动调优先级 + 保证首选层不塌到单点。
//!
//! # 优先级语义（数值越小越优先）
//!
//! - **< 50**：优先消耗档。想让哪个号先烧就放这里。
//! - **= 50**：正常档，新建凭据的默认值。
//! - **> 50**：退居二线。仍参与故障转移与负载均衡，但不做主力输出。
//!
//! # 两条自动规则
//!
//! 1. **额度守卫**：用量超过阈值（默认 95%）的凭据自动降到 `demote_to`（默认 60），
//!    把它从主力位置摘下来但不禁用——它还能接住溢出。月度重置后用量掉回阈值以下，
//!    自动恢复到降级前的优先级。
//! 2. **首选层保护**：保证「最优先那一档」至少有 `min_top_tier` 个可用凭据。
//!    单点首选层是 priority 模式下最危险的形态：粘滞选择会把全部流量压在一个号上，
//!    它一限流，整个分组跟着抖。
//!
//! # 为什么恢复要记原值而不是"降回 50"
//!
//! 运营会手工把某些号排在 40 或 55 表达意图。直接写 50 会把这份意图抹掉，而且
//! 抹掉的时机（月初重置）离设置时间很远，事后极难回溯。所以降级时把原值记在
//! `auto_demoted_from`，恢复时写回原值；人工在降级期间改过优先级则视为接管，
//! 不再自动恢复。

use serde::{Deserialize, Serialize};

/// 优先级中位线：新建凭据的默认档，也是"正常输出"的语义基准。
pub const PRIORITY_BASELINE: u32 = 50;

/// 调度自动化配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingConfig {
    /// 总开关。默认关闭——自动改优先级会改变生产路由，必须由人显式打开。
    #[serde(default)]
    pub enabled: bool,
    /// 用量超过该百分比即降级（0-100）
    #[serde(default = "default_demote_threshold_pct")]
    pub demote_threshold_pct: f64,
    /// 降级目标优先级（应 > 50，即退居二线）
    #[serde(default = "default_demote_to")]
    pub demote_to: u32,
    /// 首选层最少保留几个可用凭据
    #[serde(default = "default_min_top_tier")]
    pub min_top_tier: usize,
    /// 调度取向，见 [`SchedulingProfile`]
    #[serde(default)]
    pub profile: SchedulingProfile,
}

fn default_demote_threshold_pct() -> f64 {
    95.0
}
fn default_demote_to() -> u32 {
    60
}
fn default_min_top_tier() -> usize {
    2
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            demote_threshold_pct: default_demote_threshold_pct(),
            demote_to: default_demote_to(),
            min_top_tier: default_min_top_tier(),
            profile: SchedulingProfile::default(),
        }
    }
}

/// 调度取向：同一批凭据，想要的是「吞吐」还是「省额度」还是「烧掉某些号」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SchedulingProfile {
    /// 不主动铺排优先级，只跑两条自动规则。人工设的优先级完全保留。
    #[default]
    Manual,
    /// **加 TPM**：把所有健康凭据拉平到同一档，让 balanced 模式能真正并行铺开。
    /// 单账号并发受上游限制，拉平后总吞吐 ≈ 账号数 × 单账号并发。
    Throughput,
    /// **省额度**：余额多的排前面。让消耗自然向余量大的号倾斜，
    /// 各号见底时间趋于一致，避免"某个号先饿死→可用池缩小→并发挤在剩下的号上"。
    Conserve,
    /// **烧消耗**：余额少的排前面，优先把零头用掉。
    /// 适合月末——反正到期清零，先烧将要作废的额度。
    Drain,
}

/// 一个凭据参与调度决策所需的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialSchedulingInput {
    pub id: u64,
    pub priority: u32,
    pub disabled: bool,
    /// 上游已用百分比（0-100）。无余额数据时为 `None`——**不参与降级判断**，
    /// 取不到余额时把号降级等于拿"不知道"当"快用完了"。
    pub usage_pct: Option<f64>,
    /// 剩余额度，用于 Conserve/Drain 排序；无数据为 `None`
    pub remaining: Option<f64>,
    /// 自动降级前的原优先级；`None` 表示当前不是自动降级状态
    pub auto_demoted_from: Option<u32>,
}

/// 一条待执行的优先级调整。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityChange {
    pub id: u64,
    pub from: u32,
    pub to: u32,
    /// 降级时记下原值；恢复/铺排时为 `None`（表示清除降级标记）
    pub auto_demoted_from: Option<u32>,
    pub reason: ChangeReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeReason {
    /// 用量超阈值，退居二线
    QuotaDemote,
    /// 用量回落（通常是月度重置），恢复原优先级
    QuotaRestore,
    /// 首选层可用数不足，从次优层提一个上来
    TopTierRefill,
    /// 按调度取向重新铺排
    ProfileRebalance,
}

/// 计算需要执行的优先级调整。**纯函数，不产生副作用**——这样它的每条规则
/// 都能被单测钉住，而不用起一个真的调度器。
pub fn plan_changes(
    cfg: &SchedulingConfig,
    creds: &[CredentialSchedulingInput],
) -> Vec<PriorityChange> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut changes: Vec<PriorityChange> = Vec::new();
    // 规则之间会互相影响（降级会掏空首选层），所以在一份"生效后"的视图上依次推演
    let mut effective: Vec<CredentialSchedulingInput> = creds.to_vec();

    // ---- 规则 1：额度守卫 ----
    for c in effective.iter_mut() {
        let Some(pct) = c.usage_pct else { continue };
        match c.auto_demoted_from {
            // 已降级：用量回落到阈值以下则恢复原值
            Some(orig) => {
                if pct < cfg.demote_threshold_pct {
                    changes.push(PriorityChange {
                        id: c.id,
                        from: c.priority,
                        to: orig,
                        auto_demoted_from: None,
                        reason: ChangeReason::QuotaRestore,
                    });
                    c.priority = orig;
                    c.auto_demoted_from = None;
                }
            }
            // 未降级：超阈值且当前还在主力位置才降
            None => {
                if pct >= cfg.demote_threshold_pct && c.priority < cfg.demote_to {
                    changes.push(PriorityChange {
                        id: c.id,
                        from: c.priority,
                        to: cfg.demote_to,
                        auto_demoted_from: Some(c.priority),
                        reason: ChangeReason::QuotaDemote,
                    });
                    c.auto_demoted_from = Some(c.priority);
                    c.priority = cfg.demote_to;
                }
            }
        }
    }

    // ---- 规则 2：按取向铺排（Manual 跳过） ----
    if cfg.profile != SchedulingProfile::Manual {
        changes.extend(plan_profile(cfg, &mut effective));
    }

    // ---- 规则 3：首选层保护 ----
    changes.extend(plan_top_tier_refill(cfg, &mut effective));

    // 同一凭据可能被多条规则连续改动，只保留最终值（from 取最初值）
    dedupe_changes(changes, creds)
}

/// 按调度取向铺排优先级。只动**未被额度守卫降级**的凭据——被守卫摘下来的号
/// 不该被取向重新推回主力，否则两条规则会互相拉锯。
fn plan_profile(
    cfg: &SchedulingConfig,
    effective: &mut [CredentialSchedulingInput],
) -> Vec<PriorityChange> {
    let mut out = Vec::new();
    let mut eligible: Vec<usize> = effective
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.disabled && c.auto_demoted_from.is_none())
        .map(|(i, _)| i)
        .collect();
    if eligible.is_empty() {
        return out;
    }

    let targets: Vec<(usize, u32)> = match cfg.profile {
        SchedulingProfile::Manual => return out,
        // 全部拉平到基线：同档并列，交给 balanced 模式并行铺开
        SchedulingProfile::Throughput => {
            eligible.iter().map(|&i| (i, PRIORITY_BASELINE)).collect()
        }
        // 余额多的排前面（Conserve）/ 余额少的排前面（Drain）
        SchedulingProfile::Conserve | SchedulingProfile::Drain => {
            let desc = cfg.profile == SchedulingProfile::Conserve;
            eligible.sort_by(|&a, &b| {
                let ra = effective[a].remaining.unwrap_or(0.0);
                let rb = effective[b].remaining.unwrap_or(0.0);
                if desc {
                    rb.total_cmp(&ra)
                } else {
                    ra.total_cmp(&rb)
                }
            });
            // 从 48 起每档 +1：48/49/50/... 保持在基线附近，人工设的 <48 仍然更优先
            eligible
                .iter()
                .enumerate()
                .map(|(rank, &i)| (i, PRIORITY_BASELINE.saturating_sub(2) + rank as u32))
                .collect()
        }
    };

    for (i, target) in targets {
        if effective[i].priority != target {
            out.push(PriorityChange {
                id: effective[i].id,
                from: effective[i].priority,
                to: target,
                auto_demoted_from: None,
                reason: ChangeReason::ProfileRebalance,
            });
            effective[i].priority = target;
        }
    }
    out
}

/// 首选层不足时，从次优层提凭据上来补齐。
///
/// 「首选层」= 可用凭据里 priority 最小的那个值上的全部凭据。priority 模式下
/// 只有这一档会被粘滞选中，所以它掉到 1 个就等于全部流量压在单点上。
fn plan_top_tier_refill(
    cfg: &SchedulingConfig,
    effective: &mut [CredentialSchedulingInput],
) -> Vec<PriorityChange> {
    let mut out = Vec::new();
    if cfg.min_top_tier <= 1 {
        return out;
    }
    // 排除被额度守卫摘下的号：它们额度将尽，提回首选层会立刻耗尽并再次降级，
    // 两条规则来回拉锯。首选层补不齐时宁可留缺口——那是容量问题，得加号解决。
    let mut usable: Vec<usize> = effective
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.disabled && c.auto_demoted_from.is_none())
        .map(|(i, _)| i)
        .collect();
    if usable.len() <= 1 {
        // 只剩一个可用凭据：补不出第二个来，这是容量问题，不是排序问题
        return out;
    }
    usable.sort_by_key(|&i| effective[i].priority);
    let top = effective[usable[0]].priority;
    let top_count = usable
        .iter()
        .filter(|&&i| effective[i].priority == top)
        .count();
    let want = cfg.min_top_tier.min(usable.len());
    if top_count >= want {
        return out;
    }
    for &i in usable.iter().skip(top_count).take(want - top_count) {
        out.push(PriorityChange {
            id: effective[i].id,
            from: effective[i].priority,
            to: top,
            auto_demoted_from: None,
            reason: ChangeReason::TopTierRefill,
        });
        effective[i].priority = top;
    }
    out
}

/// 同一凭据被多条规则改动时压成一条：`from` 取原始值、`to` 取最终值。
/// 净变化为 0 的直接丢弃，避免把优先级写一遍原值、白白触发一次持久化。
fn dedupe_changes(
    changes: Vec<PriorityChange>,
    original: &[CredentialSchedulingInput],
) -> Vec<PriorityChange> {
    use std::collections::HashMap;
    let orig_priority: HashMap<u64, u32> = original.iter().map(|c| (c.id, c.priority)).collect();
    let orig_demoted: HashMap<u64, Option<u32>> =
        original.iter().map(|c| (c.id, c.auto_demoted_from)).collect();
    let mut merged: HashMap<u64, PriorityChange> = HashMap::new();
    let mut order: Vec<u64> = Vec::new();
    for ch in changes {
        match merged.get_mut(&ch.id) {
            Some(prev) => {
                prev.to = ch.to;
                prev.auto_demoted_from = ch.auto_demoted_from;
                prev.reason = ch.reason;
            }
            None => {
                order.push(ch.id);
                merged.insert(
                    ch.id,
                    PriorityChange {
                        from: *orig_priority.get(&ch.id).unwrap_or(&ch.from),
                        ..ch
                    },
                );
            }
        }
    }
    order
        .into_iter()
        .filter_map(|id| merged.remove(&id))
        .filter(|ch| {
            ch.to != ch.from || ch.auto_demoted_from != *orig_demoted.get(&ch.id).unwrap_or(&None)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(id: u64, priority: u32, usage_pct: Option<f64>) -> CredentialSchedulingInput {
        CredentialSchedulingInput {
            id,
            priority,
            disabled: false,
            usage_pct,
            remaining: usage_pct.map(|p| (100.0 - p) * 100.0),
            auto_demoted_from: None,
        }
    }

    fn on(profile: SchedulingProfile) -> SchedulingConfig {
        SchedulingConfig {
            enabled: true,
            profile,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_config_plans_nothing() {
        let cfg = SchedulingConfig::default(); // enabled = false
        let creds = vec![cred(1, 50, Some(99.0))];
        assert!(plan_changes(&cfg, &creds).is_empty(), "总开关关闭时不得改动生产路由");
    }

    #[test]
    fn exhausted_credential_is_demoted_and_remembers_its_original_priority() {
        let cfg = SchedulingConfig {
            min_top_tier: 1, // 隔离出额度守卫这一条规则
            ..on(SchedulingProfile::Manual)
        };
        let creds = vec![cred(1, 45, Some(96.0)), cred(2, 50, Some(10.0))];
        let ch = plan_changes(&cfg, &creds);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].id, 1);
        assert_eq!(ch[0].to, 60);
        assert_eq!(ch[0].auto_demoted_from, Some(45), "必须记住原值 45，恢复时写回它");
        assert_eq!(ch[0].reason, ChangeReason::QuotaDemote);
    }

    #[test]
    fn usage_falling_back_restores_the_original_priority_not_the_baseline() {
        let cfg = SchedulingConfig {
            min_top_tier: 1,
            ..on(SchedulingProfile::Manual)
        };
        let mut c = cred(1, 60, Some(3.0)); // 月度重置后用量归零
        c.auto_demoted_from = Some(42);
        let ch = plan_changes(&cfg, &[c, cred(2, 50, Some(10.0))]);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].to, 42, "恢复到降级前的 42，而不是基线 50");
        assert_eq!(ch[0].auto_demoted_from, None, "恢复后清除降级标记");
        assert_eq!(ch[0].reason, ChangeReason::QuotaRestore);
    }

    #[test]
    fn missing_balance_never_triggers_demotion() {
        let cfg = SchedulingConfig {
            min_top_tier: 1,
            ..on(SchedulingProfile::Manual)
        };
        // 取不到余额 ≠ 快用完了：拿"不知道"当"耗尽"会把好号误摘
        let ch = plan_changes(&cfg, &[cred(1, 50, None), cred(2, 50, Some(20.0))]);
        assert!(ch.is_empty());
    }

    #[test]
    fn a_credential_already_in_the_back_row_is_not_demoted_again() {
        let cfg = SchedulingConfig {
            min_top_tier: 1,
            ..on(SchedulingProfile::Manual)
        };
        // 已经在 70（>demote_to=60），不需要再动
        let ch = plan_changes(&cfg, &[cred(1, 70, Some(99.0)), cred(2, 50, Some(5.0))]);
        assert!(ch.is_empty());
    }

    #[test]
    fn top_tier_is_refilled_so_traffic_never_pins_to_one_credential() {
        let cfg = on(SchedulingProfile::Manual); // min_top_tier = 2
        // 首选层只有 #1；#2/#3 在后面。priority 模式会把全部流量压在 #1 上
        let ch = plan_changes(&cfg, &[cred(1, 40, Some(10.0)), cred(2, 50, Some(10.0)), cred(3, 55, Some(10.0))]);
        assert_eq!(ch.len(), 1, "补齐到 2 个即可，不是把所有号都拉上来");
        assert_eq!(ch[0].id, 2, "提次优的那个（50），不是最靠后的 55");
        assert_eq!(ch[0].to, 40);
        assert_eq!(ch[0].reason, ChangeReason::TopTierRefill);
    }

    #[test]
    fn top_tier_refill_does_not_invent_credentials_that_do_not_exist() {
        let cfg = SchedulingConfig {
            min_top_tier: 3,
            ..on(SchedulingProfile::Manual)
        };
        // 只有一个可用凭据：这是容量问题，排序救不了，不该产生任何改动
        let mut disabled = cred(2, 50, Some(10.0));
        disabled.disabled = true;
        let ch = plan_changes(&cfg, &[cred(1, 50, Some(10.0)), disabled]);
        assert!(ch.is_empty());
    }

    #[test]
    fn demotion_that_empties_the_top_tier_triggers_a_refill_in_the_same_pass() {
        let cfg = on(SchedulingProfile::Manual);
        // #1、#2 同在首选层 45，#1 耗尽被降级 → 首选层只剩 #2 → 补 #3 上来
        let ch = plan_changes(
            &cfg,
            &[
                cred(1, 45, Some(97.0)),
                cred(2, 45, Some(10.0)),
                cred(3, 52, Some(10.0)),
            ],
        );
        let by_id = |id: u64| ch.iter().find(|c| c.id == id).expect("应有该凭据的改动");
        assert_eq!(by_id(1).to, 60);
        assert_eq!(by_id(3).to, 45, "被降级掏空的首选层要当场补回来");
        assert_eq!(by_id(3).reason, ChangeReason::TopTierRefill);
    }

    #[test]
    fn throughput_profile_flattens_everyone_onto_the_baseline() {
        let cfg = on(SchedulingProfile::Throughput);
        let ch = plan_changes(&cfg, &[cred(1, 40, Some(10.0)), cred(2, 55, Some(10.0)), cred(3, 50, Some(10.0))]);
        for c in &ch {
            assert_eq!(c.to, PRIORITY_BASELINE, "加 TPM = 全部并列，让负载真正铺开");
        }
        assert_eq!(ch.len(), 2, "已经在基线上的那个不动");
    }

    #[test]
    fn conserve_puts_the_fullest_credential_first_and_drain_does_the_opposite() {
        let mk = |id, rem| CredentialSchedulingInput {
            id,
            priority: 50,
            disabled: false,
            usage_pct: Some(10.0),
            remaining: Some(rem),
            auto_demoted_from: None,
        };
        let creds = vec![mk(1, 1000.0), mk(2, 9000.0), mk(3, 5000.0)];

        // 最终优先级 = 有变更取变更值，无变更取原值（无变更不产生记录）
        let final_of = |ch: &[PriorityChange], id: u64| {
            ch.iter()
                .find(|c| c.id == id)
                .map(|c| c.to)
                .unwrap_or_else(|| creds.iter().find(|c| c.id == id).unwrap().priority)
        };

        // 隔离取向这一条规则：首选层保护会把次优的提上来补齐，那是另一条规则的事，
        // 由 profile_and_top_tier_refill_compose 单独验证。
        let only_profile = |p: SchedulingProfile| SchedulingConfig {
            min_top_tier: 1,
            ..on(p)
        };

        let conserve = plan_changes(&only_profile(SchedulingProfile::Conserve), &creds);
        // 省额度：余额最多的 #2 排最前，最少的 #1 排最后
        assert_eq!(final_of(&conserve, 2), 48);
        assert_eq!(final_of(&conserve, 3), 49);
        assert_eq!(final_of(&conserve, 1), 50);

        let drain = plan_changes(&only_profile(SchedulingProfile::Drain), &creds);
        // 烧消耗：反过来，余额最少的 #1 排最前
        assert_eq!(final_of(&drain, 1), 48);
        assert_eq!(final_of(&drain, 3), 49);
        assert_eq!(final_of(&drain, 2), 50);
    }

    #[test]
    fn profile_and_top_tier_refill_compose() {
        // 取向铺排会造出"首选层只有 1 个"的形态（48/49/50），
        // 首选层保护必须当场把次优的那个补到 48，否则又是单点。
        let mk = |id, rem| CredentialSchedulingInput {
            id,
            priority: 50,
            disabled: false,
            usage_pct: Some(10.0),
            remaining: Some(rem),
            auto_demoted_from: None,
        };
        let creds = vec![mk(1, 1000.0), mk(2, 9000.0), mk(3, 5000.0)];
        let ch = plan_changes(&on(SchedulingProfile::Conserve), &creds); // min_top_tier = 2
        let final_of = |id: u64| ch.iter().find(|c| c.id == id).map(|c| c.to);
        assert_eq!(final_of(2), Some(48), "余额最多的排首选层");
        assert_eq!(final_of(3), Some(48), "次优的被补进首选层，凑够 2 个");
        let top_count = [1u64, 2, 3]
            .iter()
            .filter(|&&id| final_of(id).unwrap_or(50) == 48)
            .count();
        assert_eq!(top_count, 2, "补齐到 min_top_tier 就停，不是把所有号都拉上来");
    }

    #[test]
    fn profile_rebalance_leaves_quota_demoted_credentials_alone() {
        let cfg = on(SchedulingProfile::Throughput);
        let mut demoted = cred(1, 60, Some(99.0));
        demoted.auto_demoted_from = Some(50);
        let ch = plan_changes(&cfg, &[demoted, cred(2, 44, Some(10.0))]);
        assert!(
            ch.iter().all(|c| c.id != 1),
            "被额度守卫摘下的号不能被取向推回主力，否则两条规则来回拉锯"
        );
    }

    #[test]
    fn a_change_that_nets_to_nothing_is_dropped() {
        let cfg = SchedulingConfig {
            min_top_tier: 1,
            ..on(SchedulingProfile::Throughput)
        };
        // 已经在基线且无需降级 → 不产生任何写操作
        let ch = plan_changes(&cfg, &[cred(1, 50, Some(10.0)), cred(2, 50, Some(10.0))]);
        assert!(ch.is_empty());
    }
}
