use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecallLifecycle,
    MemoryRecord, MemoryScope, MemoryTrustLevel,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Durable-memory host storage. Runner writes through `put` only after the kernel's
/// `WriteMemory` gate accepts the record.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn put(&self, agent_id: &str, record: MemoryRecord) -> crate::Result<()>;

    /// Return one record by its agent-local id, or `None` when it does not exist.
    async fn get(&self, agent_id: &str, record_id: &str) -> crate::Result<Option<MemoryRecord>>;

    /// Delete one record by its agent-local id. Missing records are a successful no-op.
    async fn delete(&self, agent_id: &str, record_id: &str) -> crate::Result<()>;

    /// Semantic search over the agent's long-term memories.
    /// Called on demand during a session when the LLM invokes the `memory` meta-tool.
    async fn search(&self, agent_id: &str, query: &MemoryQuery)
    -> crate::Result<Vec<MemoryRecall>>;

    /// Persist a completed session before the runner's one extraction pass.
    async fn save_session(
        &self,
        data: deepstrike_core::memory::durable::SessionData,
    ) -> crate::Result<()>;

    /// M3 · mirror a recall lifecycle the kernel derived into durable state. The counts arrive
    /// already computed — a store never derives `recall_count + 1` itself. The default keeps no
    /// recall history, which is the behaviour of a store that predates this hook.
    async fn record_recall(
        &self,
        _agent_id: &str,
        _recalls: &[MemoryRecallLifecycle],
    ) -> crate::Result<()> {
        Ok(())
    }
}

/// Search options for an agent-bound [`DurableMemory`] descriptor.
#[derive(Debug, Clone, Default)]
pub struct MemorySearchOptions {
    pub top_k: Option<usize>,
    pub kinds: Vec<MemoryKind>,
    pub min_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryLease {
    pub lease_id: String,
    pub agent_id: String,
    pub scope: MemoryScope,
    pub issued_at: u64,
    pub expires_at: u64,
    pub max_writes: u64,
    pub writes_used: u64,
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLeaseRejection {
    LeaseMissing,
    LeaseScopeMismatch,
    LeaseExpired,
    LeaseRevoked,
    MemoryWriteBudgetExhausted,
}

pub fn memory_record_expired(record: &MemoryRecord, now_ms: u64) -> bool {
    record.ttl_days.is_some_and(|days| now_ms > record.updated_at.saturating_add(days as u64 * 86_400_000))
}

pub fn verify_memory_lease(lease: Option<&MemoryLease>, agent_id: &str, scope: &MemoryScope, now_ms: u64) -> Result<(), MemoryLeaseRejection> {
    let lease = lease.ok_or(MemoryLeaseRejection::LeaseMissing)?;
    if lease.agent_id != agent_id || lease.scope != *scope { return Err(MemoryLeaseRejection::LeaseScopeMismatch); }
    if lease.revoked_at.is_some_and(|at| now_ms >= at) { return Err(MemoryLeaseRejection::LeaseRevoked); }
    if now_ms >= lease.expires_at { return Err(MemoryLeaseRejection::LeaseExpired); }
    if lease.writes_used >= lease.max_writes { return Err(MemoryLeaseRejection::MemoryWriteBudgetExhausted); }
    Ok(())
}

pub fn consume_memory_lease_write(lease: &mut MemoryLease) { lease.writes_used = lease.writes_used.saturating_add(1); }
pub fn revoke_memory_lease(lease: &mut MemoryLease, revoked_at: u64) { lease.revoked_at = Some(revoked_at); }

/// Public durable memory bound to one agent and scope.
///
/// This is separate from [`WorkingMemory`], which is an in-process scratch pad, and from
/// [`MemoryStore`], which remains host-owned storage for runners and public descriptors.
pub struct DurableMemory {
    store: std::sync::Arc<dyn MemoryStore>,
    agent_id: String,
    scope: MemoryScope,
}

impl DurableMemory {
    pub fn new(
        store: std::sync::Arc<dyn MemoryStore>,
        agent_id: impl Into<String>,
        scope: MemoryScope,
    ) -> Self {
        Self {
            store,
            agent_id: agent_id.into(),
            scope,
        }
    }

    pub fn namespace(&self) -> &str {
        &self.scope.namespace
    }

    pub async fn search(
        &self,
        query: impl Into<String>,
        options: MemorySearchOptions,
    ) -> crate::Result<Vec<MemoryRecord>> {
        let request = MemoryQuery {
            scope: self.scope.clone(),
            query: query.into(),
            top_k: options.top_k.unwrap_or(5),
            kinds: options.kinds,
            min_score: options.min_score,
        };
        Ok(self
            .store
            .search(&self.agent_id, &request)
            .await?
            .into_iter()
            .map(|hit| hit.record)
            .filter(|record| record.scope == self.scope)
            .collect())
    }

    pub async fn get(&self, record_id: &str) -> crate::Result<Option<MemoryRecord>> {
        Ok(self
            .store
            .get(&self.agent_id, record_id)
            .await?
            .filter(|record| record.scope == self.scope))
    }

    pub async fn put(&self, record: MemoryRecord) -> crate::Result<()> {
        if record.scope != self.scope {
            return Err(crate::Error::Other(
                "memory record scope must match the bound Memory scope".into(),
            ));
        }
        self.store.put(&self.agent_id, record).await
    }

    pub async fn delete(&self, record_id: &str) -> crate::Result<()> {
        if self.get(record_id).await?.is_some() {
            self.store.delete(&self.agent_id, record_id).await?;
        }
        Ok(())
    }
}

pub(crate) fn parse_extracted_memories(
    output: &str,
    session: &SessionData,
    scope: &MemoryScope,
) -> Vec<MemoryRecord> {
    let cleaned = output
        .trim()
        .strip_prefix("```json")
        .or_else(|| output.trim().strip_prefix("```"))
        .unwrap_or(output.trim())
        .strip_suffix("```")
        .unwrap_or(output.trim())
        .trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(cleaned) else {
        return Vec::new();
    };
    let Some(drafts) = value.get("memories").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    drafts
        .iter()
        .take(10)
        .filter_map(|draft| {
            let name = draft.get("name")?.as_str()?.trim();
            let content = draft.get("content")?.as_str()?.trim();
            if name.is_empty() || content.is_empty() {
                return None;
            }
            let kind = match draft.get("kind")?.as_str()? {
                "user" => MemoryKind::User,
                "feedback" => MemoryKind::Feedback,
                "project" => MemoryKind::Project,
                "reference" => MemoryKind::Reference,
                _ => return None,
            };
            let confidence = draft
                .get("confidence")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let strings = |field: &str| {
                draft
                    .get(field)
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Some(MemoryRecord {
                record_id: format!(
                    "{}:{}:{}:{name}",
                    scope.tenant_id,
                    scope.namespace,
                    kind.label()
                ),
                scope: scope.clone(),
                name: name.to_string(),
                kind,
                content: content.to_string(),
                description: draft
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                provenance: MemoryProvenance {
                    session_id: Some(session.session_id.clone()),
                    author: MemoryAuthor::Extraction,
                    trust: MemoryTrustLevel::Untrusted,
                    evidence_refs: strings("evidence_refs"),
                },
                created_at: session.updated_at_ms,
                updated_at: session.updated_at_ms,
                last_recalled_at: None,
                recall_count: 0,
                confidence,
                links: strings("links"),
                pinned: draft
                    .get("pinned")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                ttl_days: draft
                    .get("ttl_days")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|days| u32::try_from(days).ok())
                    .filter(|days| *days > 0),
            })
        })
        .collect()
}

/// Host-facing memory runtime policy (contract M1). Each capability is an independent switch: a
/// host can keep retrieval while turning off the auxiliary-model writes that run implicitly after
/// a session. `Disabled` is checked before any auxiliary provider call, so a disabled path costs
/// nothing. `CandidateSink` keeps a write path on but hands each write to the host's
/// [`MemoryCandidateSink`] (M2) instead of the memory store; a run refuses to start without one.
/// No policy (`RuntimeOptions::memory_runtime_policy = None`) keeps every path on and writing to
/// the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRuntimePolicy {
    pub retrieval: MemoryRetrievalPolicy,
    pub session_extraction: MemoryWritePolicy,
    pub model_writes: MemoryWritePolicy,
    pub semantic_archive: MemoryWritePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRetrievalPolicy {
    Disabled,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWritePolicy {
    Disabled,
    CandidateSink,
}

impl MemoryRuntimePolicy {
    /// The write paths set to `CandidateSink`, by their policy field names.
    pub fn candidate_sink_paths(&self) -> Vec<&'static str> {
        [
            ("session_extraction", self.session_extraction),
            ("model_writes", self.model_writes),
            ("semantic_archive", self.semantic_archive),
        ]
        .into_iter()
        .filter(|(_, policy)| *policy == MemoryWritePolicy::CandidateSink)
        .map(|(name, _)| name)
        .collect()
    }
}

/// Which write path produced a candidate. `ChildRequest` is the kernel's PersistMemory effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateSource {
    SessionExtraction,
    SemanticArchive,
    ChildRequest,
}

impl MemoryCandidateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionExtraction => "session_extraction",
            Self::SemanticArchive => "semantic_archive",
            Self::ChildRequest => "child_request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCandidateAttestation {
    pub issuer: String,
    pub key_id: String,
    pub agent_id: String,
    pub tenant_id: String,
    pub namespace: String,
    pub candidate_id: String,
    pub source: MemoryCandidateSource,
    pub payload_digest: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub policy_version: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateAttestationReasonCode {
    AttestationMissing,
    AttestationVerifierUnconfigured,
    AttestationIssuerUntrusted,
    AttestationScopeMismatch,
    AttestationCandidateMismatch,
    AttestationSourceMismatch,
    AttestationDigestMismatch,
    AttestationNotYetValid,
    AttestationExpired,
    AttestationSignatureInvalid,
    HostVerifiedRequiresHostAuthor,
}

impl MemoryCandidateAttestationReasonCode {
    pub fn label(self) -> &'static str {
        match self {
            Self::AttestationMissing => "attestation_missing",
            Self::AttestationVerifierUnconfigured => "attestation_verifier_unconfigured",
            Self::AttestationIssuerUntrusted => "attestation_issuer_untrusted",
            Self::AttestationScopeMismatch => "attestation_scope_mismatch",
            Self::AttestationCandidateMismatch => "attestation_candidate_mismatch",
            Self::AttestationSourceMismatch => "attestation_source_mismatch",
            Self::AttestationDigestMismatch => "attestation_digest_mismatch",
            Self::AttestationNotYetValid => "attestation_not_yet_valid",
            Self::AttestationExpired => "attestation_expired",
            Self::AttestationSignatureInvalid => "attestation_signature_invalid",
            Self::HostVerifiedRequiresHostAuthor => "host_verified_requires_host_author",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCandidate {
    /// Deterministic for the write that produced it (see [`memory_candidate_id`]): a retry of the
    /// same write carries the same id, so the sink can answer it from what it already holds.
    pub candidate_id: String,
    pub source: MemoryCandidateSource,
    pub record: MemoryRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<MemoryCandidateAttestation>,
}

/// The sink's answer. Its JSON form (`status` tag plus fields) is the body of the kernel's
/// `memory_candidate_result` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemoryCandidateReceipt {
    Pending {
        candidate_id: String,
        receipt_id: String,
    },
    Accepted {
        candidate_id: String,
        receipt_id: String,
        record_ref: String,
        digest: String,
    },
    Rejected {
        candidate_id: String,
        receipt_id: String,
        reason_code: String,
    },
}

impl MemoryCandidateReceipt {
    pub fn candidate_id(&self) -> &str {
        match self {
            Self::Pending { candidate_id, .. }
            | Self::Accepted { candidate_id, .. }
            | Self::Rejected { candidate_id, .. } => candidate_id,
        }
    }

