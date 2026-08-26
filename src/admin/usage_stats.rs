//! 请求用量记录 + 时序聚合
//!
//! 记录每次 `/v1/messages` 请求的 token 消耗与命中信息：
//! - 落盘：`usage_log.YYYY-MM-DD.jsonl`，每行一条 [`UsageRecord`]，按本地日期滚动
//! - 内存：[`UsageAggregator`] 维护近 31 天的小时桶 + 近 31 天的天桶，按需查询
//!
//! 启动时扫描历史 JSONL 文件重建聚合，保证重启后趋势图不丢数据。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// JSONL 文件保留天数
const RETENTION_DAYS: i64 = 31;
/// 小时桶数量（31 天）
const HOUR_BUCKETS: usize = 24 * 31;
/// 天桶数量（31 天）
const DAY_BUCKETS: usize = 31;

/// 单次请求的用量记录（与 JSONL 一行一一对应）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    /// 请求结束时间（RFC3339）
    pub ts: String,
    /// 客户端 Key id；0 表示用 master apiKey 调用
    pub key_id: u64,
    /// 实际命中的上游凭据 id；0 表示请求未走到上游
    pub credential_id: u64,
    /// 模型名（请求里声明的，可能含 -thinking 后缀）
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 上游 meteringEvent.usage 上报的 credit 计费量（浮点）
    #[serde(default)]
    pub credits: f64,
    /// 端到端耗时（毫秒）
    #[serde(default)]
    pub duration_ms: u64,
    /// "success" 或 "error"
    pub status: String,
}

/// 按天 rotate 的 JSONL writer
pub struct UsageRecorder {
    inner: Mutex<RecorderState>,
    dir: PathBuf,
    /// 保留天数（运行时可改），cleanup_old_logs 时读取。
    retention_days: std::sync::atomic::AtomicI64,
}

struct RecorderState {
    /// 当前打开的 writer 与对应日期
    current_date: Option<NaiveDate>,
    writer: Option<BufWriter<File>>,
}

impl UsageRecorder {
    /// 指定初始保留天数构造
    pub fn with_retention(dir: PathBuf, retention_days: i64) -> Self {
        // 兜底：调用方传入空路径时归一为 "."，避免 join 出无目录前缀的路径导致写入 CWD
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("创建 usage_log 目录失败 {}: {}", dir.display(), e);
            }
        }
        Self {
            inner: Mutex::new(RecorderState {
                current_date: None,
                writer: None,
            }),
            dir,
            retention_days: std::sync::atomic::AtomicI64::new(retention_days.max(1)),
        }
    }

    fn log_path(&self, date: NaiveDate) -> PathBuf {
        self.dir
            .join(format!("usage_log.{}.jsonl", date.format("%Y-%m-%d")))
    }

    /// 同步写入一条记录。失败仅 warn，不阻塞请求。
    pub fn record(&self, rec: &UsageRecord) {
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("usage_log 序列化失败: {}", e);
                return;
            }
        };
        let today = Local::now().date_naive();
        let mut state = self.inner.lock();
        if state.current_date != Some(today) || state.writer.is_none() {
            // 切换到当日文件
            let path = self.log_path(today);
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => {
                    state.writer = Some(BufWriter::new(file));
                    state.current_date = Some(today);
                }
                Err(e) => {
                    tracing::warn!("打开 usage_log {} 失败: {}", path.display(), e);
                    return;
                }
            }
        }
        if let Some(w) = state.writer.as_mut() {
            if let Err(e) = writeln!(w, "{}", line) {
                tracing::warn!("写入 usage_log 失败: {}", e);
                return;
            }
            // 立即 flush，保证崩溃时不丢失最近一条
            let _ = w.flush();
        }
    }

    /// 获取保留天数
    pub fn retention_days(&self) -> i64 {
        self.retention_days
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 设置保留天数（>=1）
    pub fn set_retention_days(&self, days: i64) {
        self.retention_days
            .store(days.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// 清理超过保留期的旧文件
    pub fn cleanup_old_logs(&self) {
        let cutoff = Local::now().date_naive() - Duration::days(self.retention_days());
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(it) => it,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Some(date) = parse_usage_log_filename(&name) {
                if date < cutoff {
                    let _ = std::fs::remove_file(entry.path());
                    tracing::info!("已清理过期 usage_log: {}", name);
                }
            }
        }
    }
}

impl UsageRecorder {
    /// 日志目录（导出对账明细时按日期定位文件用）
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 账单里能接受的 credits 值。负数 / NaN / 无穷只可能来自损坏的行，一律计 0。
///
/// **总账和明细必须共用这一个函数**——一边清洗一边不清洗，客户把明细加起来
/// 就对不上总账，而"总账与明细由构造保证一致"是这套对账的立身之本。
pub fn sane_credits(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

/// 结算时区：北京时间。中国 1991 年后不再有夏令时，固定 +08:00 与 Asia/Shanghai
/// 在任何我们会结算的日期上都完全等价，所以用固定偏移而不引入 tzdata 依赖。
pub const SETTLEMENT_OFFSET_SECS: i32 = 8 * 3600;

/// 结算时区（北京，+08:00），明细展示与账期归属都用它。
pub fn settlement_tz() -> chrono::FixedOffset {
    settlement_offset()
}

fn settlement_offset() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(SETTLEMENT_OFFSET_SECS).expect("+08:00 是合法偏移")
}

/// 逐行读取某个日期区间内某个 Key 的用量记录，交给 `sink` 处理。
///
/// `start` / `end_exclusive` 是**北京时间的自然日**——月结按北京时间算，这是账单口径。
/// 但日志文件是按服务器本地日期滚动的（生产上是 UTC），两者差 8 小时：北京 8 月 1 日
/// 00:00–08:00 的请求落在文件名为 7 月 31 日的文件里。所以这里**多扫前后各一天的文件，
/// 再逐条按记录自身的 `ts` 换算到北京时间过滤**——文件名只用来找文件，账期归属只认
/// 时间戳。少了这一层，月头月尾各会错进/错出 8 小时的流量。
///
/// 对账导出可能有几万行，全部读进内存再序列化没有必要——按文件、按行流式过一遍即可。
/// 单个文件读失败只跳过该文件：导出缺一天也比整个对账单失败强，缺口由调用方在
/// 响应里说明。
pub fn scan_usage_records(
    dir: &Path,
    start: NaiveDate,
    end_exclusive: NaiveDate,
    key_id: Option<u64>,
    sink: impl FnMut(&UsageRecord),
) -> ScanOutcome {
    let mut sink = sink;
    let tz = settlement_offset();
    let Some(window_start) = start.and_hms_opt(0, 0, 0).map(|t| t.and_local_timezone(tz)) else {
        return ScanOutcome::default();
    };
    let Some(window_end) = end_exclusive
        .and_hms_opt(0, 0, 0)
        .map(|t| t.and_local_timezone(tz))
    else {
        return ScanOutcome::default();
    };
    let (Some(window_start), Some(window_end)) = (window_start.single(), window_end.single())
    else {
        return ScanOutcome::default();
    };

    let mut scanned = 0u64;
    let mut malformed = 0u64;
    let mut missing_days: Vec<String> = Vec::new();
    // 前后各多扫一天，覆盖时区偏移把记录挤到相邻文件里的情况。
    let mut day = start.pred_opt().unwrap_or(start);
    let scan_end = end_exclusive.succ_opt().unwrap_or(end_exclusive);
    let today = Local::now().date_naive();

    while day < scan_end {
        let path = dir.join(format!("usage_log.{}.jsonl", day.format("%Y-%m-%d")));
        match File::open(&path) {
            Ok(f) => {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(rec) = serde_json::from_str::<UsageRecord>(line) else {
                        // 半截行 / 撕裂行 / 溢出数值。金额未知且不可知，
                        // 所以必须计数报出去，不能装作这个月是干净的。
                        malformed += 1;
                        continue;
                    };
                    if key_id.is_some_and(|k| rec.key_id != k) {
                        continue;
                    }
                    // 账期归属只认记录自身的时间戳。解析不出来的行宁可漏掉也不能
                    // 错记到别的月份——对账单里多一条不属于本期的记录比少一条更难解释。
                    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&rec.ts) else {
                        malformed += 1;
                        continue;
                    };
                    if ts < window_start || ts >= window_end {
                        continue;
                    }
                    scanned += 1;
                    sink(&rec);
                }
            }
            // 文件不存在通常就是那天没有流量，不是错误；但要如实报给调用方，
            // 免得"那天没数据"被当成"那天没消费"。未来的日期不算缺口。
            Err(_) if day <= today => missing_days.push(day.format("%Y-%m-%d").to_string()),
            Err(_) => {}
        }
        day = match day.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }
    ScanOutcome {
        scanned,
        missing_days,
        malformed,
    }
}

