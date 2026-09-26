//! §22.13 · the memory authority for a host that holds no live operation.
//!
//! Inside an operation every memory decision is a kernel transition: the model's `write_memory`
//! and `query_memory` syscalls, and the host's `admit_memory_write` command. Some host writes and
//! recalls happen with no operation at all — an explicit `remember`/`recall` call, a session
//! extract after the run has ended — and those are answered here, statelessly, by the very same
//! functions. No SDK carries a copy of the validation rule or computes a recall count itself.

use serde::{Deserialize, Serialize};

use super::config::{MemoryPolicy, ResolvedMemoryPolicy};
use crate::mm::memory::{
    MemoryPromotion, MemoryRecallLifecycle, MemoryRecallPrior, derive_recall_lifecycle,
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
