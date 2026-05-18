use super::partitions::Partition;
use super::pressure::PressureAction;
use crate::types::message::{Content, ContentPart};
use crate::context::partitions::ContextPartitions;

/// Trait for compression strategies.
pub trait Compressor: Send + Sync {
    fn compress(&self, partition: &mut Partition, target_tokens: u32) -> u32;
}

/// rho > 0.70: Truncate long text segments in messages.
pub struct SnipCompactor {
    pub max_chars: usize,
}

impl Default for SnipCompactor {
    fn default() -> Self {
        Self { max_chars: 2000 }
    }
}

impl Compressor for SnipCompactor {
    fn compress(&self, partition: &mut Partition, _target_tokens: u32) -> u32 {
        let mut saved: u32 = 0;
        for msg in &mut partition.messages {
            if let Content::Text(ref text) = msg.content {
                if text.len() > self.max_chars {
                    let original_tokens = msg.token_count.unwrap_or(0);
                    let truncated = format!("{}... [truncated]", &text[..self.max_chars]);
                    let ratio = truncated.len() as f64 / text.len() as f64;
                    let new_tokens = (original_tokens as f64 * ratio) as u32;
                    msg.content = Content::Text(truncated);
                    msg.token_count = Some(new_tokens);
                    saved += original_tokens.saturating_sub(new_tokens);
                }
            }
        }
        partition.token_count = partition.token_count.saturating_sub(saved);
        saved
    }
}

/// rho > 0.80: Replace tool results with compact placeholders.
pub struct MicroCompactor;

impl Compressor for MicroCompactor {
    fn compress(&self, partition: &mut Partition, _target_tokens: u32) -> u32 {
        let mut saved: u32 = 0;
        for msg in &mut partition.messages {
            if let Content::Parts(ref parts) = msg.content {
                let call_id = parts.iter().find_map(|p| {
                    if let ContentPart::ToolResult { call_id, .. } = p {
                        Some(call_id.clone())
                    } else {
                        None
                    }
                });
                if let Some(call_id) = call_id {
                    let original_tokens = msg.token_count.unwrap_or(0);
                    msg.content = Content::Text(format!("[tool result cached: {call_id}]"));
                    let new_tokens = 5;
                    msg.token_count = Some(new_tokens);
                    saved += original_tokens.saturating_sub(new_tokens);
                }
            }
        }
        partition.token_count = partition.token_count.saturating_sub(saved);
        saved
    }
}

/// rho > 0.90: Drop oldest messages from partition until within target.
pub struct CollapseCompactor;

impl Compressor for CollapseCompactor {
    fn compress(&self, partition: &mut Partition, target_tokens: u32) -> u32 {
        let mut saved: u32 = 0;
        let mut n = 0;
        for msg in &partition.messages {
            if partition.token_count.saturating_sub(saved) <= target_tokens {
                break;
            }
            saved += msg.token_count.unwrap_or(0);
            n += 1;
        }
        partition.messages.drain(..n);
        partition.token_count = partition.token_count.saturating_sub(saved);
        saved
    }
}

/// rho > 0.95: Aggressive — summarize history to a single placeholder.
pub struct AutoCompactor;

impl Compressor for AutoCompactor {
    fn compress(&self, partition: &mut Partition, _target_tokens: u32) -> u32 {
        if partition.messages.is_empty() {
            return 0;
        }
        let saved = partition.token_count.saturating_sub(10);
        let summary = format!("[{} messages compressed]", partition.messages.len());
        partition.messages.clear();
        partition.messages.push(crate::types::message::Message::user(summary));
        partition.messages[0].token_count = Some(10);
        partition.token_count = 10;
        saved
    }
}

/// Compression pipeline — all compressors operate only on the `history`
/// partition.  `memory` and `skill` partitions are excluded:
///   - `memory` is a reserved slot (currently empty after dynamic-retrieval redesign).
///   - `skill` messages are never populated; schemas flow through the tools
///     parameter in CallLLM, not through the message partitions.
pub struct CompressionPipeline {
    stages: Vec<(PressureAction, Box<dyn Compressor>)>,
    /// Target pressure after compression (fraction of max_tokens).
    /// Compressing to 70 % leaves headroom for the next response turn.
    target_fraction: f64,
}