    pub fn receipt_id(&self) -> &str {
        match self {
            Self::Pending { receipt_id, .. }
            | Self::Accepted { receipt_id, .. }
            | Self::Rejected { receipt_id, .. } => receipt_id,
        }
    }
}

/// The host's candidate store. Contract (spec §2.3):
///
/// - The sink mints `receipt_id`; it is the stable name of its answer.
/// - Identity is scoped by `agent_id`; source and review metadata are part of conflict detection.
/// - The same `candidate_id` with the same payload returns the original receipt and stores
///   nothing new, so a write whose acknowledgement was lost can be redelivered safely.
/// - The same `candidate_id` with a different payload is rejected with
///   `reason_code = "candidate_conflict"` and does not overwrite what is held.
/// - An error means no answer was obtained; the caller may redeliver the same candidate.
pub trait MemoryCandidateSourceVerifier: Send + Sync {
    fn verify(&self, agent_id: &str, candidate: &MemoryCandidate) -> std::result::Result<(), MemoryCandidateAttestationReasonCode>;
}

pub fn admit_memory_candidate(
    agent_id: &str,
    candidate: &MemoryCandidate,
    verifier: Option<&dyn MemoryCandidateSourceVerifier>,
) -> std::result::Result<(), MemoryCandidateAttestationReasonCode> {
    if candidate.record.provenance.trust == MemoryTrustLevel::HostVerified
        && candidate.record.provenance.author != MemoryAuthor::Host
    {
        return Err(MemoryCandidateAttestationReasonCode::HostVerifiedRequiresHostAuthor);
    }
    let requires = candidate.record.provenance.author == MemoryAuthor::Host
        && candidate.record.provenance.trust == MemoryTrustLevel::HostVerified;
    if requires || candidate.attestation.is_some() {
        return verifier.ok_or(MemoryCandidateAttestationReasonCode::AttestationVerifierUnconfigured)
            .and_then(|source_verifier| source_verifier.verify(agent_id, candidate));
    }
    Ok(())
}

pub struct Ed25519MemoryCandidateSourceVerifier {
    trusted_issuers: std::collections::HashMap<String, (String, String)>,
    now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    allowed_clock_skew_ms: u64,
    max_lifetime_ms: u64,
    policy_version: String,
}

impl Ed25519MemoryCandidateSourceVerifier {
    pub fn new(trusted_issuers: std::collections::HashMap<String, (String, String)>) -> Self {
        Self { trusted_issuers, now_ms: std::sync::Arc::new(now_ms), allowed_clock_skew_ms: 60_000, max_lifetime_ms: 600_000, policy_version: "memory-attestation-v1".into() }
    }

