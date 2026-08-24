//! 上游元数据事件
//!
//! Kiro 在 `metadataEvent.tokenUsage` 中返回本次模型调用的精确 token 用量。
//! 四个字段是单次调用的最终快照，不是增量事件；调用方应在同一条流内保留最后一份快照。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// 诊断探针计数器：全零 tokenUsage 的累计命中次数。
static ALL_ZERO_HITS: AtomicU64 = AtomicU64::new(0);
/// 诊断探针计数器：上游零缓存但本地已算出缓存覆盖的累计命中次数。
static CACHE_DISCARDED_HITS: AtomicU64 = AtomicU64::new(0);
/// 诊断探针计数器：uncachedInputTokens 为 0 而缓存读取有值的累计命中次数。
static INPUT_ZEROED_HITS: AtomicU64 = AtomicU64::new(0);

/// 诊断探针：量化「上游 tokenUsage 不可信」的真实频率。
///
/// 背景：`tokenUsage` 一旦是 `Some`，其优先级高于中转层 `CacheMeter` 的模拟值，
/// 会直接决定最终上报的 input / cache 分项（见 `resolve_non_stream_usage` 与
/// `StreamContext::resolved_usage`）。但由于全字段 `#[serde(default)]`，上游发来的
/// 部分 payload 会静默变成零值快照，从而把本地算好的缓存覆盖一起抹掉。
///
/// 该函数**只记日志、不改任何行为**，用于先摸清这两种情形在生产中是否真的发生、
/// 频率多高，再决定是否值得加零值守卫。为避免刷屏，仅在首次与每 100 次时告警。
/// 频率摸清并做出决策后，本函数及其调用点可整体删除。
pub fn probe_untrusted_token_usage(usage: TokenUsage, local_cache_covered_est: i32) {
    if usage.is_all_zero() {
        let hits = ALL_ZERO_HITS.fetch_add(1, Ordering::Relaxed) + 1;
        if hits == 1 || hits % 100 == 0 {
            tracing::warn!(
                occurrences = hits,
                present = usage.present,
                complete = usage.is_complete(),
                "metadataEvent.tokenUsage 四字段全零；本次用量已退化为本地估算。\
                 present=0 表示上游一个键都没给（等同未下发），非 0 表示上游明确报了零。\
                 详见 ANALYSIS.md §3.2"
            );
        }
        return;
    }
    if usage.reports_no_cache() && local_cache_covered_est > 0 {
        let hits = CACHE_DISCARDED_HITS.fetch_add(1, Ordering::Relaxed) + 1;
        if hits == 1 || hits % 100 == 0 {
            tracing::warn!(
                occurrences = hits,
                local_cache_covered_est,
                "上游未回报缓存分项，但本地 CacheMeter 已算出缓存覆盖；\
                 上游零值优先级更高，本地模拟值将被丢弃。详见 ANALYSIS.md §3.2"
            );
        }
        return;
    }
    // 第三种情形：input 归零但缓存有值。`input_tokens` 直接取 uncachedInputTokens，
    // 因此这条会让整个输入侧账目变成 0。
    //
    // 它有两种相反的成因，靠 `present` 位掩码定性：
    //   键缺失 → 残缺 payload，**真 bug**；
    //   显式 0 → 整个 prompt 全部命中缓存，**合法**（长会话里常见）。
    if usage.uncached_input_tokens == 0 && usage.cache_read_input_tokens > 0 {
        let hits = INPUT_ZEROED_HITS.fetch_add(1, Ordering::Relaxed) + 1;
        if hits == 1 || hits % 100 == 0 {
            if usage.reports_uncached_input() {
                // 上游明确报了 0：这是真的全命中缓存，不是 bug。降为 debug，
                // 只为留痕，避免把合法行为当故障告警。
                tracing::debug!(
                    occurrences = hits,
                    cache_read_input_tokens = usage.cache_read_input_tokens,
                    "上游明确下发 uncachedInputTokens=0 且缓存读取有值：整个 prompt 全命中缓存，合法"
                );
            } else {
                tracing::warn!(
                    occurrences = hits,
                    output_tokens = usage.output_tokens,
                    cache_read_input_tokens = usage.cache_read_input_tokens,
                    cache_write_input_tokens = usage.cache_write_input_tokens,
                    present = usage.present,
                    local_cache_covered_est,
                    "上游 tokenUsage **缺失** uncachedInputTokens 键而缓存读取有值 —— \
                     残缺 payload 已坐实，本次 input_tokens 被抹成 0。详见 ANALYSIS.md §3.2"
                );
            }
        }
    }
}

