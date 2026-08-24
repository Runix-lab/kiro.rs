//! 事件模型
//!
//! 定义 generateAssistantResponse 流式响应的事件类型

mod assistant;
mod base;
mod context_usage;
mod metadata;
mod metering;
mod reasoning;
mod tool_use;

pub use assistant::AssistantResponseEvent;
pub(crate) use assistant::strip_tool_use_xml_leaks;
pub use base::Event;
pub use context_usage::ContextUsageEvent;
pub use metadata::{MetadataEvent, TokenUsage, probe_untrusted_token_usage};
/// `present` 位掩码常量。目前只有测试需要构造特定掩码；生产代码用
/// `TokenUsage::is_complete()` / `reports_uncached_input()` 这些判据方法，不直接碰位。
#[cfg(test)]
pub use metadata::present;
pub use metering::MeteringEvent;
pub use reasoning::ReasoningContentEvent;
pub use tool_use::ToolUseEvent;