/// 一次扫描的结果。`missing_days` / `malformed` 都必须一路透到界面上——
/// 它们是"这张账单可能不完整"的唯一信号。
#[derive(Debug, Default, Clone)]
pub struct ScanOutcome {
    /// 落在账期内、通过过滤的记录数
    pub scanned: u64,
    /// 账期内没有日志文件的日期
    pub missing_days: Vec<String>,
    /// 解析失败的行数
    pub malformed: u64,
}

/// 一个 Key 的定价建议。
///
/// # 口径
///
/// 折扣口径下：应收 = 官方牌价 × 折扣系数，成本与折扣无关。所以
///
/// ```text
/// 毛利率 = (应收 − 成本) / 应收 = 1 − 成本 / (官方 × 折扣)
/// ```
///
/// 反解出达到目标毛利率所需的折扣：
///
/// ```text
/// 折扣 = 成本 / (官方 × (1 − 目标毛利率)) = 保本线 / (1 − 目标毛利率)
/// ```
///
/// **保本线 = 成本 ÷ 官方牌价**，是这个客户的流量结构决定的下限，与商务谈判无关。
/// 折扣系数低于保本线，无论卖多少都在亏。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingAdvice {
    pub key_id: u64,
    pub calls: u64,
    pub cost_usd: f64,
    /// 官方牌价合计；全部模型未配价时为 `None`，此时无法给建议
    pub official_usd: Option<f64>,
    /// 当前折扣系数
    pub current_discount: Option<f64>,
    /// 当前对客单价（与折扣二选一）
    pub current_price_per_credit: Option<f64>,
    /// 保本线 = 成本 ÷ 官方牌价
    pub breakeven_discount: Option<f64>,
    /// 当前毛利率（按当前折扣算）
    pub current_margin_rate: Option<f64>,
    /// 达到目标毛利率所需的折扣系数
    pub recommended_discount: Option<f64>,
    /// 按建议折扣计算的应收
    pub recommended_receivable_usd: Option<f64>,
    /// 相对当前应收的变化额
    pub receivable_delta_usd: Option<f64>,
    /// 人话结论
    pub verdict: &'static str,
}

/// 按目标毛利率给出每个 Key 的折扣建议。
///
/// 无法给建议的情况一律返回 `recommended_discount: None` 并在 `verdict` 说明原因，
/// 绝不用猜的值填上——定价建议被当成结论直接套用的风险太高。
pub fn pricing_advice(rows: &[KeyBillingRow], target_margin_rate: f64) -> Vec<PricingAdvice> {
    // 目标毛利率必须在 [0, 1)：等于 1 意味着成本为零，反解会除零炸上天
    let m = target_margin_rate.clamp(0.0, 0.95);
    rows.iter()
        .map(|r| {
            let official = r.official_usd.filter(|o| *o > 0.0);
            let breakeven = official.map(|o| r.credit_usd / o);
            let recommended = breakeven.map(|b| b / (1.0 - m));
            let rec_receivable = official.zip(recommended).map(|(o, d)| o * d);
            let current_margin = r
                .receivable_usd
                .filter(|v| *v > 0.0)
                .map(|v| (v - r.credit_usd) / v);

            let verdict = match (official, r.billing_discount, r.price_per_credit) {
                (None, _, _) => "该 Key 的模型全部未配官方价，算不出保本线",
                (_, None, None) => "未设置对客定价，先定价才能谈毛利",
                (_, _, Some(_)) => "走单价口径，毛利与折扣无关，建议仅供参考",
                (Some(_), Some(d), None) => {
                    let b = breakeven.unwrap_or(0.0);
                    if d < b {
                        "🔴 当前折扣低于保本线，每笔都在亏"
                    } else if recommended.is_some_and(|rec| d < rec) {
                        "当前有毛利但低于目标，建议上调"
                    } else {
                        "已达到或超过目标毛利率"
                    }
                }
            };

            PricingAdvice {
                key_id: r.key_id,
                calls: r.calls,
                cost_usd: r.credit_usd,
                official_usd: r.official_usd,
                current_discount: r.billing_discount,
                current_price_per_credit: r.price_per_credit,
                breakeven_discount: breakeven,
                current_margin_rate: current_margin,
                recommended_discount: recommended,
                recommended_receivable_usd: rec_receivable,
                receivable_delta_usd: rec_receivable
                    .zip(r.receivable_usd)
                    .map(|(rec, cur)| rec - cur),
                verdict,
            }
        })
        .collect()
}

/// 按北京自然日区间，从用量日志直接算出每个 Key 的月结账目。
///
/// 为什么不用内存聚合器（[`UsageAggregator::query_billing`]）：聚合器只保留 31 个
/// 日桶，9 月 5 日结 8 月的账时 8 月 1–4 日已经掉出窗口，会被静默算成零消费；而且
/// 它和导出明细是两个数据源，客户把明细加起来对不上总账就没法对账了。
/// 这里和 [`scan_usage_records`] 走同一条流式路径，总账与明细**由构造保证一致**。
///
/// 返回 `(账目行, 扫描结果)`。扫描结果里的缺失日期与坏行数必须一路透传到界面——
/// "那天没日志"和"那天没消费"是两回事，月结时必须能分辨。
pub fn billing_from_logs(
    dir: &Path,
    start: NaiveDate,
    end_exclusive: NaiveDate,
    pricing: &crate::common::pricing::PricingTable,
    pricing_of: &dyn Fn(u64) -> (Option<f64>, Option<f64>),
) -> (Vec<KeyBillingRow>, ScanOutcome) {
    #[derive(Default)]
    struct Acc {
        calls: u64,
        errors: u64,
        upstream_calls: u64,
        error_credits: f64,
        input: u64,
        output: u64,
        cache_write: u64,
        cache_read: u64,
        credits: f64,
    }

    let mut per_key: HashMap<u64, Acc> = HashMap::new();
    // 只有成功的请求参与官方牌价换算：失败请求的 token 是本地估算的、上游
    // credits 为 0（我方无成本），按牌价打折收钱等于凭空多收——而客户对账时
    // 一眼就能看到那行状态是 error。
    let mut per_key_model: HashMap<(u64, String), Acc> = HashMap::new();

    let outcome = scan_usage_records(dir, start, end_exclusive, None, |rec| {
        let success = rec.status == "success";
        let acc = per_key.entry(rec.key_id).or_default();
        acc.calls += 1;
        if !success {
            acc.errors += 1;
            acc.error_credits += sane_credits(rec.credits);
        }
        acc.credits += sane_credits(rec.credits);
        acc.input += rec.input_tokens;
        acc.output += rec.output_tokens;
        acc.cache_write += rec.cache_creation_tokens;
        acc.cache_read += rec.cache_read_tokens;

        if success && rec.credential_id != 0 {
            acc.upstream_calls += 1;
        }
        if success {
            // 历史补偿：2026-08-24 窗口修复之前，opus-5 的 token 三项被等比压小
            // 约 5 倍（详见 pricing::historical_token_scale）。官方牌价由 token
            // 换算而来，不还原就等于按 1/5 的用量给客户开票。
            //
            // 只乘 token 三项：output 不受窗口影响（它不走 contextUsageEvent），
            // credits 是上游真值更不能动——动了成本就假了。
            let scale = chrono::DateTime::parse_from_rfc3339(&rec.ts)
                .map(|t| {
                    crate::common::pricing::historical_token_scale(&rec.model, t.timestamp())
                })
                .unwrap_or(1.0);
            let up = |v: u64| if scale == 1.0 { v } else { (v as f64 * scale) as u64 };

            // websearch 兜底曾把整个 prompt 记成新鲜输入（缓存两项硬写 0），
            // 而新鲜输入的牌价是缓存读取的 10 倍——方向是我方多收客户的钱。
            // 根因已修，但已落盘的历史改不动，只能在读取侧按估算比例还原。
            let ts_secs = chrono::DateTime::parse_from_rfc3339(&rec.ts)
                .map(|t| t.timestamp())
                .unwrap_or(i64::MAX);
            let (input_tokens, cache_read_tokens) =
                crate::common::pricing::websearch_cache_correction(
                    &rec.model,
                    ts_secs,
                    rec.input_tokens,
                    rec.cache_creation_tokens,
                    rec.cache_read_tokens,
                );

            let m = per_key_model
                .entry((rec.key_id, rec.model.clone()))
                .or_default();
            m.calls += 1;
            m.credits += sane_credits(rec.credits);
            m.input += up(input_tokens);
            m.output += rec.output_tokens;
            m.cache_write += up(rec.cache_creation_tokens);
            m.cache_read += up(cache_read_tokens);
        }
    });

    // 官方牌价按模型算，再按 Key 汇总。
    // 同时记下**落在未配价模型上的量**：只要该 Key 还有别的模型配了价，
    // official_usd 就是 Some(部分和)，看起来完全正常——这些请求会静默地
    // 从应收里消失。必须单独计出来报警。
    let mut official: HashMap<u64, (f64, bool)> = HashMap::new();
    let mut unpriced: HashMap<u64, (u64, f64)> = HashMap::new();
    for ((key_id, model), a) in &per_key_model {
        match pricing.official_usd(model, a.input, a.output, a.cache_write, a.cache_read) {
            Some(usd) => {
                let e = official.entry(*key_id).or_insert((0.0, false));
                e.0 += usd;
                e.1 = true;
            }
            None => {
                let e = unpriced.entry(*key_id).or_insert((0, 0.0));
                e.0 += a.calls;
                e.1 += a.credits;
            }
        }
    }

    let mut rows: Vec<KeyBillingRow> = per_key
        .into_iter()
        .map(|(key_id, s)| {
            let official_usd = official
                .get(&key_id)
                .and_then(|(sum, any)| any.then_some(*sum));
            let (billing_discount, price_per_credit) = pricing_of(key_id);
            let credit_usd = pricing.credit_usd(s.credits);
            let (receivable_usd, receivable_basis) = match price_per_credit {
                Some(p) => (Some(s.credits * p), Some("perCredit")),
                None => match (official_usd, billing_discount) {
                    (Some(o), Some(d)) => (Some(o * d), Some("discount")),
                    _ => (None, None),
                },
            };
            let (unpriced_calls, unpriced_credits) =
                unpriced.get(&key_id).copied().unwrap_or((0, 0.0));
            KeyBillingRow {
                key_id,
                calls: s.calls,
                errors: s.errors,
                upstream_calls: s.upstream_calls,
                error_credits: pricing.credit_usd(s.error_credits),
                unpriced_calls,
                unpriced_credits,
                input_tokens: s.input,
                output_tokens: s.output,
                cache_creation_tokens: s.cache_write,
                cache_read_tokens: s.cache_read,
                credits: s.credits,
                credit_usd,
                official_usd,
                billing_discount,
                price_per_credit,
                receivable_usd,
                receivable_basis,
                margin_usd: receivable_usd.map(|r| r - credit_usd),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.credit_usd.total_cmp(&a.credit_usd));
    (rows, outcome)
}

fn parse_usage_log_filename(name: &str) -> Option<NaiveDate> {
    // 形如 usage_log.2026-05-22.jsonl
    let body = name.strip_prefix("usage_log.")?.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(body, "%Y-%m-%d").ok()
}

/// 单个时间桶的统计
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub calls: u64,
    pub errors: u64,
    pub credits: f64,
}

impl BucketStats {
    fn add(&mut self, rec: &UsageRecord) {
        self.input_tokens += rec.input_tokens;
        self.output_tokens += rec.output_tokens;
        self.cache_creation_tokens += rec.cache_creation_tokens;
        self.cache_read_tokens += rec.cache_read_tokens;
        self.credits += rec.credits;
        self.calls += 1;
        if rec.status != "success" {
            self.errors += 1;
        }
    }

    /// 把另一个 stats 累加到自己上（用于 group 过滤后重新汇总）
    fn add_stats(&mut self, other: &BucketStats) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.credits += other.credits;
        self.calls += other.calls;
        self.errors += other.errors;
    }
}

