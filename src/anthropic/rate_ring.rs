//! 分钟级速率环：RPM / TPM 的采集层。
//!
//! 为什么不复用 `UsageAggregator`：那边最细只有小时/天两档，且 `upsert_bucket` 是
//! 线性查找 + 新桶全量排序 + `remove(0)` 头删的 O(n) 三连，`BucketEntry` 还挂着 5 个
//! （其中 2 个嵌套的）HashMap。744 个小时桶勉强扛得住，照搬到 1440 个分钟桶会直接
//! 变成写入热点，内存也会按 桶数 × Key 数 × 模型数 放大。详见 `RPM-TPM-ANALYSIS.md`。
//!
//! 因此这里另建一个刻意贫瘠的结构：
//! - 固定 120 个分钟桶（覆盖 2 小时，够看当前速率与近期峰值曲线）
//! - 每桶只放标量计数器，**不放任何 by_key / by_model / by_credential 维度**
//! - `minute % 120` 直接索引，O(1) 写入，无排序、无头删
//! - 全 `AtomicU64`，写入路径不取锁
//!
//! **与 trace 开关解耦**：只要请求走过 `UsageRecordHook` 就计数。traces.db 可以被用户
//! 从面板关掉，把核心运维指标架在一个可选功能上是脆弱设计。
//!
//! **双口径**：入口（每次外部请求 +1）与上游（每跳 +1）分开记。一次外部请求若重试 3 跳
//! 会在上游侧记 3 次，两者的比值就是重试放大倍数。混在一起既算不准流量也判不出上游压力。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// 环容量（分钟桶数）。120 = 2 小时。
pub const RING_MINUTES: usize = 120;

/// 单个分钟桶。
///
/// `minute` 兼作代次标记：槽位按 `minute % RING_MINUTES` 复用，读写时都要比对
/// `minute` 是否等于期望值，否则读到的是 2 小时前那一轮的残留。
#[derive(Debug, Default)]
struct Bucket {
    /// 该桶代表的 Unix 分钟数（epoch 秒 / 60）。0 表示从未写入。
    minute: AtomicU64,
    /// 入口请求数（外部请求，每次 record 一次）。
    ingress_calls: AtomicU64,
    /// 入口错误数。
    ingress_errors: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cache_write_tokens: AtomicU64,
    cache_read_tokens: AtomicU64,
    /// 上游跳数（每次 provider attempt 一次，含重试与故障转移）。
    upstream_attempts: AtomicU64,
    /// 上游失败跳数。
    upstream_failures: AtomicU64,
}

impl Bucket {
    /// 把桶归零并改写代次。调用方必须已确认该槽位属于旧代次。
    fn reset_to(&self, minute: u64) {
        self.ingress_calls.store(0, Ordering::Relaxed);
        self.ingress_errors.store(0, Ordering::Relaxed);
        self.input_tokens.store(0, Ordering::Relaxed);
        self.output_tokens.store(0, Ordering::Relaxed);
        self.cache_write_tokens.store(0, Ordering::Relaxed);
        self.cache_read_tokens.store(0, Ordering::Relaxed);
        self.upstream_attempts.store(0, Ordering::Relaxed);
        self.upstream_failures.store(0, Ordering::Relaxed);
        // 代次最后写：先清计数再宣告归属，避免读方看到新代次却读到旧计数。
        self.minute.store(minute, Ordering::Release);
    }

    fn snapshot(&self, minute: u64) -> MinuteSample {
        MinuteSample {
            minute,
            ingress_calls: self.ingress_calls.load(Ordering::Relaxed),
            ingress_errors: self.ingress_errors.load(Ordering::Relaxed),
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            cache_write_tokens: self.cache_write_tokens.load(Ordering::Relaxed),
            cache_read_tokens: self.cache_read_tokens.load(Ordering::Relaxed),
            upstream_attempts: self.upstream_attempts.load(Ordering::Relaxed),
            upstream_failures: self.upstream_failures.load(Ordering::Relaxed),
        }
    }
}

