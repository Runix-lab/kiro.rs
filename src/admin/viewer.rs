//! 只读观察者视图 —— 给外部方看「服务在正常跑量」，不交出经营数据。
//!
//! # 为什么单独一个接口而不是给现有接口加过滤
//!
//! 现有 `/stats/*` 与 `/credentials` 的返回体里带客户 Key 名、成本、毛利、
//! 凭据邮箱。给它们加"如果是只读就删字段"的分支，等于每次新增字段都要记得
//! 去补一处过滤 —— 漏一次就是把客户名单发给了外部方。
//!
//! 所以这里是一个**独立的、白名单式的响应结构**：字段是显式列出来的，
//! 上游多返回什么都不会顺带流出去。
//!
//! # 露什么、不露什么
//!
//! 露：请求数、成功率、模型分布（只有模型名与占比）、按小时的量、当前速率、
//!     在役凭据条数（只有数字）。
//!
//! 不露：客户 Key 名与用量拆分、任何金额（成本/毛利/credit）、凭据邮箱与额度、
//!       请求体与 prompt、分组名、上游报错原文。
//!
//! 判据是「这条信息泄露了经营状况或客户身份吗」。请求量泄露不了 ——
//! 它恰恰是要证明的那件事。

use serde::Serialize;

/// 只读流量概览。**字段只增不改语义**：外部方可能已经在读它。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerTraffic {
    /// 窗口描述，如 `"today"` / `"7d"`。
    pub window: String,
    /// 该窗口内的请求总数。
    pub requests: u64,
    /// 成功率百分比，一位小数。
    pub success_rate_pct: f64,
    /// 处理的 token 总量（输入+输出合并，不拆开——拆开可反推成本结构）。
    pub tokens: u64,
    /// 模型分布，按占比降序。
    pub models: Vec<ViewerModelShare>,
    /// 按小时的请求数，最近 24 个点，老到新。
    pub hourly_requests: Vec<u64>,
    /// 当前分钟级速率。
    pub current_rpm: u64,
    /// 在役凭据条数（只有数字，没有身份）。
    pub active_credentials: u64,
    /// 生成时刻（RFC3339），让对方知道数据的新鲜度。
    pub generated_at: String,
}

/// 单个模型的占比。**没有金额字段**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerModelShare {
    pub model: String,
    pub requests: u64,
    pub share_pct: f64,
}

/// 只读会话信息，给前端判断该渲染什么。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerSession {
    /// 恒为 `"viewer"`。前端据此只渲染流量页。
    pub role: &'static str,
    /// 面板标题，让对方一眼知道这是只读视图。
    pub title: String,
    /// 明确告知哪些内容被隐去了 —— 与其让人怀疑数据被修饰过，不如说清边界。
    pub redacted: Vec<&'static str>,
}

impl ViewerSession {
    pub fn new() -> Self {
        Self {
            role: "viewer",
            title: "流量概览（只读）".to_string(),
            redacted: vec![
                "客户标识与按客户的用量拆分",
                "成本、毛利与任何金额",
                "上游账号明细与额度",
                "请求内容与 prompt",
            ],
        }
    }
}

impl Default for ViewerSession {
    fn default() -> Self {
        Self::new()
    }
}

/// 成功率：没有请求时返回 100.0 而不是 0.0。
///
/// 0.0 会被读成"全挂了"，而"没有流量"和"全部失败"是两件不同的事。
pub fn success_rate(total: u64, errors: u64) -> f64 {
    if total == 0 {
        return 100.0;
    }
    let ok = total.saturating_sub(errors);
    (ok as f64 / total as f64 * 1000.0).round() / 10.0
}

/// 把模型计数换算成占比，按占比降序、同占比按模型名升序（保证输出稳定）。
///
/// `total` 显式传入而不是对 counts 求和：调用方的窗口口径可能包含未归类到
/// 任何模型的请求，用求和会让占比合计虚高到 100%。
pub fn model_shares(counts: &[(String, u64)], total: u64) -> Vec<ViewerModelShare> {
    let mut out: Vec<ViewerModelShare> = counts
        .iter()
        .map(|(m, n)| ViewerModelShare {
            model: m.clone(),
            requests: *n,
            share_pct: if total == 0 {
                0.0
            } else {
                (*n as f64 / total as f64 * 1000.0).round() / 10.0
            },
        })
        .collect();
    out.sort_by(|a, b| {
        b.share_pct
            .partial_cmp(&a.share_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_traffic_reads_as_healthy_not_as_total_failure() {
        assert_eq!(success_rate(0, 0), 100.0);
    }

    #[test]
    fn success_rate_rounds_to_one_decimal() {
        assert_eq!(success_rate(1000, 182), 81.8);
        assert_eq!(success_rate(3, 1), 66.7);
        assert_eq!(success_rate(100, 0), 100.0);
        assert_eq!(success_rate(100, 100), 0.0);
    }

    #[test]
    fn errors_exceeding_total_do_not_underflow() {
        // 两个计数来自不同的窗口时可能对不齐，不该 panic 也不该给出负数。
        assert_eq!(success_rate(10, 99), 0.0);
    }

    #[test]
    fn model_shares_are_ordered_and_stable() {
        let counts = vec![
            ("b-model".to_string(), 30u64),
            ("a-model".to_string(), 30u64),
            ("c-model".to_string(), 40u64),
        ];
        let out = model_shares(&counts, 100);
        assert_eq!(out[0].model, "c-model");
        // 同占比按名字升序，两次调用结果一致。
        assert_eq!(out[1].model, "a-model");
        assert_eq!(out[2].model, "b-model");
        assert_eq!(out[0].share_pct, 40.0);
    }

    #[test]
    fn shares_use_the_given_total_not_the_sum() {
        // 窗口里有 100 个请求，但只有 60 个归到了具体模型。
        let counts = vec![("m".to_string(), 60u64)];
        assert_eq!(model_shares(&counts, 100)[0].share_pct, 60.0);
    }

    #[test]
    fn empty_total_does_not_divide_by_zero() {
        let counts = vec![("m".to_string(), 0u64)];
        assert_eq!(model_shares(&counts, 0)[0].share_pct, 0.0);
    }

    #[test]
    fn viewer_payload_carries_no_money_or_identity_fields() {
        // 这条测试的意义是：将来给 ViewerTraffic 加字段时，如果加的是金额或
        // 客户标识，这里会红。它守的是这个模块存在的理由。
        let t = ViewerTraffic {
            window: "today".to_string(),
            requests: 10,
            success_rate_pct: 100.0,
            tokens: 999,
            models: model_shares(&[("m".to_string(), 10)], 10),
            hourly_requests: vec![1, 2, 3],
            current_rpm: 4,
            active_credentials: 6,
            generated_at: "2026-08-31T00:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        for banned in [
            "credit", "cost", "usd", "margin", "price", "keyName", "email",
            "group", "prompt", "remaining", "quota",
        ] {
            assert!(
                !json.to_lowercase().contains(&banned.to_lowercase()),
                "只读响应里不该出现 {banned}：{json}"
            );
        }
    }

    #[test]
    fn session_states_what_is_hidden() {
        let s = ViewerSession::new();
        assert_eq!(s.role, "viewer");
        assert!(!s.redacted.is_empty(), "要明确告知隐去了什么");
    }
}
