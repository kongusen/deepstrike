use super::partitions::Partition;
use super::pressure::PressureAction;
use crate::types::message::{Content, ContentPart};
use crate::context::partitions::ContextPartitions;

/// Trait for compression strategies.
pub trait Compressor: Send + Sync {
    /// Compress the partition to fit within target_tokens.
    /// Returns the number of tokens saved.
    fn compress(&self, partition: &mut Partition, target_tokens: u32) -> u32;
}

/// rho > 0.70: Truncate long text segments in messages.
pub struct SnipCompactor {
    /// Max characters per message before truncation.
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
                    // Rough estimate: tokens proportional to char reduction
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

/// rho > 0.90: Drop oldest messages from partition.
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

/// rho > 0.95: Aggressive — summarize to a single message placeholder.
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

/// Compression pipeline: runs compressors in order based on pressure level.
pub struct CompressionPipeline {
    stages: Vec<(PressureAction, Box<dyn Compressor>)>,
}

impl CompressionPipeline {
    pub fn new() -> Self {
        Self {
            stages: vec![
                (PressureAction::SnipCompact, Box::new(SnipCompactor::default())),
                (PressureAction::MicroCompact, Box::new(MicroCompactor)),
                (PressureAction::ContextCollapse, Box::new(CollapseCompactor)),
                (PressureAction::AutoCompact, Box::new(AutoCompactor)),
            ],
        }
    }

    /// Run compression on all compressible partitions for the given pressure action.
    /// Returns total tokens saved.
    ///
    /// Partition escalation by pressure level:
    /// - `SnipCompact` (rho>0.70): history only (text snip)
    /// - `MicroCompact` (rho>0.80): + skill (snip text-heavy skill descriptions)
    /// - `ContextCollapse` (rho>0.90): + drop oldest skill entries
    /// - `AutoCompact` (rho>0.95): aggressive on history; skill snip+collapse
    pub fn compress(&self, partitions: &mut ContextPartitions, action: PressureAction) -> u32 {
        if action == PressureAction::None {
            return 0;
        }
        let target = partitions.total_tokens() / 2;
        let mut total_saved: u32 = 0;

        // History uses the compressor designed for the current pressure band.
        if let Some(compressor) = self.compressor_for(action) {
            total_saved += compressor.compress(&mut partitions.history, target);
        }

        // Skill is text-heavy; tool-result caching (MicroCompactor) is a no-op on it.
        // Use SnipCompactor for shrinking and CollapseCompactor at higher pressure.
        if action >= PressureAction::MicroCompact && partitions.skill.compressible {
            total_saved += SnipCompactor::default().compress(&mut partitions.skill, target);
            if action >= PressureAction::ContextCollapse {
                total_saved += CollapseCompactor.compress(&mut partitions.skill, target);
            }
        }

        total_saved
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

    #[test]
    fn snip_compactor_truncates_long_text() {
        let compactor = SnipCompactor { max_chars: 10 };
        let mut ctx = ContextPartitions::new();
        ctx.history.push(Message::user("a]".repeat(100)), 200);

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
    fn pipeline_compresses_skill_at_micro_and_above() {
        let pipeline = CompressionPipeline::new();
        let mut ctx = ContextPartitions::new();
        // Long text in skill partition triggers SnipCompactor
        ctx.skill.push(Message::system("a".repeat(5_000)), 1_000);
        ctx.history.push(Message::user("hist"), 100);

        let before = ctx.skill.token_count;
        let saved = pipeline.compress(&mut ctx, PressureAction::MicroCompact);
        assert!(saved > 0);
        assert!(ctx.skill.token_count < before);
    }

    #[test]
    fn pipeline_skips_skill_at_snip_level() {
        // SnipCompact only touches history per design.
        let pipeline = CompressionPipeline::new();
        let mut ctx = ContextPartitions::new();
        ctx.skill.push(Message::system("a".repeat(5_000)), 1_000);
        ctx.history.push(Message::user("a".repeat(5_000)), 1_000);

        let skill_before = ctx.skill.token_count;
        pipeline.compress(&mut ctx, PressureAction::SnipCompact);
        assert_eq!(ctx.skill.token_count, skill_before);
        // history is touched
        assert!(ctx.history.token_count < 1_000);
    }
}