/// 单个时间桶含分组数据
#[derive(Debug, Default, Clone)]
struct BucketEntry {
    /// 桶起始时间戳（小时桶为整点 Unix 秒；天桶为本地 0 点 Unix 秒）
    ts: i64,
    overall: BucketStats,
    by_key: HashMap<u64, BucketStats>,
    by_model: HashMap<String, BucketStats>,
    by_credential: HashMap<u64, BucketStats>,
    by_key_model: HashMap<u64, HashMap<String, BucketStats>>,
    by_key_credential: HashMap<u64, HashMap<u64, BucketStats>>,
}

/// 时间维度聚合器
pub struct UsageAggregator {
    inner: parking_lot::RwLock<AggregatorInner>,
}

struct AggregatorInner {
    /// 小时桶（环形数组按桶起始时间索引），最近 31 天
    hour_buckets: Vec<BucketEntry>,
    /// 天桶（按本地日期），最近 31 天
    day_buckets: Vec<BucketEntry>,
}

/// 预设聚合查询时间范围
#[derive(Debug, Clone, Copy)]
pub enum Range {
    /// 小时级档位。桶是按小时聚合的，所以 1h/3h/6h 拿到的是 1~6 个桶——
    /// 分钟级细节看速率环（`/stats/rate?minutes=N`），这里只保证**窗口口径一致**：
    /// 上面选了几小时，所有面板就都只算这几小时，不会退化成"今天"。
    Last1h,
    Last3h,
    Last6h,
    Last24h,
    Last7d,
    Last30d,
}