    pub fn with_clock(
        trusted_issuers: std::collections::HashMap<String, (String, String)>,
        now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self { trusted_issuers, now_ms, allowed_clock_skew_ms: 60_000, max_lifetime_ms: 600_000, policy_version: "memory-attestation-v1".into() }
    }
}

impl MemoryCandidateSourceVerifier for Ed25519MemoryCandidateSourceVerifier {
    fn verify(&self, agent_id: &str, candidate: &MemoryCandidate) -> std::result::Result<(), MemoryCandidateAttestationReasonCode> {
        use base64::Engine as _;
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let attestation = candidate.attestation.as_ref().ok_or(MemoryCandidateAttestationReasonCode::AttestationMissing)?;
        let (issuer, public_key) = self.trusted_issuers.get(&attestation.key_id).ok_or(MemoryCandidateAttestationReasonCode::AttestationIssuerUntrusted)?;
        if issuer != &attestation.issuer || attestation.agent_id != agent_id { return Err(MemoryCandidateAttestationReasonCode::AttestationScopeMismatch); }
        if attestation.tenant_id != candidate.record.scope.tenant_id || attestation.namespace != candidate.record.scope.namespace {
            return Err(MemoryCandidateAttestationReasonCode::AttestationScopeMismatch);
        }
        if attestation.candidate_id != candidate.candidate_id { return Err(MemoryCandidateAttestationReasonCode::AttestationCandidateMismatch); }
        if attestation.source != candidate.source { return Err(MemoryCandidateAttestationReasonCode::AttestationSourceMismatch); }
        if attestation.payload_digest != memory_candidate_payload_digest(&candidate.record) { return Err(MemoryCandidateAttestationReasonCode::AttestationDigestMismatch); }
        if attestation.policy_version != self.policy_version { return Err(MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid); }
        if attestation.expires_at <= attestation.issued_at || attestation.expires_at - attestation.issued_at > self.max_lifetime_ms {
            return Err(MemoryCandidateAttestationReasonCode::AttestationExpired);
        }
        let now = (self.now_ms)();
        if now.saturating_add(self.allowed_clock_skew_ms) < attestation.issued_at { return Err(MemoryCandidateAttestationReasonCode::AttestationNotYetValid); }
        if now.saturating_sub(self.allowed_clock_skew_ms) > attestation.expires_at { return Err(MemoryCandidateAttestationReasonCode::AttestationExpired); }
        let encoded_signature = attestation.signature.strip_prefix("ed25519:").ok_or(MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        let signature_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded_signature).map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        let signature = Signature::from_slice(&signature_bytes).map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(public_key.trim_end_matches('=')).map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        let key_array: [u8; 32] = key_bytes.try_into().map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        let verifying_key = VerifyingKey::from_bytes(&key_array).map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)?;
        verifying_key.verify(memory_candidate_attestation_canonical_payload(attestation).as_bytes(), &signature).map_err(|_| MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid)
    }
}

pub struct HmacMemoryCandidateSourceVerifier {
    trusted_issuers: std::collections::HashMap<String, (String, String)>,
    now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    allowed_clock_skew_ms: u64,
    max_lifetime_ms: u64,
    policy_version: String,
}

impl HmacMemoryCandidateSourceVerifier {
    pub fn new(trusted_issuers: std::collections::HashMap<String, (String, String)>) -> Self {
        Self { trusted_issuers, now_ms: std::sync::Arc::new(now_ms), allowed_clock_skew_ms: 60_000, max_lifetime_ms: 600_000, policy_version: "memory-attestation-v1".into() }
    }

