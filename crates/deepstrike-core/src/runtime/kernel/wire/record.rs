//! The durable kernel record, its canonical bytes and its digest chain
//! (spec §8.1, §8.2, §12.1, §15.2).
//!
//! Three properties define this module, and every public item exists to make one of them
//! unrepresentable-if-violated rather than merely documented:
//!
//! 1. **Core is the only implementation of canonical bytes and digests** (§15.2). A record is
//!    built by [`KernelRecord::chain`], which computes every digest itself; the fields are private
//!    and read-only, so a host cannot hand-assemble a record or re-serialise one to recompute a
//!    hash. Decoding a record re-verifies it, which is why a tampered journal entry fails at the
//!    boundary instead of somewhere deep in a replay.
//! 2. **The durable record never carries the planned step** (§8.1, §22.12). It stores the
//!    normalised input plus a `step_digest`; a rebuild re-runs the deterministic transition over
//!    the canonical input and compares digests. That is what keeps rendered provider contexts and
//!    large action payloads out of the journal, so record size is a function of the *input*, never
//!    of the step it produced.
//! 3. **The chain is the operation's identity** (§12.1). The genesis record binds the
//!    [`ResolvedOperationConfig`] — not the sparse config, and not "whatever this binary defaults
//!    to today" — and carries no previous digest at all; its `record_digest` is the operation's
//!    `genesis_digest`.

use std::fmt;
use std::fmt::Write as _;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::config::{ConfigDefaults, ResolvedOperationConfig};
use super::effect::Digest;
use super::envelope::{
    DeliverExternalEvent, HostControl, KernelInput, OperationLifecycle, ResolveEffect,
    StartOperation, WireEnvelope, WireRejection,
};
use super::fault::KernelFaultCode;
use super::scalar::{CanonicalBytes, InputId, JS_SAFE_INTEGER_MAX, OperationId, WireU64};

// ---------------------------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------------------------

/// Prefix of every record-layer rejection, so all four hosts can classify on one marker.
pub const RECORD_ERROR_MARKER: &str = "kernel record rejected";

/// Why a record could not be built, decoded or verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// A value has no canonical byte representation (a non-finite float, an integer no JSON host
    /// can hold exactly, a document nested past the canonical bound).
    NotCanonical(String),
    /// The record does not follow its predecessor: wrong operation, wrong sequence, wrong
    /// previous digest, a second genesis, or a genesis that is not a configuration.
    ChainBroken(String),
    /// A stored digest disagrees with the bytes it claims to summarise — the record was edited
    /// after core produced it.
    DigestMismatch(String),
}

impl RecordError {
    pub fn message(&self) -> &str {
        match self {
            Self::NotCanonical(message)
            | Self::ChainBroken(message)
            | Self::DigestMismatch(message) => message,
        }
    }

    /// Fault code a host-facing rejection carries (§7.13).
    ///
    /// `DigestMismatch` is [`KernelFaultCode::RecordCorrupted`] — its own code since the 2026-07-29
    /// adjudication, because a broken record and a broken checkpoint have different recovery
    /// ladders and folding them together told a host to fall back to a checkpoint when the journal
    /// itself was the thing that no longer verified.
    ///
    /// `ChainBroken` stays [`KernelFaultCode::TransactionConflict`]: the same variant reports "this
    /// input has no legal position after that head" (a caller error, no corruption involved) and
    /// "this stored chain does not link up". The transaction layer, which knows it is verifying
    /// *stored* records rather than placing a new one, re-labels the latter as `RecordCorrupted`.
    pub fn code(&self) -> KernelFaultCode {
        match self {
            Self::NotCanonical(_) => KernelFaultCode::MalformedEnvelope,
            Self::ChainBroken(_) => KernelFaultCode::TransactionConflict,
            Self::DigestMismatch(_) => KernelFaultCode::RecordCorrupted,
        }
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{RECORD_ERROR_MARKER}: {}", self.message())
    }
}

impl std::error::Error for RecordError {}

// ---------------------------------------------------------------------------------------------
// canonical bytes (§7.1.1)
// ---------------------------------------------------------------------------------------------

/// Nesting bound of the canonical writer. Well above the §7.3 bootstrap depth ceiling: this is a
/// stack guard for a recursive writer, not a second contract limit.
pub const CANONICAL_MAX_DEPTH: usize = 128;

/// Digest algorithm label. Carried in every [`Digest`] as an explicit `sha256:` prefix so a future
/// algorithm change is a visible wire change rather than a silent reinterpretation.
pub const DIGEST_ALGORITHM: &str = "sha256";

/// Serialise a value to canonical bytes.
///
/// The rules, in full — they are the contract every host validator re-implements in Phase 6:
///
/// | shape | canonical form |
/// | --- | --- |
/// | `null` / `true` / `false` | the literal |
/// | string | JSON string, minimal escaping (`"`, `\`, C0 controls) |
/// | integer | shortest decimal; `-0` is `0`; magnitudes above 2^53−1 are **rejected** |
/// | non-integral float | shortest round-trip decimal |
/// | array | `[a,b]` — no whitespace |
/// | object | `{"a":1,"b":2}` — keys ascending by Unicode code point, no whitespace |
///
/// Two rules earn their keep. Integers beyond the double-safe range are rejected rather than
/// emitted, because a JS host that parses record bytes would silently round them — every logical
/// `u64` on this wire already travels as a decimal string (§7.1.1), so the only way to hit this is
/// an opaque host payload, and failing closed beats a digest that means two different numbers in
/// two languages. Key ordering is by **code point**, which for UTF-8 is byte order; a JavaScript
/// validator must therefore sort with a code-point comparator rather than the default
/// UTF-16 `Array.prototype.sort`, which differs above the BMP.
pub fn canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<CanonicalBytes, RecordError> {
    let value = serde_json::to_value(value).map_err(|error| {
        RecordError::NotCanonical(format!("value is not serialisable: {error}"))
    })?;
    let mut out = String::new();
    write_canonical(&value, 1, &mut out)?;
    Ok(CanonicalBytes::new(out.into_bytes()))
}