impl Range {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1h" => Some(Range::Last1h),
            "3h" => Some(Range::Last3h),
            "6h" => Some(Range::Last6h),
            "24h" => Some(Range::Last24h),
            "7d" => Some(Range::Last7d),
            "30d" => Some(Range::Last30d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsGranularity {
    Hour,
    Day,
}

impl StatsGranularity {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "hour" => Some(StatsGranularity::Hour),
            "day" => Some(StatsGranularity::Day),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StatsQueryWindow {
    pub start_ts: i64,
    pub end_ts: i64,
    pub granularity: StatsGranularity,
}

impl StatsQueryWindow {
    pub fn preset(range: Range, granularity: StatsGranularity) -> Self {
        let now = Utc::now().timestamp();
        let start_ts = match range {
            Range::Last1h => now - 3600,
            Range::Last3h => now - 3 * 3600,
            Range::Last6h => now - 6 * 3600,
            Range::Last24h => now - 24 * 3600,
            Range::Last7d => now - 7 * 24 * 3600,
            Range::Last30d => now - 30 * 24 * 3600,
        };
        Self {
            start_ts,
            end_ts: now,
            granularity,
        }
    }
}

/// 时序点（导出给前端）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSeriesPoint {
    /// 桶起始时间（RFC3339）
    pub ts: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub calls: u64,
    pub errors: u64,
    pub credits: f64,
    /// 实付成本：credits × creditUsdRate。
    pub credit_usd: f64,
    /// 官方牌价成本（仅已配价模型计入）。`None` 表示该桶无法按模型拆分
    /// （分组筛选路径没有 凭据×模型 维度）或桶内没有任何已配价模型。
    pub official_usd: Option<f64>,
}

/// 模型分布
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDistribution {
    pub model: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub errors: u64,
    pub credits: f64,
    /// 实付成本：credits × creditUsdRate。
    pub credit_usd: f64,
    /// 官方牌价成本。`None` = 该模型未配价（≠ 免费）。
    pub official_usd: Option<f64>,
    /// 折扣比 = 实付 ÷ 官方（0.14 即 1.4 折）。未配价时为 `None`。
    pub discount_ratio: Option<f64>,
}

/// 单个入口 Key 的账单行。
///
/// 三个金额是三件不同的事，不能互相替代：
/// - `credit_usd` **成本**：付给上游的钱（credits × 汇率），权威、无歧义
/// - `official_usd` **官方牌价**：同样这批 token 直连官方要花多少，是定价的锚
/// - `receivable_usd` **应收**：官方牌价 × 该 Key 的对客折扣；未配折扣时为 `None`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyBillingRow {
    pub key_id: u64,
    pub calls: u64,
    /// 其中失败的次数（失败请求不参与官方牌价换算，但成本照记）
    pub errors: u64,
    /// 真正打到上游的成功调用数（剔除本地 WebSearch —— 那类请求不走上游、
    /// credits 恒为 0）。判"credits 全零是不是上游协议变了"只能用这个数。
    pub upstream_calls: u64,
    /// 失败请求携带的 credits 折算成本。上游已计费但请求失败 —— 这笔钱我方承担、
    /// 不向客户收取，所以它不在应收里；但必须能看见，否则毛利凭空少一块没人知道。
    pub error_credits: f64,
    /// 落在**未配官方价**模型上的调用数。>0 且走 discount 口径 = 这部分静默漏收
    pub unpriced_calls: u64,
    /// 同上，对应的 credits
    pub unpriced_credits: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub credits: f64,
    pub credit_usd: f64,
    /// 已配价模型的官方牌价合计；全部未配价时为 `None`
    pub official_usd: Option<f64>,
    /// 该 Key 的对客折扣系数（应收 ÷ 官方）；未设置为 `None`
    pub billing_discount: Option<f64>,
    /// 该 Key 的对客单价（美元/credit）；未设置为 `None`
    pub price_per_credit: Option<f64>,
    /// 应收。优先按 credits × 单价（可靠口径），否则按 官方牌价 × 折扣（估算口径）
    pub receivable_usd: Option<f64>,
    /// 应收采用了哪种口径："perCredit"（可靠）/ "discount"（依赖估算 token）/ null
    pub receivable_basis: Option<&'static str>,
    /// 毛利 = 应收 − 成本；应收缺失则为 `None`
    pub margin_usd: Option<f64>,
}

/// 上游凭据分布
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDistribution {
    pub credential_id: u64,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub errors: u64,
    pub credits: f64,
    /// 实付成本：credits × creditUsdRate。
    pub credit_usd: f64,
}

/// 概览：今日 + 累计
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewStats {
    /// 今日（本地 0 点起）的调用次数
    pub today_calls: u64,
    pub today_input_tokens: u64,
    pub today_output_tokens: u64,
    pub today_errors: u64,
    pub today_credits: f64,
    /// 最近 7 天累计
    pub week_calls: u64,
    pub week_input_tokens: u64,
    pub week_output_tokens: u64,
    pub week_credits: f64,
}

impl UsageAggregator {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::RwLock::new(AggregatorInner {
                hour_buckets: Vec::new(),
                day_buckets: Vec::new(),
            }),
        }
    }

    /// 启动时从历史 JSONL 重建聚合
    pub fn rebuild_from_logs(&self, dir: &Path) {
        // 兜底：空路径归一为 "."，否则 read_dir("") 会失败导致重建为 0
        let dir_buf;
        let dir = if dir.as_os_str().is_empty() {
            dir_buf = PathBuf::from(".");
            dir_buf.as_path()
        } else {
            dir
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(e) => {
                tracing::warn!("读取 usage_log 目录失败 {}: {}", dir.display(), e);
                return;
            }
        };
        let cutoff = Local::now().date_naive() - Duration::days(RETENTION_DAYS);
        let mut count = 0u64;
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let Some(date) = parse_usage_log_filename(&name) else {
                continue;
            };
            if date < cutoff {
                continue;
            }
            let file = match File::open(entry.path()) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(rec) = serde_json::from_str::<UsageRecord>(&line) {
                    self.ingest(&rec);
                    count += 1;
                }
            }
        }
        tracing::info!(
            "UsageAggregator 重建完成：从 {} 装载 {} 条历史记录",
            dir.display(),
            count
        );
    }

    /// 接收一条记录并落入对应桶
    pub fn ingest(&self, rec: &UsageRecord) {
        let dt: DateTime<Utc> = match DateTime::parse_from_rfc3339(&rec.ts) {
            Ok(d) => d.with_timezone(&Utc),
            Err(_) => Utc::now(),
        };
        let local = dt.with_timezone(&Local);

        // 小时桶起始：当地小时整点 → 转回 UTC unix 秒
        let hour_start = Local
            .with_ymd_and_hms(local.year(), local.month(), local.day(), local.hour(), 0, 0)
            .single();
        // 天桶起始：本地 0 点 → 转回 UTC unix 秒
        let day_start = Local
            .with_ymd_and_hms(local.year(), local.month(), local.day(), 0, 0, 0)
            .single();

        let hour_ts = hour_start.map(|d| d.timestamp()).unwrap_or(0);
        let day_ts = day_start.map(|d| d.timestamp()).unwrap_or(0);

        let mut inner = self.inner.write();

        upsert_bucket(&mut inner.hour_buckets, hour_ts, rec, HOUR_BUCKETS);
        upsert_bucket(&mut inner.day_buckets, day_ts, rec, DAY_BUCKETS);
    }

    /// 时序数据查询
    pub fn query_timeseries(
        &self,
        window: StatsQueryWindow,
        key_id: Option<u64>,
        cred_filter: Option<&std::collections::HashSet<u64>>,
        pricing: &crate::common::pricing::PricingTable,
    ) -> Vec<TimeSeriesPoint> {
        let inner = self.inner.read();
        let buckets = select_buckets(&inner, window.granularity);

        let mut points: Vec<TimeSeriesPoint> = buckets
            .iter()
            .filter(|b| bucket_in_window(b, window))
            .filter(|b| bucket_matches_key(b, key_id))
            .map(|b| {
                // 不带 group 过滤 → 走老逻辑（更快，命中预聚合 by_key/overall 桶）
                let stats = match cred_filter {
                    None => stats_for_key(b, key_id),
                    Some(allow) => credential_group_for_key(b, key_id)
                        .map(|group| {
                            let mut s = BucketStats::default();
                            for (cid, cs) in group {
                                if allow.contains(cid) {
                                    s.add_stats(cs);
                                }
                            }
                            s
                        })
                        .unwrap_or_default(),
                };
                // 官方口径成本要按模型逐项算（各模型单价不同），只有非分组路径
                // 才有 模型 维度可用；分组筛选路径没有 凭据×模型 数据，标 None。
                let official_usd = match cred_filter {
                    None => sum_official_usd(model_group_for_key(b, key_id), pricing),
                    Some(_) => None,
                };
                TimeSeriesPoint {
                    ts: ts_to_rfc3339(b.ts),
                    input_tokens: stats.input_tokens,
                    output_tokens: stats.output_tokens,
                    cache_creation_tokens: stats.cache_creation_tokens,
                    cache_read_tokens: stats.cache_read_tokens,
                    calls: stats.calls,
                    errors: stats.errors,
                    credits: stats.credits,
                    credit_usd: pricing.credit_usd(stats.credits),
                    official_usd,
                }
            })
            .collect();
        points.sort_by_key(|p| p.ts.clone());
        points
    }

    /// 模型分布
    pub fn query_by_model(
        &self,
        window: StatsQueryWindow,
        key_id: Option<u64>,
        pricing: &crate::common::pricing::PricingTable,
    ) -> Vec<ModelDistribution> {
        let inner = self.inner.read();
        let buckets = select_buckets(&inner, window.granularity);
        let mut acc: HashMap<String, BucketStats> = HashMap::new();
        for b in buckets.iter().filter(|b| bucket_in_window(b, window)) {
            let Some(group) = model_group_for_key(b, key_id) else {
                continue;
            };
            for (model, stats) in group {
                acc.entry(model.clone()).or_default().add_stats(stats);
            }
        }
        let mut out: Vec<ModelDistribution> = acc
            .into_iter()
            .map(|(model, stats)| {
                let credit_usd = pricing.credit_usd(stats.credits);
                let official_usd = pricing.official_usd(
                    &model,
                    stats.input_tokens,
                    stats.output_tokens,
                    stats.cache_creation_tokens,
                    stats.cache_read_tokens,
                );
                ModelDistribution {
                    calls: stats.calls,
                    input_tokens: stats.input_tokens,
                    output_tokens: stats.output_tokens,
                    cache_creation_tokens: stats.cache_creation_tokens,
                    cache_read_tokens: stats.cache_read_tokens,
                    errors: stats.errors,
                    credits: stats.credits,
                    credit_usd,
                    official_usd,
                    discount_ratio: crate::common::pricing::discount_ratio(
                        credit_usd,
                        official_usd,
                    ),
                    model,
                }
            })
            .collect();
        out.sort_by(|a, b| b.calls.cmp(&a.calls));
        out
    }

    /// 上游凭据分布
    pub fn query_by_credential(
        &self,
        window: StatsQueryWindow,
        key_id: Option<u64>,
        cred_filter: Option<&std::collections::HashSet<u64>>,
        pricing: &crate::common::pricing::PricingTable,
    ) -> Vec<CredentialDistribution> {
        let inner = self.inner.read();
        let buckets = select_buckets(&inner, window.granularity);
        let mut acc: HashMap<u64, BucketStats> = HashMap::new();
        for b in buckets.iter().filter(|b| bucket_in_window(b, window)) {
            let Some(group) = credential_group_for_key(b, key_id) else {
                continue;
            };
            for (id, stats) in group {
                if let Some(allow) = cred_filter {
                    if !allow.contains(id) {
                        continue;
                    }
                }
                acc.entry(*id).or_default().add_stats(stats);
            }
        }
        let mut out: Vec<CredentialDistribution> = acc
            .into_iter()
            .map(|(id, stats)| CredentialDistribution {
                credential_id: id,
                calls: stats.calls,
                input_tokens: stats.input_tokens,
                output_tokens: stats.output_tokens,
                cache_creation_tokens: stats.cache_creation_tokens,
                cache_read_tokens: stats.cache_read_tokens,
                errors: stats.errors,
                credits: stats.credits,
                credit_usd: pricing.credit_usd(stats.credits),
            })
            .collect();
        out.sort_by(|a, b| b.calls.cmp(&a.calls));
        out
    }

    /// 按入口 Key 出账单（月度结算用）。
    ///
    /// 官方牌价必须按 Key×模型 逐项算——各模型单价差几十倍，用聚合后的 token 总量
    /// 乘任何单一价都是错的。`by_key_model` 正好提供这个维度。
    ///
    /// `discount_of` 由调用方提供（读 ClientKeyManager），本模块不感知 Key 的配置。
    #[deprecated(
        note = "月结走 billing_from_logs：本函数只保留 31 个日桶，9 月初结 8 月账会把月初几天静默算成零消费"
    )]
    pub fn query_billing(
        &self,
        window: StatsQueryWindow,
        pricing: &crate::common::pricing::PricingTable,
        // pricing_of 返回 (对客折扣, 对客单价$/credit)
        pricing_of: &dyn Fn(u64) -> (Option<f64>, Option<f64>),
    ) -> Vec<KeyBillingRow> {
        let inner = self.inner.read();
        let buckets = select_buckets(&inner, window.granularity);

        // key -> (总量, 按模型的官方成本累加)
        let mut totals: HashMap<u64, BucketStats> = HashMap::new();
        let mut official: HashMap<u64, (f64, bool)> = HashMap::new();
        for b in buckets.iter().filter(|b| bucket_in_window(b, window)) {
            for (key_id, per_model) in &b.by_key_model {
                for (model, stats) in per_model {
                    totals.entry(*key_id).or_default().add_stats(stats);
                    if let Some(usd) = pricing.official_usd(
                        model,
                        stats.input_tokens,
                        stats.output_tokens,
                        stats.cache_creation_tokens,
                        stats.cache_read_tokens,
                    ) {
                        let e = official.entry(*key_id).or_insert((0.0, false));
                        e.0 += usd;
                        e.1 = true;
                    }
                }
            }
        }

        let mut rows: Vec<KeyBillingRow> = totals
            .into_iter()
            .map(|(key_id, s)| {
                let official_usd = official
                    .get(&key_id)
                    .and_then(|(sum, any)| any.then_some(*sum));
                let (billing_discount, price_per_credit) = pricing_of(key_id);
                let credit_usd = pricing.credit_usd(s.credits);
                // 单价口径优先：credits 是上游真值，而折扣的分母（官方牌价）要靠
                // token 明细换算，那部分在上游不下发时是本地估算的。
                let (receivable_usd, receivable_basis) = match price_per_credit {
                    Some(p) => (Some(s.credits * p), Some("perCredit")),
                    None => match (official_usd, billing_discount) {
                        (Some(o), Some(d)) => (Some(o * d), Some("discount")),
                        _ => (None, None),
                    },
                };
                KeyBillingRow {
                    key_id,
                    calls: s.calls,
                    // 聚合器路径已弃用，不再计算这些告警口径
                    errors: 0,
                    upstream_calls: 0,
                    error_credits: 0.0,
                    unpriced_calls: 0,
                    unpriced_credits: 0.0,
                    input_tokens: s.input_tokens,
                    output_tokens: s.output_tokens,
                    cache_creation_tokens: s.cache_creation_tokens,
                    cache_read_tokens: s.cache_read_tokens,
                    credits: s.credits,
                    credit_usd,
                    official_usd,
                    billing_discount,
                    price_per_credit,
                    receivable_usd,
                    receivable_basis,
                    margin_usd: receivable_usd.map(|r| r - credit_usd),
                }
            })
            .collect();
        rows.sort_by(|a, b| b.credit_usd.total_cmp(&a.credit_usd));
        rows
    }

    /// 概览（今日 + 最近 7 天）
    pub fn overview(&self) -> OverviewStats {
        let inner = self.inner.read();
        let today_start = Local
            .with_ymd_and_hms(
                Local::now().year(),
                Local::now().month(),
                Local::now().day(),
                0,
                0,
                0,
            )
            .single()
            .map(|d| d.timestamp())
            .unwrap_or(0);

        let mut today = BucketStats::default();
        for b in inner.hour_buckets.iter().filter(|b| b.ts >= today_start) {
            today.input_tokens += b.overall.input_tokens;
            today.output_tokens += b.overall.output_tokens;
            today.calls += b.overall.calls;
            today.errors += b.overall.errors;
            today.credits += b.overall.credits;
        }

        let week_cutoff = Utc::now().timestamp() - 7 * 24 * 3600;
        let mut week = BucketStats::default();
        for b in inner.hour_buckets.iter().filter(|b| b.ts >= week_cutoff) {
            week.input_tokens += b.overall.input_tokens;
            week.output_tokens += b.overall.output_tokens;
            week.calls += b.overall.calls;
            week.credits += b.overall.credits;
        }

        OverviewStats {
            today_calls: today.calls,
            today_input_tokens: today.input_tokens,
            today_output_tokens: today.output_tokens,
            today_errors: today.errors,
            today_credits: today.credits,
            week_calls: week.calls,
            week_input_tokens: week.input_tokens,
            week_output_tokens: week.output_tokens,
            week_credits: week.credits,
        }
    }
}

