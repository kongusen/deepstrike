//! Long-term memory management (Phase 7).
//!
//! Kernel defines memory types and validation rules; SDKs perform I/O and selection.
//! No I/O in this module — pure classification and validation logic.

use serde::{Deserialize, Serialize};

/// Memory kind (4 types, mirroring Claude Code's taxonomy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// User profile: who they are, expertise level, role.
    User,
    /// Behavior preference: what they like/dislike, approved patterns.
    #[serde(rename = "feedback")]
    BehaviorPreference,
    /// Project context: what's happening, milestones, phases.
    Project,
    /// External pointer: where to find things (tickets, docs).
    Reference,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::BehaviorPreference => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }

}

/// Lightweight memory metadata (kernel stores, SDK provides full content).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryMetadata {
    /// Memory slug (unique identifier).
    pub name: String,

    /// One-line description (for index display).
    pub description: String,

    /// Memory kind (optional; kernel infers if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MemoryKind>,

    /// Creation timestamp (for stale warnings).
    #[serde(default)]
    pub created_at: u64,

    /// Last update timestamp.
    #[serde(default)]
    pub updated_at: u64,

    /// Associated session ID (for provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

}

/// Memory write request (SDK → kernel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWriteRequest {
    pub metadata: MemoryMetadata,
    pub content: String,
}

/// Memory query request (kernel → SDK).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    /// Current context summary (for selection).
    pub current_context: String,

    /// Active tools (filter recentTools).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tools: Vec<String>,

    /// Recently surfaced memory IDs (filter alreadySurfaced).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub already_surfaced: Vec<String>,

    /// Return count limit (default: 5).
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_top_k() -> usize { 5 }

impl Default for MemoryQuery {
    fn default() -> Self {
        Self {
            current_context: String::new(),
            active_tools: Vec::new(),
            already_surfaced: Vec::new(),
            top_k: 5,
        }
    }
}

/// Memory retrieval response (SDK → kernel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRetrieval {
    /// Selected memory IDs.
    pub selected_memory_ids: Vec<String>,

    /// Selection rationale (for kernel logging).
    pub selection_rationale: String,
}

/// Memory validation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "error_kind", rename_all = "snake_case")]
pub enum MemoryValidationError {
    MissingRequiredField { field: String },
    ContentTooLarge { size: u32, limit: u32 },
    ForbiddenPattern { pattern: String, reason: String },
    InvalidKind { kind: String },
    NameTooLong { length: usize, limit: usize },
}

/// Memory validation rules (kernel-enforced).
#[derive(Debug, Clone)]
pub struct MemoryValidation {
    pub max_size_bytes: u32,
    pub max_name_length: usize,
    pub required_fields: Vec<String>,
    pub forbidden_patterns: Vec<(String, &'static str)>,
}

impl MemoryValidation {
    /// Validate a memory write request.
    pub fn validate(&self, request: &MemoryWriteRequest) -> Result<(), MemoryValidationError> {
        // Check required fields
        for field in &self.required_fields {
            match field.as_str() {
                "name" if request.metadata.name.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField { field: "name".into() });
                }
                "description" if request.metadata.description.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField { field: "description".into() });
                }
                _ => {}
            }
        }

        // Check name length
        if request.metadata.name.len() > self.max_name_length {
            return Err(MemoryValidationError::NameTooLong {
                length: request.metadata.name.len(),
                limit: self.max_name_length,
            });
        }

        // Check content size
        if request.content.len() > self.max_size_bytes as usize {
            return Err(MemoryValidationError::ContentTooLarge {
                size: request.content.len() as u32,
                limit: self.max_size_bytes,
            });
        }

        // Check forbidden patterns
        for (pattern, reason) in &self.forbidden_patterns {
            if request.content.contains(pattern) {
                return Err(MemoryValidationError::ForbiddenPattern {
                    pattern: pattern.clone(),
                    reason: reason.to_string(),
                });
            }
        }

        Ok(())
    }
}

/// Validate a memory write request with default validation rules.
pub fn validate_memory_write(request: &MemoryWriteRequest) -> Result<(), MemoryValidationError> {
    MemoryValidation::default().validate(request)
}