/// 一个分钟桶的读出结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinuteSample {
    /// Unix 分钟数（epoch 秒 / 60）。
    pub minute: u64,
    pub ingress_calls: u64,
    pub ingress_errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub upstream_attempts: u64,
    pub upstream_failures: u64,
}

impl MinuteSample {
    /// TPM 全口径：四类 token 全算，含缓存读取。
    ///
    /// 与 `billable_tokens` 可以差几十倍——缓存读取往往是输入的大头。对外暴露必须
    /// 说清是哪个口径，否则数字无法解释。
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens + self.cache_read_tokens
    }

    /// TPM 计费口径：不含缓存读取（缓存命中部分通常不按全价计）。
    pub fn billable_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens
    }
}

/// 分钟级速率环。
pub struct RateRing {
    buckets: Vec<Bucket>,
}

impl Default for RateRing {
    fn default() -> Self {
        Self::new()
    }
}

impl RateRing {
    pub fn new() -> Self {
        Self {
            buckets: (0..RING_MINUTES).map(|_| Bucket::default()).collect(),
        }
    }

    fn now_minute() -> u64 {
        chrono::Utc::now().timestamp().max(0) as u64 / 60
    }

    /// 取当前分钟对应的桶，必要时先翻代（归零复用）。
    fn bucket_for(&self, minute: u64) -> &Bucket {
        let slot = &self.buckets[(minute % RING_MINUTES as u64) as usize];
        if slot.minute.load(Ordering::Acquire) != minute {
            // 槽位属于旧代次（或从未用过）→ 归零后据为己有。
            // 并发下多个线程可能同时走到这里，重复 reset 是幂等的；代价是极端情况下
            // 丢失同一分钟内极早的几次计数，对速率指标可以接受，换来的是完全无锁。
            slot.reset_to(minute);
        }
        slot
    }

    /// 记一次外部请求（入口口径）。
    ///
    /// 只要请求走过 `UsageRecordHook` 就会调到这里，与 trace 开关无关。
    pub fn record_ingress(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
        is_error: bool,
    ) {
        let b = self.bucket_for(Self::now_minute());
        b.ingress_calls.fetch_add(1, Ordering::Relaxed);
        if is_error {
            b.ingress_errors.fetch_add(1, Ordering::Relaxed);
        }
        b.input_tokens.fetch_add(input_tokens, Ordering::Relaxed);
        b.output_tokens.fetch_add(output_tokens, Ordering::Relaxed);
        b.cache_write_tokens
            .fetch_add(cache_write_tokens, Ordering::Relaxed);
        b.cache_read_tokens
            .fetch_add(cache_read_tokens, Ordering::Relaxed);
    }

