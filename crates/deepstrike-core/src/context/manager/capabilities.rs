//! What the model may call: skills, the capability manifest and the kernel's own tool schemas.

use super::*;

impl ContextManager {
    // ── Section pinning ───────────────────────────────────────────────────────

    // ── Skills ────────────────────────────────────────────────────────────────

    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.capabilities.remove_kind(CapabilityKind::Skill);
        for skill in &skills {
            self.capabilities.add_skill(skill.clone());
        }
        self.skills.set_available(skills);
    }

    /// P1-B/D: set the stable-core tool ids (always exposed under skill gating). Replaces any prior.
    pub fn set_stable_core_tools(&mut self, ids: impl IntoIterator<Item = CompactString>) {
        self.stable_core_tools = ids.into_iter().collect();
    }

    /// P1-B: record that the model has loaded a skill (its content is now in context). Returns
    /// `true` if this changed the active set — an epoch boundary the SDK can use to re-anchor the
    /// prompt cache (D). Re-activating an already-active skill refreshes its lease (K3) but
    /// returns false (no epoch change).
    pub fn activate_skill(&mut self, name: impl Into<CompactString>) -> bool {
        self.activate_skill_leased(name, None)
    }

    /// K3: activate with an optional lease expiry turn (`None` = permanent). Same epoch semantics
    /// as [`Self::activate_skill`]; a re-activation overwrites the prior lease (latest wins).
    pub fn activate_skill_leased(
        &mut self,
        name: impl Into<CompactString>,
        expires_at_turn: Option<u32>,
    ) -> bool {
        self.active_skills
            .insert(name.into(), expires_at_turn)
            .is_none()
    }

    /// A refused `skill` activation: withdraw the `skill:<name>` knowledge the host staged for it,
    /// unless the skill is already active under an earlier admission.
    pub fn retract_refused_skill(&mut self, name: &str) -> bool {
        if self.active_skills.contains_key(name) {
            return false;
        }
        self.partitions
            .knowledge
            .retract_staged(&format!("skill:{name}"))
    }

    /// K3: deactivate a skill — the toolset re-widens at the next `emit_call_llm` (an epoch event,
    /// same cache cost class as activation) and the skill's `skill:<name>` knowledge pin is marked
    /// for the next boundary sweep. Errs-open: not-active is a no-op (returns false).
    pub fn deactivate_skill(&mut self, name: &str) -> bool {
        if self.active_skills.remove(name).is_none() {
            return false;
        }
        let key = format!("skill:{name}");
        // A skill body restored from an older checkpoint may hold a permanent lease.
        if !self.partitions.knowledge.end_lease(LeaseEvent::SkillDeactivated, &key) {
            self.partitions.knowledge.remove(&key);
        }
        true
    }

    /// K3: expire skill leases whose turn has passed (mirrors the capability lease sweep — runs at
    /// the head of every event). Each expiry takes the same path as an explicit deactivation.
    pub fn sweep_expired_skill_leases(&mut self, current_turn: u32) {
        let expired: Vec<CompactString> = self
            .active_skills
            .iter()
            .filter(|(_, lease)| lease.is_some_and(|t| current_turn >= t))
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired {
            self.deactivate_skill(&name);
            // P9: lease expiry re-widens the toolset invisibly otherwise — tell the model.
            self.partitions
                .signals
                .push(SignalItem::new(crate::context::voice::skill_lease_expired(&name)));
        }
    }

    /// P1-B: the tool-id allow-set to narrow the exposed toolset to, given the active skills.
    /// Returns `None` ⇒ **do not narrow** (no skill active, or some active skill declares no
    /// `allowed_tools` ⇒ unbounded, errs-open per D3). `Some(set)` ⇒ narrow to `set` (the union of
    /// every active skill's declared tools). Meta-tools and stable-core are layered on in
    /// `emit_call_llm`, not here.
    pub fn active_skill_tool_filter(&self) -> Option<std::collections::HashSet<CompactString>> {
        if self.active_skills.is_empty() {
            return None;
        }
        let mut union = std::collections::HashSet::new();
        for name in self.active_skills.keys() {
            let declared = self.skills.allowed_tools(name);
            if declared.is_empty() {
                return None; // an unrestricted active skill ⇒ no narrowing (D3)
            }
            union.extend(declared.iter().cloned());
        }
        Some(union)
    }

    /// Fine-grained authority contributed by active skills. Activation is checked by the canonical
    /// driver before it mutates this set; deactivation and lease expiry remove grants by removing
    /// the skill name from `active_skills`.
    pub fn active_skill_capabilities(&self) -> Vec<Capability> {
        self.active_skills
            .keys()
            .flat_map(|name| self.skills.capability_grants(name).iter().cloned())
            .collect()
    }

    pub fn skill_capability_grants(&self, name: &str) -> &[Capability] {
        self.skills.capability_grants(name)
    }

    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
    }

    /// Whether the operation's catalog declares this skill (see
    /// [`SkillCatalog::is_available`](crate::context::skill_catalog::SkillCatalog::is_available)).
    pub fn skill_available(&self, name: &str) -> bool {
        self.skills.is_available(name)
    }

    // ── Meta-tools ────────────────────────────────────────────────────────────

    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Memory,
                MEMORY_TOOL_NAME,
                "Search long-term memory through the memory meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Memory, MEMORY_TOOL_NAME);
        }
    }

    pub fn set_propose_memory_enabled(&mut self, enabled: bool) {
        self.propose_memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Memory,
                PROPOSE_MEMORY_TOOL_NAME,
                "Propose a candidate for long-term memory review.",
            );
        } else {
            self.capabilities.remove(CapabilityKind::Memory, PROPOSE_MEMORY_TOOL_NAME);
        }
    }

    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.knowledge_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Knowledge,
                KNOWLEDGE_TOOL_NAME,
                "Search external knowledge through the knowledge meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Knowledge, KNOWLEDGE_TOOL_NAME);
        }
    }

    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.plan_tool_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Tool,
                "update_plan",
                "Update task plan and progress through the planning meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Tool, "update_plan");
        }
    }

    pub fn capability_inventory(&self) -> String {
        self.capabilities.format_inventory()
    }

    pub fn meta_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut tools = Vec::new();
        if let Some(t) = self.skill_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.memory_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.propose_memory_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.knowledge_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.plan_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.note_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.read_result_tool_schema() {
            tools.push(t);
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// Whether a dependency output or an evicted tool result is there to be read back.
    pub(super) fn anything_to_read_back(&self) -> bool {
        !self.dependency_bodies.is_empty()
            || self
                .handles
                .all()
                .iter()
                .any(|h| !h.residency.occupies_context())
    }

    /// Latch the `read_result` offer once there is something to read back. Called before every
    /// provider toolset is built; from then on the tool stays for the rest of the run.
    pub fn latch_read_result_offer(&mut self) {
        if !self.read_result_offered && self.anything_to_read_back() {
            self.read_result_offered = true;
        }
    }

    /// O7: the `read_result` meta-tool — re-fetch a tool result the kernel evicted from context
    /// (external / collapsed / paged out). Offered once at least one handle has left residency
    /// (or a dependency output is held), so runs that never evict see an unchanged toolset
    /// (golden fixtures and cache prefixes stay byte-stable); once offered it is never withdrawn
    /// ([`Self::read_result_offered`]). Content is host-resolved through the payload store — the
    /// kernel only advertises the capability.
    pub fn read_result_tool_schema(&self) -> Option<ToolSchema> {
        if !(self.read_result_offered || self.anything_to_read_back()) {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(READ_RESULT_TOOL_NAME),
            description: crate::context::voice::read_result_tool_description(),
            parameters: crate::context::voice::read_result_tool_parameters(),
        })
    }

    pub fn plan_tool_schema(&self) -> Option<ToolSchema> {
        if !self.plan_tool_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new("update_plan"),
            description: crate::context::voice::update_plan_tool_description(),
            parameters: crate::context::voice::update_plan_tool_parameters(),
        })
    }

    pub fn memory_tool_schema(&self) -> Option<ToolSchema> {
        if !self.memory_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(MEMORY_TOOL_NAME),
            description: crate::context::voice::memory_tool_description(),
            parameters: crate::context::voice::memory_tool_parameters(),
        })
    }

    pub fn propose_memory_tool_schema(&self) -> Option<ToolSchema> {
        if !self.propose_memory_enabled { return None; }
        Some(ToolSchema {
            name: CompactString::new(PROPOSE_MEMORY_TOOL_NAME),
            description: crate::context::voice::propose_memory_tool_description(),
            parameters: crate::context::voice::propose_memory_tool_parameters(),
        })
    }

    pub fn knowledge_tool_schema(&self) -> Option<ToolSchema> {
        if !self.knowledge_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(KNOWLEDGE_TOOL_NAME),
            description: crate::context::voice::knowledge_tool_description(),
            parameters: crate::context::voice::knowledge_tool_parameters(),
        })
    }
}