/// `present` 位掩码：上游 payload 里实际出现过哪些键。
///
/// 存在的意义是把「键缺失」与「键存在且为 0」区分开。四个字段原先都是
/// `#[serde(default)]`，两种情形反序列化后完全一样，导致诊断探针无法定性
/// 「残缺 payload（真 bug）」还是「整个 prompt 全命中缓存（合法）」。
pub mod present {
    pub const UNCACHED_INPUT: u8 = 1 << 0;
    pub const OUTPUT: u8 = 1 << 1;
    pub const CACHE_READ: u8 = 1 << 2;
    pub const CACHE_WRITE: u8 = 1 << 3;
    /// 四个键全都出现。
    pub const ALL: u8 = UNCACHED_INPUT | OUTPUT | CACHE_READ | CACHE_WRITE;
}

/// 单次 Kiro 模型调用的精确 token 用量。
///
/// 手写 `Deserialize`（不用 derive）**只为多记一个 `present` 位掩码**：四个数值
/// 字段的名字与语义完全不变，所有既有读取点无需改动。缺失的键仍然填 0，行为与
/// 原先的 `#[serde(default)]` 一致 —— 这个改动不改变任何上报数值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 未命中缓存、也未写入缓存的输入 token。
    pub uncached_input_tokens: i32,
    /// 模型输出 token。
    pub output_tokens: i32,
    /// 从服务端 prompt cache 读取的输入 token。
    pub cache_read_input_tokens: i32,
    /// 本次写入服务端 prompt cache 的输入 token。
    pub cache_write_input_tokens: i32,
    /// 上游实际下发了哪些键（见 [`present`]）。**仅供诊断，不参与任何计量。**
    pub present: u8,
}

impl<'de> Deserialize<'de> for TokenUsage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 先落到中间结构：`Option` 让「键缺失」与「显式 0」可分。
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            uncached_input_tokens: Option<i32>,
            #[serde(default)]
            output_tokens: Option<i32>,
            #[serde(default)]
            cache_read_input_tokens: Option<i32>,
            #[serde(default)]
            cache_write_input_tokens: Option<i32>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut present = 0u8;
        if raw.uncached_input_tokens.is_some() {
            present |= present::UNCACHED_INPUT;
        }
        if raw.output_tokens.is_some() {
            present |= present::OUTPUT;
        }
        if raw.cache_read_input_tokens.is_some() {
            present |= present::CACHE_READ;
        }
        if raw.cache_write_input_tokens.is_some() {
            present |= present::CACHE_WRITE;
        }
        // 缺失键填 0，与原先 `#[serde(default)]` 的行为完全一致。
        Ok(Self {
            uncached_input_tokens: raw.uncached_input_tokens.unwrap_or(0),
            output_tokens: raw.output_tokens.unwrap_or(0),
            cache_read_input_tokens: raw.cache_read_input_tokens.unwrap_or(0),
            cache_write_input_tokens: raw.cache_write_input_tokens.unwrap_or(0),
            present,
        })
    }
}

impl TokenUsage {
    /// 上游是否下发了全部四个键。`false` = 残缺 payload。
    pub fn is_complete(self) -> bool {
        self.present == present::ALL
    }

    /// 上游是否明确下发了 `uncachedInputTokens`（而非缺失后被填 0）。
    pub fn reports_uncached_input(self) -> bool {
        self.present & present::UNCACHED_INPUT != 0
    }

    /// 清理不可信上游值，确保所有计数非负。
    pub fn sanitized(self) -> Self {
        Self {
            uncached_input_tokens: self.uncached_input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_read_input_tokens: self.cache_read_input_tokens.max(0),
            cache_write_input_tokens: self.cache_write_input_tokens.max(0),
            // 钳负不改变「上游下发过哪些键」这个事实，原样带过。
            present: self.present,
        }
    }

    /// 四个字段是否全为零。
    ///
    /// 全零快照与「上游未下发 tokenUsage」在语义上**不可区分**：因为每个字段都带
    /// `#[serde(default)]`，一个只含无关字段的 payload 也会反序列化成全零快照，
    /// 而 `Option` 仍是 `Some`。调用方据此判断该快照是否值得信任。
    pub fn is_all_zero(self) -> bool {
        self.uncached_input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_input_tokens == 0
            && self.cache_write_input_tokens == 0
    }

