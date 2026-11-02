//! M2 · memory candidate identity and source attestation.
//!
//! A candidate's id, the digest of what it proposes and the canonical form of a host-issued
//! attestation are pure functions of the write. They live here once, so every SDK hands the
//! same write the same identity and judges an attestation by the same checks in the same order.
//! The SDKs keep only what needs host state or platform crypto: key lookup and Ed25519.
//!
//! Canonical forms are compact JSON arrays written the way ECMAScript `JSON.stringify` writes
//! them — the rule the TypeScript SDKs have always hashed — so ids minted before this module
//! existed still match.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Which write path produced a candidate. `ChildRequest` is the kernel's PersistMemory effect for
/// a child agent's request; `ModelProposal` is the model's own `propose_memory` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateSource {
    ChildRequest,
    ModelProposal,
}

impl MemoryCandidateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::ChildRequest => "child_request",
            Self::ModelProposal => "model_proposal",
        }
    }
}

/// A host-issued proof that a candidate came from where it claims. `signature` is empty while
/// the attestation is being signed.
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
    #[serde(default)]
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

/// The part of a record a candidate is judged by. Store ids, timestamps and recall counters are
/// left out: a retry of the same write may mint them afresh without changing what is proposed.
/// Deserialized leniently from any SDK's record, so fields it does not know are ignored; kinds,
/// authors and trust levels stay strings because the digest hashes their labels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MemoryCandidatePayload {
    pub scope: PayloadScope,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub provenance: PayloadProvenance,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub ttl_days: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PayloadScope {
    pub tenant_id: String,
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PayloadProvenance {
    pub author: String,
    pub trust: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl MemoryCandidatePayload {
    pub fn of(record: &super::memory::MemoryRecord) -> Self {
        let label = |value: serde_json::Value| value.as_str().unwrap_or_default().to_string();
        Self {
            scope: PayloadScope {
                tenant_id: record.scope.tenant_id.clone(),
                namespace: record.scope.namespace.clone(),
            },
            kind: label(serde_json::json!(record.kind)),
            name: record.name.clone(),
            description: record.description.clone(),
            content: record.content.clone(),
            provenance: PayloadProvenance {
                author: label(serde_json::json!(record.provenance.author)),
                trust: label(serde_json::json!(record.provenance.trust)),
                session_id: record.provenance.session_id.clone(),
                evidence_refs: record.provenance.evidence_refs.clone(),
            },
            confidence: Some(record.confidence),
            links: record.links.clone(),
            pinned: record.pinned,
            ttl_days: record.ttl_days.map(f64::from),
        }
    }

    /// The `sha256:` digest of the payload's canonical array.
    pub fn digest(&self) -> String {
        let mut canon = Canon::default();
        canon.string(&self.scope.tenant_id);
        canon.string(&self.scope.namespace);
        canon.string(&self.kind);
        canon.string(&self.name);
        canon.string(&self.description);
        canon.string(&self.content);
        canon.string(&self.provenance.author);
        canon.string(&self.provenance.trust);
        canon.optional_string(self.provenance.session_id.as_deref());
        canon.strings(&self.provenance.evidence_refs);
        canon.optional_number(self.confidence);
        canon.strings(&self.links);
        canon.raw(if self.pinned { "true" } else { "false" });
        canon.optional_number(self.ttl_days);
        format!("sha256:{}", sha256_hex(canon.finish().as_bytes()))
    }
}

/// A candidate id from the write's canonical identity — never random, so a retry repeats it.
pub fn memory_candidate_id<S: AsRef<str>>(parts: &[S]) -> String {
    let mut canon = Canon::default();
    for part in parts {
        canon.string(part.as_ref());
    }
    format!("candidate:{}", sha256_hex(canon.finish().as_bytes()))
}

pub fn memory_candidate_payload_digest(record: &super::memory::MemoryRecord) -> String {
    MemoryCandidatePayload::of(record).digest()
}

/// The bytes an attestation's signature covers: every field but the signature, in fixed order.
pub fn memory_candidate_attestation_canonical_payload(
    attestation: &MemoryCandidateAttestation,
) -> String {
    let mut canon = Canon::default();
    canon.string(&attestation.issuer);
    canon.string(&attestation.key_id);
    canon.string(&attestation.agent_id);
    canon.string(&attestation.tenant_id);
    canon.string(&attestation.namespace);
    canon.string(&attestation.candidate_id);
    canon.string(attestation.source.label());
    canon.string(&attestation.payload_digest);
    canon.raw(&attestation.issued_at.to_string());
    canon.raw(&attestation.expires_at.to_string());
    canon.string(&attestation.policy_version);
    canon.finish()
}

pub fn memory_candidate_attestation_digest(attestation: &MemoryCandidateAttestation) -> String {
    format!(
        "sha256:{}",
        sha256_hex(memory_candidate_attestation_canonical_payload(attestation).as_bytes())
    )
}

/// The `hmac-sha256:` signature of an attestation under a shared secret.
pub fn sign_memory_candidate_attestation(
    attestation: &MemoryCandidateAttestation,
    secret: &str,
) -> String {
    let canonical = memory_candidate_attestation_canonical_payload(attestation);
    let mac = hmac_sha256(secret.as_bytes(), canonical.as_bytes());
    format!("hmac-sha256:{}", hex(&mac))
}

/// What a candidate's provenance asks of its source check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCandidateSourceCheck {
    /// No attestation is required and none was supplied.
    Admit,
    /// An attestation was supplied (or is required and present): a verifier must judge it.
    Verify,
    Reject(MemoryCandidateAttestationReasonCode),
}

/// The provenance rule every sink applies before it holds a candidate: only the host may claim
/// `host_verified`, and that claim must carry an attestation. Any attestation supplied is
/// verified, required or not.
pub fn memory_candidate_source_check(
    author: &str,
    trust: &str,
    has_attestation: bool,
) -> MemoryCandidateSourceCheck {
    use MemoryCandidateAttestationReasonCode as Code;
    if trust == "host_verified" && author != "host" {
        return MemoryCandidateSourceCheck::Reject(Code::HostVerifiedRequiresHostAuthor);
    }
    match (has_attestation, trust == "host_verified") {
        (true, _) => MemoryCandidateSourceCheck::Verify,
        (false, true) => MemoryCandidateSourceCheck::Reject(Code::AttestationMissing),
        (false, false) => MemoryCandidateSourceCheck::Admit,
    }
}

/// The verifier's settings and clock, as the host configured them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationWindow {
    pub now_ms: u64,
    #[serde(default = "default_clock_skew_ms")]
    pub allowed_clock_skew_ms: u64,
    #[serde(default = "default_max_lifetime_ms")]
    pub max_lifetime_ms: u64,
    #[serde(default = "default_policy_version")]
    pub policy_version: String,
}

