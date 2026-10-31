//! §22.13 · the memory authority for a host that holds no live operation.
//!
//! Inside an operation every memory decision is a kernel transition: the model's `write_memory`
//! and `query_memory` syscalls, and the host's `admit_memory_write` command. Some host writes and
//! recalls happen with no operation at all — an explicit `remember`/`recall` call, a session
//! extract after the run has ended — and those are answered here, statelessly, by the very same
//! functions. No SDK carries a copy of the validation rule or computes a recall count itself.
//!
//! The same bridge answers M2 candidate identity: candidate ids, payload digests, attestation
//! canonical forms and every source check but an Ed25519 signature (`mm::memory_candidate`).

use serde::{Deserialize, Serialize};

use super::config::{MemoryPolicy, ResolvedMemoryPolicy};
use crate::mm::memory::{
    MemoryPromotion, MemoryRecallLifecycle, MemoryRecallPrior, derive_recall_lifecycle,
};
use crate::mm::memory_candidate::{
    AttestationWindow, AttestedCandidate, MemoryCandidateAttestation,
    MemoryCandidateAttestationReasonCode, MemoryCandidatePayload, MemoryCandidateSource,
    MemoryCandidateSourceCheck, check_memory_candidate_attestation,
    memory_candidate_attestation_canonical_payload, memory_candidate_attestation_digest,
    memory_candidate_id, memory_candidate_source_check, sign_memory_candidate_attestation,
    verify_memory_candidate_attestation_hmac,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryAuthorityRequest {
    /// Would the policy admit a write with this name and this many content bytes?
    CheckWrite {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<MemoryPolicy>,
        name: String,
        content_bytes: u32,
    },
    /// Derive the lifecycle of one recall from the state the store reported it held.
    DeriveRecall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<MemoryPolicy>,
        recalled_at: u64,
        recalls: Vec<MemoryRecallPrior>,
    },
    /// The candidate id for a write's identity parts.
    CandidateId { parts: Vec<String> },
    /// The digest of what a candidate proposes. `record` is the SDK's record as JSON.
    CandidatePayloadDigest { record: serde_json::Value },
    /// An attestation's canonical payload and digest, and its HMAC signature under `secret`.
    Attestation {
        attestation: MemoryCandidateAttestation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
    /// Judge a candidate's source before a sink holds it.
    VerifySource {
        agent_id: String,
        candidate: SourceCandidate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier: Option<SourceVerifier>,
    },
}

/// A candidate as the sink received it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCandidate {
    pub candidate_id: String,
    pub source: MemoryCandidateSource,
    pub record: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<MemoryCandidateAttestation>,
}