    /// 记一跳上游调用（上游口径）。一次外部请求重试 N 次就调 N 次。
    pub fn record_upstream_attempt(&self, succeeded: bool) {
        let b = self.bucket_for(Self::now_minute());
        b.upstream_attempts.fetch_add(1, Ordering::Relaxed);
        if !succeeded {
            b.upstream_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 读出最近 `n` 个分钟桶，按时间升序，缺口补零。
    ///
    /// 补零很重要：前端画曲线需要连续的时间轴，没有流量的分钟必须是 0 而不是缺点。
    pub fn recent(&self, n: usize) -> Vec<MinuteSample> {
        let n = n.min(RING_MINUTES);
        let now = Self::now_minute();
        let mut out = Vec::with_capacity(n);
        // 从最老到最新
        for back in (0..n as u64).rev() {
            let minute = now.saturating_sub(back);
            let slot = &self.buckets[(minute % RING_MINUTES as u64) as usize];
            if slot.minute.load(Ordering::Acquire) == minute {
                out.push(slot.snapshot(minute));
            } else {
                out.push(MinuteSample {
                    minute,
                    ..Default::default()
                });
            }
        }
        out
    }

    /// 组装对外快照。
    ///
    /// 速率取**上一个完整分钟**而不是当前分钟：当前分钟还在累加，读出来是个偏低的
    /// 半成品，会让面板上的 RPM 看起来总比实际低。
    pub fn snapshot(&self) -> RateSnapshot {
        let now = Self::now_minute();
        let last_complete = now.saturating_sub(1);
        let series = self.recent(RING_MINUTES);
        let current = series
            .iter()
            .find(|s| s.minute == last_complete)
            .copied()
            .unwrap_or_default();

        let peak_ingress_rpm = series.iter().map(|s| s.ingress_calls).max().unwrap_or(0);
        let peak_upstream_rpm = series
            .iter()
            .map(|s| s.upstream_attempts)
            .max()
            .unwrap_or(0);
        let peak_tpm_total = series.iter().map(|s| s.total_tokens()).max().unwrap_or(0);
        let peak_tpm_billable = series
            .iter()
            .map(|s| s.billable_tokens())
            .max()
            .unwrap_or(0);

        // 重试放大倍数：上游跳数 / 入口请求数。1.0 表示零重试。
        let amplification = if current.ingress_calls > 0 {
            current.upstream_attempts as f64 / current.ingress_calls as f64
        } else {
            0.0
        };

        RateSnapshot {
            minute: last_complete,
            ingress_rpm: current.ingress_calls,
            ingress_errors: current.ingress_errors,
            upstream_rpm: current.upstream_attempts,
            upstream_failures: current.upstream_failures,
            tpm_total: current.total_tokens(),
            tpm_billable: current.billable_tokens(),
            peak_ingress_rpm,
            peak_upstream_rpm,
            peak_tpm_total,
            peak_tpm_billable,
            retry_amplification: amplification,
            window_minutes: RING_MINUTES,
            series,
        }
    }
}

/// 对外速率快照。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateSnapshot {
    /// 这些速率对应的 Unix 分钟（上一个完整分钟）。
    pub minute: u64,
    /// 入口 RPM：外部请求数/分钟。这是"真实流量"。
    pub ingress_rpm: u64,
    pub ingress_errors: u64,
    /// 上游 RPM：provider 跳数/分钟，含重试。这是"上游压力"。
    pub upstream_rpm: u64,
    pub upstream_failures: u64,
    /// TPM 全口径（含缓存读取）。
    pub tpm_total: u64,
    /// TPM 计费口径（不含缓存读取）。
    pub tpm_billable: u64,
    pub peak_ingress_rpm: u64,
    pub peak_upstream_rpm: u64,
    pub peak_tpm_total: u64,
    pub peak_tpm_billable: u64,
    /// 上游跳数 / 入口请求数。1.0 = 零重试，2.0 = 平均每请求打了两跳。
    pub retry_amplification: f64,
    /// 环覆盖的分钟数。
    pub window_minutes: usize,
    /// 逐分钟序列，时间升序、缺口补零。
    pub series: Vec<MinuteSample>,
}

/// 共享速率环。
pub type SharedRateRing = Arc<RateRing>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 直接摆桶，避免测试依赖真实时钟。
    fn seed(ring: &RateRing, minute: u64, calls: u64, attempts: u64, tokens: u64) {
        let slot = &ring.buckets[(minute % RING_MINUTES as u64) as usize];
        slot.reset_to(minute);
        slot.ingress_calls.store(calls, Ordering::Relaxed);
        slot.upstream_attempts.store(attempts, Ordering::Relaxed);
        slot.input_tokens.store(tokens, Ordering::Relaxed);
    }

    #[test]
    fn ingress_and_upstream_are_counted_separately() {
        let ring = RateRing::new();
        ring.record_ingress(10, 20, 3, 400, false);
        // 同一个外部请求打了三跳上游
        ring.record_upstream_attempt(false);
        ring.record_upstream_attempt(false);
        ring.record_upstream_attempt(true);

        let now = RateRing::now_minute();
        let s = ring
            .recent(RING_MINUTES)
            .into_iter()
            .find(|s| s.minute == now)
            .expect("当前分钟应在序列内");

        assert_eq!(s.ingress_calls, 1, "入口只算一次外部请求");
        assert_eq!(s.upstream_attempts, 3, "上游按跳数算");
        assert_eq!(s.upstream_failures, 2);
        assert_eq!(s.input_tokens, 10);
        assert_eq!(s.cache_read_tokens, 400);
    }

