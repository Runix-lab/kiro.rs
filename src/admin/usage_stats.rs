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
    Last24h,
    Last7d,
    Last30d,
}

impl Range {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
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

    /// 账单口径：单价优先于折扣，两者都缺则不出应收（不猜）。
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