    /// 上游是否完全没有回报缓存分项（读写两个 cache 字段都为零）。
    pub fn reports_no_cache(self) -> bool {
        self.cache_read_input_tokens == 0 && self.cache_write_input_tokens == 0
    }

    #[cfg(test)]
    /// OpenAI 口径的总输入 token（缓存读取是其中的子集）。
    pub fn total_input_tokens(self) -> i32 {
        let usage = self.sanitized();
        usage
            .uncached_input_tokens
            .saturating_add(usage.cache_write_input_tokens)
            .saturating_add(usage.cache_read_input_tokens)
    }

    /// 合并多次真实 provider 调用的用量。
    pub fn saturating_add(self, other: Self) -> Self {
        let left = self.sanitized();
        let right = other.sanitized();
        Self {
            uncached_input_tokens: left
                .uncached_input_tokens
                .saturating_add(right.uncached_input_tokens),
            output_tokens: left.output_tokens.saturating_add(right.output_tokens),
            cache_read_input_tokens: left
                .cache_read_input_tokens
                .saturating_add(right.cache_read_input_tokens),
            cache_write_input_tokens: left
                .cache_write_input_tokens
                .saturating_add(right.cache_write_input_tokens),
            // 取交集：合并后只有两侧都下发过的键才算「上游确实报了」。
            // 用并集会让一份残缺 payload 被另一份完整的掩盖掉。
            present: left.present & right.present,
        }
    }
}

/// `metadataEvent` payload。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEvent {
    /// 有些 metadataEvent 只携带 stopReason，因此 tokenUsage 必须保持可选。
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