    pub fn with_clock(
        trusted_issuers: std::collections::HashMap<String, (String, String)>,
        now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self { trusted_issuers, now_ms, allowed_clock_skew_ms: 60_000, max_lifetime_ms: 600_000, policy_version: "memory-attestation-v1".into() }
    }
}

impl MemoryCandidateSourceVerifier for HmacMemoryCandidateSourceVerifier {
    fn verify(&self, agent_id: &str, candidate: &MemoryCandidate) -> std::result::Result<(), MemoryCandidateAttestationReasonCode> {
        let attestation = candidate.attestation.as_ref().ok_or(MemoryCandidateAttestationReasonCode::AttestationMissing)?;
        let (issuer, key) = self.trusted_issuers.get(&attestation.key_id).ok_or(MemoryCandidateAttestationReasonCode::AttestationIssuerUntrusted)?;
        if issuer != &attestation.issuer || attestation.agent_id != agent_id { return Err(MemoryCandidateAttestationReasonCode::AttestationScopeMismatch); }
        if attestation.tenant_id != candidate.record.scope.tenant_id || attestation.namespace != candidate.record.scope.namespace {
            return Err(MemoryCandidateAttestationReasonCode::AttestationScopeMismatch);
        }
        if attestation.candidate_id != candidate.candidate_id { return Err(MemoryCandidateAttestationReasonCode::AttestationCandidateMismatch); }
        if attestation.source != candidate.source { return Err(MemoryCandidateAttestationReasonCode::AttestationSourceMismatch); }
        if attestation.payload_digest != memory_candidate_payload_digest(&candidate.record) { return Err(MemoryCandidateAttestationReasonCode::AttestationDigestMismatch); }
        if attestation.policy_version != self.policy_version { return Err(MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid); }
        if attestation.expires_at <= attestation.issued_at { return Err(MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid); }
        if attestation.expires_at - attestation.issued_at > self.max_lifetime_ms { return Err(MemoryCandidateAttestationReasonCode::AttestationExpired); }
        let now = (self.now_ms)();
        if now.saturating_add(self.allowed_clock_skew_ms) < attestation.issued_at { return Err(MemoryCandidateAttestationReasonCode::AttestationNotYetValid); }
        if now.saturating_sub(self.allowed_clock_skew_ms) > attestation.expires_at { return Err(MemoryCandidateAttestationReasonCode::AttestationExpired); }
        if !constant_time_equal(&sign_memory_candidate_attestation(attestation, key), &attestation.signature) {
            return Err(MemoryCandidateAttestationReasonCode::AttestationSignatureInvalid);
        }
        Ok(())
    }
}

pub fn memory_candidate_attestation_canonical_payload(attestation: &MemoryCandidateAttestation) -> String {
    serde_json::to_string(&[
        serde_json::json!(attestation.issuer), serde_json::json!(attestation.key_id), serde_json::json!(attestation.agent_id), serde_json::json!(attestation.tenant_id),
        serde_json::json!(attestation.namespace), serde_json::json!(attestation.candidate_id), serde_json::json!(attestation.source), serde_json::json!(attestation.payload_digest),
        serde_json::json!(attestation.issued_at), serde_json::json!(attestation.expires_at), serde_json::json!(attestation.policy_version),
    ]).expect("attestation canonical form serializes")
}

pub fn memory_candidate_attestation_digest(attestation: &MemoryCandidateAttestation) -> String {
    format!("sha256:{}", sha256_hex(&memory_candidate_attestation_canonical_payload(attestation)))
}

pub fn sign_memory_candidate_attestation(attestation: &MemoryCandidateAttestation, key: &str) -> String {
    let canonical = memory_candidate_attestation_canonical_payload(attestation);
    let digest = hmac_sha256(key.as_bytes(), canonical.as_bytes());
    format!("hmac-sha256:{}", digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut padded = [0u8; 64];
    if key.len() > 64 { padded[..32].copy_from_slice(&sha2::Sha256::digest(key)); } else { padded[..key.len()].copy_from_slice(key); }
    let mut inner = [0u8; 64];
    let mut outer = [0u8; 64];
    for index in 0..64 { inner[index] = padded[index] ^ 0x36; outer[index] = padded[index] ^ 0x5c; }
    let mut inner_input = inner.to_vec(); inner_input.extend_from_slice(message);
    let inner_digest = sha2::Sha256::digest(inner_input);
    let mut outer_input = outer.to_vec(); outer_input.extend_from_slice(&inner_digest);
    sha2::Sha256::digest(outer_input).into()
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    left.as_bytes().len() == right.as_bytes().len()
        && left.as_bytes().iter().zip(right.as_bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

#[async_trait]
pub trait MemoryCandidateSink: Send + Sync {
    async fn submit(
        &self,
        agent_id: &str,
        candidate: MemoryCandidate,
    ) -> crate::Result<MemoryCandidateReceipt>;
}

/// A state transition applied to a pending candidate by the host reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemoryCandidateResolution {
    Accepted { record_ref: String, digest: String },
    Rejected { reason_code: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCandidateLedgerEntry {
    pub agent_id: String,
    pub candidate: MemoryCandidate,
    pub payload_digest: String,
    pub receipt: MemoryCandidateReceipt,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCandidateOutboxEvent {
    pub event_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub agent_id: String,
    pub candidate_id: String,
    pub receipt: MemoryCandidateReceipt,
    pub occurred_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileCandidateState {
    version: u8,
    next_receipt: u64,
    next_event: u64,
    entries: Vec<MemoryCandidateLedgerEntry>,
    outbox: Vec<MemoryCandidateOutboxEvent>,
    #[serde(default)]
    verification_attempts: Vec<MemoryCandidateVerificationAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCandidateVerificationAttempt {
    pub attempt_id: String,
    pub agent_id: String,
    pub candidate_id: String,
    pub reason_code: String,
    #[serde(default)]
    pub attestation_digest: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub key_id: Option<String>,
    pub occurred_at: u64,
}

/// Durable file-backed candidate ledger with restart recovery and an atomic outbox snapshot.
///
/// A lock directory serializes writers across processes. Each state update is written to a private
/// file, fsynced, and atomically renamed over the snapshot. Invalid JSON or an invalid schema is a
/// hard error: the ledger never silently starts empty after corruption.
pub struct FileMemoryCandidateSink {
    path: PathBuf,
    lock_path: PathBuf,
    source_verifier: Option<std::sync::Arc<dyn MemoryCandidateSourceVerifier>>,
    state_lock: tokio::sync::Mutex<()>,
    lock_retry: Duration,
    lock_timeout: Duration,
}

impl FileMemoryCandidateSink {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self::with_source_verifier(path, None)
    }

    pub fn with_source_verifier(
        path: impl AsRef<Path>,
        source_verifier: Option<std::sync::Arc<dyn MemoryCandidateSourceVerifier>>,
    ) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            lock_path: PathBuf::from(format!("{}.lock", path.display())),
            path,
            source_verifier,
            state_lock: tokio::sync::Mutex::new(()),
            lock_retry: Duration::from_millis(10),
            lock_timeout: Duration::from_secs(30),
        }
    }

    pub async fn list(&self, agent_id: Option<&str>) -> crate::Result<Vec<MemoryCandidateLedgerEntry>> {
        let _guard = self.state_lock.lock().await;
        self.with_file_lock(|| async {
            Ok(self.read_state().await?.entries.into_iter()
                .filter(|entry| agent_id.is_none_or(|owner| owner == entry.agent_id))
                .collect())
        }).await
    }

    pub async fn resolve(
        &self,
        agent_id: &str,
        candidate_id: &str,
        resolution: MemoryCandidateResolution,
    ) -> crate::Result<MemoryCandidateReceipt> {
        let _guard = self.state_lock.lock().await;
        self.with_file_lock(|| async {
            let mut state = self.read_state().await?;
            let entry_index = state.entries.iter().position(|entry| {
                entry.agent_id == agent_id && entry.candidate.candidate_id == candidate_id
            }).ok_or_else(|| crate::Error::Other(format!("unknown memory candidate: {agent_id}/{candidate_id}")))?;
            if !matches!(state.entries[entry_index].receipt, MemoryCandidateReceipt::Pending { .. }) {
                return Ok(state.entries[entry_index].receipt.clone());
            }
            let receipt_id = state.entries[entry_index].receipt.receipt_id().to_string();
            let receipt = match resolution {
                MemoryCandidateResolution::Accepted { record_ref, digest } => MemoryCandidateReceipt::Accepted {
                    candidate_id: candidate_id.into(), receipt_id, record_ref, digest,
                },
                MemoryCandidateResolution::Rejected { reason_code } => MemoryCandidateReceipt::Rejected {
                    candidate_id: candidate_id.into(), receipt_id, reason_code,
                },
            };
            state.entries[entry_index].receipt = receipt.clone();
            state.entries[entry_index].updated_at = now_ms();
            let event_type = if matches!(receipt, MemoryCandidateReceipt::Accepted { .. }) {
                "candidate_accepted"
            } else { "candidate_rejected" };
            append_outbox(&mut state, event_type, agent_id, candidate_id, &receipt);
            self.write_state(&state).await?;
            Ok(receipt)
        }).await
    }

    pub async fn outbox(&self, agent_id: Option<&str>) -> crate::Result<Vec<MemoryCandidateOutboxEvent>> {
        let _guard = self.state_lock.lock().await;
        self.with_file_lock(|| async {
            Ok(self.read_state().await?.outbox.into_iter()
                .filter(|event| agent_id.is_none_or(|owner| owner == event.agent_id))
                .collect())
        }).await
    }

    pub async fn ack_outbox(&self, event_id: &str) -> crate::Result<bool> {
        let _guard = self.state_lock.lock().await;
        self.with_file_lock(|| async {
            let mut state = self.read_state().await?;
            let before = state.outbox.len();
            state.outbox.retain(|event| event.event_id != event_id);
            if state.outbox.len() == before { return Ok(false); }
            self.write_state(&state).await?;
            Ok(true)
        }).await
    }

    async fn read_state(&self) -> crate::Result<FileCandidateState> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let state: FileCandidateState = serde_json::from_slice(&bytes)
                    .map_err(|error| crate::Error::Other(format!("invalid memory candidate ledger {}: {error}", self.path.display())))?;
                if state.version != 1 || state.next_receipt == 0 || state.next_event == 0 {
                    return Err(crate::Error::Other(format!("invalid memory candidate ledger: {}", self.path.display())));
                }
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileCandidateState {
                version: 1, next_receipt: 1, next_event: 1, entries: Vec::new(), outbox: Vec::new(), verification_attempts: Vec::new(),
            }),
            Err(error) => Err(error.into()),
        }
    }

    async fn write_state(&self, state: &FileCandidateState) -> crate::Result<()> {
        if let Some(parent) = self.path.parent() { tokio::fs::create_dir_all(parent).await?; }
        let temporary = self.path.with_file_name(format!("{}.{}.tmp", self.path.file_name().unwrap().to_string_lossy(), uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| crate::Error::Other(error.to_string()))?;
        {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::File::create(&temporary).await?;
            file.write_all(&bytes).await?;
            file.sync_all().await?;
        }
        tokio::fs::rename(&temporary, &self.path).await?;
        if let Ok(directory) = tokio::fs::File::open(self.path.parent().unwrap_or(Path::new("."))).await {
            let _ = directory.sync_all().await;
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        Ok(())
    }

    async fn with_file_lock<T, F, Fut>(&self, operation: F) -> crate::Result<T>
    where F: FnOnce() -> Fut, Fut: std::future::Future<Output = crate::Result<T>> {
        if let Some(parent) = self.lock_path.parent() { tokio::fs::create_dir_all(parent).await?; }
        let started = tokio::time::Instant::now();
        loop {
            match tokio::fs::create_dir(&self.lock_path).await {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if started.elapsed() >= self.lock_timeout {
                        return Err(crate::Error::Other(format!("timed out acquiring memory candidate ledger lock: {}", self.lock_path.display())));
                    }
                    tokio::time::sleep(self.lock_retry).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let result = operation().await;
        let _ = tokio::fs::remove_dir(&self.lock_path).await;
        result
    }
}

#[async_trait]
impl MemoryCandidateSink for FileMemoryCandidateSink {
    async fn submit(&self, agent_id: &str, candidate: MemoryCandidate) -> crate::Result<MemoryCandidateReceipt> {
        let _guard = self.state_lock.lock().await;
        self.with_file_lock(|| async {
            let mut state = self.read_state().await?;
            let digest = memory_candidate_payload_digest(&candidate.record);
            if let Some(existing) = state.entries.iter().find(|entry| {
                entry.agent_id == agent_id && entry.candidate.candidate_id == candidate.candidate_id
            }) {
                if existing.payload_digest == digest && existing.candidate.source == candidate.source {
                    return Ok(existing.receipt.clone());
                }
                let receipt_id = mint_receipt(&mut state);
                let receipt = MemoryCandidateReceipt::Rejected {
                    candidate_id: candidate.candidate_id.clone(), receipt_id,
                    reason_code: "candidate_conflict".into(),
                };
                append_outbox(&mut state, "candidate_rejected", agent_id, &candidate.candidate_id, &receipt);
                self.write_state(&state).await?;
                return Ok(receipt);
            }
            if let Err(reason) = admit_memory_candidate(agent_id, &candidate, self.source_verifier.as_deref()) {
                let receipt = MemoryCandidateReceipt::Rejected {
                    candidate_id: candidate.candidate_id.clone(), receipt_id: mint_receipt(&mut state),
                    reason_code: reason.label().into(),
                };
                state.verification_attempts.push(MemoryCandidateVerificationAttempt {
                    attempt_id: format!("verification:{}", state.next_event), agent_id: agent_id.into(),
                    candidate_id: candidate.candidate_id.clone(), reason_code: reason.label().into(),
                    attestation_digest: candidate.attestation.as_ref().map(memory_candidate_attestation_digest),
                    issuer: candidate.attestation.as_ref().map(|attestation| attestation.issuer.clone()),
                    key_id: candidate.attestation.as_ref().map(|attestation| attestation.key_id.clone()),
                    occurred_at: now_ms(),
                });
                state.next_event += 1;
                self.write_state(&state).await?;
                return Ok(receipt);
            }
            let receipt = MemoryCandidateReceipt::Pending {
                candidate_id: candidate.candidate_id.clone(), receipt_id: mint_receipt(&mut state),
            };
            let timestamp = now_ms();
            state.entries.push(MemoryCandidateLedgerEntry {
                agent_id: agent_id.into(), candidate, payload_digest: digest, receipt: receipt.clone(),
                created_at: timestamp, updated_at: timestamp,
            });
            append_outbox(&mut state, "candidate_submitted", agent_id, receipt.candidate_id(), &receipt);
            self.write_state(&state).await?;
            Ok(receipt)
        }).await
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn mint_receipt(state: &mut FileCandidateState) -> String {
    let receipt = format!("receipt:{}", state.next_receipt);
    state.next_receipt += 1;
    receipt
}

fn append_outbox(state: &mut FileCandidateState, event_type: &str, agent_id: &str, candidate_id: &str, receipt: &MemoryCandidateReceipt) {
    state.outbox.push(MemoryCandidateOutboxEvent {
        event_id: format!("outbox:{}", state.next_event), event_type: event_type.into(), agent_id: agent_id.into(),
        candidate_id: candidate_id.into(), receipt: receipt.clone(), occurred_at: now_ms(),
    });
    state.next_event += 1;
}

/// A candidate id from the write's canonical identity — never random, so a retry repeats it.
/// The parts are hashed as a compact JSON array, byte-identical to the Node/Python/WASM rule.
pub fn memory_candidate_id(parts: &[&str]) -> String {
    format!(
        "candidate:{}",
        sha256_hex(&serde_json::to_string(parts).expect("string parts serialize"))
    )
}

/// The part of a record a candidate is judged by. Store ids, timestamps and recall counters are
/// excluded: a retry of the same write may mint them afresh without changing what is proposed.
pub fn memory_candidate_payload_digest(record: &MemoryRecord) -> String {
    // JSON.stringify writes an integral number without a fraction; match it so the digest is the
    // same one the other SDKs compute.
    let confidence = if record.confidence.fract() == 0.0 && record.confidence.abs() < 1e15 {
        serde_json::json!(record.confidence as i64)
    } else {
        serde_json::json!(record.confidence)
    };
    let payload = serde_json::json!([
        record.scope.tenant_id,
        record.scope.namespace,
        record.kind,
        record.name,
        record.description,
        record.content,
        record.provenance.author,
        record.provenance.trust,
        record.provenance.session_id,
        record.provenance.evidence_refs,
        confidence,
        record.links,
        record.pinned,
        record.ttl_days,
    ]);
    format!("sha256:{}", sha256_hex(&payload.to_string()))
}

/// M5 · the history turn one recalled record becomes. A host prefetch (turn 1 and after each
/// sprint renewal) lands every accepted hit in `history` as its own ordinary user turn, never in
/// `knowledge` (docs/guides/context-engineering.md, Level 5). The header names the record and its
/// trust level so the model can tell host-verified facts from untrusted ones; the retrieval score
/// is left out because it changes between retrievals while the record does not. Every SDK pins
/// the same vector.
pub fn memory_recall_turn(record: &MemoryRecord) -> String {
    let label = |value: serde_json::Value| value.as_str().unwrap_or_default().to_string();
    format!(
        "[MEMORY record_id={} kind={} trust={}] {}",
        record.record_id,
        label(serde_json::json!(record.kind)),
        label(serde_json::json!(record.provenance.trust)),
        record.content
    )
}

/// A session-extraction candidate id. The extraction model is re-asked on retry and may answer
/// differently, so the id is addressed by the proposed payload, not by its position in the answer:
/// a retry that proposes the same record repeats the id, one that proposes something new mints a
/// new candidate instead of colliding with what the first attempt left in the sink.
pub fn session_extraction_candidate_id(
    agent_id: &str,
    session_id: &str,
    operation_id: &str,
    record: &MemoryRecord,
) -> String {
    let digest = memory_candidate_payload_digest(record);
    memory_candidate_id(&[
        "v3",
        agent_id,
        MemoryCandidateSource::SessionExtraction.label(),
        session_id,
        operation_id,
        &digest,
    ])
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct HeldCandidate {
    agent_id: String,
    candidate: MemoryCandidate,
    payload_digest: String,
    receipt: MemoryCandidateReceipt,
}

/// In-process sink: every candidate is held as `pending`. For tests and local development; it
/// does not persist across processes and does not authenticate where a candidate came from.
#[derive(Default)]
pub struct InMemoryMemoryCandidateSink {
    state: std::sync::Mutex<InMemoryCandidateState>,
}

#[derive(Default)]
struct InMemoryCandidateState {
    held: Vec<HeldCandidate>,
    next_receipt: u64,
}

impl InMemoryCandidateState {
    fn mint_receipt_id(&mut self) -> String {
        self.next_receipt += 1;
        format!("receipt:{}", self.next_receipt)
    }
}

impl InMemoryMemoryCandidateSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Candidates held for `agent_id` (all agents when `None`), in submission order.
    pub fn candidates(&self, agent_id: Option<&str>) -> Vec<MemoryCandidate> {
        self.state
            .lock()
            .unwrap()
            .held
            .iter()
            .filter(|held| agent_id.is_none_or(|agent_id| held.agent_id == agent_id))
            .map(|held| held.candidate.clone())
            .collect()
    }
}

#[async_trait]
impl MemoryCandidateSink for InMemoryMemoryCandidateSink {
    async fn submit(
        &self,
        agent_id: &str,
        candidate: MemoryCandidate,
    ) -> crate::Result<MemoryCandidateReceipt> {
        let payload_digest = memory_candidate_payload_digest(&candidate.record);
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.held.iter().find(|held| {
            held.agent_id == agent_id && held.candidate.candidate_id == candidate.candidate_id
        }) {
            if existing.payload_digest == payload_digest
                && existing.candidate.source == candidate.source
            {
                return Ok(existing.receipt.clone());
            }
            return Ok(MemoryCandidateReceipt::Rejected {
                candidate_id: candidate.candidate_id,
                receipt_id: state.mint_receipt_id(),
                reason_code: "candidate_conflict".into(),
            });
        }
        let receipt = MemoryCandidateReceipt::Pending {
            candidate_id: candidate.candidate_id.clone(),
            receipt_id: state.mint_receipt_id(),
        };
        state.held.push(HeldCandidate {
            agent_id: agent_id.to_string(),
            candidate,
            payload_digest,
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }
}

/// In-process scratch pad for within-run state.
#[derive(Default)]
pub struct WorkingMemory {
    store: std::collections::HashMap<String, serde_json::Value>,
}

impl WorkingMemory {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.store.insert(key.into(), value.into());
    }
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.store.get(key)
    }
    pub fn clear(&mut self) {
        self.store.clear();
    }
}

/// `InMemoryMemoryStore` — a lightweight `MemoryStore` backed by per-agent in-memory maps.
///
/// Rust port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests, and local
/// development where persistent memory isn't needed. `search()` is a deterministic reference
/// ranker: distinct lexical overlap first, metadata recency second, insertion order last.
pub struct InMemoryMemoryStore {
    memories: std::sync::Mutex<std::collections::HashMap<String, Vec<MemoryRecord>>>,
    initial_memories: Vec<MemoryRecord>,
    saved_sessions: std::sync::Mutex<Vec<SessionData>>,
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::with_initial_memories(Vec::new())
    }

    pub fn with_initial_memories(initial: Vec<MemoryRecord>) -> Self {
        Self {
            memories: std::sync::Mutex::new(std::collections::HashMap::new()),
            initial_memories: initial,
            saved_sessions: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn saved_sessions(&self) -> Vec<SessionData> {
        self.saved_sessions.lock().unwrap().clone()
    }
}

impl Default for InMemoryMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn put(&self, agent_id: &str, incoming: MemoryRecord) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let kept = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        if let Some(index) = kept.iter().position(|record| {
            record.scope == incoming.scope
                && record.kind == incoming.kind
                && record.name == incoming.name
        }) {
            kept[index] = incoming;
        } else {
            kept.push(incoming);
        }
        Ok(())
    }

    async fn get(&self, agent_id: &str, record_id: &str) -> crate::Result<Option<MemoryRecord>> {
        let mut memories = self.memories.lock().unwrap();
        Ok(memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone())
            .iter()
            .find(|record| record.record_id == record_id)
            .cloned())
    }

    async fn delete(&self, agent_id: &str, record_id: &str) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let records = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        records.retain(|record| record.record_id != record_id);
        Ok(())
    }

    async fn search(
        &self,
        agent_id: &str,
        query: &MemoryQuery,
    ) -> crate::Result<Vec<MemoryRecall>> {
        let all = {
            let mut memories = self.memories.lock().unwrap();
            memories
                .entry(agent_id.to_string())
                .or_insert_with(|| self.initial_memories.clone())
                .clone()
        };
        let now = now_ms();
        let query_terms = memory_terms(&query.query);
        let mut ranked = all
            .into_iter()
            .enumerate()
            .filter(|(_, record)| {
                !memory_record_expired(record, now)
                    && record.scope == query.scope
                    && (query.kinds.is_empty() || query.kinds.contains(&record.kind))
                    && query
                        .min_score
                        .is_none_or(|minimum| record.confidence >= minimum)
            })
            .filter_map(|(insertion_index, record)| {
                let searchable =
                    format!("{} {} {}", record.name, record.description, record.content);
                let candidate_terms = memory_terms(&searchable);
                let lexical_matches = query_terms
                    .iter()
                    .filter(|term| candidate_terms.contains(*term))
                    .count();
                if !query_terms.is_empty() && lexical_matches == 0 {
                    return None;
                }
                Some((record, lexical_matches, insertion_index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
                .then_with(|| left.2.cmp(&right.2))
        });
        Ok(ranked
            .into_iter()
            .take(query.top_k)
            .map(|(record, _, _)| MemoryRecall {
                score: record.confidence.clamp(0.0, 1.0),
                record,
                why: "deterministic lexical relevance with recency tie-breaking".into(),
            })
            .collect())
    }

    async fn save_session(&self, data: SessionData) -> crate::Result<()> {
        self.saved_sessions.lock().unwrap().push(data);
        Ok(())
    }

    async fn record_recall(
        &self,
        agent_id: &str,
        recalls: &[MemoryRecallLifecycle],
    ) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let records = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        for recall in recalls {
            if let Some(record) = records
                .iter_mut()
                .find(|record| record.record_id == recall.record_id)
            {
                record.recall_count = recall.recall_count;
                record.last_recalled_at = Some(recall.last_recalled_at);
            }
        }
        Ok(())
    }
}

fn memory_terms(text: &str) -> std::collections::HashSet<String> {
    let mut terms = std::collections::HashSet::new();
    let mut segment = String::new();
    let flush = |segment: &mut String, terms: &mut std::collections::HashSet<String>| {
        if segment.is_empty() {
            return;
        }
        let lowered = segment.to_lowercase();
        terms.insert(lowered.clone());
        let characters = lowered.chars().collect::<Vec<_>>();
        if characters.iter().any(|character| is_han(*character)) {
            for pair in characters.windows(2) {
                terms.insert(pair.iter().collect());
            }
        }
        segment.clear();
    };
    for character in text.chars() {
        if character.is_alphanumeric() {
            segment.push(character);
        } else {
            flush(&mut segment, &mut terms);
        }
    }
    flush(&mut segment, &mut terms);
    terms
}

fn is_han(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

#[cfg(test)]
mod candidate_tests {
    use super::{
        InMemoryMemoryCandidateSink, MemoryCandidate, MemoryCandidateReceipt, MemoryCandidateSink,
        MemoryCandidateSource, MemoryRuntimePolicy, memory_candidate_attestation_canonical_payload,
        memory_candidate_attestation_digest, memory_candidate_id, memory_candidate_payload_digest,
        session_extraction_candidate_id,
    };
    use deepstrike_core::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryRecord, MemoryScope, MemoryTrustLevel,
    };

    #[test]
    fn attestation_vector_matches_the_other_sdks() {
        let attestation = super::MemoryCandidateAttestation {
            issuer: "issuer-a".into(), key_id: "ed-key-1".into(), agent_id: "agent".into(),
            tenant_id: "t".into(), namespace: "n".into(), candidate_id: "host-candidate".into(),
            source: MemoryCandidateSource::SessionExtraction,
            payload_digest: "sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f".into(),
            issued_at: 1000, expires_at: 2000, policy_version: "memory-attestation-v1".into(),
            signature: "ed25519:s0c5phB9Nk10XXPf2XQWqZBXfT1S2BZd-gPE7dWdUz2GLf9tpTCJJ9Ypy3aTUJL5X0cxYXrxNiFG2Ld-wHuYAQ".into(),
        };
        assert_eq!(
            memory_candidate_attestation_canonical_payload(&attestation),
            "[\"issuer-a\",\"ed-key-1\",\"agent\",\"t\",\"n\",\"host-candidate\",\"session_extraction\",\"sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f\",1000,2000,\"memory-attestation-v1\"]"
        );
        assert_eq!(memory_candidate_attestation_digest(&attestation), "sha256:d1b35cf5fc9eb479e350e6f606c4a162595c95daff94805276f8fdc4773c7a07");
    }

    fn record(content: &str) -> MemoryRecord {
        MemoryRecord {
            record_id: "memory:1".into(),
            scope: MemoryScope::new("tenant", "ns"),
            name: "proposal".into(),
            kind: MemoryKind::Project,
            content: content.into(),
            description: "a proposal".into(),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Model,
                trust: MemoryTrustLevel::Untrusted,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at: 1,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 1.0,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    fn candidate(id: &str, content: &str) -> MemoryCandidate {
        MemoryCandidate {
            candidate_id: id.into(),
            source: MemoryCandidateSource::ChildRequest,
            record: record(content),
            attestation: None,
        }
    }

    /// The fixed vector the Node, Python and WASM identity tests pin.
    #[test]
    fn candidate_ids_match_the_other_sdks() {
        assert_eq!(
            memory_candidate_id(&["child_request", "op-1", "effect-3", "0"]),
            "candidate:ead40e56240c408c06eeb95c7fe297bbb3b3c187c8cf181e0227aa269bcd7043"
        );
        assert_ne!(
            memory_candidate_id(&["a", "bc"]),
            memory_candidate_id(&["ab", "c"])
        );
    }

    /// The record, agent, session and operation every SDK's extraction-identity test pins.
    #[test]
    fn extraction_candidate_ids_are_payload_addressed_and_match_the_other_sdks() {
        let proposed = MemoryRecord {
            scope: MemoryScope::new("t", "n"),
            name: "brief-style".into(),
            content: "prefers numbered sections".into(),
            description: String::new(),
            provenance: MemoryProvenance {
                author: MemoryAuthor::Extraction,
                ..record("").provenance
            },
            confidence: 0.5,
            ..record("")
        };
        let id = session_extraction_candidate_id("agent", "s-1", "op-1", &proposed);
        assert_eq!(
            id,
            "candidate:8616f8c306bd0e0947ba3915403e02c1e47501fb8aed29e4d3a4ceec3e8207e7"
        );
        assert_ne!(
            session_extraction_candidate_id("agent", "s-1", "op-2", &proposed),
            id
        );
        assert_ne!(
            session_extraction_candidate_id("other", "s-1", "op-1", &proposed),
            id
        );
        // A retry that re-mints store id and timestamps proposes the same candidate; a retry
        // whose extraction answered differently proposes a new one rather than conflicting.
        let restamped = MemoryRecord {
            record_id: "memory:retry".into(),
            updated_at: 9,
            ..proposed.clone()
        };
        assert_eq!(
            session_extraction_candidate_id("agent", "s-1", "op-1", &restamped),
            id
        );
        let changed = MemoryRecord {
            content: "prefers numbered headings".into(),
            ..proposed
        };
        assert_ne!(
            session_extraction_candidate_id("agent", "s-1", "op-1", &changed),
            id
        );
    }

    #[test]
    fn the_payload_digest_ignores_store_ids_timestamps_and_recall_counts() {
        let base = record("fact");
        let digest = memory_candidate_payload_digest(&base);
        assert!(digest.starts_with("sha256:") && digest.len() == 7 + 64);
        let restamped = MemoryRecord {
            record_id: "memory:2".into(),
            created_at: 9,
            updated_at: 9,
            recall_count: 4,
            last_recalled_at: Some(9),
            ..base.clone()
        };
        assert_eq!(memory_candidate_payload_digest(&restamped), digest);
        let edited = MemoryRecord {
            confidence: 0.5,
            ..base
        };
        assert_ne!(memory_candidate_payload_digest(&edited), digest);
    }

    #[tokio::test]
    async fn the_in_memory_sink_replays_and_rejects_conflicts() {
        let sink = InMemoryMemoryCandidateSink::new();
        let first = sink.submit("a", candidate("c-1", "fact")).await.unwrap();
        assert!(matches!(first, MemoryCandidateReceipt::Pending { .. }));
        assert_eq!(
            sink.submit("a", candidate("c-1", "fact")).await.unwrap(),
            first
        );
        let conflict = sink.submit("a", candidate("c-1", "other")).await.unwrap();
        match &conflict {
            MemoryCandidateReceipt::Rejected {
                reason_code,
                receipt_id,
                ..
            } => {
                assert_eq!(reason_code, "candidate_conflict");
                assert_ne!(receipt_id, first.receipt_id());
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        let resourced = MemoryCandidate {
            source: MemoryCandidateSource::SessionExtraction,
            attestation: None,
            ..candidate("c-1", "fact")
        };
        assert!(matches!(
            sink.submit("a", resourced).await.unwrap(),
            MemoryCandidateReceipt::Rejected { .. }
        ));
        // Identity is scoped by agent.
        let other_agent = sink.submit("b", candidate("c-1", "fact")).await.unwrap();
        assert_ne!(other_agent.receipt_id(), first.receipt_id());
        assert_eq!(sink.candidates(Some("a")).len(), 1);
        assert_eq!(sink.candidates(Some("a"))[0].record.content, "fact");
        assert_eq!(sink.candidates(None).len(), 2);
    }

    #[test]
    fn receipts_and_policies_use_the_shared_wire_shape() {
        let receipt = MemoryCandidateReceipt::Rejected {
            candidate_id: "c".into(),
            receipt_id: "r".into(),
            reason_code: "candidate_conflict".into(),
        };
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "status": "rejected",
                "candidate_id": "c",
                "receipt_id": "r",
                "reason_code": "candidate_conflict",
            })
        );
        let policy: MemoryRuntimePolicy = serde_json::from_value(serde_json::json!({
            "retrieval": "host",
            "session_extraction": "candidate_sink",
            "model_writes": "disabled",
            "semantic_archive": "candidate_sink",
        }))
        .unwrap();
        assert_eq!(
            policy.candidate_sink_paths(),
            ["session_extraction", "semantic_archive"]
        );
        assert!(
            serde_json::from_value::<MemoryRuntimePolicy>(serde_json::json!({
                "retrieval": "host",
                "session_extraction": "store",
                "model_writes": "disabled",
                "semantic_archive": "disabled",
            }))
            .is_err(),
            "a write path is disabled or candidate_sink"
        );
        assert!(
            serde_json::from_value::<MemoryRuntimePolicy>(serde_json::json!({
                "retrieval": "host",
            }))
            .is_err(),
            "every switch is required"
        );
    }
}

#[cfg(test)]
mod ranking_tests {
    use super::{DurableMemory, InMemoryMemoryStore, MemorySearchOptions, MemoryStore};
    use deepstrike_core::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
        MemoryTrustLevel,
    };

    fn entry(text: &str, updated_at: u64) -> MemoryRecord {
        MemoryRecord {
            record_id: format!("record-{updated_at}"),
            scope: MemoryScope::new("tenant-test", "ranking"),
            name: text.into(),
            kind: MemoryKind::Project,
            content: text.into(),
            description: text.into(),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Host,
                trust: MemoryTrustLevel::HostVerified,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 1.0,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    #[tokio::test]
    async fn search_uses_query_and_never_falls_back_to_unrelated_entries() {
        let store = InMemoryMemoryStore::with_initial_memories(vec![
            entry("database migration checklist", 1),
            entry("rust scheduler fairness", 2),
            entry("newer unrelated note", 3),
        ]);

        let query = |text: &str| MemoryQuery {
            scope: MemoryScope::new("tenant-test", "ranking"),
            query: text.into(),
            top_k: 5,
            kinds: Vec::new(),
            min_score: None,
        };
        let hits = store
            .search("agent", &query("scheduler rust"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.content, "rust scheduler fairness");
        assert!(
            store
                .search("agent", &query("nonexistent"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn durable_memory_binds_crud_to_one_agent_scope() {
        let store: std::sync::Arc<dyn MemoryStore> =
            std::sync::Arc::new(InMemoryMemoryStore::new());
        let scope = MemoryScope::new("tenant-test", "public-contract");
        let memory = DurableMemory::new(store.clone(), "agent-a", scope.clone());
        let record = MemoryRecord {
            scope: scope.clone(),
            ..entry("architecture", 1)
        };

        memory.put(record.clone()).await.unwrap();
        assert_eq!(memory.namespace(), "public-contract");
        assert_eq!(
            memory.get(&record.record_id).await.unwrap(),
            Some(record.clone())
        );
        assert_eq!(
            memory
                .search("architecture", MemorySearchOptions::default())
                .await
                .unwrap(),
            vec![record.clone()]
        );
        memory.delete(&record.record_id).await.unwrap();
        assert_eq!(memory.get(&record.record_id).await.unwrap(), None);

        let foreign = MemoryRecord {
            scope: MemoryScope::new("tenant-test", "private"),
            ..entry("foreign", 2)
        };
        assert!(memory.put(foreign.clone()).await.is_err());
        store.put("agent-a", foreign.clone()).await.unwrap();
        assert_eq!(memory.get(&foreign.record_id).await.unwrap(), None);
        memory.delete(&foreign.record_id).await.unwrap();
        assert_eq!(
            store.get("agent-a", &foreign.record_id).await.unwrap(),
            Some(foreign)
        );
    }

    struct LeakyStore {
        foreign: MemoryRecord,
    }

    #[async_trait::async_trait]
    impl MemoryStore for LeakyStore {
        async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
            Ok(())
        }

        async fn get(
            &self,
            _agent_id: &str,
            _record_id: &str,
        ) -> crate::Result<Option<MemoryRecord>> {
            Ok(None)
        }

        async fn delete(&self, _agent_id: &str, _record_id: &str) -> crate::Result<()> {
            Ok(())
        }

        async fn search(
            &self,
            _agent_id: &str,
            _query: &MemoryQuery,
        ) -> crate::Result<Vec<deepstrike_core::mm::memory::MemoryRecall>> {
            Ok(vec![deepstrike_core::mm::memory::MemoryRecall {
                record: self.foreign.clone(),
                score: 1.0,
                why: "broken host store".into(),
            }])
        }

        async fn save_session(
            &self,
            _data: deepstrike_core::memory::durable::SessionData,
        ) -> crate::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn durable_memory_filters_cross_scope_host_search_results() {
        let scope = MemoryScope::new("tenant-test", "public-contract");
        let foreign = MemoryRecord {
            scope: MemoryScope::new("tenant-test", "private"),
            ..entry("foreign", 1)
        };
        let store: std::sync::Arc<dyn MemoryStore> = std::sync::Arc::new(LeakyStore { foreign });
        let memory = DurableMemory::new(store, "agent-a", scope);

        assert!(
            memory
                .search("private note", MemorySearchOptions::default())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
