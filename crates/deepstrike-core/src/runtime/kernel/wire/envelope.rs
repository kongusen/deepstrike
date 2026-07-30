//! Input envelope and the five-class input taxonomy (spec §7.1, §7.2).

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

use super::command::HostCommand;
use super::config::OperationConfig;
use super::effect::EffectOutcome;
use super::event::ExternalEvent;
use super::root::{InitialContext, RootEntry};
use super::scalar::{EffectId, InputId, OperationId, SCALAR_ERROR_MARKER, WireU64};
use super::{KERNEL_ABI_VERSION, KernelBootstrapLimits};

// ---------------------------------------------------------------------------------------------
// revision
// ---------------------------------------------------------------------------------------------

/// The wire revision marker. Only [`KERNEL_ABI_VERSION`] decodes — §16.2 has no negotiation, no
/// adapter and no inference from missing fields, so the revision check belongs in the type, not
/// in one lucky code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbiRevision(u32);

impl AbiRevision {
    pub const CURRENT: Self = Self(KERNEL_ABI_VERSION);

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for AbiRevision {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl Serialize for AbiRevision {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for AbiRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RevisionVisitor;

        impl Visitor<'_> for RevisionVisitor {
            type Value = AbiRevision;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "the kernel ABI revision {KERNEL_ABI_VERSION}")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                if value == u64::from(KERNEL_ABI_VERSION) {
                    Ok(AbiRevision::CURRENT)
                } else {
                    Err(E::custom(format!(
                        "{SCALAR_ERROR_MARKER}: unsupported kernel ABI revision {value}; \
                         this kernel accepts only revision {KERNEL_ABI_VERSION}"
                    )))
                }
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if value < 0 {
                    return Err(E::custom(format!(
                        "{SCALAR_ERROR_MARKER}: unsupported kernel ABI revision {value}"
                    )));
                }
                self.visit_u64(value as u64)
            }
        }

        deserializer.deserialize_u32(RevisionVisitor)
    }
}

// ---------------------------------------------------------------------------------------------
// envelope
// ---------------------------------------------------------------------------------------------

/// The one shape a host may hand the kernel (spec §7.1 calls it `KernelEnvelope`).
///
/// Everything the kernel needs about *this delivery* lives here and nowhere else:
///
/// * `operation_id` — bound by the first accepted input, immutable afterwards;
/// * `input_id` — the caller-suppliable idempotency key (DEC-2). Retrying an intent with the
///   same key must reach the same durable record; hosts mint one only when the caller does not;
/// * `observed_at_ms` — the **only** host clock fact. No business input, effect outcome or
///   external event may carry a second wall clock (§11.2);
/// * `input` — the five-class business payload, which never repeats any of the above.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEnvelope {
    pub abi_version: AbiRevision,
    pub operation_id: OperationId,
    pub input_id: InputId,
    pub observed_at_ms: WireU64,
    pub input: KernelInput,
}

impl WireEnvelope {
    pub fn new(
        operation_id: OperationId,
        input_id: InputId,
        observed_at_ms: WireU64,
        input: KernelInput,
    ) -> Self {
        Self {
            abi_version: AbiRevision::CURRENT,
            operation_id,
            input_id,
            observed_at_ms,
            input,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// taxonomy
// ---------------------------------------------------------------------------------------------

/// The closed five-class input taxonomy (§7.2).
///
/// The classes exist for **authority and lifecycle**, not to shrink an enum: each one enters a
/// different validation path ([`KernelInput::authority`]) and is admissible in a different set of
/// lifecycle states ([`KernelInput::admissible_lifecycles`]). Both tables are exhaustive matches
/// — the historical `_ =>` catch-all that let any unlisted variant through while `Running` has no
/// equivalent here.
///
/// P1 syscalls are deliberately **not** a sixth class: a caller is always derived from a
/// kernel-owned pending effect or a task attempt, never declared by the host (§7.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelInput {
    /// Genesis input. Decoded inside the absolute bootstrap boundary and normalised into a
    /// resolved configuration before it becomes the first journal record.
    ConfigureOperation(ConfigureOperation),
    /// The single atomic root start: one entry, one initial context.
    StartOperation(StartOperation),
    /// The one entry point for the outcome of a kernel-owned pending effect.
    ResolveEffect(ResolveEffect),
    /// A fact the host observed (a signal, a child completion) — not an effect result.
    DeliverExternalEvent(DeliverExternalEvent),
    /// The live control plane: cancel, compaction, task/capability/knowledge/policy updates.
    HostControl(HostControl),
}

impl KernelInput {
    /// Which validation path this class enters. Distinct per class by construction.
    pub fn authority(&self) -> InputAuthority {
        match self {
            Self::ConfigureOperation(_) => InputAuthority::HostBootstrap,
            Self::StartOperation(_) => InputAuthority::HostRoot,
            Self::ResolveEffect(_) => InputAuthority::HostEffectResolution,
            Self::DeliverExternalEvent(_) => InputAuthority::HostObservedFact,
            Self::HostControl(_) => InputAuthority::HostControlPlane,
        }
    }

    /// Lifecycle states in which this class is admissible (§6.1). Terminal states appear in no
    /// list: after a terminal every state-changing input is refused, `DeliverSignal` included
    /// (DEC-4).
    pub fn admissible_lifecycles(&self) -> &'static [OperationLifecycle] {
        match self {
            Self::ConfigureOperation(_) => &[OperationLifecycle::Created],
            Self::StartOperation(_) => &[OperationLifecycle::Configured],
            Self::ResolveEffect(_) | Self::DeliverExternalEvent(_) => {
                &[OperationLifecycle::Running, OperationLifecycle::Suspended]
            }
            Self::HostControl(_) => &[
                OperationLifecycle::Configured,
                OperationLifecycle::Running,
                OperationLifecycle::Suspended,
            ],
        }
    }
}

/// The validation path an input class enters. One per class — the type-level statement that the
/// taxonomy is about authority rather than enum arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputAuthority {
    /// Boot configuration, admissible exactly once, before any execution exists.
    HostBootstrap,
    /// Root start authority: the host chooses the root entry, and only once.
    HostRoot,
    /// Resolution of an effect the kernel itself published and is still waiting on.
    HostEffectResolution,
    /// A fact the host observed about the outside world.
    HostObservedFact,
    /// Live control-plane commands against a running operation.
    HostControlPlane,
}