impl EventPayload for MetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_token_usage_shape() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{
                "tokenUsage": {
                    "uncachedInputTokens": 101,
                    "outputTokens": 23,
                    "cacheReadInputTokens": 300,
                    "cacheWriteInputTokens": 40
                },
                "stopReason": "end_turn"
            }"#,
        )
        .unwrap();

        let usage = event.token_usage.unwrap();
        assert_eq!(usage.uncached_input_tokens, 101);
        assert_eq!(usage.output_tokens, 23);
        assert_eq!(usage.cache_read_input_tokens, 300);
        assert_eq!(usage.cache_write_input_tokens, 40);
        assert_eq!(usage.total_input_tokens(), 441);
    }

    #[test]
    fn metadata_without_token_usage_is_not_treated_as_zero_truth() {
        let event: MetadataEvent = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert!(event.token_usage.is_none());
    }

    #[test]
    fn token_usage_with_missing_fields_defaults_only_missing_fields_to_zero() {
        let event: MetadataEvent =
            serde_json::from_str(r#"{"tokenUsage":{"outputTokens":9}}"#).unwrap();

        // present 只置 OUTPUT 位：这正是「残缺 payload」的指纹 —— 缺失的三个键填 0，
        // 但 present 记下了它们从未出现过，与「上游明确报 0」可区分。
        assert_eq!(
            event.token_usage,
            Some(TokenUsage {
                present: present::OUTPUT,
                uncached_input_tokens: 0,
                output_tokens: 9,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            })
        );
        // 反过来验证判据：这份 payload 不完整，且没报过 uncachedInputTokens。
        let usage = event.token_usage.unwrap();
        assert!(!usage.is_complete());
        assert!(!usage.reports_uncached_input());
    }

    /// 这条是本次改动的存在理由：同样解析出 `uncached_input_tokens == 0`，
    /// 「键缺失」与「上游明确报 0」必须能分开——前者是残缺 payload（真 bug），
    /// 后者是整个 prompt 全命中缓存（合法）。`#[serde(default)]` 曾把两者抹平。
    #[test]
    fn absent_key_is_distinguishable_from_an_explicit_zero() {
        // 缺失：payload 里根本没有 uncachedInputTokens
        let absent: MetadataEvent = serde_json::from_str(
            r#"{"tokenUsage":{"outputTokens":7,"cacheReadInputTokens":5000}}"#,
        )
        .unwrap();
        let absent = absent.token_usage.unwrap();

        // 显式 0：上游明确报了 0
        let explicit: MetadataEvent = serde_json::from_str(
            r#"{"tokenUsage":{"uncachedInputTokens":0,"outputTokens":7,"cacheReadInputTokens":5000}}"#,
        )
        .unwrap();
        let explicit = explicit.token_usage.unwrap();

        // 数值上完全一样——这正是旧探针无法定性的原因
        assert_eq!(absent.uncached_input_tokens, explicit.uncached_input_tokens);
        assert_eq!(
            absent.cache_read_input_tokens,
            explicit.cache_read_input_tokens
        );

        // present 位掩码把两者分开
        assert!(!absent.reports_uncached_input(), "缺失键应判为未下发");
        assert!(explicit.reports_uncached_input(), "显式 0 应判为已下发");
    }

    /// `sanitized()` 只钳负，不能顺手抹掉「上游下发过哪些键」这个事实。
    #[test]
    fn sanitized_preserves_the_present_mask() {
        let usage = TokenUsage {
            uncached_input_tokens: -5,
            output_tokens: 3,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            present: present::UNCACHED_INPUT | present::OUTPUT,
        };
        let clean = usage.sanitized();
        assert_eq!(clean.uncached_input_tokens, 0, "负值应被钳到 0");
        assert_eq!(clean.present, usage.present, "present 必须原样保留");
    }

    /// 合并多跳用量时 present 取交集：一份完整 payload 不该掩盖另一份的残缺。
    #[test]
    fn saturating_add_intersects_the_present_mask() {
        let complete = TokenUsage {
            uncached_input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 1,
            cache_write_input_tokens: 1,
            present: present::ALL,
        };
        let partial = TokenUsage {
            uncached_input_tokens: 0,
            output_tokens: 4,
            cache_read_input_tokens: 2,
            cache_write_input_tokens: 0,
            present: present::OUTPUT | present::CACHE_READ,
        };

        let merged = complete.saturating_add(partial);
        assert_eq!(merged.output_tokens, 9, "数值照常累加");
        assert_eq!(
            merged.present,
            present::OUTPUT | present::CACHE_READ,
            "取交集，残缺不被掩盖"
        );
        assert!(!merged.is_complete());
        assert!(!merged.reports_uncached_input());
    }

    /// 全零快照与「未下发 tokenUsage」在语义上不可区分，调用方必须显式识别。
    #[test]
    fn all_zero_snapshot_is_indistinguishable_from_missing() {
        assert!(TokenUsage::default().is_all_zero());
        let event: MetadataEvent = serde_json::from_str(r#"{"tokenUsage":{}}"#).unwrap();
        assert!(event.token_usage.unwrap().is_all_zero());
    }

    /// 危险情形：部分 payload 不是全零（躲过 `is_all_zero`），但缓存分项确实缺失。
    /// 此时该快照优先级仍最高，会把本地 CacheMeter 的模拟值一并丢弃。
    #[test]
    fn partial_payload_evades_all_zero_check_but_lacks_cache_accounting() {
        let event: MetadataEvent =
            serde_json::from_str(r#"{"tokenUsage":{"outputTokens":9}}"#).unwrap();
        let usage = event.token_usage.unwrap();

        assert!(!usage.is_all_zero(), "有 outputTokens 就不算全零");
        assert!(usage.reports_no_cache(), "但缓存分项确实缺失");
    }

    #[test]
    fn full_snapshot_reports_cache_and_is_not_all_zero() {
        let usage = TokenUsage {
            uncached_input_tokens: 101,
            output_tokens: 23,
            cache_read_input_tokens: 300,
            cache_write_input_tokens: 40,
            ..Default::default()
        };
        assert!(!usage.is_all_zero());
        assert!(!usage.reports_no_cache());
    }

    #[test]
    fn sanitizes_negative_values() {
        let usage = TokenUsage {
            uncached_input_tokens: -1,
            output_tokens: -2,
            cache_read_input_tokens: -3,
            cache_write_input_tokens: -4,
            ..Default::default()
        }
        .sanitized();

        assert_eq!(usage, TokenUsage::default());
    }

    #[test]
    fn adds_multiple_provider_calls_without_overflowing() {
        let first = TokenUsage {
            uncached_input_tokens: i32::MAX,
            output_tokens: 3,
            cache_read_input_tokens: 20,
            cache_write_input_tokens: 4,
            ..Default::default()
        };
        let second = TokenUsage {
            uncached_input_tokens: 7,
            output_tokens: 5,
            cache_read_input_tokens: 11,
            cache_write_input_tokens: 2,
            ..Default::default()
        };

        assert_eq!(
            first.saturating_add(second),
            TokenUsage {
                uncached_input_tokens: i32::MAX,
                output_tokens: 8,
                cache_read_input_tokens: 31,
                cache_write_input_tokens: 6,
                ..Default::default()
            }
        );
    }
}
