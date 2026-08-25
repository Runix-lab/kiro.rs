//! 上下文窗口常量的自诊断守卫
//!
//! # 这个模块存在的原因
//!
//! 上游只回报"上下文用了百分之几"，我们靠 `pct × window / 100` 还原 token 数，
//! 而 `window` 来自 `get_context_window_size` 里的一张**硬编码白名单**。
//! 白名单漏掉一个 1M 模型，它就掉进 200K 兜底，该模型的整个 prompt 计量
//! （input + 缓存写 + 缓存读）全部缩小 5 倍。
//!
//! 这不是假设。2026-08-20~23 线上跑的 v0.7.4 漏了 `claude-opus-5`，实测：
//! - 10936 条 opus-5 记录**没有一条**超过 200,000（修好后 56% 超过）
//! - credits/Mtok 在修复部署那一刻跳了 4.49×，而三个对照模型只有 1.00~1.10×
//! - 折算下来，那段时间官方牌价被低估 $1,917~$2,197，直接影响对客账单
//!
//! 靠"新增模型时记得同步白名单"是防不住的——它已经漏过一次，而且漏了两周没人发现。
//!
//! # 指纹
//!
//! 窗口配小了，客户端塞进来的 prompt 会**持续**超过我们以为的上限，于是上游
//! 回报的百分比长期顶在 100%。窗口配对了的模型极少满上下文。所以
//! 「某模型 100% 占比异常高」就是"这个常量太小"的可靠信号——**与模型名无关**，
//! 下一个漏配的模型同样会被抓到。

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;

/// 判定前至少要看到多少次，避免小样本误报
const MIN_SAMPLES: u64 = 50;
/// 顶格占比超过多少就认为窗口常量可疑
const SUSPICIOUS_RATIO: f64 = 0.15;
/// 每累计多少次样本才允许再警告一次（避免刷屏）
const WARN_EVERY: u64 = 500;

#[derive(Default, Clone, Copy)]
struct ModelStat {
    total: u64,
    pinned: u64,
    warned_at: u64,
}

fn registry() -> &'static Mutex<HashMap<String, ModelStat>> {
    static REG: OnceLock<Mutex<HashMap<String, ModelStat>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 记录一次 `contextUsageEvent`，必要时发出告警。
///
/// `percentage` 是上游回报的上下文占用百分比，`window` 是我们当前假定的窗口大小。
/// 每个请求最多调用几次，锁竞争可以忽略。
pub fn observe(model: &str, percentage: f64, window: i32) {
    if !percentage.is_finite() {
        return;
    }
    let mut reg = registry().lock();
    let stat = reg.entry(model.to_string()).or_default();
    stat.total += 1;
    if percentage >= 100.0 {
        stat.pinned += 1;
    }

    if stat.total < MIN_SAMPLES {
        return;
    }
    // 刚够样本时留一条 INFO：否则"守卫没告警"和"守卫根本没在跑"长得一模一样，
    // 而这个守卫的全部价值就在于它平时不出声。
    if stat.total == MIN_SAMPLES {
        tracing::info!(
            model = %model,
            assumed_window = window,
            pinned = stat.pinned,
            total = stat.total,
            "上下文窗口守卫已开始监测该模型"
        );
    }
    let ratio = stat.pinned as f64 / stat.total as f64;
    if ratio < SUSPICIOUS_RATIO {
        return;
    }
    if stat.warned_at != 0 && stat.total - stat.warned_at < WARN_EVERY {
        return;
    }
    stat.warned_at = stat.total;
    let (total, pinned) = (stat.total, stat.pinned);
    drop(reg);

    tracing::warn!(
        model = %model,
        assumed_window = window,
        pinned,
        total,
        ratio = format!("{:.1}%", ratio * 100.0),
        "该模型的上下文占用长期顶在 100%，assumed_window 很可能配小了。\
         token 计量按 pct×window/100 还原，窗口偏小会等比例压低 input/缓存 token，\
         进而低估官方牌价、抬高账面折扣。请核对 get_context_window_size 里该模型的档位。"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset(model: &str) {
        registry().lock().remove(model);
    }

    /// 窗口配对的模型：偶尔满上下文，不该告警
    #[test]
    fn healthy_model_does_not_trip() {
        let m = "test-healthy";
        reset(m);
        for i in 0..200 {
            // 5% 顶格
            observe(m, if i % 20 == 0 { 100.0 } else { 42.0 }, 1_000_000);
        }
        let s = registry().lock().get(m).copied().unwrap();
        assert_eq!(s.warned_at, 0, "健康模型不该告警");
        reset(m);
    }

    /// 窗口配小的模型：持续顶格，必须被抓到
    #[test]
    fn undersized_window_is_flagged() {
        let m = "test-undersized";
        reset(m);
        for _ in 0..100 {
            observe(m, 100.0, 200_000);
        }
        let s = registry().lock().get(m).copied().unwrap();
        assert!(s.warned_at > 0, "顶格 100% 却没告警");
        assert_eq!(s.pinned, 100);
        reset(m);
    }

    /// 小样本不告警——刚上线的模型跑几次满上下文很正常
    #[test]
    fn small_samples_stay_quiet() {
        let m = "test-small";
        reset(m);
        for _ in 0..(MIN_SAMPLES - 1) {
            observe(m, 100.0, 200_000);
        }
        let s = registry().lock().get(m).copied().unwrap();
        assert_eq!(s.warned_at, 0, "样本不足就不该下结论");
        reset(m);
    }
}