fn default_clock_skew_ms() -> u64 {
    60_000
}
fn default_max_lifetime_ms() -> u64 {
    600_000
}
fn default_policy_version() -> String {
    "memory-attestation-v1".into()
}

/// The candidate an attestation is checked against.
pub struct AttestedCandidate<'a> {
    pub agent_id: &'a str,
    pub candidate_id: &'a str,
    pub source: MemoryCandidateSource,
    pub payload: &'a MemoryCandidatePayload,
}

/// Every check of an attestation except the signature, in the order every SDK reports them.
/// `trusted_issuer` is the issuer the host's key table names for `attestation.key_id`, or `None`
/// when it holds no such key. The caller verifies the signature only after this passes.
pub fn check_memory_candidate_attestation(
    candidate: &AttestedCandidate<'_>,
    attestation: &MemoryCandidateAttestation,
    trusted_issuer: Option<&str>,
    window: &AttestationWindow,
) -> Result<(), MemoryCandidateAttestationReasonCode> {
    use MemoryCandidateAttestationReasonCode as Code;
    if trusted_issuer != Some(attestation.issuer.as_str()) {
        return Err(Code::AttestationIssuerUntrusted);
    }
    if attestation.agent_id != candidate.agent_id
        || attestation.tenant_id != candidate.payload.scope.tenant_id
        || attestation.namespace != candidate.payload.scope.namespace
    {
        return Err(Code::AttestationScopeMismatch);
    }
    if attestation.candidate_id != candidate.candidate_id {
        return Err(Code::AttestationCandidateMismatch);
    }
    if attestation.source != candidate.source {
        return Err(Code::AttestationSourceMismatch);
    }
    if attestation.payload_digest != candidate.payload.digest() {
        return Err(Code::AttestationDigestMismatch);
    }
    if attestation.policy_version != window.policy_version {
        return Err(Code::AttestationSignatureInvalid);
    }
    if attestation.expires_at <= attestation.issued_at
        || attestation.expires_at - attestation.issued_at > window.max_lifetime_ms
    {
        return Err(Code::AttestationExpired);
    }
    if window.now_ms.saturating_add(window.allowed_clock_skew_ms) < attestation.issued_at {
        return Err(Code::AttestationNotYetValid);
    }
    if window.now_ms.saturating_sub(window.allowed_clock_skew_ms) > attestation.expires_at {
        return Err(Code::AttestationExpired);
    }
    Ok(())
}

