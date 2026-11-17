//! The prompt-cache ledger: what the last request sent and where its prefix broke.

use super::*;

impl ContextManager {
    /// The rendered context is about to go to a provider: from now on system[1] is cached.
    pub fn mark_prefix_sent(&mut self) {
        self.prefix_sent = true;
    }

    /// Plan task 8.0 · `rendered` goes to a provider with `tools` on `turn`: system[1] is cached
    /// from now on, and the ledger records where this request broke the previous one's prefix.
    pub fn note_request_sent(
        &mut self,
        rendered: &InternalRenderedContext,
        tools: &[ToolSchema],
        turn: u32,
    ) -> Option<crate::context::cache_ledger::PrefixBreak> {
        use crate::context::cache_ledger::{Segment, SentPrefix, SentTurn};
        self.mark_prefix_sent();
        let prev = self.cache_ledger.sent.as_ref();
        let known: std::collections::HashMap<&str, u32> = prev
            .into_iter()
            .flat_map(|sent| {
                [&sent.tools, &sent.system, &sent.knowledge]
                    .into_iter()
                    .chain(sent.turns.iter().map(|turn| &turn.segment))
            })
            .map(|segment| (segment.hash.as_str(), segment.tokens))
            .collect();
        let engine = &self.engine;
        let segment = |material: Vec<u8>, count: &dyn Fn() -> u32| {
            let mut segment = Segment::of(&material, 0);
            segment.tokens = known.get(segment.hash.as_str()).copied().unwrap_or_else(count);
            segment
        };
        let tool_material = serde_json::to_vec(tools).unwrap_or_default();
        let tools_segment = segment(tool_material.clone(), &|| {
            engine.count(&String::from_utf8_lossy(&tool_material))
        });
        let system = segment(rendered.system_stable.as_bytes().to_vec(), &|| {
            engine.count(&rendered.system_stable)
        });
        let knowledge = segment(rendered.system_knowledge.as_bytes().to_vec(), &|| {
            engine.count(&rendered.system_knowledge)
        });
        let turns = rendered
            .turns
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let item = rendered.turn_items.get(index).copied().unwrap_or(0);
                let replay = rendered.turn_replays.get(index).and_then(Option::as_ref);
                let material = serde_json::to_vec(&(
                    &message.role,
                    wire_content(&message.content),
                    &message.tool_calls,
                    replay,
                ))
                .unwrap_or_default();
                let replay_tokens = self.item_replays.get(&item).map_or(0, |(_, tokens)| *tokens);
                SentTurn {
                    segment: segment(material, &|| {
                        engine.count_message(message).saturating_add(replay_tokens)
                    }),
                    item,
                }
            })
            .collect();
        let next = SentPrefix {
            tools: tools_segment,
            system,
            knowledge,
            turns,
            tail: rendered.state_turn.as_ref().map_or(0, |state| engine.count_message(state)),
        };
        self.cache_ledger.observe(turn, next)
    }

    /// History tokens whose cache mid-run system[1] promotions discarded.
    pub fn cache_invalidated_tokens(&self) -> u64 {
        self.cache_invalidated_tokens
    }
}
