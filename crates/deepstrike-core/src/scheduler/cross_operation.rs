//! spc_016-05: deterministic cross-operation message routing.
//!
//! The kernel never opens a transport, fetches a payload, or retains a credential. Hosts submit
//! delivery facts and carry the returned message over their own transport. This module owns only
//! authorization, ordering, retry, and the durable state needed to replay those decisions.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::mm::handle::ObjectDescriptor;
use crate::scheduler::tcb::TaskId;
use crate::types::capability::Capability;

pub const CROSS_OPERATION_IPC_ABI_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationAddress {
    pub operation_id: CompactString,
    pub task_id: TaskId,
}

impl OperationAddress {
    pub fn new(operation_id: impl Into<CompactString>, task_id: impl Into<TaskId>) -> Self {
        Self {
            operation_id: operation_id.into(),
            task_id: task_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadLocator {
    /// Host-owned opaque locator. It is never opened or interpreted by the kernel.
    pub reference: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossOperationMessage {
    pub id: CompactString,
    pub from: OperationAddress,
    pub to: OperationAddress,
    pub kind: CompactString,
    pub object: ObjectDescriptor,
    pub payload: PayloadLocator,
    /// Strictly increasing per destination, supplied by the Host's transport plane.
    pub sequence: u64,
    pub sent_at_turn: u32,
    pub ttl_turns: u32,
    /// A requested attenuation, never an authority grant to the receiver.
    #[serde(default)]
    pub delegated_capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRegistration {
    pub address: OperationAddress,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    InFlight,
    Delivered,
    Failed,
    Expired,
    Cancelled,
}

impl DeliveryState {
    fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::InFlight)
    }

    fn is_terminal(self) -> bool {
        !self.is_pending()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverySettlement {
    Delivered,
    RetryableFailure,
    PermanentFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryFailure {
    UnknownOperation,
    Cancelled,
    SourceDoesNotOwnObject,
    InvalidPayloadLocator,
    PayloadUnavailable,
    SenderPermissionDenied,
    ReceiverPermissionDenied,
    CapabilityAttenuationDenied,
    ExpiredCapabilityLease,
    ExpiredTtl,
    OutOfOrder,
    Backpressure,
    ConflictingDuplicate,
}

impl DeliveryFailure {
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::PayloadUnavailable | Self::Backpressure)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RouteOutcome {
    Accepted { state: DeliveryState },
    Duplicate { state: DeliveryState },
    Rejected { failure: DeliveryFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableDelivery {
    pub message: CrossOperationMessage,
    pub state: DeliveryState,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverQueueSnapshot {
    pub receiver: OperationAddress,
    pub message_ids: Vec<CompactString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverSequenceSnapshot {
    pub receiver: OperationAddress,
    pub last_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossOperationRouterSnapshot {
    pub version: u32,
    pub max_pending_per_recipient: usize,
    pub operations: Vec<OperationRegistration>,
    pub deliveries: Vec<DurableDelivery>,
    pub queues: Vec<ReceiverQueueSnapshot>,
    pub last_sequences: Vec<ReceiverSequenceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    UnsupportedVersion(u32),
    DuplicateOperation(OperationAddress),
    DuplicateDelivery(CompactString),
    DuplicateQueue(OperationAddress),
    DuplicateSequence(OperationAddress),
    SequenceForUnknownOperation(OperationAddress),
    DeliveryForUnknownOperation(CompactString),
    UnknownQueuedDelivery(CompactString),
    QueuedDeliveryForWrongReceiver(CompactString),
    NonQueuedDelivery(CompactString),
    DuplicateQueuedDelivery(CompactString),
    OutOfOrderQueue(CompactString),
}

/// Deterministic kernel-side routing state. Every map is converted to an ordered vector at the
/// checkpoint boundary, avoiding map-key JSON encoding quirks in cross-SDK snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrossOperationRouter {
    max_pending_per_recipient: usize,
    operations: std::collections::BTreeMap<OperationAddress, OperationRegistration>,
    deliveries: std::collections::BTreeMap<CompactString, DurableDelivery>,
    queues: std::collections::BTreeMap<OperationAddress, std::collections::VecDeque<CompactString>>,
    last_sequences: std::collections::BTreeMap<OperationAddress, u64>,
}

impl CrossOperationRouter {
    pub fn new(max_pending_per_recipient: usize) -> Self {
        Self {
            max_pending_per_recipient,
            ..Self::default()
        }
    }

    pub fn register(&mut self, registration: OperationRegistration) -> bool {
        self.operations
            .insert(registration.address.clone(), registration)
            .is_none()
    }

    pub fn operation(&self, address: &OperationAddress) -> Option<&OperationRegistration> {
        self.operations.get(address)
    }

    pub fn delivery(&self, id: &str) -> Option<&DurableDelivery> {
        self.deliveries.get(id)
    }

    pub fn route(
        &mut self,
        message: CrossOperationMessage,
        now_turn: u32,
        payload_availability: PayloadAvailability,
    ) -> RouteOutcome {
        if let Some(existing) = self.deliveries.get(message.id.as_str()) {
            return if existing.message == message {
                RouteOutcome::Duplicate {
                    state: existing.state,
                }
            } else {
                RouteOutcome::Rejected {
                    failure: DeliveryFailure::ConflictingDuplicate,
                }
            };
        }

        let Some(source) = self.operations.get(&message.from) else {
            return rejected(DeliveryFailure::UnknownOperation);
        };
        let Some(receiver) = self.operations.get(&message.to) else {
            return rejected(DeliveryFailure::UnknownOperation);
        };
        if source.cancelled || receiver.cancelled {
            return rejected(DeliveryFailure::Cancelled);
        }
        if message.object.owner != message.from.task_id {
            return rejected(DeliveryFailure::SourceDoesNotOwnObject);
        }
        if !valid_payload_locator(&message.object, &message.payload) {
            return rejected(DeliveryFailure::InvalidPayloadLocator);
        }
        if payload_availability == PayloadAvailability::Unavailable {
            return rejected(DeliveryFailure::PayloadUnavailable);
        }
        if expired(message.sent_at_turn, message.ttl_turns, now_turn) {
            return rejected(DeliveryFailure::ExpiredTtl);
        }
        if matching_expired_lease(&source.capabilities, "share", &message.object, now_turn)
            && !object_action_allowed(&source.capabilities, "share", &message.object, now_turn)
            || matching_expired_lease(&receiver.capabilities, "read", &message.object, now_turn)
                && !object_action_allowed(&receiver.capabilities, "read", &message.object, now_turn)
        {
            return rejected(DeliveryFailure::ExpiredCapabilityLease);
        }
        if !object_action_allowed(&source.capabilities, "share", &message.object, now_turn) {
            return rejected(DeliveryFailure::SenderPermissionDenied);
        }
        if !object_action_allowed(&receiver.capabilities, "read", &message.object, now_turn) {
            return rejected(DeliveryFailure::ReceiverPermissionDenied);
        }
        if !delegation_is_attenuated(
            &message.delegated_capabilities,
            &source.capabilities,
            &receiver.capabilities,
            now_turn,
        ) {
            return rejected(DeliveryFailure::CapabilityAttenuationDenied);
        }
        if self
            .last_sequences
            .get(&message.to)
            .is_some_and(|last| message.sequence <= *last)
        {
            return rejected(DeliveryFailure::OutOfOrder);
        }
        if self.pending_for(&message.to) >= self.max_pending_per_recipient {
            return rejected(DeliveryFailure::Backpressure);
        }

        self.last_sequences
            .insert(message.to.clone(), message.sequence);
        self.queues
            .entry(message.to.clone())
            .or_default()
            .push_back(message.id.clone());
        self.deliveries.insert(
            message.id.clone(),
            DurableDelivery {
                message,
                state: DeliveryState::Queued,
                attempts: 0,
            },
        );
        RouteOutcome::Accepted {
            state: DeliveryState::Queued,
        }
    }

    /// Returns the next delivery only after all earlier accepted deliveries for the receiver have
    /// settled. Host transport can therefore retry without changing receiver-visible order.
    pub fn dispatch_next(
        &mut self,
        receiver: &OperationAddress,
        now_turn: u32,
    ) -> Option<CrossOperationMessage> {
        if self
            .operations
            .get(receiver)
            .is_none_or(|operation| operation.cancelled)
            || self.has_in_flight_for(receiver)
        {
            return None;
        }

        loop {
            let id = self.queues.get_mut(receiver)?.pop_front()?;
            let delivery = self.deliveries.get_mut(id.as_str())?;
            if delivery.state != DeliveryState::Queued {
                continue;
            }
            if expired(
                delivery.message.sent_at_turn,
                delivery.message.ttl_turns,
                now_turn,
            ) {
                delivery.state = DeliveryState::Expired;
                continue;
            }
            delivery.state = DeliveryState::InFlight;
            delivery.attempts = delivery.attempts.saturating_add(1);
            return Some(delivery.message.clone());
        }
    }

    pub fn settle(
        &mut self,
        message_id: &str,
        settlement: DeliverySettlement,
        now_turn: u32,
    ) -> Option<DeliveryState> {
        let (receiver, state) = {
            let delivery = self.deliveries.get(message_id)?;
            (delivery.message.to.clone(), delivery.state)
        };
        if state.is_terminal() || state != DeliveryState::InFlight {
            return Some(state);
        }

        let next_state = match settlement {
            DeliverySettlement::Delivered => DeliveryState::Delivered,
            DeliverySettlement::PermanentFailure => DeliveryState::Failed,
            DeliverySettlement::RetryableFailure => {
                let delivery = self.deliveries.get(message_id)?;
                if expired(
                    delivery.message.sent_at_turn,
                    delivery.message.ttl_turns,
                    now_turn,
                ) || self
                    .operations
                    .get(&receiver)
                    .is_none_or(|operation| operation.cancelled)
                {
                    if self
                        .operations
                        .get(&receiver)
                        .is_some_and(|operation| operation.cancelled)
                    {
                        DeliveryState::Cancelled
                    } else {
                        DeliveryState::Expired
                    }
                } else {
                    self.queues
                        .entry(receiver)
                        .or_default()
                        .push_front(CompactString::from(message_id));
                    DeliveryState::Queued
                }
            }
        };
        self.deliveries.get_mut(message_id)?.state = next_state;
        Some(next_state)
    }

    /// Cancelling either endpoint is a durable cancellation fact for all nonterminal deliveries
    /// touching that operation. A later Host acknowledgement cannot resurrect them.
    pub fn cancel_operation(&mut self, address: &OperationAddress) -> usize {
        let Some(operation) = self.operations.get_mut(address) else {
            return 0;
        };
        operation.cancelled = true;
        let mut cancelled = 0;
        for delivery in self.deliveries.values_mut() {
            if (delivery.message.from == *address || delivery.message.to == *address)
                && delivery.state.is_pending()
            {
                delivery.state = DeliveryState::Cancelled;
                cancelled += 1;
            }
        }
        for queue in self.queues.values_mut() {
            queue.retain(|id| {
                self.deliveries
                    .get(id.as_str())
                    .is_some_and(|delivery| delivery.state == DeliveryState::Queued)
            });
        }
        cancelled
    }

    pub fn snapshot(&self) -> CrossOperationRouterSnapshot {
        CrossOperationRouterSnapshot {
            version: CROSS_OPERATION_IPC_ABI_VERSION,
            max_pending_per_recipient: self.max_pending_per_recipient,
            operations: self.operations.values().cloned().collect(),
            deliveries: self.deliveries.values().cloned().collect(),
            queues: self
                .queues
                .iter()
                .map(|(receiver, message_ids)| ReceiverQueueSnapshot {
                    receiver: receiver.clone(),
                    message_ids: message_ids.iter().cloned().collect(),
                })
                .collect(),
            last_sequences: self
                .last_sequences
                .iter()
                .map(|(receiver, last_sequence)| ReceiverSequenceSnapshot {
                    receiver: receiver.clone(),
                    last_sequence: *last_sequence,
                })
                .collect(),
        }
    }

    pub fn from_snapshot(snapshot: CrossOperationRouterSnapshot) -> Result<Self, SnapshotError> {
        if snapshot.version != CROSS_OPERATION_IPC_ABI_VERSION {
            return Err(SnapshotError::UnsupportedVersion(snapshot.version));
        }
        let mut router = Self::new(snapshot.max_pending_per_recipient);
        for operation in snapshot.operations {
            if !router.register(operation.clone()) {
                return Err(SnapshotError::DuplicateOperation(operation.address));
            }
        }
        for delivery in snapshot.deliveries {
            let mut delivery = delivery;
            // A checkpoint cannot include the host's acknowledgement. An in-flight delivery is
            // therefore replayed at least once after restart, with the same identity and sequence.
            if delivery.state == DeliveryState::InFlight {
                delivery.state = DeliveryState::Queued;
            }
            if router
                .deliveries
                .insert(delivery.message.id.clone(), delivery.clone())
                .is_some()
            {
                return Err(SnapshotError::DuplicateDelivery(delivery.message.id));
            }
            if !router.operations.contains_key(&delivery.message.from)
                || !router.operations.contains_key(&delivery.message.to)
            {
                return Err(SnapshotError::DeliveryForUnknownOperation(
                    delivery.message.id,
                ));
            }
        }
        for queue in snapshot.queues {
            if router.queues.contains_key(&queue.receiver) {
                return Err(SnapshotError::DuplicateQueue(queue.receiver));
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut last_sequence = None;
            for id in &queue.message_ids {
                if !seen.insert(id.clone()) {
                    return Err(SnapshotError::DuplicateQueuedDelivery(id.clone()));
                }
                let Some(delivery) = router.deliveries.get(id.as_str()) else {
                    return Err(SnapshotError::UnknownQueuedDelivery(id.clone()));
                };
                if delivery.message.to != queue.receiver {
                    return Err(SnapshotError::QueuedDeliveryForWrongReceiver(id.clone()));
                }
                if delivery.state != DeliveryState::Queued {
                    return Err(SnapshotError::NonQueuedDelivery(id.clone()));
                }
                if last_sequence.is_some_and(|last| delivery.message.sequence <= last) {
                    return Err(SnapshotError::OutOfOrderQueue(id.clone()));
                }
                last_sequence = Some(delivery.message.sequence);
            }
            router
                .queues
                .insert(queue.receiver, queue.message_ids.into_iter().collect());
        }
        let mut replay_ids: Vec<(OperationAddress, CompactString, u64)> = router
            .deliveries
            .values()
            .filter(|delivery| delivery.state == DeliveryState::Queued)
            .filter(|delivery| {
                !router
                    .queues
                    .get(&delivery.message.to)
                    .is_some_and(|queue| queue.contains(&delivery.message.id))
            })
            .map(|delivery| {
                (
                    delivery.message.to.clone(),
                    delivery.message.id.clone(),
                    delivery.message.sequence,
                )
            })
            .collect();
        replay_ids.sort_by_key(|(_, _, sequence)| *sequence);
        for (receiver, id, _) in replay_ids.into_iter().rev() {
            router.queues.entry(receiver).or_default().push_front(id);
        }
        for sequence in snapshot.last_sequences {
            if !router.operations.contains_key(&sequence.receiver) {
                return Err(SnapshotError::SequenceForUnknownOperation(
                    sequence.receiver,
                ));
            }
            if router
                .last_sequences
                .insert(sequence.receiver.clone(), sequence.last_sequence)
                .is_some()
            {
                return Err(SnapshotError::DuplicateSequence(sequence.receiver));
            }
        }
        // The persisted watermarks are an index, not an authority source. Recompute their lower
        // bound from durable messages so a partial/older snapshot cannot accept a sequence the
        // router had already seen before restart.
        for delivery in router.deliveries.values() {
            let watermark = router
                .last_sequences
                .entry(delivery.message.to.clone())
                .or_default();
            *watermark = (*watermark).max(delivery.message.sequence);
        }
        Ok(router)
    }

    fn pending_for(&self, receiver: &OperationAddress) -> usize {
        self.deliveries
            .values()
            .filter(|delivery| delivery.message.to == *receiver && delivery.state.is_pending())
            .count()
    }

    fn has_in_flight_for(&self, receiver: &OperationAddress) -> bool {
        self.deliveries.values().any(|delivery| {
            delivery.message.to == *receiver && delivery.state == DeliveryState::InFlight
        })
    }
}

fn rejected(failure: DeliveryFailure) -> RouteOutcome {
    RouteOutcome::Rejected { failure }
}

fn expired(sent_at_turn: u32, ttl_turns: u32, now_turn: u32) -> bool {
    now_turn >= sent_at_turn.saturating_add(ttl_turns)
}

fn valid_payload_locator(object: &ObjectDescriptor, payload: &PayloadLocator) -> bool {
    !payload.reference.is_empty()
        && !payload.digest.is_empty()
        && object.payload_ref.as_deref() == Some(payload.reference.as_str())
        && object.digest == payload.digest
        && object.size == payload.size
}

fn matching_expired_lease(
    capabilities: &[Capability],
    action: &str,
    object: &ObjectDescriptor,
    now_turn: u32,
) -> bool {
    let resource = format!("object:{}/{}", object.owner, object.id);
    capabilities.iter().any(|capability| {
        capability.actions.0.contains(action)
            && resource.starts_with(crate::types::capability::resource_prefix(
                &capability.resource,
            ))
            && capability
                .lease
                .as_ref()
                .is_some_and(|lease| lease.is_expired(now_turn))
    })
}

fn object_action_allowed(
    capabilities: &[Capability],
    action: &str,
    object: &ObjectDescriptor,
    now_turn: u32,
) -> bool {
    capabilities.iter().any(|capability| {
        !capability
            .lease
            .as_ref()
            .is_some_and(|lease| lease.is_expired(now_turn))
            && capability.actions.0.contains(action)
            && format!("object:{}/{}", object.owner, object.id).starts_with(
                crate::types::capability::resource_prefix(&capability.resource),
            )
    })
}

fn delegation_is_attenuated(
    delegated: &[Capability],
    sender: &[Capability],
    receiver: &[Capability],
    now_turn: u32,
) -> bool {
    delegated.iter().all(|requested| {
        requested
            .lease
            .as_ref()
            .is_none_or(|lease| !lease.is_expired(now_turn))
            && sender.iter().any(|parent| {
                parent.delegatable
                    && !parent
                        .lease
                        .as_ref()
                        .is_some_and(|lease| lease.is_expired(now_turn))
                    && crate::types::capability::is_attenuation_of(requested, parent)
                    && lease_attenuates(requested, parent)
            })
            && receiver.iter().any(|parent| {
                !parent
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.is_expired(now_turn))
                    && crate::types::capability::is_attenuation_of(requested, parent)
                    && lease_attenuates(requested, parent)
            })
    })
}

fn lease_attenuates(child: &Capability, parent: &Capability) -> bool {
    match (
        child.lease.as_ref().and_then(|lease| lease.expires_at_turn),
        parent
            .lease
            .as_ref()
            .and_then(|lease| lease.expires_at_turn),
    ) {
        (_, None) => true,
        (Some(child_expiry), Some(parent_expiry)) => child_expiry <= parent_expiry,
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::handle::{ObjectId, ObjectKind, Residency};
    use crate::types::capability::{
        ActionSet, CapabilityId, CapabilityKind, ConstraintSet, Lease, Principal, ResourceSelector,
    };

    fn address(operation: &str, task: &str) -> OperationAddress {
        OperationAddress::new(operation, task)
    }

    fn cap(actions: &[&str], delegatable: bool) -> Capability {
        Capability {
            id: CapabilityId("object-route".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:source/7".into()),
            actions: ActionSet(actions.iter().map(|action| (*action).into()).collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable,
            issuer: Principal("kernel-test".into()),
        }
    }

    fn external_object() -> ObjectDescriptor {
        ObjectDescriptor::external(
            ObjectId::from(7_u32),
            ObjectKind::Artifact,
            TaskId::from("source"),
            1,
            Residency::External {
                payload_ref: "host://payload/7".into(),
                digest: "sha256:abc".into(),
                original_size: 9,
            },
            "preview",
        )
    }

    fn message(id: &str, sequence: u64, ttl_turns: u32) -> CrossOperationMessage {
        CrossOperationMessage {
            id: id.into(),
            from: address("operation-a", "source"),
            to: address("operation-b", "sink"),
            kind: "artifact".into(),
            object: external_object(),
            payload: PayloadLocator {
                reference: "host://payload/7".into(),
                digest: "sha256:abc".into(),
                size: 9,
            },
            sequence,
            sent_at_turn: 5,
            ttl_turns,
            delegated_capabilities: vec![cap(&["read"], false)],
        }
    }

    fn router(max_pending: usize) -> CrossOperationRouter {
        let mut router = CrossOperationRouter::new(max_pending);
        assert!(router.register(OperationRegistration {
            address: address("operation-a", "source"),
            capabilities: vec![cap(&["share", "read"], true)],
            cancelled: false,
        }));
        assert!(router.register(OperationRegistration {
            address: address("operation-b", "sink"),
            capabilities: vec![cap(&["read"], false)],
            cancelled: false,
        }));
        router
    }

    #[test]
    fn two_operations_route_by_address_with_payload_locator_and_receiver_attenuation() {
        let mut router = router(8);
        let original_receiver_caps = router
            .operation(&address("operation-b", "sink"))
            .unwrap()
            .capabilities
            .clone();
        let msg = message("message-1", 1, 20);

        assert_eq!(
            router.route(msg.clone(), 5, PayloadAvailability::Available),
            RouteOutcome::Accepted {
                state: DeliveryState::Queued
            }
        );
        let dispatched = router
            .dispatch_next(&address("operation-b", "sink"), 6)
            .expect("the host can now deliver the opaque locator");
        assert_eq!(dispatched.payload.reference, "host://payload/7");
        assert_eq!(
            router.settle("message-1", DeliverySettlement::Delivered, 6),
            Some(DeliveryState::Delivered)
        );
        assert_eq!(
            router
                .operation(&address("operation-b", "sink"))
                .unwrap()
                .capabilities,
            original_receiver_caps,
            "a sender's delegated capability must never modify receiver authority"
        );

        let mut widened = message("message-2", 2, 20);
        widened.delegated_capabilities = vec![cap(&["write"], false)];
        assert_eq!(
            router.route(widened, 5, PayloadAvailability::Available),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::CapabilityAttenuationDenied
            }
        );
    }

    #[test]
    fn restart_replay_preserves_dedupe_order_and_ttl() {
        let mut router = router(8);
        let first = message("message-1", 1, 10);
        let second = message("message-2", 2, 10);
        assert!(matches!(
            router.route(first.clone(), 5, PayloadAvailability::Available),
            RouteOutcome::Accepted { .. }
        ));
        assert!(matches!(
            router.route(second, 5, PayloadAvailability::Available),
            RouteOutcome::Accepted { .. }
        ));

        let json = serde_json::to_string(&router.snapshot()).expect("durable snapshot serializes");
        let snapshot: CrossOperationRouterSnapshot =
            serde_json::from_str(&json).expect("durable snapshot restores");
        let mut restored = CrossOperationRouter::from_snapshot(snapshot).expect("valid snapshot");
        assert_eq!(
            restored.route(first, 5, PayloadAvailability::Available),
            RouteOutcome::Duplicate {
                state: DeliveryState::Queued
            }
        );
        assert_eq!(
            restored.route(message("older", 1, 10), 5, PayloadAvailability::Available),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::OutOfOrder
            }
        );
        assert_eq!(
            restored
                .dispatch_next(&address("operation-b", "sink"), 6)
                .unwrap()
                .id,
            CompactString::from("message-1")
        );
        assert_eq!(
            restored.settle("message-1", DeliverySettlement::RetryableFailure, 6),
            Some(DeliveryState::Queued)
        );
        assert_eq!(
            restored.dispatch_next(&address("operation-b", "sink"), 15),
            None,
            "expired retry and later queued message are never dispatched after restart"
        );
        assert_eq!(
            restored.delivery("message-1").unwrap().state,
            DeliveryState::Expired
        );
        assert_eq!(
            restored.delivery("message-2").unwrap().state,
            DeliveryState::Expired
        );
    }

    #[test]
    fn restart_requeues_an_in_flight_delivery_for_at_least_once_replay() {
        let mut router = router(8);
        assert!(matches!(
            router.route(
                message("message-1", 1, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted { .. }
        ));
        assert!(router
            .dispatch_next(&address("operation-b", "sink"), 6)
            .is_some());

        let mut restored =
            CrossOperationRouter::from_snapshot(router.snapshot()).expect("valid snapshot");
        assert_eq!(
            restored.delivery("message-1").unwrap().state,
            DeliveryState::Queued
        );
        assert_eq!(
            restored
                .dispatch_next(&address("operation-b", "sink"), 7)
                .unwrap()
                .id,
            CompactString::from("message-1")
        );
    }

    #[test]
    fn snapshot_rebuilds_missing_sequence_watermarks_before_accepting_new_delivery() {
        let mut router = router(8);
        assert!(matches!(
            router.route(
                message("message-2", 2, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted { .. }
        ));
        let mut snapshot = router.snapshot();
        snapshot.last_sequences.clear();
        let mut restored =
            CrossOperationRouter::from_snapshot(snapshot).expect("watermark derives");

        assert_eq!(
            restored.route(
                message("message-1", 1, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::OutOfOrder
            }
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_or_out_of_order_receiver_queue_entries() {
        let mut router = router(8);
        assert!(matches!(
            router.route(
                message("message-1", 1, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted { .. }
        ));
        assert!(matches!(
            router.route(
                message("message-2", 2, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted { .. }
        ));
        let mut duplicate = router.snapshot();
        duplicate.queues[0].message_ids.push("message-1".into());
        assert!(matches!(
            CrossOperationRouter::from_snapshot(duplicate),
            Err(SnapshotError::DuplicateQueuedDelivery(id)) if id == "message-1"
        ));

        let mut out_of_order = router.snapshot();
        out_of_order.queues[0].message_ids.swap(0, 1);
        assert!(matches!(
            CrossOperationRouter::from_snapshot(out_of_order),
            Err(SnapshotError::OutOfOrderQueue(id)) if id == "message-1"
        ));
    }

    #[test]
    fn payload_failure_backpressure_expired_lease_and_cancellation_are_explicit() {
        let mut router = router(1);
        let first = message("message-1", 1, 20);
        assert_eq!(
            router.route(first.clone(), 5, PayloadAvailability::Unavailable),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::PayloadUnavailable
            }
        );
        assert!(DeliveryFailure::PayloadUnavailable.is_retryable());
        assert!(matches!(
            router.route(first, 5, PayloadAvailability::Available),
            RouteOutcome::Accepted { .. }
        ));
        assert_eq!(
            router.route(
                message("message-2", 2, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::Backpressure
            }
        );

        let source = address("operation-a", "source");
        router.operation(&source).unwrap();
        router.operations.get_mut(&source).unwrap().capabilities[0].lease = Some(Lease {
            expires_at_turn: Some(5),
        });
        assert_eq!(
            router.route(
                message("expired-lease", 2, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::ExpiredCapabilityLease
            }
        );
        assert_eq!(router.cancel_operation(&address("operation-b", "sink")), 1);
        assert_eq!(
            router.delivery("message-1").unwrap().state,
            DeliveryState::Cancelled
        );
        assert_eq!(
            router.route(
                message("after-cancel", 2, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::Cancelled
            }
        );
    }

    #[test]
    fn unrelated_expired_capability_does_not_block_an_authorized_delivery() {
        let mut router = router(8);
        let source = address("operation-a", "source");
        router
            .operations
            .get_mut(&source)
            .unwrap()
            .capabilities
            .push(Capability {
                id: CapabilityId("unrelated".into()),
                kind: CapabilityKind::Tool,
                resource: ResourceSelector("object:other/99".into()),
                actions: ActionSet(["read".into()].into_iter().collect()),
                constraints: ConstraintSet::default(),
                lease: Some(Lease {
                    expires_at_turn: Some(5),
                }),
                delegatable: false,
                issuer: Principal("kernel-test".into()),
            });

        assert_eq!(
            router.route(
                message("message-1", 1, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted {
                state: DeliveryState::Queued
            }
        );
    }

    #[test]
    fn an_expired_duplicate_capability_does_not_override_a_matching_live_capability() {
        let mut router = router(8);
        let source = address("operation-a", "source");
        router
            .operations
            .get_mut(&source)
            .unwrap()
            .capabilities
            .push(Capability {
                id: CapabilityId("expired-duplicate".into()),
                kind: CapabilityKind::Tool,
                resource: ResourceSelector("object:source/7".into()),
                actions: ActionSet(["share".into()].into_iter().collect()),
                constraints: ConstraintSet::default(),
                lease: Some(Lease {
                    expires_at_turn: Some(5),
                }),
                delegatable: false,
                issuer: Principal("kernel-test".into()),
            });

        assert!(matches!(
            router.route(
                message("message-1", 1, 20),
                5,
                PayloadAvailability::Available
            ),
            RouteOutcome::Accepted { .. }
        ));
    }

    #[test]
    fn delegated_capability_cannot_outlive_its_parent_or_arrive_expired() {
        let mut router = router(8);
        let source = address("operation-a", "source");
        let receiver = address("operation-b", "sink");
        router.operations.get_mut(&source).unwrap().capabilities[0].lease = Some(Lease {
            expires_at_turn: Some(10),
        });
        router.operations.get_mut(&receiver).unwrap().capabilities[0].lease = Some(Lease {
            expires_at_turn: Some(10),
        });

        let mut permanent = message("permanent", 1, 20);
        permanent.delegated_capabilities[0].lease = None;
        assert_eq!(
            router.route(permanent, 5, PayloadAvailability::Available),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::CapabilityAttenuationDenied
            }
        );

        let mut expired = message("expired", 1, 20);
        expired.delegated_capabilities[0].lease = Some(Lease {
            expires_at_turn: Some(5),
        });
        assert_eq!(
            router.route(expired, 5, PayloadAvailability::Available),
            RouteOutcome::Rejected {
                failure: DeliveryFailure::CapabilityAttenuationDenied
            }
        );
    }
}