/// Does `attestation.signature` match its HMAC under `secret`? Compared in constant time.
pub fn verify_memory_candidate_attestation_hmac(
    attestation: &MemoryCandidateAttestation,
    secret: &str,
) -> bool {
    let expected = sign_memory_candidate_attestation(attestation, secret);
    let (left, right) = (expected.as_bytes(), attestation.signature.as_bytes());
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// A compact JSON array in `JSON.stringify` form.
#[derive(Default)]
struct Canon {
    out: String,
    any: bool,
}

impl Canon {
    fn raw(&mut self, value: &str) {
        self.out.push(if self.any { ',' } else { '[' });
        self.any = true;
        self.out.push_str(value);
    }

    fn string(&mut self, value: &str) {
        self.raw(&serde_json::to_string(value).expect("a string serializes"));
    }

    fn optional_string(&mut self, value: Option<&str>) {
        match value {
            Some(value) => self.string(value),
            None => self.raw("null"),
        }
    }

    fn strings(&mut self, values: &[String]) {
        self.raw(&serde_json::to_string(values).expect("strings serialize"));
    }

    fn optional_number(&mut self, value: Option<f64>) {
        match value {
            Some(value) => self.raw(&js_number(value)),
            None => self.raw("null"),
        }
    }

    fn finish(mut self) -> String {
        if !self.any {
            self.out.push('[');
        }
        self.out.push(']');
        self.out
    }
}

/// ECMAScript `Number::toString` for a finite number (`JSON.stringify` writes a non-finite one
/// as `null`). Rust's shortest round-trip digits are the same digits; only the layout differs.
fn js_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("{:e} writes an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    let n = exponent.parse::<i32>().expect("an integral exponent") + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let fraction = if k > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{}{fraction}e{sign}{}", &digits[..1], (n - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 64];
    if key.len() > 64 {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(padded.map(|byte| byte ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(padded.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryRecord, MemoryScope, MemoryTrustLevel,
    };

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

    fn attestation() -> MemoryCandidateAttestation {
        MemoryCandidateAttestation {
            issuer: "issuer-a".into(),
            key_id: "ed-key-1".into(),
            agent_id: "agent".into(),
            tenant_id: "t".into(),
            namespace: "n".into(),
            candidate_id: "host-candidate".into(),
            source: MemoryCandidateSource::ModelProposal,
            payload_digest:
                "sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f".into(),
            issued_at: 1000,
            expires_at: 2000,
            policy_version: "memory-attestation-v1".into(),
            signature: String::new(),
        }
    }

    /// The vectors the Node, Python, WASM and Rust SDK identity tests have always pinned.
    #[test]
    fn identities_match_the_vectors_every_sdk_pins() {
        assert_eq!(
            memory_candidate_id(&["child_request", "op-1", "effect-3", "0"]),
            "candidate:ead40e56240c408c06eeb95c7fe297bbb3b3c187c8cf181e0227aa269bcd7043"
        );
        assert_ne!(
            memory_candidate_id(&["a", "bc"]),
            memory_candidate_id(&["ab", "c"])
        );
        assert_eq!(
            memory_candidate_attestation_canonical_payload(&attestation()),
            "[\"issuer-a\",\"ed-key-1\",\"agent\",\"t\",\"n\",\"host-candidate\",\"model_proposal\",\"sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f\",1000,2000,\"memory-attestation-v1\"]"
        );
        assert_eq!(
            memory_candidate_attestation_digest(&attestation()),
            "sha256:d69f2de333253735f34e09cd5ef2d8d9c2c71b9b066d302e9a7716bb8b19b63e"
        );
        // tests/fixtures/memory-attestation-ed25519.json: `record_digest_input` → `payload_digest`.
        let host_fact = MemoryRecord {
            scope: MemoryScope::new("t", "n"),
            name: "brief-style".into(),
            description: String::new(),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Host,
                trust: MemoryTrustLevel::HostVerified,
                evidence_refs: Vec::new(),
            },
            confidence: 0.5,
            ..record("host fact")
        };
        assert_eq!(
            memory_candidate_payload_digest(&host_fact),
            "sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f"
        );
    }

    #[test]
    fn the_payload_digest_ignores_store_ids_timestamps_and_recall_counts() {
        let base = record("fact");
        let digest = memory_candidate_payload_digest(&base);
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

    /// A record read from an SDK's JSON digests exactly as the typed record does.
    #[test]
    fn a_record_from_json_digests_like_the_typed_record() {
        let typed = record("fact");
        let json: MemoryCandidatePayload =
            serde_json::from_value(serde_json::to_value(&typed).unwrap()).unwrap();
        assert_eq!(json.digest(), memory_candidate_payload_digest(&typed));
    }

    #[test]
    fn numbers_are_written_as_json_stringify_writes_them() {
        for (value, expected) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (-0.25, "-0.25"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e21, "1e+21"),
            (0.1 + 0.2, "0.30000000000000004"),
            (30.0, "30"),
        ] {
            assert_eq!(js_number(value), expected, "{value}");
        }
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn checks_run_in_the_shared_order() {
        use MemoryCandidateAttestationReasonCode as Code;
        let payload = MemoryCandidatePayload::of(&MemoryRecord {
            scope: MemoryScope::new("t", "n"),
            ..record("fact")
        });
        let mut attested = attestation();
        attested.payload_digest = payload.digest();
        let candidate = AttestedCandidate {
            agent_id: "agent",
            candidate_id: "host-candidate",
            source: MemoryCandidateSource::ModelProposal,
            payload: &payload,
        };
        let window = AttestationWindow {
            now_ms: 1500,
            allowed_clock_skew_ms: 0,
            max_lifetime_ms: 600_000,
            policy_version: "memory-attestation-v1".into(),
        };
        let check = |attestation: &MemoryCandidateAttestation, issuer: Option<&str>| {
            check_memory_candidate_attestation(&candidate, attestation, issuer, &window)
        };
        assert_eq!(check(&attested, Some("issuer-a")), Ok(()));
        assert_eq!(
            check(&attested, None),
            Err(Code::AttestationIssuerUntrusted)
        );
        assert_eq!(
            check(&attested, Some("issuer-b")),
            Err(Code::AttestationIssuerUntrusted)
        );
        let wrong_agent = MemoryCandidateAttestation {
            agent_id: "other".into(),
            ..attested.clone()
        };
        assert_eq!(
            check(&wrong_agent, Some("issuer-a")),
            Err(Code::AttestationScopeMismatch)
        );
        let inverted = MemoryCandidateAttestation {
            expires_at: 1000,
            ..attested.clone()
        };
        assert_eq!(
            check(&inverted, Some("issuer-a")),
            Err(Code::AttestationExpired)
        );
        let early = MemoryCandidateAttestation {
            issued_at: 1600,
            expires_at: 2000,
            ..attested.clone()
        };
        assert_eq!(
            check(&early, Some("issuer-a")),
            Err(Code::AttestationNotYetValid)
        );

        let signed = MemoryCandidateAttestation {
            signature: sign_memory_candidate_attestation(&attested, "secret"),
            ..attested
        };
        assert!(verify_memory_candidate_attestation_hmac(&signed, "secret"));
        assert!(!verify_memory_candidate_attestation_hmac(&signed, "other"));
    }

    #[test]
    fn only_the_host_may_claim_host_verified_and_it_must_attest() {
        use MemoryCandidateAttestationReasonCode as Code;
        use MemoryCandidateSourceCheck as Check;
        assert_eq!(
            memory_candidate_source_check("model", "host_verified", true),
            Check::Reject(Code::HostVerifiedRequiresHostAuthor)
        );
        assert_eq!(
            memory_candidate_source_check("host", "host_verified", false),
            Check::Reject(Code::AttestationMissing)
        );
        assert_eq!(
            memory_candidate_source_check("host", "host_verified", true),
            Check::Verify
        );
        assert_eq!(
            memory_candidate_source_check("model", "untrusted", true),
            Check::Verify
        );
        assert_eq!(
            memory_candidate_source_check("model", "untrusted", false),
            Check::Admit
        );
    }
}
