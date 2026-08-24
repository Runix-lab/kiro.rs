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
                "metadataEvent.tokenUsage 四字段全零，与「未下发」不可区分；\
                 本次用量已退化为本地估算。详见 ANALYSIS.md §3.2"
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
    // 因此这条会让整个输入侧账目变成 0 —— 但它有两种相反的成因，光看解析后的值分不出：
    //   缺失键 → 残缺 payload（真 bug）；显式 0 → 整个 prompt 全部命中缓存（合法）。
    // `#[serde(default)]` 把两者抹成同一个值，所以这里只能先把现场打全，由人判读。
    // 彻底分辨需要在反序列化时记录键是否出现，见 ANALYSIS.md §3.2 末尾。
    if usage.uncached_input_tokens == 0 && usage.cache_read_input_tokens > 0 {
        let hits = INPUT_ZEROED_HITS.fetch_add(1, Ordering::Relaxed) + 1;
        if hits == 1 || hits % 100 == 0 {
            tracing::warn!(
                occurrences = hits,
                uncached_input_tokens = usage.uncached_input_tokens,
                output_tokens = usage.output_tokens,
                cache_read_input_tokens = usage.cache_read_input_tokens,
                cache_write_input_tokens = usage.cache_write_input_tokens,
                local_cache_covered_est,
                "上游 tokenUsage 的 uncachedInputTokens 为 0 而缓存读取有值，\
                 本次 input_tokens 将上报为 0；无法区分「残缺 payload」与「全命中缓存」。\
                 详见 ANALYSIS.md §3.2"
            );
        }
    }
}

/// 单次 Kiro 模型调用的精确 token 用量。
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 未命中缓存、也未写入缓存的输入 token。
    #[serde(default)]
    pub uncached_input_tokens: i32,
    /// 模型输出 token。
    #[serde(default)]
    pub output_tokens: i32,
    /// 从服务端 prompt cache 读取的输入 token。
    #[serde(default)]
    pub cache_read_input_tokens: i32,
    /// 本次写入服务端 prompt cache 的输入 token。
    #[serde(default)]
    pub cache_write_input_tokens: i32,
}

impl TokenUsage {
    /// 清理不可信上游值，确保所有计数非负。
    pub fn sanitized(self) -> Self {
        Self {
            uncached_input_tokens: self.uncached_input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_read_input_tokens: self.cache_read_input_tokens.max(0),
            cache_write_input_tokens: self.cache_write_input_tokens.max(0),
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

        assert_eq!(
            event.token_usage,
            Some(TokenUsage {
                uncached_input_tokens: 0,
                output_tokens: 9,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            })
        );
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
        };
        let second = TokenUsage {
            uncached_input_tokens: 7,
            output_tokens: 5,
            cache_read_input_tokens: 11,
            cache_write_input_tokens: 2,
        };

        assert_eq!(
            first.saturating_add(second),
            TokenUsage {
                uncached_input_tokens: i32::MAX,
                output_tokens: 8,
                cache_read_input_tokens: 31,
                cache_write_input_tokens: 6,
            }
        );
    }
}