/// Operation lifecycle (§6). Wire-local on purpose: the canonical contract owns its own
/// vocabulary rather than borrowing the legacy protocol's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationLifecycle {
    Created,
    Configured,
    Running,
    Suspended,
    Completed,
    Cancelled,
    Failed,
}

impl OperationLifecycle {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// §7.2 · `ConfigureOperation { config }`. The 16 historical setup events collapse here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureOperation {
    pub config: OperationConfig,
}

/// §7.2 · `StartOperation { entry, initial_context }`. The five historical start-ish events
/// collapse into this one atomic input; `initial_context` lives here and **only** here, never
/// duplicated inside a [`RootEntry`] variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartOperation {
    pub entry: RootEntry,
    pub initial_context: InitialContext,
}

/// §7.2 · `ResolveEffect { effect_id, outcome }`. The 11 historical result events collapse here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveEffect {
    pub effect_id: EffectId,
    pub outcome: EffectOutcome,
}

/// §7.2 · `DeliverExternalEvent { event }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverExternalEvent {
    pub event: ExternalEvent,
}

/// §7.2 · `HostControl { command }`. The 10 historical control events collapse here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostControl {
    pub command: HostCommand,
}

// ---------------------------------------------------------------------------------------------
// rejection
// ---------------------------------------------------------------------------------------------

/// Why an envelope never became a typed input. Every kind is fail-closed: nothing is decoded,
/// nothing is staged, no state moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRejectionKind {
    /// Absolute byte boundary, enforced before parsing.
    InputTooLarge,
    /// Absolute nesting boundary, enforced before parsing.
    DepthExceeded,
    /// Absolute per-container entry boundary, enforced before parsing.
    CollectionTooLarge,
    /// The revision marker is absent or is not the single supported revision.
    VersionMismatch,
    /// The bytes are not JSON at all.
    MalformedJson,
    /// A struct carried a field the contract does not define.
    UnknownField,
    /// A tagged union carried a tag the contract does not define.
    UnknownVariant,
    /// A required field is absent.
    MissingField,
    /// A scalar broke a §7.1.1 rule (decimal `u64`, fixed-point ratio, finite float, branded id…).
    InvalidScalar,
    /// A value had the wrong JSON type for its field.
    TypeMismatch,
    /// The document decoded, but a value — or a relationship between values — breaks a contract
    /// rule: a cross-field invariant, an operation limit that would widen its bootstrap ceiling,
    /// a live patch that would grow a quota, a stale policy revision.
    ///
    /// Distinct from [`Self::InvalidScalar`] on purpose. A scalar rejection means the bytes never
    /// became a value and no host could have meant anything by them; a policy violation means the
    /// host stated a coherent value the kernel refuses to adopt. The two need different host
    /// handling — the first is a serialization bug, the second is a configuration decision — and
    /// collapsing them makes "re-read and rebase this patch" indistinguishable from "your encoder
    /// is broken".
    PolicyViolation,
}

impl WireRejectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InputTooLarge => "input_too_large",
            Self::DepthExceeded => "depth_exceeded",
            Self::CollectionTooLarge => "collection_too_large",
            Self::VersionMismatch => "version_mismatch",
            Self::MalformedJson => "malformed_json",
            Self::UnknownField => "unknown_field",
            Self::UnknownVariant => "unknown_variant",
            Self::MissingField => "missing_field",
            Self::InvalidScalar => "invalid_scalar",
            Self::TypeMismatch => "type_mismatch",
            Self::PolicyViolation => "policy_violation",
        }
    }
}

/// A structured decode rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRejection {
    pub kind: WireRejectionKind,
    pub message: String,
}

