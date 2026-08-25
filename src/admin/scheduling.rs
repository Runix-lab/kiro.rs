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
    /// **吞吐模式专用**：用量低于该百分比的凭据进前排，尽情烧。
    #[serde(default = "default_throughput_burn_below_pct")]
    pub throughput_burn_below_pct: f64,
    /// **吞吐模式专用**：用量达到该百分比的凭据退到溢出储备档，
    /// 不接主力流量，只接前排 429 之后溢出的那部分。
    #[serde(default = "default_throughput_reserve_at_pct")]
    pub throughput_reserve_at_pct: f64,
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
fn default_throughput_burn_below_pct() -> f64 {
    80.0
}
fn default_throughput_reserve_at_pct() -> f64 {
    95.0
}

/// 吞吐模式的三档优先级。数值越小越优先。
///
/// 之所以拉开到 40/50/70 而不是挤在基线附近：选凭据是按 priority 分档的，
/// 档与档之间必须有明确间隔，溢出档才不会被误当成主力候选。
pub const THROUGHPUT_FRONT: u32 = 40;
pub const THROUGHPUT_MID: u32 = 50;
pub const THROUGHPUT_RESERVE: u32 = 70;

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            demote_threshold_pct: default_demote_threshold_pct(),
            demote_to: default_demote_to(),
            min_top_tier: default_min_top_tier(),
            throughput_burn_below_pct: default_throughput_burn_below_pct(),
            throughput_reserve_at_pct: default_throughput_reserve_at_pct(),
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
    //
    // 吞吐模式跳过这条：它的分档本身就把 >= reserve_at_pct 的号放到了比
    // demote_to 更靠后的溢出档。两条规则同时跑会互相拉锯——守卫把号拽到 60、
    // 分档又把它推到 70，每轮都产生一次变更，日志里全是抖动。
    let quota_guard_active = cfg.profile != SchedulingProfile::Throughput;
    for c in effective.iter_mut() {
        if !quota_guard_active {
            break;
        }
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
    // 吞吐模式接管全部优先级，包括此前被额度守卫降级的号——它自己会把
    // 高用量的号放进溢出档，不需要守卫的记账。其它取向仍然避开守卫降级的号，
    // 否则两条规则拉锯。
    let takes_over = cfg.profile == SchedulingProfile::Throughput;
    let mut eligible: Vec<usize> = effective
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.disabled && (takes_over || c.auto_demoted_from.is_none()))
        .map(|(i, _)| i)
        .collect();
    if eligible.is_empty() {
        return out;
    }

    let targets: Vec<(usize, u32)> = match cfg.profile {
        SchedulingProfile::Manual => return out,
        // 两档铺排：
        //   用量 < burn_below   → 前排，尽情烧（同档并列，靠 balanced 并行铺开）
        //   burn_below..reserve → 中间档，正常输出
        //   用量 >= reserve     → 溢出储备，只接前排 429 之后溢出的流量
        //
        // 取不到用量的号放中间档：把"不知道"当"快用完了"会平白少掉一个主力，
        // 当成"还很空"又会把流量压给一个可能已经见底的号。中间档是唯一不做
        // 假设的位置。
        SchedulingProfile::Throughput => eligible
            .iter()
            .map(|&i| {
                let target = match effective[i].usage_pct {
                    Some(pct) if pct >= cfg.throughput_reserve_at_pct => THROUGHPUT_RESERVE,
                    Some(pct) if pct < cfg.throughput_burn_below_pct => THROUGHPUT_FRONT,
                    Some(_) => THROUGHPUT_MID,
                    None => THROUGHPUT_MID,
                };
                (i, target)
            })
            .collect(),
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
    //
    // 吞吐模式下额外排除**溢出储备档**：那一档的存在意义就是"不接主力流量、
    // 只接前排 429 之后溢出的部分"。把它提回首选层等于取消了储备，前排一撑不住
    // 就连储备一起烧穿，整池同时见底。
    let reserve_excluded = cfg.profile == SchedulingProfile::Throughput;
    let mut usable: Vec<usize> = effective
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if c.disabled || c.auto_demoted_from.is_some() {
                return false;
            }
            if reserve_excluded && c.priority >= THROUGHPUT_RESERVE {
                return false;
            }
            true
        })
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

    /// 吞吐模式的两档：<80% 进前排尽情烧，>=95% 退到溢出储备，中间正常输出。
    #[test]
    fn throughput_mode_splits_into_burn_and_reserve_bands() {
        let cfg = on(SchedulingProfile::Throughput);
        let creds = vec![
            cred(1, 50, Some(1.4)),   // 几乎没用 → 前排
            cred(2, 50, Some(48.9)),  // 用了一半 → 前排
            cred(3, 50, Some(86.6)),  // 中间带
            cred(4, 50, Some(93.0)),  // 中间带（还没到 95）
            cred(5, 50, Some(96.5)),  // 见底 → 溢出储备
        ];
        let ch = plan_changes(&cfg, &creds);
        let to = |id: u64| ch.iter().find(|c| c.id == id).map(|c| c.to);
        assert_eq!(to(1), Some(THROUGHPUT_FRONT), "空号该进前排");
        assert_eq!(to(2), Some(THROUGHPUT_FRONT));
        assert_eq!(to(3), None, "已在中间档，无需变更");
        assert_eq!(to(4), None);
        assert_eq!(to(5), Some(THROUGHPUT_RESERVE), "快见底的该退到溢出储备");
    }

    /// 溢出储备不能被首选层补齐规则拽回前排——那等于取消了储备，
    /// 前排一撑不住就连储备一起烧穿，整池同时见底。
    #[test]
    fn top_tier_refill_never_pulls_the_reserve_band_forward() {
        let cfg = SchedulingConfig {
            enabled: true,
            profile: SchedulingProfile::Throughput,
            min_top_tier: 3, // 故意要求 3 个，而前排只有 1 个
            ..Default::default()
        };
        let creds = vec![
            cred(1, 50, Some(10.0)),  // 唯一的前排
            cred(2, 50, Some(97.0)),  // 溢出储备
            cred(3, 50, Some(98.0)),  // 溢出储备
        ];
        let ch = plan_changes(&cfg, &creds);
        for c in &ch {
            if c.id == 2 || c.id == 3 {
                assert_eq!(
                    c.to, THROUGHPUT_RESERVE,
                    "储备档 #{} 被拽到了 {}，储备形同虚设",
                    c.id, c.to
                );
            }
        }
    }

    /// 吞吐模式接管额度守卫：同一个号不能既被守卫拽到 60、又被分档推到 70，
    /// 那样每轮都产生一次变更，日志里全是抖动。
    #[test]
    fn throughput_mode_takes_over_the_quota_guard() {
        let cfg = on(SchedulingProfile::Throughput);
        let creds = vec![cred(1, 50, Some(99.0)), cred(2, 50, Some(20.0))];
        let ch = plan_changes(&cfg, &creds);
        let c1 = ch.iter().find(|c| c.id == 1).expect("高用量号应被安排");
        assert_eq!(c1.to, THROUGHPUT_RESERVE, "应进溢出档而不是守卫的 demote_to");
        assert_eq!(c1.auto_demoted_from, None, "吞吐模式不走守卫的记账");
        assert!(
            matches!(c1.reason, ChangeReason::ProfileRebalance),
            "变更原因应是分档而非额度降级，实得 {:?}",
            c1.reason
        );
    }

    /// 取不到用量的号放中间档：当成"快用完"平白少一个主力，
    /// 当成"还很空"又可能把流量压给已经见底的号。
    #[test]
    fn unknown_usage_lands_in_the_middle_band() {
        let cfg = on(SchedulingProfile::Throughput);
        let creds = vec![cred(1, 40, None), cred(2, 70, None)];
        let ch = plan_changes(&cfg, &creds);
        for c in &ch {
            assert_eq!(c.to, THROUGHPUT_MID, "用量未知的号不该被猜到任何一端");
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

    /// 加 TPM 的核心仍然是「并列铺开」——只是并列的位置从基线换成了前排档。
    /// 额度充足的号无论人工设成 40/50/55，都要落到同一档，balanced 才能真正并行。
    #[test]
    fn throughput_profile_flattens_the_front_band_onto_one_tier() {
        let cfg = on(SchedulingProfile::Throughput);
        let ch = plan_changes(
            &cfg,
            &[cred(1, 40, Some(10.0)), cred(2, 55, Some(10.0)), cred(3, 50, Some(10.0))],
        );
        for c in &ch {
            assert_eq!(c.to, THROUGHPUT_FRONT, "额度充足的号必须并列在前排同一档");
        }
        assert_eq!(ch.len(), 2, "已经在前排档上的那个不动");
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
        // 用 Conserve：Throughput 现在**故意**接管额度守卫（见
        // throughput_mode_takes_over_the_quota_guard），这条不变量守的是
        // 「守卫与取向不得互相拉锯」，对其余取向依然成立。
        let cfg = on(SchedulingProfile::Conserve);
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
        // 已经在各自该在的档位上 → 不产生任何写操作
        // （用量 10% 的号本来就该在前排档，写一遍原值只会白白触发持久化）
        let ch = plan_changes(
            &cfg,
            &[
                cred(1, THROUGHPUT_FRONT, Some(10.0)),
                cred(2, THROUGHPUT_FRONT, Some(10.0)),
            ],
        );
        assert!(ch.is_empty(), "净变化为 0 不应产生写操作，实得 {:?}", ch);
    }
}

// ============================================================================
// 吞吐预估
// ============================================================================

/// 开启吞吐模式前的预估。
///
/// # 为什么要有这个
///
/// 「打开吞吐模式」听起来像个免费的加速开关，其实不是：它只改变**流量怎么分布**，
/// 不改变**能烧多少**。上游额度是按月固定的，把并发铺开只会让同样的额度烧得更快。
/// 所以打开之前必须让人看到两个数：能提到多少并发，以及这样烧还能撑几天。
///
/// # 口径
///
/// - **并发**：前排档凭据数 × 单凭据实测并发上限。这是能力上限，不是保证值。
/// - **可持续 TPM**：前排剩余额度 ÷ 距离重置的时间，换算成 token。这才是真天花板——
///   实测中位 TPM 已经是可持续值的 3 倍时，提并发只会让见底提前。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputEstimate {
    /// 前排档（尽情烧）凭据数
    pub front_tier: usize,
    /// 中间档凭据数
    pub mid_tier: usize,
    /// 溢出储备档凭据数
    pub reserve_tier: usize,
    /// 预估并发上限 = 参与主力的凭据数 × 单凭据实测并发
    pub estimated_concurrency: u32,
    /// 当前并发上限（按当前实际参与主力的凭据数算），用于给出提升倍数
    pub current_concurrency: u32,
    /// 相对当前的提升倍数
    pub concurrency_gain: f64,
    /// 前排 + 中间档剩余额度合计
    pub usable_credits: f64,
    /// 按当前烧速，这些额度还能撑多少小时；无烧速数据为 `None`
    pub runway_hours: Option<f64>,
    /// 距离额度重置还有多少小时
    pub hours_to_reset: Option<f64>,
    /// 可持续 TPM：可用额度 ÷ 距重置时间，按每 credit 折合 token 换算
    pub sustainable_tpm: Option<u64>,
    /// 人话说明，直接显示给运营
    pub notes: Vec<String>,
}