impl CompressionPipeline {
    pub fn new() -> Self {
        Self {
            stages: vec![
                (PressureAction::SnipCompact,     Box::new(SnipCompactor::default())),
                (PressureAction::MicroCompact,    Box::new(MicroCompactor)),
                (PressureAction::ContextCollapse, Box::new(CollapseCompactor)),
                (PressureAction::AutoCompact,     Box::new(AutoCompactor)),
            ],
            target_fraction: 0.70,
        }
    }

    /// Run compression on the **history** partition only for the given pressure
    /// action, targeting `max_tokens * 0.70` tokens after compression.
    ///
    /// Pressure escalation:
    ///   SnipCompact     (rho > 0.70): snip long texts in history
    ///   MicroCompact    (rho > 0.80): replace tool-result bodies with placeholders
    ///   ContextCollapse (rho > 0.90): drop oldest history turns
    ///   AutoCompact     (rho > 0.95): collapse entire history to one placeholder
    pub fn compress(
        &self,
        partitions: &mut ContextPartitions,
        action: PressureAction,
        max_tokens: u32,
    ) -> u32 {
        if action == PressureAction::None {
            return 0;
        }
        let target = (max_tokens as f64 * self.target_fraction) as u32;

        if let Some(compressor) = self.compressor_for(action) {
            compressor.compress(&mut partitions.history, target)
        } else {
            0
        }
    }

    fn compressor_for(&self, action: PressureAction) -> Option<&dyn Compressor> {
        self.stages
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, c)| c.as_ref())
    }
}

impl Default for CompressionPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::partitions::ContextPartitions;
    use crate::types::message::Message;

    const MAX: u32 = 1_000;

    #[test]
    fn snip_compactor_truncates_long_text() {
        let compactor = SnipCompactor { max_chars: 10 };
        let mut ctx = ContextPartitions::new();
        ctx.history.push(Message::user("a".repeat(100)), 200);
        let saved = compactor.compress(&mut ctx.history, 0);
        assert!(saved > 0);
        if let Content::Text(ref t) = ctx.history.messages[0].content {
            assert!(t.ends_with("... [truncated]"));
        }
    }

    #[test]
    fn collapse_drops_oldest() {
        let compactor = CollapseCompactor;
        let mut ctx = ContextPartitions::new();
        ctx.history.push(Message::user("first"), 50);
        ctx.history.push(Message::user("second"), 50);
        ctx.history.push(Message::user("third"), 50);
        let saved = compactor.compress(&mut ctx.history, 60);
        assert!(saved > 0);
        assert!(ctx.history.token_count <= 60);
    }

    #[test]
    fn pipeline_targets_70_percent_of_max_tokens() {
        let pipeline = CompressionPipeline::new();
        let mut ctx = ContextPartitions::new();
        for _ in 0..20 {
            ctx.history.push(Message::user("msg".repeat(50)), 50);
        }
        // total = 1000; target = 700
        pipeline.compress(&mut ctx, PressureAction::ContextCollapse, MAX);
        assert!(ctx.history.token_count <= (MAX as f64 * 0.70) as u32 + 50); // within one message
    }

    #[test]
    fn pipeline_only_touches_history_not_memory() {
        let pipeline = CompressionPipeline::new();
        let mut ctx = ContextPartitions::new();
        ctx.memory.push(Message::user("memory"), 200);
        for _ in 0..5 {
            ctx.history.push(Message::user("hist"), 50);
        }
        let memory_before = ctx.memory.token_count;
        pipeline.compress(&mut ctx, PressureAction::AutoCompact, MAX);
        assert_eq!(ctx.memory.token_count, memory_before);
    }

    #[test]
    fn autocompact_collapses_history_to_placeholder() {
        let pipeline = CompressionPipeline::new();
        let mut ctx = ContextPartitions::new();
        for i in 0..10 {
            ctx.history.push(Message::user(format!("msg {i}")), 50);
        }
        pipeline.compress(&mut ctx, PressureAction::AutoCompact, MAX);
        assert_eq!(ctx.history.messages.len(), 1);
        if let Content::Text(ref t) = ctx.history.messages[0].content {
            assert!(t.contains("compressed"));
        }
    }
}