impl Default for UsageAggregator {
    fn default() -> Self {
        Self::new()
    }
}

/// 把记录写入对应桶；不存在则插入并按时间排序，超过容量时移除最旧的
fn upsert_bucket(buckets: &mut Vec<BucketEntry>, ts: i64, rec: &UsageRecord, max: usize) {
    if let Some(b) = buckets.iter_mut().find(|b| b.ts == ts) {
        add_record_to_bucket(b, rec);
        return;
    }
    let mut entry = BucketEntry {
        ts,
        ..Default::default()
    };
    add_record_to_bucket(&mut entry, rec);
    buckets.push(entry);
    buckets.sort_by_key(|b| b.ts);
    while buckets.len() > max {
        buckets.remove(0);
    }
}

fn add_record_to_bucket(bucket: &mut BucketEntry, rec: &UsageRecord) {
    bucket.overall.add(rec);
    bucket.by_key.entry(rec.key_id).or_default().add(rec);
    bucket
        .by_model
        .entry(rec.model.clone())
        .or_default()
        .add(rec);
    bucket
        .by_key_model
        .entry(rec.key_id)
        .or_default()
        .entry(rec.model.clone())
        .or_default()
        .add(rec);
    if rec.credential_id == 0 {
        return;
    }
    bucket
        .by_credential
        .entry(rec.credential_id)
        .or_default()
        .add(rec);
    bucket
        .by_key_credential
        .entry(rec.key_id)
        .or_default()
        .entry(rec.credential_id)
        .or_default()
        .add(rec);
}

fn bucket_matches_key(bucket: &BucketEntry, key_id: Option<u64>) -> bool {
    key_id
        .map(|id| bucket.by_key.contains_key(&id))
        .unwrap_or(true)
}

fn credential_group_for_key(
    bucket: &BucketEntry,
    key_id: Option<u64>,
) -> Option<&HashMap<u64, BucketStats>> {
    match key_id {
        Some(id) => bucket.by_key_credential.get(&id),
        None => Some(&bucket.by_credential),
    }
}

fn model_group_for_key(
    bucket: &BucketEntry,
    key_id: Option<u64>,
) -> Option<&HashMap<String, BucketStats>> {
    match key_id {
        Some(id) => bucket.by_key_model.get(&id),
        None => Some(&bucket.by_model),
    }
}

/// 把一个 模型→用量 分组按官方牌价折成美金后求和。
///
/// 只累计已配价模型；分组缺失或没有任何已配价模型时返回 `None`——
/// 「无法计价」必须与「$0」区分开，否则未配价模型会把折扣显示成免费。
fn sum_official_usd(
    group: Option<&HashMap<String, BucketStats>>,
    pricing: &crate::common::pricing::PricingTable,
) -> Option<f64> {
    let group = group?;
    let mut sum = 0.0;
    let mut priced_any = false;
    for (model, stats) in group {
        if let Some(usd) = pricing.official_usd(
            model,
            stats.input_tokens,
            stats.output_tokens,
            stats.cache_creation_tokens,
            stats.cache_read_tokens,
        ) {
            sum += usd;
            priced_any = true;
        }
    }
    priced_any.then_some(sum)
}

fn bucket_in_window(bucket: &BucketEntry, window: StatsQueryWindow) -> bool {
    bucket.ts >= window.start_ts && bucket.ts < window.end_ts
}

fn select_buckets(inner: &AggregatorInner, granularity: StatsGranularity) -> &[BucketEntry] {
    match granularity {
        StatsGranularity::Hour => &inner.hour_buckets,
        StatsGranularity::Day => &inner.day_buckets,
    }
}

fn stats_for_key(bucket: &BucketEntry, key_id: Option<u64>) -> BucketStats {
    match key_id {
        Some(id) => bucket.by_key.get(&id).copied().unwrap_or_default(),
        None => bucket.overall,
    }
}