/// Declarative configuration for the kernel's long-term memory subsystem.
///
/// Installed via the `set_memory_policy` input event (opt-in). When no policy is installed the
/// kernel preserves pre-policy behavior: every `write_memory` is validated with the default rules
/// and `query_memory` uses the requested `top_k` verbatim. Installing a policy makes these knobs
/// authoritative:
/// - `validation_enabled = false` admits every write without validation.
/// - `retrieval_top_k` is an upper bound: the emitted `requested_k` is `min(query.top_k, top_k)`.
/// - `max_content_bytes` / `max_name_length` override the validation size limits when set.
///
/// `memory_path` and `stale_warning_days` are not enforced inside the kernel (the kernel performs
/// no recall I/O); they are carried so the SDK consumes a single authoritative config.
#[derive(Debug, Clone)]
pub struct MemoryPolicy {
    pub memory_path: String,
    pub stale_warning_days: u32,
    pub retrieval_top_k: usize,
    pub validation_enabled: bool,
    pub max_content_bytes: Option<u32>,
    pub max_name_length: Option<usize>,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            memory_path: String::new(),
            stale_warning_days: 2,
            retrieval_top_k: 5,
            validation_enabled: true,
            max_content_bytes: None,
            max_name_length: None,
        }
    }
}

impl MemoryPolicy {
    /// Build the validation rules this policy implies, starting from the kernel defaults and
    /// applying any size / name-length overrides.
    pub fn validation(&self) -> MemoryValidation {
        let mut v = MemoryValidation::default();
        if let Some(bytes) = self.max_content_bytes {
            v.max_size_bytes = bytes;
        }
        if let Some(len) = self.max_name_length {
            v.max_name_length = len;
        }
        v
    }

    /// Clamp a requested retrieval count to this policy's `retrieval_top_k` upper bound.
    pub fn clamp_top_k(&self, requested: usize) -> usize {
        requested.min(self.retrieval_top_k)
    }
}

/// Default validation rules (aligned with Claude Code's "what NOT to store").
impl Default for MemoryValidation {
    fn default() -> Self {
        Self {
            max_size_bytes: 10_000,
            max_name_length: 100,
            required_fields: vec!["name".into(), "description".into()],
            // P13: no baked-in content heuristics — what belongs in memory is host/model
            // judgment. Hosts configure forbidden prefixes via MemoryPolicy when they want them.
            forbidden_patterns: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_kind_labels_correct() {
        assert_eq!(MemoryKind::User.label(), "user");
        assert_eq!(MemoryKind::BehaviorPreference.label(), "feedback");
        assert_eq!(MemoryKind::Project.label(), "project");
        assert_eq!(MemoryKind::Reference.label(), "reference");
    }

    #[test]
    fn validation_passes_for_valid_request() {
        let validation = MemoryValidation::default();
        let request = MemoryWriteRequest {
            metadata: MemoryMetadata {
                name: "test-memory".into(),
                description: "A valid memory".into(),
                ..Default::default()
            },
            content: "This is fine".to_string(),
        };
        assert!(validation.validate(&request).is_ok());
    }

    #[test]
    fn validation_rejects_missing_name() {
        let validation = MemoryValidation::default();
        let request = MemoryWriteRequest {
            metadata: MemoryMetadata {
                name: "".into(),
                description: "Missing name".into(),
                ..Default::default()
            },
            content: "content".to_string(),
        };
        assert!(matches!(
            validation.validate(&request),
            Err(MemoryValidationError::MissingRequiredField { field }) if field == "name"
        ));
    }

    #[test]
    fn validation_rejects_host_configured_forbidden_pattern() {
        // P13: no baked-in content heuristics — the mechanism only bites when a HOST
        // configures forbidden prefixes on its MemoryPolicy.
        let mut validation = MemoryValidation::default();
        assert!(validation.forbidden_patterns.is_empty(), "no defaults");
        validation
            .forbidden_patterns
            .push(("TODO:".into(), "transient tasks do not belong in memory"));
        let request = MemoryWriteRequest {
            metadata: MemoryMetadata {
                name: "bad-memory".into(),
                description: "Contains forbidden pattern".into(),
                ..Default::default()
            },
            content: "TODO: ship it".to_string(),
        };
        assert!(matches!(
            validation.validate(&request),
            Err(MemoryValidationError::ForbiddenPattern { .. })
        ));
    }

    #[test]
    fn validation_rejects_oversized_content() {
        let validation = MemoryValidation::default();
        let request = MemoryWriteRequest {
            metadata: MemoryMetadata {
                name: "huge-memory".into(),
                description: "Too large".into(),
                ..Default::default()
            },
            content: "x".repeat(20_000),
        };
        assert!(matches!(
            validation.validate(&request),
            Err(MemoryValidationError::ContentTooLarge { .. })
        ));
    }

    #[test]
    fn memory_query_defaults_top_k_to_5() {
        let query = MemoryQuery {
            current_context: "test".into(),
            ..Default::default()
        };
        assert_eq!(query.top_k, 5);
    }
}