/// SHA-256 over exactly these bytes, projected as `sha256:<64 lowercase hex digits>`.
pub fn canonical_digest(bytes: &[u8]) -> Digest {
    let hash = Sha256::digest(bytes);
    let mut text = String::with_capacity(DIGEST_ALGORITHM.len() + 1 + hash.len() * 2);
    text.push_str(DIGEST_ALGORITHM);
    text.push(':');
    for byte in hash {
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Digest::new(text).expect("an algorithm-prefixed hex digest is always a legal branded ref")
}

fn write_canonical(
    value: &serde_json::Value,
    depth: usize,
    out: &mut String,
) -> Result<(), RecordError> {
    if depth > CANONICAL_MAX_DEPTH {
        return Err(RecordError::NotCanonical(format!(
            "value nests deeper than {CANONICAL_MAX_DEPTH}"
        )));
    }
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => write_canonical_number(number, out)?,
        serde_json::Value::String(text) => write_canonical_string(text, out),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, depth + 1, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_string(key, out);
                out.push(':');
                write_canonical(&map[key], depth + 1, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_canonical_string(text: &str, out: &mut String) {
    let encoded = serde_json::to_string(text).expect("a Rust string is always JSON-encodable");
    out.push_str(&encoded);
}

fn write_canonical_number(
    number: &serde_json::Number,
    out: &mut String,
) -> Result<(), RecordError> {
    const SAFE: i128 = JS_SAFE_INTEGER_MAX as i128;

    let unsafe_integer = |value: i128| {
        RecordError::NotCanonical(format!(
            "integer {value} exceeds the cross-language exact-integer range \
             (±{JS_SAFE_INTEGER_MAX}); logical u64 travels as a decimal string"
        ))
    };

    if let Some(value) = number.as_u64() {
        if i128::from(value) > SAFE {
            return Err(unsafe_integer(i128::from(value)));
        }
        write!(out, "{value}").expect("writing to a String cannot fail");
        return Ok(());
    }
    if let Some(value) = number.as_i64() {
        if i128::from(value) < -SAFE {
            return Err(unsafe_integer(i128::from(value)));
        }
        write!(out, "{value}").expect("writing to a String cannot fail");
        return Ok(());
    }

    let float = number
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| RecordError::NotCanonical(format!("number {number} is not finite")))?;
    // An integral float and the same integer must produce the same bytes, or two numerically
    // equal documents would digest differently. `-0.0` collapses into `0` here too.
    if float.fract() == 0.0 && float.abs() <= JS_SAFE_INTEGER_MAX as f64 {
        write!(out, "{}", float as i64).expect("writing to a String cannot fail");
    } else {
        let encoded = serde_json::to_string(&float).expect("a finite f64 is always JSON-encodable");
        out.push_str(&encoded);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// §8.1 · the normalised input a record stores
// ---------------------------------------------------------------------------------------------

/// One accepted envelope after normalisation — the shape whose canonical bytes the record stores
/// (§12.1 calls the serialised form a `CanonicalInput`).
///
/// It mirrors [`WireEnvelope`] with exactly one difference: the genesis arm carries the **resolved**
/// configuration instead of the sparse one. That single substitution is what makes a journal
/// replayable across kernel upgrades (§15.2, Task 6b) — every default this operation runs on is
/// frozen in its first record, so changing a compile-time default cannot change an old operation's
/// decisions.
///
/// The envelope-shaped wrapper is deliberate: `observed_at_ms` is the operation's only clock fact
/// (§11.2) and `input_id` is its idempotency key (§7.1), so a canonical input that dropped them
/// could not be replayed on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedInput {
    pub operation_id: OperationId,
    pub input_id: InputId,
    pub observed_at_ms: WireU64,
    pub input: NormalizedPayload,
}

/// The five input classes after normalisation. Same tag vocabulary as [`KernelInput`] — a record
/// does not invent a second name for an input class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NormalizedPayload {
    /// Genesis. Carries the dense [`ResolvedOperationConfig`], never the sparse wire config.
    ConfigureOperation(ResolvedConfiguration),
    StartOperation(StartOperation),
    ResolveEffect(ResolveEffect),
    DeliverExternalEvent(DeliverExternalEvent),
    HostControl(HostControl),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedConfiguration {
    pub config: ResolvedOperationConfig,
}

impl NormalizedPayload {
    /// Whether this payload may only appear as the first record of an operation.
    pub fn is_genesis(&self) -> bool {
        matches!(self, Self::ConfigureOperation(_))
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::ConfigureOperation(_) => "configure_operation",
            Self::StartOperation(_) => "start_operation",
            Self::ResolveEffect(_) => "resolve_effect",
            Self::DeliverExternalEvent(_) => "deliver_external_event",
            Self::HostControl(_) => "host_control",
        }
    }

    /// §6.1 · the lifecycles this class is admissible in, read off the *normalised* payload.
    ///
    /// The same table [`KernelInput::admissible_lifecycles`] states, reachable from a record. A
    /// restore replays canonical inputs rather than envelopes and must apply exactly the same
    /// lifecycle gate, so the table has to be reachable from both sides of normalisation — and
    /// delegating keeps there being one table.
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

impl NormalizedInput {
    /// Normalise one decoded envelope. The genesis arm resolves its configuration against the
    /// binary's defaults **once**, here; every later reader takes the resolved value from the
    /// record instead of re-deriving it.
    pub fn normalize(
        envelope: &WireEnvelope,
        defaults: &ConfigDefaults,
    ) -> Result<Self, WireRejection> {
        let input = match &envelope.input {
            KernelInput::ConfigureOperation(configure) => {
                NormalizedPayload::ConfigureOperation(ResolvedConfiguration {
                    config: configure.config.resolve(defaults)?,
                })
            }
            KernelInput::StartOperation(start) => NormalizedPayload::StartOperation(start.clone()),
            KernelInput::ResolveEffect(resolve) => {
                NormalizedPayload::ResolveEffect(resolve.clone())
            }
            KernelInput::DeliverExternalEvent(event) => {
                NormalizedPayload::DeliverExternalEvent(event.clone())
            }
            KernelInput::HostControl(control) => NormalizedPayload::HostControl(control.clone()),
        };
        Ok(Self {
            operation_id: envelope.operation_id.clone(),
            input_id: envelope.input_id.clone(),
            observed_at_ms: envelope.observed_at_ms,
            input,
        })
    }

    pub fn is_genesis(&self) -> bool {
        self.input.is_genesis()
    }

    /// The resolved configuration, on the genesis input only.
    pub fn resolved_config(&self) -> Option<&ResolvedOperationConfig> {
        match &self.input {
            NormalizedPayload::ConfigureOperation(configure) => Some(&configure.config),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// §8.1 · the durable record
// ---------------------------------------------------------------------------------------------

/// One durable transition (§8.1).
///
/// Every field is private and every digest is computed by [`KernelRecord::chain`]: there is no
/// constructor that takes a digest, so "the host recomputed the hash and disagreed" is not a
/// reachable state. [`KernelRecord::record_bytes`] is what the host hands to
/// `KernelJournal::compare_and_append`, and [`KernelRecord::expected_head`] is the CAS precondition
/// that goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KernelRecord {
    operation_id: OperationId,
    input_id: InputId,
    step_seq: WireU64,
    /// `None` on the genesis record only — the CAS expected head of an operation's first append is
    /// empty (§8.1).
    previous_record_digest: Option<Digest>,
    /// Canonical bytes of the [`NormalizedInput`]. Projected as an explicit base64 envelope in
    /// JSON (§7.1.1); native bindings see bytes.
    canonical_input: CanonicalBytes,
    input_digest: Digest,
    /// Digest of the ephemeral planned step. The step itself never enters the journal (§22.12).
    step_digest: Digest,
    record_digest: Digest,
}

/// Everything a chain successor needs to know about its predecessor.
///
/// Exactly three facts — the operation it belongs to, where it sits, and what it hashes to — and
/// deliberately not the record itself. §12.2 restores a runtime whose predecessor record may have
/// been pruned under an acked checkpoint; the anchor is what survives that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainAnchor {
    pub operation_id: OperationId,
    pub step_seq: WireU64,
    pub record_digest: Digest,
}

/// The digested body: every field of a record except the digest that summarises it.
#[derive(Serialize)]
struct RecordBody<'a> {
    operation_id: &'a OperationId,
    input_id: &'a InputId,
    step_seq: WireU64,
    previous_record_digest: Option<&'a Digest>,
    canonical_input: &'a CanonicalBytes,
    input_digest: &'a Digest,
    step_digest: &'a Digest,
}

impl KernelRecord {
    /// Build the next record of an operation.
    ///
    /// `previous = None` builds the genesis record: `step_seq` 0, no previous digest, and a payload
    /// that **must** be the resolved configuration. Every later record must carry a non-genesis
    /// payload, the same `operation_id`, and links to its predecessor's digest — a second
    /// `ConfigureOperation` has no legal position in a chain (§6.1: it is admissible only in
    /// `Created`).
    pub fn chain<S: Serialize + ?Sized>(
        previous: Option<&Self>,
        input: &NormalizedInput,
        planned_step: &S,
    ) -> Result<Self, RecordError> {
        Self::chain_after(previous.map(Self::anchor).as_ref(), input, planned_step)
    }

    /// The three facts a successor reads off this record.
    pub fn anchor(&self) -> ChainAnchor {
        ChainAnchor {
            operation_id: self.operation_id.clone(),
            step_seq: self.step_seq,
            record_digest: self.record_digest.clone(),
        }
    }

    /// [`Self::chain`], anchored on the predecessor's *facts* rather than on the predecessor.
    ///
    /// The distinction is what makes §12.2's bounded-tail restore possible at all: a restored
    /// runtime replays the tail on top of a checkpoint whose covered prefix may already have been
    /// pruned, so the record before the first tail entry no longer exists anywhere — only its
    /// digest and its sequence do, and the checkpoint carries them.
    pub fn chain_after<S: Serialize + ?Sized>(
        previous: Option<&ChainAnchor>,
        input: &NormalizedInput,
        planned_step: &S,
    ) -> Result<Self, RecordError> {
        let (step_seq, previous_record_digest) = match previous {
            None => {
                if !input.is_genesis() {
                    return Err(RecordError::ChainBroken(format!(
                        "an operation's first record must be its resolved configuration, got {}",
                        input.input.kind()
                    )));
                }
                (WireU64::ZERO, None)
            }
            Some(previous) => {
                if input.is_genesis() {
                    return Err(RecordError::ChainBroken(
                        "an operation is configured exactly once; a second configure_operation \
                         has no position in the chain"
                            .to_string(),
                    ));
                }
                if previous.operation_id != input.operation_id {
                    return Err(RecordError::ChainBroken(format!(
                        "input belongs to operation {}, but the chain head belongs to {}",
                        input.operation_id, previous.operation_id
                    )));
                }
                let next = previous.step_seq.get().checked_add(1).ok_or_else(|| {
                    RecordError::ChainBroken("step sequence overflowed u64".to_string())
                })?;
                (WireU64::new(next), Some(previous.record_digest.clone()))
            }
        };

        let canonical_input = canonical_bytes(input)?;
        let input_digest = canonical_digest(canonical_input.as_slice());
        let step_digest = canonical_digest(canonical_bytes(planned_step)?.as_slice());
        let record_digest = Self::body_digest(
            &input.operation_id,
            &input.input_id,
            step_seq,
            previous_record_digest.as_ref(),
            &canonical_input,
            &input_digest,
            &step_digest,
        )?;

        Ok(Self {
            operation_id: input.operation_id.clone(),
            input_id: input.input_id.clone(),
            step_seq,
            previous_record_digest,
            canonical_input,
            input_digest,
            step_digest,
            record_digest,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn body_digest(
        operation_id: &OperationId,
        input_id: &InputId,
        step_seq: WireU64,
        previous_record_digest: Option<&Digest>,
        canonical_input: &CanonicalBytes,
        input_digest: &Digest,
        step_digest: &Digest,
    ) -> Result<Digest, RecordError> {
        let body = RecordBody {
            operation_id,
            input_id,
            step_seq,
            previous_record_digest,
            canonical_input,
            input_digest,
            step_digest,
        };
        Ok(canonical_digest(canonical_bytes(&body)?.as_slice()))
    }

    // ----- read-only accessors -----

    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    pub fn input_id(&self) -> &InputId {
        &self.input_id
    }

    pub fn step_seq(&self) -> WireU64 {
        self.step_seq
    }

    pub fn previous_record_digest(&self) -> Option<&Digest> {
        self.previous_record_digest.as_ref()
    }

    /// The CAS precondition for appending this record (§8.2 line 5). `None` means "the operation
    /// has no journal head yet", which only its genesis record may assert.
    pub fn expected_head(&self) -> Option<&Digest> {
        self.previous_record_digest.as_ref()
    }

    pub fn canonical_input(&self) -> &CanonicalBytes {
        &self.canonical_input
    }

    pub fn input_digest(&self) -> &Digest {
        &self.input_digest
    }

    pub fn step_digest(&self) -> &Digest {
        &self.step_digest
    }

    pub fn record_digest(&self) -> &Digest {
        &self.record_digest
    }

    pub fn is_genesis(&self) -> bool {
        self.previous_record_digest.is_none()
    }

    // ----- journal projection -----

    /// Canonical bytes of the whole record — what the journal stores and what
    /// [`Self::from_record_bytes`] reads back.
    pub fn record_bytes(&self) -> CanonicalBytes {
        canonical_bytes(self).expect("a record contains only canonical scalars")
    }

    /// Decode a record from its journal bytes, verifying every digest it carries.
    pub fn from_record_bytes(bytes: &[u8]) -> Result<Self, RecordError> {
        let text = std::str::from_utf8(bytes).map_err(|error| {
            RecordError::NotCanonical(format!("record bytes are not UTF-8: {error}"))
        })?;
        serde_json::from_str(text).map_err(|error| decode_error(&error.to_string()))
    }

    /// Decode the normalised input this record froze — the entry point of a rebuild (§12.2).
    pub fn normalized_input(&self) -> Result<NormalizedInput, RecordError> {
        let text = std::str::from_utf8(self.canonical_input.as_slice()).map_err(|error| {
            RecordError::NotCanonical(format!("canonical input is not UTF-8: {error}"))
        })?;
        serde_json::from_str(text).map_err(|error| {
            RecordError::NotCanonical(format!("canonical input does not decode: {error}"))
        })
    }

    // ----- verification -----

    /// Recompute this record's own digests from the bytes it carries.
    pub fn verify(&self) -> Result<(), RecordError> {
        let input_digest = canonical_digest(self.canonical_input.as_slice());
        if input_digest != self.input_digest {
            return Err(RecordError::DigestMismatch(format!(
                "record {} step {}: canonical input hashes to {input_digest}, \
                 but the record claims {}",
                self.operation_id, self.step_seq, self.input_digest
            )));
        }
        let record_digest = Self::body_digest(
            &self.operation_id,
            &self.input_id,
            self.step_seq,
            self.previous_record_digest.as_ref(),
            &self.canonical_input,
            &self.input_digest,
            &self.step_digest,
        )?;
        if record_digest != self.record_digest {
            return Err(RecordError::DigestMismatch(format!(
                "record {} step {}: body hashes to {record_digest}, but the record claims {}",
                self.operation_id, self.step_seq, self.record_digest
            )));
        }
        Ok(())
    }

    /// Verify a rebuilt step against the digest this record froze (§8.1).
    pub fn verify_step<S: Serialize + ?Sized>(&self, planned_step: &S) -> Result<(), RecordError> {
        let digest = canonical_digest(canonical_bytes(planned_step)?.as_slice());
        if digest != self.step_digest {
            return Err(RecordError::DigestMismatch(format!(
                "record {} step {}: the rebuilt step hashes to {digest}, \
                 but the record froze {}",
                self.operation_id, self.step_seq, self.step_digest
            )));
        }
        Ok(())
    }

    /// Verify this record follows `previous` (`None` = it claims to be a genesis).
    pub fn verify_follows(&self, previous: Option<&Self>) -> Result<(), RecordError> {
        self.verify()?;
        match (previous, self.previous_record_digest.as_ref()) {
            (None, None) => {
                if self.step_seq != WireU64::ZERO {
                    return Err(RecordError::ChainBroken(format!(
                        "a genesis record is step 0, got step {}",
                        self.step_seq
                    )));
                }
                if self.normalized_input()?.is_genesis() {
                    Ok(())
                } else {
                    Err(RecordError::ChainBroken(
                        "a genesis record must carry the resolved configuration".to_string(),
                    ))
                }
            }
            (None, Some(digest)) => Err(RecordError::ChainBroken(format!(
                "record {} step {} expects head {digest}, but the operation has no head",
                self.operation_id, self.step_seq
            ))),
            (Some(previous), None) => Err(RecordError::ChainBroken(format!(
                "record {} step {} claims to be a genesis, but the operation head is {}",
                self.operation_id, self.step_seq, previous.record_digest
            ))),
            (Some(previous), Some(digest)) => {
                if previous.operation_id != self.operation_id {
                    return Err(RecordError::ChainBroken(format!(
                        "record belongs to operation {}, its predecessor to {}",
                        self.operation_id, previous.operation_id
                    )));
                }
                if digest != &previous.record_digest {
                    return Err(RecordError::ChainBroken(format!(
                        "record {} step {} expects head {digest}, but the head is {}",
                        self.operation_id, self.step_seq, previous.record_digest
                    )));
                }
                if previous.step_seq.get().checked_add(1) != Some(self.step_seq.get()) {
                    return Err(RecordError::ChainBroken(format!(
                        "record {} is step {}, but its predecessor is step {}",
                        self.operation_id, self.step_seq, previous.step_seq
                    )));
                }
                Ok(())
            }
        }
    }
}

/// The §7.13 preparation result once its durable half is known: [`KernelRecord`] is the `Record`
/// instance of [`KernelPreparation`], and Task 7 fills in the ephemeral planned step.
pub type RecordPreparation<Step> = super::fault::KernelPreparation<KernelRecord, Step>;

/// Verify a whole chain and return the operation's `genesis_digest` (§12.1).
///
/// The returned digest is the identity a checkpoint binds itself to: an operation is its genesis
/// record's digest, so a checkpoint built from another operation's journal cannot be installed by
/// accident.
pub fn verify_record_chain(records: &[KernelRecord]) -> Result<&Digest, RecordError> {
    let Some(genesis) = records.first() else {
        return Err(RecordError::ChainBroken(
            "an operation chain starts at its genesis record; this one is empty".to_string(),
        ));
    };
    genesis.verify_follows(None)?;
    for pair in records.windows(2) {
        pair[1].verify_follows(Some(&pair[0]))?;
    }
    Ok(&genesis.record_digest)
}

// ---------------------------------------------------------------------------------------------
// decoding
// ---------------------------------------------------------------------------------------------

/// Wire projection of a record, used only as the decode target. Decoding goes through it so that
/// [`KernelRecord`]'s fields stay private and every decoded record is verified before it exists.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordProjection {
    operation_id: OperationId,
    input_id: InputId,
    step_seq: WireU64,
    previous_record_digest: Option<Digest>,
    canonical_input: CanonicalBytes,
    input_digest: Digest,
    step_digest: Digest,
    record_digest: Digest,
}

fn decode_error(message: &str) -> RecordError {
    if message.contains(RECORD_ERROR_MARKER) {
        RecordError::DigestMismatch(message.to_string())
    } else {
        RecordError::NotCanonical(format!("record does not decode: {message}"))
    }
}

impl<'de> Deserialize<'de> for KernelRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let projection = RecordProjection::deserialize(deserializer)?;
        let record = Self {
            operation_id: projection.operation_id,
            input_id: projection.input_id,
            step_seq: projection.step_seq,
            previous_record_digest: projection.previous_record_digest,
            canonical_input: projection.canonical_input,
            input_digest: projection.input_digest,
            step_digest: projection.step_digest,
            record_digest: projection.record_digest,
        };
        record
            .verify()
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::*;

    // -----------------------------------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------------------------------

    fn operation() -> OperationId {
        OperationId::new("op-record-1").unwrap()
    }

    fn input_id(seq: u32) -> InputId {
        InputId::new(format!("in-{seq}")).unwrap()
    }

    fn boot_config() -> OperationConfig {
        OperationConfig {
            execution_policy: Some(ExecutionPolicy {
                max_turns: Some(12),
                ..ExecutionPolicy::default()
            }),
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
            ]),
            ..OperationConfig::default()
        }
    }

    fn envelope(seq: u32, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
        WireEnvelope::new(
            operation(),
            input_id(seq),
            WireU64::new(observed_at_ms),
            input,
        )
    }

    fn configure_envelope() -> WireEnvelope {
        envelope(
            0,
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation {
                config: boot_config(),
            }),
        )
    }

    fn start_envelope() -> WireEnvelope {
        envelope(
            1,
            1_700_000_000_500,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the brief"),
                    run_spec: None,
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    fn resolve_envelope() -> WireEnvelope {
        envelope(
            2,
            1_700_000_001_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: EffectId::new("op-record-1:step:1:effect:0").unwrap(),
                outcome: EffectOutcome::Failed(EffectFailed {
                    failure: HostEffectFailure {
                        kind: HostEffectFailureKind::ProtocolError,
                        message: "provider refused the request".to_string(),
                        retryable: Some(false),
                    },
                }),
            }),
        )
    }

    fn cancel_envelope() -> WireEnvelope {
        envelope(
            3,
            1_700_000_002_000,
            KernelInput::HostControl(HostControl {
                command: HostCommand::Cancel(CancelCommand {
                    reason: CancellationReason::User,
                    pending_call_ids: vec![],
                }),
            }),
        )
    }

    fn normalize(envelope: &WireEnvelope) -> NormalizedInput {
        NormalizedInput::normalize(envelope, &ConfigDefaults::default())
            .expect("the sample envelope normalises")
    }

    fn step(name: &str) -> Value {
        json!({ "planned": name, "effects": [{ "kind": "call_provider" }] })
    }

    fn genesis_record() -> KernelRecord {
        KernelRecord::chain(None, &normalize(&configure_envelope()), &step("configure")).unwrap()
    }

    /// genesis then start then resolve
    fn sample_chain() -> Vec<KernelRecord> {
        let genesis = genesis_record();
        let started = KernelRecord::chain(
            Some(&genesis),
            &normalize(&start_envelope()),
            &step("start"),
        )
        .unwrap();
        let resolved = KernelRecord::chain(
            Some(&started),
            &normalize(&resolve_envelope()),
            &step("resolve"),
        )
        .unwrap();
        vec![genesis, started, resolved]
    }

    fn canonical_text<T: Serialize + ?Sized>(value: &T) -> String {
        String::from_utf8(canonical_bytes(value).unwrap().into_vec()).unwrap()
    }

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
    }

    /// Read a frozen golden, or rewrite it when `BLESS_KERNEL_RECORD_FIXTURES=1`.
    ///
    /// The blessing path exists so a deliberate contract change is a one-command, reviewable diff;
    /// the default path is an assertion that today's bytes are the frozen bytes.
    fn golden(name: &str, produced: &Value) -> Value {
        let path = fixture_dir().join(name);
        if std::env::var("BLESS_KERNEL_RECORD_FIXTURES").as_deref() == Ok("1") {
            let mut text = serde_json::to_string_pretty(produced).unwrap();
            text.push('\n');
            fs::write(&path, text).unwrap_or_else(|e| panic!("cannot bless {name}: {e}"));
            return produced.clone();
        }
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("missing golden {name} ({e}); re-bless with BLESS_KERNEL_RECORD_FIXTURES=1")
        });
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
    }

    // -----------------------------------------------------------------------------------------
    // canonical bytes (spec 7.1.1)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn canonical_bytes_sort_keys_and_carry_no_whitespace() {
        assert_eq!(
            canonical_text(&json!({ "b": 1, "a": { "d": [1, 2], "c": true }, "": null })),
            r#"{"":null,"a":{"c":true,"d":[1,2]},"b":1}"#
        );
        assert_eq!(canonical_text(&json!([])), "[]");
        assert_eq!(canonical_text(&json!({})), "{}");
        assert_eq!(
            canonical_text(&json!("quote \" backslash \\ newline \n tab \t")),
            r#""quote \" backslash \\ newline \n tab \t""#
        );
        assert_eq!(canonical_text(&json!("\u{1}")), "\"\\u0001\"");
    }

    #[test]
    fn canonical_object_keys_sort_by_code_point_not_utf16() {
        // U+10000 precedes U+FFFD under UTF-16 ordering (its lead surrogate is 0xD800) and follows
        // it under code-point ordering. Canonical bytes use code points, so a JavaScript validator
        // must sort with a code-point comparator rather than the default one.
        assert_eq!(
            canonical_text(&json!({ "\u{10000}": 1, "\u{fffd}": 2, "z": 3 })),
            "{\"z\":3,\"\u{fffd}\":2,\"\u{10000}\":1}"
        );
    }

    #[test]
    fn canonical_numbers_are_language_neutral() {
        assert_eq!(canonical_text(&json!(0)), "0");
        assert_eq!(canonical_text(&json!(-0.0)), "0", "-0 and 0 are one value");
        assert_eq!(canonical_text(&json!(2.0)), "2", "2 and 2.0 are one value");
        assert_eq!(canonical_text(&json!(-17)), "-17");
        assert_eq!(canonical_text(&json!(0.5)), "0.5");
        assert_eq!(
            canonical_text(&json!(JS_SAFE_INTEGER_MAX)),
            "9007199254740991"
        );
    }

    #[test]
    fn canonical_bytes_reject_what_no_host_can_read_back() {
        let too_large = serde_json::from_str::<Value>("9007199254740992").unwrap();
        let error = canonical_bytes(&too_large).expect_err("beyond the exact-integer range");
        assert!(matches!(error, RecordError::NotCanonical(_)), "{error}");

        let too_negative = serde_json::from_str::<Value>("-9007199254740992").unwrap();
        assert!(canonical_bytes(&too_negative).is_err());

        let mut deep = json!(0);
        for _ in 0..(CANONICAL_MAX_DEPTH + 2) {
            deep = Value::Array(vec![deep]);
        }
        assert!(canonical_bytes(&deep).is_err(), "recursion must be bounded");
    }

    #[test]
    fn canonical_bytes_are_byte_identical_across_repeated_runs() {
        let input = normalize(&configure_envelope());
        let first = canonical_bytes(&input).unwrap();
        for _ in 0..8 {
            assert_eq!(canonical_bytes(&input).unwrap(), first);
        }

        // and a value assembled in a different key order is the same value
        let one = canonical_bytes(&json!({ "a": 1, "b": 2 })).unwrap();
        let other = canonical_bytes(&json!({ "b": 2, "a": 1 })).unwrap();
        assert_eq!(one, other);
    }

    #[test]
    fn the_digest_is_sha256_over_exactly_the_canonical_bytes() {
        // known answers: the algorithm is plain SHA-256, hex, lowercase
        assert_eq!(
            canonical_digest(b"").as_str(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            canonical_digest(b"abc").as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            canonical_digest(canonical_text(&json!({ "a": 1 })).as_bytes()).as_str(),
            canonical_digest(br#"{"a":1}"#).as_str()
        );
    }

    // -----------------------------------------------------------------------------------------
    // genesis (spec 8.1, 12.1, Task 6b)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn genesis_binds_the_resolved_config_and_an_empty_previous_head() {
        let record = genesis_record();

        assert!(record.is_genesis());
        assert_eq!(record.step_seq(), WireU64::ZERO);
        assert_eq!(record.previous_record_digest(), None);
        assert_eq!(
            record.expected_head(),
            None,
            "genesis appends against an empty head"
        );

        let stored = record.normalized_input().unwrap();
        let resolved = stored
            .resolved_config()
            .expect("genesis stores a resolved config");
        assert_eq!(
            resolved,
            &boot_config().resolve(&ConfigDefaults::default()).unwrap(),
            "the genesis record freezes the resolved configuration, not the sparse one"
        );
        assert_eq!(resolved.execution_policy.max_turns, 12);

        // and it is dense: every field a later replay needs is present, no Option stands for
        // "ask the binary".
        let value = serde_json::to_value(resolved).unwrap();
        for required in [
            "execution_policy",
            "governance_policy",
            "scheduler_policy",
            "resource_quota",
            "signal_policy",
            "context_policy",
            "recovery_policy",
            "payload_policy",
            "kernel_limits",
            "memory_policy",
            "feature_policy",
            "host_effect_support",
        ] {
            assert!(
                value.get(required).is_some(),
                "resolved config lacks {required}"
            );
        }
    }

    #[test]
    fn the_genesis_digest_is_the_operations_identity() {
        let chain = sample_chain();
        let genesis_digest = verify_record_chain(&chain).unwrap();
        assert_eq!(genesis_digest, chain[0].record_digest());
        assert_eq!(
            chain[1].previous_record_digest(),
            Some(genesis_digest),
            "the first transition links straight to the genesis digest"
        );
    }

    #[test]
    fn a_genesis_replay_survives_kernel_default_drift() {
        // A newer binary with different compile-time defaults resolves *new* configs differently...
        let mut drifted = ConfigDefaults::default();
        drifted.baseline.execution_policy.max_context_tokens = 999;
        drifted.baseline.recovery_policy.provider_recovery_attempts = 9;

        let record = genesis_record();
        // ...but a rebuild reads the frozen resolved config out of the record instead of resolving
        // again, so the drift cannot reach the replay.
        let replayed = record.normalized_input().unwrap();
        assert_eq!(
            replayed
                .resolved_config()
                .unwrap()
                .execution_policy
                .max_context_tokens,
            ConfigDefaults::default()
                .baseline
                .execution_policy
                .max_context_tokens
        );
        assert_ne!(
            drifted.baseline.execution_policy.max_context_tokens,
            replayed
                .resolved_config()
                .unwrap()
                .execution_policy
                .max_context_tokens,
            "the fixture must actually drift, or this test proves nothing"
        );
        assert_eq!(
            canonical_bytes(&replayed).unwrap(),
            *record.canonical_input()
        );
        record
            .verify_step(&step("configure"))
            .expect("the frozen step still verifies");
    }

    // -----------------------------------------------------------------------------------------
    // the chain (spec 8.1, 15.2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn each_record_links_to_its_predecessor() {
        let chain = sample_chain();
        for (index, record) in chain.iter().enumerate() {
            assert_eq!(record.step_seq(), WireU64::new(index as u64));
            assert_eq!(record.operation_id(), &operation());
            if index == 0 {
                assert!(record.previous_record_digest().is_none());
            } else {
                assert_eq!(
                    record.previous_record_digest(),
                    Some(chain[index - 1].record_digest())
                );
            }
        }
        verify_record_chain(&chain).unwrap();
    }

    #[test]
    fn the_chain_refuses_a_genesis_in_the_wrong_position() {
        // a non-configuration cannot open an operation
        let error = KernelRecord::chain(None, &normalize(&start_envelope()), &step("start"))
            .expect_err("only a resolved configuration opens a chain");
        assert!(matches!(error, RecordError::ChainBroken(_)), "{error}");

        // and a configuration cannot re-open one
        let genesis = genesis_record();
        let error = KernelRecord::chain(
            Some(&genesis),
            &normalize(&configure_envelope()),
            &step("configure again"),
        )
        .expect_err("an operation is configured exactly once");
        assert!(matches!(error, RecordError::ChainBroken(_)), "{error}");
    }

    #[test]
    fn a_record_cannot_chain_onto_another_operation() {
        let genesis = genesis_record();
        let mut foreign = normalize(&start_envelope());
        foreign.operation_id = OperationId::new("op-other").unwrap();
        let error = KernelRecord::chain(Some(&genesis), &foreign, &step("start"))
            .expect_err("operations do not share a chain");
        assert!(matches!(error, RecordError::ChainBroken(_)), "{error}");
    }

    #[test]
    fn tampering_with_any_link_is_detected() {
        let chain = sample_chain();

        // 1. an edited field inside a record no longer matches its own digest
        for field in [
            "operation_id",
            "input_id",
            "step_seq",
            "input_digest",
            "step_digest",
            "previous_record_digest",
        ] {
            let mut value = serde_json::to_value(&chain[2]).unwrap();
            value[field] = match field {
                "step_seq" => json!("99"),
                "previous_record_digest" | "input_digest" | "step_digest" => {
                    json!(canonical_digest(b"forged").as_str())
                }
                _ => json!("forged"),
            };
            let error = serde_json::from_value::<KernelRecord>(value)
                .expect_err(&format!("editing {field} must be detected"));
            assert!(
                error.to_string().contains(RECORD_ERROR_MARKER),
                "{field}: {error}"
            );
        }

        // 2. an edited canonical input no longer matches input_digest
        let mut value = serde_json::to_value(&chain[1]).unwrap();
        value["canonical_input"]["data"] =
            serde_json::to_value(CanonicalBytes::new(b"{}".to_vec())).unwrap()["data"].clone();
        assert!(serde_json::from_value::<KernelRecord>(value).is_err());

        // 3. a re-digested forgery passes verify() but breaks the chain
        let forged = KernelRecord::chain(
            Some(&chain[0]),
            &normalize(&cancel_envelope()),
            &step("forged"),
        )
        .unwrap();
        let mut broken = chain.clone();
        broken[1] = forged;
        let error = verify_record_chain(&broken).expect_err("link 2 no longer follows link 1");
        assert!(matches!(error, RecordError::ChainBroken(_)), "{error}");

        // 4. dropping a link is a broken chain, not a shorter one
        let gapped = vec![chain[0].clone(), chain[2].clone()];
        assert!(verify_record_chain(&gapped).is_err());

        // 5. an empty journal has no genesis
        assert!(verify_record_chain(&[]).is_err());
    }

    #[test]
    fn a_record_verifies_the_step_a_rebuild_recomputes() {
        let chain = sample_chain();
        chain[1]
            .verify_step(&step("start"))
            .expect("the same step verifies");
        let error = chain[1]
            .verify_step(&step("something else"))
            .expect_err("a different step must not verify");
        assert!(matches!(error, RecordError::DigestMismatch(_)), "{error}");
    }

    // -----------------------------------------------------------------------------------------
    // the record never carries the step (spec 8.1, 22.12)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_record_is_exactly_the_eight_declared_fields() {
        let value = serde_json::to_value(genesis_record()).unwrap();
        let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "operation_id".to_string(),
                "input_id".to_string(),
                "step_seq".to_string(),
                "previous_record_digest".to_string(),
                "canonical_input".to_string(),
                "input_digest".to_string(),
                "step_digest".to_string(),
                "record_digest".to_string(),
            ])
        );
    }

    #[test]
    fn no_planned_step_survives_into_the_durable_record() {
        const BANNED: [&str; 8] = [
            "step",
            "planned_step",
            "committed_step",
            "actions",
            "effects",
            "rendered_context",
            "messages",
            "faults",
        ];

        let record = KernelRecord::chain(
            None,
            &normalize(&configure_envelope()),
            &json!({
                "actions": [{ "kind": "call_provider" }],
                "rendered_context": { "messages": [{ "role": "user", "content": "x" }] },
                "effects": [{ "effect_id": "op-record-1:step:0:effect:0" }],
            }),
        )
        .unwrap();

        let text = String::from_utf8(record.record_bytes().into_vec()).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let mut keys = BTreeSet::new();
        collect_keys(&value, &mut keys);
        for banned in BANNED {
            assert!(
                !keys.contains(banned),
                "the record leaked the step key {banned:?}"
            );
        }
        assert!(!text.contains("rendered_context"));
        assert!(!text.contains("call_provider"));
    }

    #[test]
    fn record_size_is_a_function_of_the_input_not_of_the_step() {
        let input = normalize(&resolve_envelope());
        let genesis = genesis_record();

        let tiny = KernelRecord::chain(Some(&genesis), &input, &json!({})).unwrap();
        let huge = KernelRecord::chain(
            Some(&genesis),
            &input,
            &json!({ "rendered_context": "x".repeat(200_000) }),
        )
        .unwrap();

        assert_eq!(
            tiny.record_bytes().len(),
            huge.record_bytes().len(),
            "a 200 KiB step must not grow the journal record by one byte"
        );
        assert_ne!(tiny.step_digest(), huge.step_digest());
        assert_eq!(tiny.input_digest(), huge.input_digest());
    }

    fn collect_keys(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    out.insert(key.clone());
                    collect_keys(item, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_keys(item, out)),
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // journal projection (spec 8.2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn records_round_trip_through_their_journal_bytes() {
        for record in sample_chain() {
            let bytes = record.record_bytes();
            let back = KernelRecord::from_record_bytes(bytes.as_slice()).unwrap();
            assert_eq!(back, record);
            assert_eq!(back.record_bytes(), bytes, "journal bytes are stable");

            // the canonical input decodes back into the typed input a rebuild replays
            let input = record.normalized_input().unwrap();
            assert_eq!(canonical_bytes(&input).unwrap(), *record.canonical_input());
        }
    }

    #[test]
    fn a_record_with_removed_or_unknown_fields_fails_closed() {
        let mut value = serde_json::to_value(genesis_record()).unwrap();
        value["abi_version"] = json!(1);
        assert!(serde_json::from_value::<KernelRecord>(value).is_err());

        let mut value = serde_json::to_value(genesis_record()).unwrap();
        value["surprise"] = json!(true);
        assert!(
            serde_json::from_value::<KernelRecord>(value).is_err(),
            "a record with an unknown field is not a record"
        );
    }

    #[test]
    fn the_record_is_the_durable_half_of_a_preparation() {
        let record = genesis_record();
        let preparation: RecordPreparation<Value> =
            KernelPreparation::Prepared(PreparedTransition {
                token: PrepareToken::new("prepare-1").unwrap(),
                record: record.clone(),
                planned_step: step("configure"),
            });

        assert_eq!(preparation.record(), Some(&record));
        assert!(
            preparation.token().is_some(),
            "only a prepared transition commits"
        );

        let text = serde_json::to_string(&preparation).unwrap();
        let back: RecordPreparation<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, preparation);

        // the planned step travels with the preparation and stops there
        record
            .verify_step(preparation.step().unwrap())
            .expect("the preparation's step is the one the record froze");
    }

    // -----------------------------------------------------------------------------------------
    // golden fixtures: the Phase 6 cross-language source of truth
    // -----------------------------------------------------------------------------------------

    #[test]
    fn golden_canonical_bytes_vectors() {
        let vectors = vec![
            ("empty_object", json!({})),
            ("empty_array", json!([])),
            ("key_order", json!({ "b": 1, "A": 2, "a": 3, "": 4 })),
            (
                "code_point_key_order",
                json!({ "\u{10000}": 1, "\u{fffd}": 2, "z": 3 }),
            ),
            ("escapes", json!("\" \\ \n \t \u{1} \u{e9} \u{1f600}")),
            (
                "numbers",
                json!([0, -0.0, 2.0, -17, 0.5, 9007199254740991u64]),
            ),
            (
                "null_and_bools",
                json!({ "a": null, "b": true, "c": false }),
            ),
            (
                "nested",
                json!({ "outer": { "inner": [1, { "deep": "value" }] } }),
            ),
            (
                "wire_u64_is_a_string",
                json!({ "step_seq": "18446744073709551615" }),
            ),
        ];

        let produced = json!({
            "description":
                "Canonical byte vectors (spec 7.1.1). `canonical` is the exact UTF-8 byte string \
                 core produces; `digest` is SHA-256 over those bytes, hex, sha256-prefixed. \
                 Object keys sort by Unicode code point.",
            "vectors": vectors
                .iter()
                .map(|(name, value)| {
                    let canonical = canonical_text(value);
                    json!({
                        "name": name,
                        "value": value,
                        "canonical": canonical,
                        "digest": canonical_digest(canonical.as_bytes()).as_str(),
                    })
                })
                .collect::<Vec<_>>(),
            "rejected": [
                { "name": "integer_beyond_exact_range", "value": 9007199254740992u64 },
                { "name": "negative_integer_beyond_exact_range", "value": -9007199254740992i64 },
            ],
        });

        let expected = golden("golden_record_canonical_bytes.json", &produced);
        assert_eq!(produced, expected, "canonical byte vectors drifted");

        for rejected in expected["rejected"].as_array().unwrap() {
            assert!(
                canonical_bytes(&rejected["value"]).is_err(),
                "{} must have no canonical form",
                rejected["name"]
            );
        }
    }

    #[test]
    fn golden_genesis_record() {
        let envelope = configure_envelope();
        let input = normalize(&envelope);
        let planned = step("configure");
        let record = KernelRecord::chain(None, &input, &planned).unwrap();

        let produced = json!({
            "description":
                "Genesis record (spec 8.1, 12.1). `canonical_input` holds the resolved \
                 configuration, `previous_record_digest` is null, and `record_digest` is the \
                 operation's genesis_digest.",
            "envelope": serde_json::to_value(&envelope).unwrap(),
            "step": planned,
            "normalized_input": serde_json::to_value(&input).unwrap(),
            "canonical_input": canonical_text(&input),
            "record": serde_json::to_value(&record).unwrap(),
            "record_bytes": String::from_utf8(record.record_bytes().into_vec()).unwrap(),
            "genesis_digest": record.record_digest().as_str(),
        });

        let expected = golden("golden_record_genesis.json", &produced);
        assert_eq!(produced, expected, "the genesis record drifted");
        assert_eq!(expected["record"]["previous_record_digest"], Value::Null);
    }

    #[test]
    fn golden_transition_record() {
        let genesis = genesis_record();
        let envelope = resolve_envelope();
        let input = normalize(&envelope);
        let planned = step("resolve");
        let record = KernelRecord::chain(Some(&genesis), &input, &planned).unwrap();

        let produced = json!({
            "description":
                "A non-genesis record (spec 8.1). It stores the normalised envelope plus a step \
                 digest, never the planned step, and links to `previous_record`'s digest.",
            "previous_record": serde_json::to_value(&genesis).unwrap(),
            "envelope": serde_json::to_value(&envelope).unwrap(),
            "step": planned,
            "normalized_input": serde_json::to_value(&input).unwrap(),
            "canonical_input": canonical_text(&input),
            "record": serde_json::to_value(&record).unwrap(),
            "record_bytes": String::from_utf8(record.record_bytes().into_vec()).unwrap(),
        });

        let expected = golden("golden_record_transition.json", &produced);
        assert_eq!(produced, expected, "the transition record drifted");

        // the fixture is self-checking: its previous record really is this record's head
        let previous: KernelRecord =
            serde_json::from_value(expected["previous_record"].clone()).unwrap();
        let decoded: KernelRecord = serde_json::from_value(expected["record"].clone()).unwrap();
        decoded.verify_follows(Some(&previous)).unwrap();
    }

    #[test]
    fn golden_record_chain_of_three() {
        let envelopes = [configure_envelope(), start_envelope(), resolve_envelope()];
        let steps = [step("configure"), step("start"), step("resolve")];

        let mut records: Vec<KernelRecord> = Vec::new();
        let mut links = Vec::new();
        for (envelope, planned) in envelopes.iter().zip(steps.iter()) {
            let input = normalize(envelope);
            let record = KernelRecord::chain(records.last(), &input, planned).unwrap();
            links.push(json!({
                "envelope": serde_json::to_value(envelope).unwrap(),
                "step": planned,
                "record": serde_json::to_value(&record).unwrap(),
            }));
            records.push(record);
        }

        let produced = json!({
            "description":
                "Three-link record chain (spec 8.1, 12.1). Replaying the envelopes through \
                 normalisation and KernelRecord::chain must reproduce every record byte for \
                 byte; `genesis_digest` is the operation's identity and `head_digest` its CAS head.",
            "genesis_digest": records[0].record_digest().as_str(),
            "head_digest": records[2].record_digest().as_str(),
            "links": links,
        });

        let expected = golden("golden_record_chain.json", &produced);
        assert_eq!(produced, expected, "the record chain drifted");

        let decoded: Vec<KernelRecord> = expected["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|link| serde_json::from_value(link["record"].clone()).unwrap())
            .collect();
        assert_eq!(
            verify_record_chain(&decoded).unwrap().as_str(),
            expected["genesis_digest"].as_str().unwrap()
        );
    }
}