fn ts_to_rfc3339(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

pub type SharedRecorder = Arc<UsageRecorder>;
pub type SharedAggregator = Arc<UsageAggregator>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_filename() {
        assert!(parse_usage_log_filename("usage_log.2026-05-22.jsonl").is_some());
        assert!(parse_usage_log_filename("foo.bar").is_none());
    }

    /// 每个测试一个独立目录，互不干扰。用进程 id + 名字，不引额外依赖。
    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kiro_billing_test_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 写一个用量日志文件（文件名给的是"服务器本地日期"，内容 ts 是 UTC）
    fn write_log(dir: &Path, file_day: &str, records: &[(&str, u64, f64)]) {
        let path = dir.join(format!("usage_log.{}.jsonl", file_day));
        let mut body = String::new();
        for (ts, key_id, credits) in records {
            body.push_str(&format!(
                r#"{{"ts":"{}","keyId":{},"credentialId":1,"model":"claude-opus-5","inputTokens":10,"outputTokens":20,"cacheCreationTokens":0,"cacheReadTokens":0,"credits":{},"durationMs":100,"status":"success"}}"#,
                ts, key_id, credits
            ));
            body.push('\n');
        }
        std::fs::write(path, body).unwrap();
    }

    /// 账期按北京时间切：日志文件按 UTC 日期滚动，北京 8/1 00:00–08:00 的请求
    /// 落在文件名为 7-31 的文件里。这条请求必须算进 8 月，不能算进 7 月。
    ///
    /// 这是真金白银的边界——错了就是月头月尾各错 8 小时的流量。
    #[test]
    fn billing_period_follows_beijing_days_not_file_names() {
        let dir = temp_dir("tz");
        let d = dir.as_path();

        write_log(
            d,
            "2026-07-31",
            &[
                // UTC 7-31 15:59 = 北京 7-31 23:59 → 属于 7 月
                ("2026-07-31T15:59:00+00:00", 1, 100.0),
                // UTC 7-31 16:00 = 北京 8-01 00:00 → 属于 8 月
                ("2026-07-31T16:00:00+00:00", 1, 7.0),
            ],
        );
        write_log(
            d,
            "2026-08-31",
            &[
                // UTC 8-31 15:59 = 北京 8-31 23:59 → 属于 8 月
                ("2026-08-31T15:59:00+00:00", 1, 11.0),
                // UTC 8-31 16:00 = 北京 9-01 00:00 → 属于 9 月
                ("2026-08-31T16:00:00+00:00", 1, 500.0),
            ],
        );

        let mut credits = 0.0;
        let out = scan_usage_records(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            None,
            |r| credits += r.credits,
        );

        assert_eq!(out.scanned, 2, "8 月应当只收进跨界的那两条");
        assert_eq!(
            credits, 18.0,
            "7+11：北京 7-31 23:59 那条和北京 9-01 00:00 那条都不该进 8 月账"
        );
    }

    /// 缺失的日期必须报出来。"那天没日志"和"那天没消费"结论完全不同，
    /// 静默当成零消费会让账单少收而没人发现。
    #[test]
    fn missing_day_files_are_reported_not_silently_zeroed() {
        let dir = temp_dir("missing");
        let d = dir.as_path();
        write_log(d, "2026-08-01", &[("2026-08-01T02:00:00+00:00", 1, 5.0)]);
        // 8-02 故意不写

        let out = scan_usage_records(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 4).unwrap(),
            None,
            |_| {},
        );
        assert!(
            out.missing_days.contains(&"2026-08-02".to_string()),
            "缺失日期没报出来: {:?}",
            out.missing_days
        );
    }

    /// 总账与明细必须同源：同一区间，明细逐条加出来的 credits
    /// 必须等于总账那一行的 credits。对不上客户就没法对账。
    #[test]
    fn summary_and_detail_agree_by_construction() {
        let dir = temp_dir("agree");
        let d = dir.as_path();
        write_log(
            d,
            "2026-08-10",
            &[
                ("2026-08-10T01:00:00+00:00", 7, 3.5),
                ("2026-08-10T02:00:00+00:00", 7, 1.25),
                ("2026-08-10T03:00:00+00:00", 9, 2.0),
            ],
        );

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (None, None),
        );

        let mut detail = 0.0;
        scan_usage_records(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            Some(7),
            |r| detail += r.credits,
        );

        let row7 = rows.iter().find(|r| r.key_id == 7).expect("key 7 应在账目里");
        assert_eq!(row7.credits, detail, "总账与明细不一致");
        assert_eq!(row7.calls, 2);
    }

    /// 失败的请求不能按官方牌价收钱：它 credits=0（我方无成本），
    /// 而 token 是本地估算的。按牌价打折 = 凭空多收，且客户在明细里
    /// 一眼就看到那行状态是 error。
    #[test]
    fn failed_requests_do_not_generate_receivable() {
        let dir = temp_dir("errors");
        let d = dir.as_path();
        let path = d.join("usage_log.2026-08-10.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"ts":"2026-08-10T01:00:00+00:00","keyId":1,"credentialId":1,"model":"claude-opus-4-5","inputTokens":1000000,"outputTokens":0,"credits":0.0,"status":"error"}"#, "\n",
                r#"{"ts":"2026-08-10T02:00:00+00:00","keyId":1,"credentialId":1,"model":"claude-opus-4-5","inputTokens":1000000,"outputTokens":0,"credits":10.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (Some(0.5), None),
        );
        let row = rows.iter().find(|r| r.key_id == 1).unwrap();
        assert_eq!(row.calls, 2);
        assert_eq!(row.errors, 1);
        // 官方牌价只算成功那一条：两条 input 相同，若把失败那条也算进去
        // official 会正好翻倍
        let one_call_official = pricing
            .official_usd("claude-opus-4-5", 1_000_000, 0, 0, 0)
            .expect("opus-4-5 必须已配价");
        assert!(
            (row.official_usd.unwrap() - one_call_official).abs() < 1e-9,
            "失败请求被算进了官方牌价: {:?} vs {}",
            row.official_usd,
            one_call_official
        );
    }

    /// 混合流量里的未配价模型必须被单独计出来。只要该 Key 还有别的模型
    /// 配了价，official_usd 就是 Some(部分和)，看上去完全正常——
    /// 这部分请求已经静默地从应收里消失了。
    #[test]
    fn partially_unpriced_traffic_is_counted_for_alerting() {
        let dir = temp_dir("mixed");
        let d = dir.as_path();
        let path = d.join("usage_log.2026-08-10.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"ts":"2026-08-10T01:00:00+00:00","keyId":1,"credentialId":1,"model":"claude-opus-4-5","inputTokens":1000,"outputTokens":100,"credits":1.0,"status":"success"}"#, "\n",
                r#"{"ts":"2026-08-10T02:00:00+00:00","keyId":1,"credentialId":1,"model":"deepseek-3.2","inputTokens":9000,"outputTokens":900,"credits":9.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (Some(0.5), None),
        );
        let row = rows.iter().find(|r| r.key_id == 1).unwrap();
        assert!(row.official_usd.is_some(), "有配价模型，官方价应为部分和");
        assert_eq!(row.unpriced_calls, 1, "未配价的那条必须被计出来");
        assert_eq!(row.unpriced_credits, 9.0);
    }

    /// 本地 WebSearch 请求不走上游、credits 恒为 0。它们不能算进 upstream_calls，
    /// 否则一个只跑 websearch 的 Key 会每月稳定触发"credits 全零"红色告警，
    /// 把唯一那条能救命的告警训练成"忽略项"。
    #[test]
    fn local_websearch_calls_do_not_trip_the_zero_credit_alarm() {
        let dir = temp_dir("websearch");
        let d = dir.as_path();
        let path = d.join("usage_log.2026-08-10.jsonl");
        std::fs::write(
            &path,
            concat!(
                // credentialId=0 = 本地 WebSearch，没走上游
                r#"{"ts":"2026-08-10T01:00:00+00:00","keyId":1,"credentialId":0,"model":"claude-opus-4-5","inputTokens":100,"outputTokens":0,"credits":0.0,"status":"success"}"#, "\n",
                r#"{"ts":"2026-08-10T02:00:00+00:00","keyId":1,"credentialId":0,"model":"claude-opus-4-5","inputTokens":100,"outputTokens":0,"credits":0.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (None, None),
        );
        let row = rows.iter().find(|r| r.key_id == 1).unwrap();
        assert_eq!(row.calls, 2);
        assert_eq!(row.upstream_calls, 0, "本地请求不该算成上游调用");
    }

    /// 失败请求携带的 credits 是我方实付、不向客户收取的成本。
    /// 它必须能被单独看到，否则毛利凭空少一块而没人知道少在哪。
    #[test]
    fn error_credits_are_tracked_separately_from_receivable() {
        let dir = temp_dir("errcredits");
        let d = dir.as_path();
        let path = d.join("usage_log.2026-08-10.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"ts":"2026-08-10T01:00:00+00:00","keyId":1,"credentialId":2,"model":"claude-opus-4-5","inputTokens":100,"outputTokens":10,"credits":4.0,"status":"error"}"#, "\n",
                r#"{"ts":"2026-08-10T02:00:00+00:00","keyId":1,"credentialId":2,"model":"claude-opus-4-5","inputTokens":100,"outputTokens":10,"credits":6.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (None, Some(0.05)),
        );
        let row = rows.iter().find(|r| r.key_id == 1).unwrap();
        // 成本含失败请求：10 credits 全算
        assert_eq!(row.credits, 10.0, "失败请求的成本必须照记");
        // 其中失败那部分单独可见
        assert!(
            (row.error_credits - pricing.credit_usd(4.0)).abs() < 1e-9,
            "失败成本没有被单独计出来: {}",
            row.error_credits
        );
        // 单价口径下应收按全部 credits 算（口径统一，导出侧也是这么算的）
        assert!((row.receivable_usd.unwrap() - 10.0 * 0.05).abs() < 1e-9);
    }

    /// 历史补偿必须只作用于修复之前的 opus-5，且只动 token 不动 credits。
    /// 乘错模型 = 凭空多收；乘到 credits 上 = 成本变假、毛利全错。
    #[test]
    fn historical_compensation_scales_tokens_but_never_credits() {
        let dir = temp_dir("histscale");
        let d = dir.as_path();
        // 2026-08-20 = 窗口修复之前；2026-08-25 = 之后
        std::fs::write(
            d.join("usage_log.2026-08-20.jsonl"),
            concat!(
                r#"{"ts":"2026-08-20T01:00:00+00:00","keyId":1,"credentialId":2,"model":"claude-opus-5","inputTokens":1000,"outputTokens":100,"cacheCreationTokens":200,"cacheReadTokens":300,"credits":7.0,"status":"success"}"#, "\n",
                r#"{"ts":"2026-08-20T02:00:00+00:00","keyId":2,"credentialId":2,"model":"claude-sonnet-5","inputTokens":1000,"outputTokens":100,"cacheCreationTokens":200,"cacheReadTokens":300,"credits":7.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 20).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(),
            &pricing,
            &|_| (None, None),
        );

        let opus = rows.iter().find(|r| r.key_id == 1).unwrap();
        let sonnet = rows.iter().find(|r| r.key_id == 2).unwrap();

        // credits 是上游真值：两边都不得被放大
        assert_eq!(opus.credits, 7.0, "credits 被补偿系数污染了，成本会变假");
        assert_eq!(sonnet.credits, 7.0);

        // 官方牌价按补偿后的 token 算：opus-5 的三项各 ×5，output 不动
        let expect_opus = pricing
            .official_usd("claude-opus-5", 5000, 100, 1000, 1500)
            .unwrap();
        assert!(
            (opus.official_usd.unwrap() - expect_opus).abs() < 1e-9,
            "opus-5 未按 5 倍还原: {:?} vs {}",
            opus.official_usd,
            expect_opus
        );

        // sonnet-5 窗口配置一直是对的，补偿会变成凭空多收
        let expect_sonnet = pricing
            .official_usd("claude-sonnet-5", 1000, 100, 200, 300)
            .unwrap();
        assert!(
            (sonnet.official_usd.unwrap() - expect_sonnet).abs() < 1e-9,
            "sonnet-5 被误补偿了，等于凭空多收"
        );
    }

    /// 目标毛利率反解：折扣 = 保本线 / (1 − 目标毛利率)。
    /// 这是要直接拿去改客户价的数，算错就是真金白银。
    #[test]
    fn pricing_advice_solves_for_the_target_margin() {
        let row = KeyBillingRow {
            key_id: 1,
            calls: 100,
            errors: 0,
            upstream_calls: 100,
            unpriced_calls: 0,
            unpriced_credits: 0.0,
            error_credits: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 5000.0,
            credit_usd: 100.0,
            official_usd: Some(1000.0),
            billing_discount: Some(0.15),
            price_per_credit: None,
            receivable_usd: Some(150.0),
            receivable_basis: Some("discount"),
            margin_usd: Some(50.0),
        };
        let a = &pricing_advice(&[row], 0.5)[0];
        // 保本线 = 100/1000 = 0.10
        assert!((a.breakeven_discount.unwrap() - 0.10).abs() < 1e-9);
        // 50% 毛利 → 折扣 = 0.10 / 0.5 = 0.20
        assert!((a.recommended_discount.unwrap() - 0.20).abs() < 1e-9);
        // 应收 = 1000 × 0.20 = 200，比当前 150 多 50
        assert!((a.recommended_receivable_usd.unwrap() - 200.0).abs() < 1e-9);
        assert!((a.receivable_delta_usd.unwrap() - 50.0).abs() < 1e-9);
        // 验算：毛利率 = (200-100)/200 = 0.5 ✓
        assert!((a.current_margin_rate.unwrap() - (150.0 - 100.0) / 150.0).abs() < 1e-9);
    }

    /// 低于保本线必须明确标红，不能只给个数字让人自己看出来
    #[test]
    fn pricing_advice_flags_below_breakeven() {
        let mut row = KeyBillingRow {
            key_id: 2,
            calls: 10,
            errors: 0,
            upstream_calls: 10,
            unpriced_calls: 0,
            unpriced_credits: 0.0,
            error_credits: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 5000.0,
            credit_usd: 100.0,
            official_usd: Some(500.0), // 保本线 0.20
            billing_discount: Some(0.15), // 低于保本线
            price_per_credit: None,
            receivable_usd: Some(75.0),
            receivable_basis: Some("discount"),
            margin_usd: Some(-25.0),
        };
        assert!(pricing_advice(&[row.clone()], 0.5)[0].verdict.contains("亏"));
        // 提到 0.40 就达标了（0.20 / 0.5）
        row.billing_discount = Some(0.40);
        row.receivable_usd = Some(200.0);
        let a = &pricing_advice(&[row], 0.5)[0];
        assert!(a.verdict.contains("达到"), "实得: {}", a.verdict);
    }

    /// 官方价算不出来时不许瞎给建议
    #[test]
    fn no_advice_without_an_official_price() {
        let row = KeyBillingRow {
            key_id: 3,
            calls: 1,
            errors: 0,
            upstream_calls: 1,
            unpriced_calls: 1,
            unpriced_credits: 1.0,
            error_credits: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 1.0,
            credit_usd: 0.02,
            official_usd: None,
            billing_discount: Some(0.3),
            price_per_credit: None,
            receivable_usd: None,
            receivable_basis: None,
            margin_usd: None,
        };
        let a = &pricing_advice(&[row], 0.5)[0];
        assert!(a.recommended_discount.is_none(), "无官方价却给了建议");
        assert!(a.verdict.contains("未配官方价"));
    }

    /// 损坏的行（负 credits / NaN）不能污染账单，也不能让整个导出失败。
    #[test]
    fn corrupt_lines_do_not_poison_the_bill() {
        let dir = temp_dir("corrupt");
        let d = dir.as_path();
        let path = d.join("usage_log.2026-08-10.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"ts":"2026-08-10T01:00:00+00:00","keyId":1,"credentialId":1,"model":"claude-opus-5","inputTokens":10,"outputTokens":20,"credits":5.0,"status":"success"}"#, "\n",
                "这不是 json\n",
                r#"{"ts":"2026-08-10T02:00:00+00:00","keyId":1,"credentialId":1,"model":"claude-opus-5","inputTokens":10,"outputTokens":20,"credits":-999.0,"status":"success"}"#, "\n",
                r#"{"ts":"坏时间","keyId":1,"credentialId":1,"model":"claude-opus-5","inputTokens":10,"outputTokens":20,"credits":42.0,"status":"success"}"#, "\n",
            ),
        )
        .unwrap();

        let pricing = crate::common::pricing::PricingTable::from_config(
            &crate::common::pricing::PricingConfig::default(),
        );
        let (rows, _) = billing_from_logs(
            d,
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            &pricing,
            &|_| (None, None),
        );
        let row = rows.iter().find(|r| r.key_id == 1).unwrap();
        assert_eq!(row.credits, 5.0, "负数/坏行必须计 0，不能加也不能减");
    }

    /// 账单口径：单价优先于折扣，两者都缺则不出应收（不猜）。
    #[allow(deprecated)] // 覆盖弃用路径本身的行为，保留至该函数删除
    #[test]
    fn billing_prefers_the_reliable_per_credit_basis() {
        let agg = UsageAggregator::new();
        let now = Utc::now();
        let rec = |key_id: u64, credits: f64| UsageRecord {
            ts: now.to_rfc3339(),
            key_id,
            credential_id: 1,
            model: "claude-opus-4-8".to_string(),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits,
            duration_ms: 10,
            status: "success".to_string(),
        };
        agg.ingest(&rec(1, 100.0)); // 只配单价
        agg.ingest(&rec(2, 100.0)); // 只配折扣
        agg.ingest(&rec(3, 100.0)); // 都没配

        let pricing = crate::common::pricing::PricingTable::default();
        let window = StatsQueryWindow::preset(Range::Last24h, StatsGranularity::Hour);
        let rows = agg.query_billing(window, &pricing, &|id| match id {
            1 => (None, Some(0.05)),       // $0.05/credit
            2 => (Some(0.3), None),        // 官方价三折
            _ => (None, None),
        });
        let row = |id: u64| rows.iter().find(|r| r.key_id == id).unwrap();

        // 成本对所有人一致：100 credits × 0.02
        for id in [1, 2, 3] {
            assert!((row(id).credit_usd - 2.0).abs() < 1e-9);
        }
        // 单价口径：100 × 0.05 = $5，标为可靠
        assert!((row(1).receivable_usd.unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(row(1).receivable_basis, Some("perCredit"));
        assert!((row(1).margin_usd.unwrap() - 3.0).abs() < 1e-9);
        // 折扣口径：官方 1M input @ $5 = $5，三折 = $1.5，标为估算口径
        assert!((row(2).receivable_usd.unwrap() - 1.5).abs() < 1e-9);
        assert_eq!(row(2).receivable_basis, Some("discount"));
        // 未定价：不出应收也不出毛利——把未配置当免费是账单里最容易漏收的默认值
        assert!(row(3).receivable_usd.is_none());
        assert!(row(3).margin_usd.is_none());
        assert!(row(3).receivable_basis.is_none());
    }

    /// 两种口径都配时以单价为准（credits 是上游真值，折扣的分母是估算）。
    #[allow(deprecated)] // 覆盖弃用路径本身的行为，保留至该函数删除
    #[test]
    fn billing_per_credit_wins_when_both_are_set() {
        let agg = UsageAggregator::new();
        agg.ingest(&UsageRecord {
            ts: Utc::now().to_rfc3339(),
            key_id: 7,
            credential_id: 1,
            model: "claude-opus-4-8".to_string(),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 10.0,
            duration_ms: 10,
            status: "success".to_string(),
        });
        let pricing = crate::common::pricing::PricingTable::default();
        let window = StatsQueryWindow::preset(Range::Last24h, StatsGranularity::Hour);
        let rows = agg.query_billing(window, &pricing, &|_| (Some(0.9), Some(0.04)));
        let r = &rows[0];
        assert_eq!(r.receivable_basis, Some("perCredit"));
        assert!((r.receivable_usd.unwrap() - 0.4).abs() < 1e-9, "10 credits × $0.04");
    }

    /// 官方价按 Key×模型 逐项算：同一 Key 混用不同单价的模型时不能用单一均价。
    #[allow(deprecated)] // 覆盖弃用路径本身的行为，保留至该函数删除
    #[test]
    fn billing_official_value_is_summed_per_model() {
        let agg = UsageAggregator::new();
        let now = Utc::now().to_rfc3339();
        let mk = |model: &str| UsageRecord {
            ts: now.clone(),
            key_id: 1,
            credential_id: 1,
            model: model.to_string(),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 1.0,
            duration_ms: 10,
            status: "success".to_string(),
        };
        agg.ingest(&mk("claude-opus-4-8")); // $5/M input
        agg.ingest(&mk("claude-haiku-4-5")); // $1/M input
        agg.ingest(&mk("deepseek-3.2")); // 未配价，不计入

        let pricing = crate::common::pricing::PricingTable::default();
        let window = StatsQueryWindow::preset(Range::Last24h, StatsGranularity::Hour);
        let rows = agg.query_billing(window, &pricing, &|_| (Some(1.0), None));
        let r = &rows[0];
        // 5 + 1 = 6；未配价那条既不按 0 计也不整行作废
        assert!((r.official_usd.unwrap() - 6.0).abs() < 1e-9);
        assert_eq!(r.calls, 3, "调用数仍然全计");
    }

    #[test]
    fn aggregator_basic_ingest_and_overview() {
        let agg = UsageAggregator::new();
        let now = Utc::now();
        let rec = UsageRecord {
            ts: now.to_rfc3339(),
            key_id: 1,
            credential_id: 5,
            model: "claude-opus-4-7".to_string(),
            input_tokens: 1000,
            output_tokens: 200,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.05,
            duration_ms: 1500,
            status: "success".to_string(),
        };
        agg.ingest(&rec);
        agg.ingest(&rec);

        let ov = agg.overview();
        assert_eq!(ov.today_calls, 2);
        assert_eq!(ov.today_input_tokens, 2000);

        let window = StatsQueryWindow::preset(Range::Last24h, StatsGranularity::Hour);
        let pricing = crate::common::pricing::PricingTable::default();
        let series = agg.query_timeseries(window, None, None, &pricing);
        assert!(!series.is_empty());

        let by_model = agg.query_by_model(window, None, &pricing);
        assert_eq!(by_model.len(), 1);
        assert_eq!(by_model[0].model, "claude-opus-4-7");
        assert_eq!(by_model[0].calls, 2);

        let by_cred = agg.query_by_credential(window, None, None, &pricing);
        assert_eq!(by_cred.len(), 1);
        assert_eq!(by_cred[0].credential_id, 5);
    }

    #[test]
    fn aggregator_filters_by_client_key() {
        let agg = UsageAggregator::new();
        let now = Utc::now().to_rfc3339();
        let rec_a = UsageRecord {
            ts: now.clone(),
            key_id: 1,
            credential_id: 5,
            model: "m-a".to_string(),
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.01,
            duration_ms: 100,
            status: "success".to_string(),
        };
        let rec_b = UsageRecord {
            ts: now,
            key_id: 2,
            credential_id: 6,
            model: "m-b".to_string(),
            input_tokens: 300,
            output_tokens: 40,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.02,
            duration_ms: 200,
            status: "error".to_string(),
        };
        agg.ingest(&rec_a);
        agg.ingest(&rec_b);

        let window = StatsQueryWindow::preset(Range::Last24h, StatsGranularity::Hour);
        let pricing = crate::common::pricing::PricingTable::default();
        let series = agg.query_timeseries(window, Some(1), None, &pricing);
        assert_eq!(series.iter().map(|p| p.calls).sum::<u64>(), 1);
        assert_eq!(series.iter().map(|p| p.input_tokens).sum::<u64>(), 100);

        let by_model = agg.query_by_model(window, Some(1), &pricing);
        assert_eq!(by_model.len(), 1);
        assert_eq!(by_model[0].model, "m-a");

        let by_cred = agg.query_by_credential(window, Some(1), None, &pricing);
        assert_eq!(by_cred.len(), 1);
        assert_eq!(by_cred[0].credential_id, 5);
    }

    #[test]
    fn aggregator_filters_by_custom_window_and_granularity() {
        let agg = UsageAggregator::new();
        let today = Local::now().date_naive();
        let yesterday = today - Duration::days(1);
        let yesterday_noon = Local
            .with_ymd_and_hms(
                yesterday.year(),
                yesterday.month(),
                yesterday.day(),
                12,
                0,
                0,
            )
            .single()
            .unwrap()
            .with_timezone(&Utc)
            .to_rfc3339();
        let today_noon = Local
            .with_ymd_and_hms(today.year(), today.month(), today.day(), 12, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
            .to_rfc3339();
        let rec_yesterday = UsageRecord {
            ts: yesterday_noon,
            key_id: 0,
            credential_id: 5,
            model: "m-yesterday".to_string(),
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.01,
            duration_ms: 100,
            status: "success".to_string(),
        };
        let rec_today = UsageRecord {
            ts: today_noon,
            key_id: 0,
            credential_id: 5,
            model: "m-today".to_string(),
            input_tokens: 300,
            output_tokens: 40,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.02,
            duration_ms: 100,
            status: "success".to_string(),
        };
        agg.ingest(&rec_yesterday);
        agg.ingest(&rec_today);

        let start_ts = Local
            .with_ymd_and_hms(today.year(), today.month(), today.day(), 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let end_ts = Local
            .with_ymd_and_hms(today.year(), today.month(), today.day(), 23, 59, 59)
            .single()
            .unwrap()
            .timestamp();
        let hour_window = StatsQueryWindow {
            start_ts,
            end_ts,
            granularity: StatsGranularity::Hour,
        };
        let day_window = StatsQueryWindow {
            start_ts,
            end_ts,
            granularity: StatsGranularity::Day,
        };

        let pricing = crate::common::pricing::PricingTable::default();
        let hourly = agg.query_timeseries(hour_window, None, None, &pricing);
        assert_eq!(hourly.iter().map(|p| p.calls).sum::<u64>(), 1);
        assert_eq!(hourly.iter().map(|p| p.input_tokens).sum::<u64>(), 300);

        let daily = agg.query_timeseries(day_window, None, None, &pricing);
        assert_eq!(daily.iter().map(|p| p.calls).sum::<u64>(), 1);
        assert_eq!(daily.iter().map(|p| p.output_tokens).sum::<u64>(), 40);
    }

    #[test]
    fn error_record_increments_errors() {
        let agg = UsageAggregator::new();
        let rec = UsageRecord {
            ts: Utc::now().to_rfc3339(),
            key_id: 0,
            credential_id: 0,
            model: "claude-opus-4-7".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits: 0.0,
            duration_ms: 100,
            status: "error".to_string(),
        };
        agg.ingest(&rec);
        let ov = agg.overview();
        assert_eq!(ov.today_errors, 1);
    }
}
