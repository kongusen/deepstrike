//! Capability management impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopStateMachine};

impl LoopStateMachine {
    /// Drop capability leases whose expiry turn has passed. Runs at the head of
    /// every event so expired temporary capabilities are unmounted promptly.
    pub(super) fn sweep_expired_leases(&mut self) {
        let current_turn = self.turn;
        let mut to_remove = Vec::new();
        for cap in self.ctx.capabilities.capabilities() {
            if let Some(ref lease) = cap.lease {
                if current_turn >= lease.expires_at_turn {
                    to_remove.push((cap.kind, cap.id.to_string()));
                }
            }
        }
        for (kind, id) in to_remove {
            self.unmount_capability(kind, &id);
        }
    }

    /// Emit a `CapabilityChanged` observation for the current turn.
    /// Single construction site for all mount/unmount/replace/pin changes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_capability_change(
        &mut self,
        added: Vec<String>,
        removed: Vec<String>,
        change_kind: &str,
        capability_id: Option<String>,
        version: Option<String>,
        mounted_by: Option<String>,
        mount_reason: Option<String>,
    ) {
        self.observations.push(KernelObservation::CapabilityChanged {
            turn: self.turn,
            added,
            removed,
            change_kind: Some(change_kind.to_string()),
            capability_id,
            version,
            mounted_by,
            mount_reason,
        });
    }

    pub fn execute_capability_command(&mut self, cmd: crate::types::capability::CapabilityCommand) {
        use crate::types::capability::CapabilityCommand;
        match cmd {
            CapabilityCommand::Mount {
                capability,
                mounted_by,
                mount_reason,
            } => {
                self.mount_capability(capability, mounted_by, mount_reason);
            }
            CapabilityCommand::Unmount { kind, id } => {
                self.unmount_capability(kind, &id);
            }
            CapabilityCommand::Replace {
                old_kind,
                old_id,
                new_capability,
            } => {
                let new_id = new_capability.id.to_string();
                let version = new_capability.version.clone();
                let old_kind_str = old_kind.label();
                let new_kind_str = new_capability.kind.label();

                self.ctx.capabilities.remove(old_kind, &old_id);
                self.ctx.capabilities.upsert(new_capability);

                self.push_capability_change(
                    vec![format!("{}:{}", new_kind_str, new_id)],
                    vec![format!("{}:{}", old_kind_str, old_id)],
                    "replace",
                    Some(new_id),
                    version,
                    None,
                    None,
                );
            }
            CapabilityCommand::Pin { kind, id } => {
                let version = self
                    .ctx
                    .capabilities
                    .get_mut(kind, &id)
                    .and_then(|c| c.version.clone());
                if let Some(cap) = self.ctx.capabilities.get_mut(kind, &id) {
                    cap.is_pinned = true;
                    self.push_capability_change(
                        vec![],
                        vec![],
                        "pin",
                        Some(id),
                        version,
                        None,
                        None,
                    );
                }
            }
        }
    }

    pub fn mount_capability(
        &mut self,
        mut descriptor: crate::types::capability::CapabilityDescriptor,
        mounted_by: Option<String>,
        mount_reason: Option<String>,
    ) {
        if mounted_by.is_some() {
            descriptor.mounted_by = mounted_by.clone();
        }
        if mount_reason.is_some() {
            descriptor.mount_reason = mount_reason.clone();
        }
        let id = descriptor.id.to_string();
        let kind_str = descriptor.kind.label();
        let version = descriptor.version.clone();
        self.ctx.capabilities.upsert(descriptor);
        self.push_capability_change(
            vec![format!("{}:{}", kind_str, id)],
            vec![],
            "mount",
            Some(id),
            version,
            mounted_by,
            mount_reason,
        );
    }

    pub fn unmount_capability(&mut self, kind: crate::types::capability::CapabilityKind, id: &str) {
        let version = self
            .ctx
            .capabilities
            .get_mut(kind, id)
            .and_then(|c| c.version.clone());
        self.ctx.capabilities.remove(kind, id);
        let kind_str = kind.label();
        self.push_capability_change(
            vec![],
            vec![format!("{}:{}", kind_str, id)],
            "unmount",
            Some(id.to_string()),
            version,
            None,
            None,
        );
    }
}