/// What the host's verifier knows for this attestation: the issuer its key table names for
/// `attestation.key_id` (absent when it holds no such key), the HMAC secret when the key is a
/// shared secret, and its clock and limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceVerifier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hmac_secret: Option<String>,
    pub window: AttestationWindow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MemoryAuthorityResponse {
    Write {
        admitted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Recall {
        recalls: Vec<MemoryRecallLifecycle>,
        promotions: Vec<MemoryPromotion>,
    },
    CandidateId {
        candidate_id: String,
    },
    PayloadDigest {
        payload_digest: String,
    },
    Attestation {
        canonical_payload: String,
        digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// `valid` with a `signed_payload` means every check passed but the signature, which the
    /// host verifies over exactly those bytes (an Ed25519 key never enters the kernel).
    Source {
        valid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<MemoryCandidateAttestationReasonCode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed_payload: Option<String>,
    },
}

pub fn answer(request: &MemoryAuthorityRequest) -> Result<MemoryAuthorityResponse, String> {
    let resolve = |policy: &Option<MemoryPolicy>| {
        ResolvedMemoryPolicy::resolve(policy.as_ref()).map_err(|rejection| rejection.message)
    };
    Ok(match request {
        MemoryAuthorityRequest::CheckWrite {
            policy,
            name,
            content_bytes,
        } => match resolve(policy)?.check_write(name, *content_bytes as usize) {
            Ok(()) => MemoryAuthorityResponse::Write {
                admitted: true,
                error: None,
            },
            Err(error) => MemoryAuthorityResponse::Write {
                admitted: false,
                error: Some(error),
            },
        },
        MemoryAuthorityRequest::DeriveRecall {
            policy,
            recalled_at,
            recalls,
        } => {
            let threshold = resolve(policy)?
                .promotion_recall_threshold
                .map(|threshold| threshold.get());
            let (recalls, promotions) = derive_recall_lifecycle(recalls, *recalled_at, threshold);
            MemoryAuthorityResponse::Recall {
                recalls,
                promotions,
            }
        }
        MemoryAuthorityRequest::CandidateId { parts } => MemoryAuthorityResponse::CandidateId {
            candidate_id: memory_candidate_id(parts),
        },
        MemoryAuthorityRequest::CandidatePayloadDigest { record } => {
            MemoryAuthorityResponse::PayloadDigest {
                payload_digest: payload(record)?.digest(),
            }
        }
        MemoryAuthorityRequest::Attestation {
            attestation,
            secret,
        } => MemoryAuthorityResponse::Attestation {
            canonical_payload: memory_candidate_attestation_canonical_payload(attestation),
            digest: memory_candidate_attestation_digest(attestation),
            signature: secret
                .as_deref()
                .map(|secret| sign_memory_candidate_attestation(attestation, secret)),
        },
        MemoryAuthorityRequest::VerifySource {
            agent_id,
            candidate,
            verifier,
        } => verify_source(agent_id, candidate, verifier.as_ref())?,
    })
}

fn payload(record: &serde_json::Value) -> Result<MemoryCandidatePayload, String> {
    MemoryCandidatePayload::deserialize(record)
        .map_err(|error| format!("invalid memory candidate record: {error}"))
}

fn verify_source(
    agent_id: &str,
    candidate: &SourceCandidate,
    verifier: Option<&SourceVerifier>,
) -> Result<MemoryAuthorityResponse, String> {
    use MemoryCandidateAttestationReasonCode as Code;
    let refuse = |code: Code| MemoryAuthorityResponse::Source {
        valid: false,
        reason_code: Some(code),
        signed_payload: None,
    };
    let payload = payload(&candidate.record)?;
    let check = memory_candidate_source_check(
        &payload.provenance.author,
        &payload.provenance.trust,
        candidate.attestation.is_some(),
    );
    let attestation = match (check, candidate.attestation.as_ref()) {
        (MemoryCandidateSourceCheck::Reject(code), _) => return Ok(refuse(code)),
        (MemoryCandidateSourceCheck::Verify, Some(attestation)) => attestation,
        _ => {
            return Ok(MemoryAuthorityResponse::Source {
                valid: true,
                reason_code: None,
                signed_payload: None,
            });
        }
    };
    let Some(verifier) = verifier else {
        return Ok(refuse(Code::AttestationVerifierUnconfigured));
    };
    let attested = AttestedCandidate {
        agent_id,
        candidate_id: &candidate.candidate_id,
        source: candidate.source,
        payload: &payload,
    };
    if let Err(code) = check_memory_candidate_attestation(
        &attested,
        attestation,
        verifier.trusted_issuer.as_deref(),
        &verifier.window,
    ) {
        return Ok(refuse(code));
    }
    Ok(match verifier.hmac_secret.as_deref() {
        Some(secret) if !verify_memory_candidate_attestation_hmac(attestation, secret) => {
            refuse(Code::AttestationSignatureInvalid)
        }
        Some(_) => MemoryAuthorityResponse::Source {
            valid: true,
            reason_code: None,
            signed_payload: None,
        },
        None => MemoryAuthorityResponse::Source {
            valid: true,
            reason_code: None,
            signed_payload: Some(memory_candidate_attestation_canonical_payload(attestation)),
        },
    })
}

/// The JSON bridge every binding exposes.
pub fn memory_authority_json(request: &str) -> Result<String, String> {
    let request: MemoryAuthorityRequest = serde_json::from_str(request)
        .map_err(|error| format!("invalid memory authority request: {error}"))?;
    serde_json::to_string(&answer(&request)?)
        .map_err(|error| format!("could not encode memory authority response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn ask(request: Value) -> Value {
        serde_json::from_str(&memory_authority_json(&request.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn a_write_is_judged_by_the_kernel_policy_with_the_kernel_defaults() {
        assert_eq!(
            ask(json!({ "op": "check_write", "name": "brief", "content_bytes": 10 })),
            json!({ "admitted": true })
        );
        assert_eq!(
            ask(json!({ "op": "check_write", "name": "  ", "content_bytes": 10 })),
            json!({ "admitted": false, "error": "memory name must not be empty" })
        );
        assert_eq!(
            ask(json!({ "op": "check_write", "name": "brief", "content_bytes": 10_001 })),
            json!({ "admitted": false, "error": "memory content exceeds 10000 bytes" })
        );
        assert_eq!(
            ask(json!({
                "op": "check_write",
                "policy": { "max_content_bytes": 4 },
                "name": "brief",
                "content_bytes": 5,
            })),
            json!({ "admitted": false, "error": "memory content exceeds 4 bytes" })
        );
        assert_eq!(
            ask(json!({
                "op": "check_write",
                "policy": { "validation_enabled": false },
                "name": "",
                "content_bytes": 1_000_000,
            })),
            json!({ "admitted": true })
        );
    }

    #[test]
    fn a_recall_count_is_derived_from_the_stored_count_and_never_supplied() {
        assert_eq!(
            ask(json!({
                "op": "derive_recall",
                "policy": { "promotion_recall_threshold": "2" },
                "recalled_at": 99,
                "recalls": [
                    { "record_id": "a", "recall_count": 1 },
                    { "record_id": "b", "recall_count": 1, "pinned": true },
                ],
            })),
            json!({
                "recalls": [
                    { "record_id": "a", "recall_count": 2, "last_recalled_at": 99 },
                    { "record_id": "b", "recall_count": 2, "last_recalled_at": 99 },
                ],
                "promotions": [{ "record_id": "a", "recall_count": 2 }],
            })
        );
    }

    #[test]
    fn candidate_identity_is_answered_by_the_kernel() {
        assert_eq!(
            ask(
                json!({ "op": "candidate_id", "parts": ["child_request", "op-1", "effect-3", "0"] })
            ),
            json!({ "candidate_id": "candidate:ead40e56240c408c06eeb95c7fe297bbb3b3c187c8cf181e0227aa269bcd7043" })
        );
        let record = json!({
            "record_id": "memory:1", "scope": { "tenant_id": "tenant", "namespace": "ns" },
            "name": "proposal", "kind": "project", "content": "fact", "description": "a proposal",
            "provenance": { "author": "model", "trust": "untrusted", "evidence_refs": [] },
            "created_at": 1, "updated_at": 1, "recall_count": 0, "confidence": 1,
            "links": [], "pinned": false, "embedding_hint": "ignored",
        });
        let digest = ask(json!({ "op": "candidate_payload_digest", "record": record }));
        assert!(
            digest["payload_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn a_source_is_verified_up_to_the_signature_the_host_holds() {
        let record = json!({
            "scope": { "tenant_id": "t", "namespace": "n" }, "name": "fact", "kind": "user",
            "content": "c", "description": "d", "confidence": 1, "links": [], "pinned": false,
            "provenance": { "author": "host", "trust": "host_verified", "evidence_refs": [] },
        });
        let digest =
            ask(json!({ "op": "candidate_payload_digest", "record": record }))["payload_digest"]
                .clone();
        let mut attestation = json!({
            "issuer": "issuer-a", "key_id": "k", "agent_id": "agent", "tenant_id": "t",
            "namespace": "n", "candidate_id": "c-1", "source": "model_proposal",
            "payload_digest": digest, "issued_at": 1000, "expires_at": 2000,
            "policy_version": "memory-attestation-v1",
        });
        let signature = ask(
            json!({ "op": "attestation", "attestation": attestation, "secret": "s" }),
        )["signature"]
            .clone();
        attestation["signature"] = signature;
        let candidate = json!({
            "candidate_id": "c-1", "source": "model_proposal", "record": record, "attestation": attestation,
        });
        let window = json!({ "now_ms": 1500 });
        let verify = |verifier: Value| {
            ask(
                json!({ "op": "verify_source", "agent_id": "agent", "candidate": candidate, "verifier": verifier }),
            )
        };
        assert_eq!(
            verify(json!({ "trusted_issuer": "issuer-a", "hmac_secret": "s", "window": window })),
            json!({ "valid": true })
        );
        assert_eq!(
            verify(json!({ "trusted_issuer": "issuer-a", "hmac_secret": "x", "window": window })),
            json!({ "valid": false, "reason_code": "attestation_signature_invalid" })
        );
        assert_eq!(
            verify(json!({ "window": window })),
            json!({ "valid": false, "reason_code": "attestation_issuer_untrusted" })
        );
        let ed25519 = verify(json!({ "trusted_issuer": "issuer-a", "window": window }));
        assert_eq!(ed25519["valid"], json!(true));
        assert!(
            ed25519["signed_payload"]
                .as_str()
                .unwrap()
                .starts_with("[\"issuer-a\"")
        );
        assert_eq!(
            ask(json!({ "op": "verify_source", "agent_id": "agent", "candidate": candidate })),
            json!({ "valid": false, "reason_code": "attestation_verifier_unconfigured" })
        );
    }

    #[test]
    fn an_invalid_policy_or_request_is_refused() {
        assert!(
            memory_authority_json(
                &json!({ "op": "check_write", "policy": { "retrieval_top_k": 0 }, "name": "a", "content_bytes": 1 })
                    .to_string()
            )
            .is_err()
        );
        assert!(
            memory_authority_json(
                &json!({ "op": "check_write", "name": "a", "content": "x" }).to_string()
            )
            .is_err()
        );
    }
}