/// 预估所需的实测输入。全部来自真实观测，不要拍脑袋填。
#[derive(Debug, Clone, Copy)]
pub struct ThroughputObservations {
    /// 单凭据实测并发峰值（近几天观测到的最大同时在飞请求数）
    pub per_credential_concurrency: u32,
    /// 每 credit 折合多少 token（计费口径，实测值）
    pub tokens_per_credit: f64,
    /// 当前烧速：credits/小时
    pub credits_per_hour: f64,
    /// 距离额度重置还有多少小时
    pub hours_to_reset: f64,
}

/// 按分档结果算出吞吐预估。纯函数，便于测试。
pub fn estimate_throughput(
    cfg: &SchedulingConfig,
    creds: &[CredentialSchedulingInput],
    obs: ThroughputObservations,
) -> ThroughputEstimate {
    let live: Vec<&CredentialSchedulingInput> = creds.iter().filter(|c| !c.disabled).collect();

    let band_of = |c: &CredentialSchedulingInput| match c.usage_pct {
        Some(p) if p >= cfg.throughput_reserve_at_pct => 2u8,
        Some(p) if p < cfg.throughput_burn_below_pct => 0,
        _ => 1,
    };
    let front = live.iter().filter(|c| band_of(c) == 0).count();
    let mid = live.iter().filter(|c| band_of(c) == 1).count();
    let reserve = live.iter().filter(|c| band_of(c) == 2).count();

    // 主力 = 前排 + 中间档。储备档只接溢出，不计入常态并发能力。
    let primary = front + mid;
    let estimated = primary as u32 * obs.per_credential_concurrency;

    // 当前能力：没开吞吐模式时，priority 最小的那一档才是主力，
    // 其余的号只在故障转移时才会被碰到。
    let min_priority = live.iter().map(|c| c.priority).min().unwrap_or(PRIORITY_BASELINE);
    let current_primary = live.iter().filter(|c| c.priority == min_priority).count();
    let current = current_primary as u32 * obs.per_credential_concurrency;

    let usable_credits: f64 = live
        .iter()
        .filter(|c| band_of(c) != 2)
        .filter_map(|c| c.remaining)
        .sum();

    let runway_hours = if obs.credits_per_hour > 0.0 {
        Some(usable_credits / obs.credits_per_hour)
    } else {
        None
    };
    let hours_to_reset = (obs.hours_to_reset > 0.0).then_some(obs.hours_to_reset);
    let sustainable_tpm = hours_to_reset.map(|h| {
        // 额度撑到重置的前提下，平均每分钟能烧多少 token
        (usable_credits * obs.tokens_per_credit / (h * 60.0)).max(0.0) as u64
    });

    let mut notes = Vec::new();
    if primary == 0 {
        notes.push("没有凭据落在主力档——全部已达溢出储备阈值，此时开吞吐模式不会提升任何东西，先加号或等额度重置。".to_string());
    } else if front == 0 {
        notes.push(format!(
            "前排档为空：{} 个主力号的用量都已超过 {:.0}%，吞吐提升有限且会加速见底。",
            mid, cfg.throughput_burn_below_pct
        ));
    }
    if reserve > 0 {
        notes.push(format!(
            "{} 个号已达 {:.0}% 用量，退到溢出储备档：不接主力流量，只在前排 429 时兜底。",
            reserve, cfg.throughput_reserve_at_pct
        ));
    }
    if let (Some(rw), Some(reset)) = (runway_hours, hours_to_reset) {
        if rw < reset {
            notes.push(format!(
                "按当前烧速，可用额度还能撑 {:.1} 小时，但距离重置还有 {:.1} 小时——会提前 {:.1} 小时断供。提并发会让这个缺口更大。",
                rw, reset, reset - rw
            ));
        } else {
            notes.push(format!(
                "按当前烧速可撑 {:.1} 小时，够撑到 {:.1} 小时后的重置，有 {:.0}% 余量。",
                rw, reset, (rw / reset - 1.0) * 100.0
            ));
        }
    }

    ThroughputEstimate {
        front_tier: front,
        mid_tier: mid,
        reserve_tier: reserve,
        estimated_concurrency: estimated,
        current_concurrency: current,
        concurrency_gain: if current > 0 {
            estimated as f64 / current as f64
        } else {
            0.0
        },
        usable_credits,
        runway_hours,
        hours_to_reset,
        sustainable_tpm,
        notes,
    }
}