impl WireRejection {
    pub fn new(kind: WireRejectionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for WireRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for WireRejection {}

// ---------------------------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------------------------

/// Only field read by the revision probe. Everything else is skipped, so a payload from another
/// revision is answered with a revision fault instead of a deserialization error about a body
/// this kernel never agreed to parse.
#[derive(Deserialize)]
struct AbiRevisionProbe {
    #[serde(default)]
    abi_version: Option<u32>,
}

/// Decode one wire envelope in the mandated order: **measure bytes → absolute structural
/// boundary → probe revision → decode** (§7.1). Parsing before measuring would let an oversized
/// or pathologically nested document allocate first and be rejected second.
pub fn decode_envelope_json(
    input_json: &str,
    limits: &KernelBootstrapLimits,
) -> Result<WireEnvelope, WireRejection> {
    if input_json.len() > limits.absolute_max_input_bytes as usize {
        return Err(WireRejection::new(
            WireRejectionKind::InputTooLarge,
            format!(
                "kernel input is {} bytes; the absolute bound is {} bytes",
                input_json.len(),
                limits.absolute_max_input_bytes
            ),
        ));
    }

    scan_structural_boundary(input_json, limits)?;

    let probe: AbiRevisionProbe = serde_json::from_str(input_json).map_err(classify_serde_error)?;
    match probe.abi_version {
        Some(KERNEL_ABI_VERSION) => {}
        other => {
            let received = other.map_or_else(|| "missing".to_string(), |value| value.to_string());
            return Err(WireRejection::new(
                WireRejectionKind::VersionMismatch,
                format!(
                    "kernel ABI revision mismatch: input revision {received}, \
                     this kernel accepts only revision {KERNEL_ABI_VERSION}"
                ),
            ));
        }
    }

    serde_json::from_str(input_json).map_err(classify_serde_error)
}

/// Serialize an envelope back to JSON. Canonical record bytes are a separate, core-owned
/// concern (Task 6); this is the plain wire projection.
pub fn encode_envelope_json(envelope: &WireEnvelope) -> String {
    serde_json::to_string(envelope).expect("wire envelope is always serializable")
}

fn classify_serde_error(error: serde_json::Error) -> WireRejection {
    let message = error.to_string();
    let kind = if error.classify() == serde_json::error::Category::Syntax
        || error.classify() == serde_json::error::Category::Eof
    {
        WireRejectionKind::MalformedJson
    } else if message.contains(SCALAR_ERROR_MARKER) {
        WireRejectionKind::InvalidScalar
    } else if message.contains("unknown field") {
        WireRejectionKind::UnknownField
    } else if message.contains("unknown variant") {
        WireRejectionKind::UnknownVariant
    } else if message.contains("missing field") {
        WireRejectionKind::MissingField
    } else {
        WireRejectionKind::TypeMismatch
    };
    WireRejection::new(kind, message)
}

/// Single pass over the raw bytes enforcing the absolute nesting depth and per-container entry
/// bounds **before** any parser allocates. This is a boundary check, not a validator: malformed
/// JSON is still the parser's business.
///
/// Public because the legacy protocol's decode paths use it too — a boundary that only the new
/// contract enforces would leave the running kernel unprotected for the whole migration.
pub fn scan_structural_boundary(
    input_json: &str,
    limits: &KernelBootstrapLimits,
) -> Result<(), WireRejection> {
    /// (separators seen at this level, whether the container has any content)
    type Frame = (u64, bool);

    let mut stack: Vec<Frame> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for &byte in input_json.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => {
                in_string = true;
                mark_content(&mut stack);
            }
            b'{' | b'[' => {
                mark_content(&mut stack);
                stack.push((0, false));
                if stack.len() > limits.absolute_max_json_depth as usize {
                    return Err(WireRejection::new(
                        WireRejectionKind::DepthExceeded,
                        format!(
                            "kernel input nests {} levels deep; the absolute bound is {}",
                            stack.len(),
                            limits.absolute_max_json_depth
                        ),
                    ));
                }
            }
            b'}' | b']' => {
                let Some((separators, has_content)) = stack.pop() else {
                    // unbalanced — leave the diagnosis to the parser
                    return Ok(());
                };
                let entries = if has_content { separators + 1 } else { 0 };
                if entries > u64::from(limits.absolute_max_collection_entries) {
                    return Err(WireRejection::new(
                        WireRejectionKind::CollectionTooLarge,
                        format!(
                            "kernel input has a container with {entries} entries; \
                             the absolute bound is {}",
                            limits.absolute_max_collection_entries
                        ),
                    ));
                }
            }
            b',' => {
                if let Some(frame) = stack.last_mut() {
                    frame.0 += 1;
                    frame.1 = true;
                }
            }
            byte if byte.is_ascii_whitespace() => {}
            _ => mark_content(&mut stack),
        }
    }

    Ok(())
}

fn mark_content(stack: &mut [(u64, bool)]) {
    if let Some(frame) = stack.last_mut() {
        frame.1 = true;
    }
}