    #[test]
    fn tpm_distinguishes_total_from_billable() {
        let s = MinuteSample {
            input_tokens: 100,
            output_tokens: 50,
            cache_write_tokens: 20,
            cache_read_tokens: 9000,
            ..Default::default()
        };
        // 两个口径差近 50 倍——这正是必须分开暴露的理由
        assert_eq!(s.total_tokens(), 9170);
        assert_eq!(s.billable_tokens(), 170);
    }

    #[test]
    fn errors_are_tracked_on_both_sides() {
        let ring = RateRing::new();
        ring.record_ingress(1, 0, 0, 0, true);
        ring.record_upstream_attempt(false);

        let now = RateRing::now_minute();
        let s = ring
            .recent(RING_MINUTES)
            .into_iter()
            .find(|s| s.minute == now)
            .unwrap();
        assert_eq!(s.ingress_errors, 1);
        assert_eq!(s.upstream_failures, 1);
    }

    #[test]
    fn stale_slot_does_not_leak_into_the_new_generation() {
        let ring = RateRing::new();
        // 摆一个恰好 RING_MINUTES 之前的桶：它与当前分钟共用同一个槽位
        let now = RateRing::now_minute();
        let stale = now - RING_MINUTES as u64;
        seed(&ring, stale, 999, 999, 999_999);

        // 当前分钟写入时必须先翻代，不能把 999 累加上去
        ring.record_ingress(5, 0, 0, 0, false);
        let s = ring
            .recent(RING_MINUTES)
            .into_iter()
            .find(|s| s.minute == now)
            .unwrap();
        assert_eq!(s.ingress_calls, 1, "旧代次计数必须被清掉");
        assert_eq!(s.input_tokens, 5);
    }

    #[test]
    fn recent_fills_gaps_with_zeros_in_ascending_order() {
        let ring = RateRing::new();
        let now = RateRing::now_minute();
        seed(&ring, now - 5, 7, 9, 100);

        let series = ring.recent(10);
        assert_eq!(series.len(), 10);
        // 时间升序
        for w in series.windows(2) {
            assert_eq!(w[1].minute, w[0].minute + 1);
        }
        let hit = series.iter().find(|s| s.minute == now - 5).unwrap();
        assert_eq!(hit.ingress_calls, 7);
        // 没写过的分钟是补零而不是缺项
        let gap = series.iter().find(|s| s.minute == now - 4).unwrap();
        assert_eq!(gap.ingress_calls, 0);
    }

    #[test]
    fn snapshot_reads_the_last_complete_minute_not_the_partial_one() {
        let ring = RateRing::new();
        let now = RateRing::now_minute();
        seed(&ring, now - 1, 42, 84, 1000); // 上一个完整分钟
        seed(&ring, now, 3, 3, 10); // 当前分钟仍在累加

        let snap = ring.snapshot();
        assert_eq!(snap.minute, now - 1);
        assert_eq!(snap.ingress_rpm, 42, "取完整分钟，不取半成品");
        assert_eq!(snap.upstream_rpm, 84);
        // 放大倍数 = 84/42
        assert!((snap.retry_amplification - 2.0).abs() < 1e-9);
    }

    #[test]
    fn peaks_scan_the_whole_ring() {
        let ring = RateRing::new();
        let now = RateRing::now_minute();
        seed(&ring, now - 1, 5, 5, 10);
        seed(&ring, now - 30, 500, 900, 77_000);

        let snap = ring.snapshot();
        assert_eq!(snap.ingress_rpm, 5, "当前速率仍是上一分钟");
        assert_eq!(snap.peak_ingress_rpm, 500, "峰值扫全环");
        assert_eq!(snap.peak_upstream_rpm, 900);
        assert_eq!(snap.peak_tpm_total, 77_000);
    }

    #[test]
    fn amplification_is_zero_when_no_ingress_traffic() {
        let ring = RateRing::new();
        let snap = ring.snapshot();
        assert_eq!(snap.ingress_rpm, 0);
        assert_eq!(snap.retry_amplification, 0.0, "不能除零");
    }
}
