use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// Sandbox profile specifying network and filesystem boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxProfile {
    #[serde(default = "default_allow_network")]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_fs_read: Vec<String>,
    #[serde(default)]
    pub allow_fs_write: Vec<String>,
}

fn default_allow_network() -> bool {
    true
}

impl Default for SandboxProfile {
    fn default() -> Self {
        Self {
            allow_network: true,
            allow_fs_read: Vec::new(),
            allow_fs_write: Vec::new(),
        }
    }
}

/// Sandbox policy checker that enforces SandboxProfile limits on tool calls.
pub struct SandboxPolicy {
    pub profile: Option<SandboxProfile>,
}

impl SandboxPolicy {
    pub fn new() -> Self {
        Self { profile: None }
    }

    pub fn with_profile(profile: SandboxProfile) -> Self {
        Self {
            profile: Some(profile),
        }
    }

    pub fn check(&self, call: &ToolCall) -> Option<GovernanceVerdict> {
        let Some(ref profile) = self.profile else {
            return None;
        };

        let name_lower = call.name.to_lowercase();

        // 1. Network check
        if !profile.allow_network {
            let is_network_tool = name_lower.contains("net")
                || name_lower.contains("http")
                || name_lower.contains("fetch")
                || name_lower.contains("download")
                || name_lower.contains("curl")
                || name_lower.contains("request")
                || name_lower.contains("url");

            if is_network_tool {
                return Some(GovernanceVerdict::Deny {
                    stage: "sandbox_policy",
                    reason: format!("tool '{}' blocked: network access disabled by sandbox", call.name),
                });
            }
        }

        // 2. Filesystem check
        // Check if there is any argument that specifies a file path or directory.
        let path_keys = ["path", "filename", "dir", "directory", "filepath", "dest", "src", "target"];
        let mut target_paths = Vec::new();

        if let serde_json::Value::Object(ref args) = call.arguments {
            for key in path_keys {
                if let Some(val) = args.get(key) {
                    if let Some(path_str) = val.as_str() {
                        target_paths.push(path_str);
                    }
                }
            }
        }

        if !target_paths.is_empty() {
            let is_write = name_lower.contains("write")
                || name_lower.contains("delete")
                || name_lower.contains("save")
                || name_lower.contains("create")
                || name_lower.contains("overwrite")
                || name_lower.contains("remove")
                || name_lower.contains("edit")
                || name_lower.contains("update")
                || name_lower.contains("patch")
                || name_lower.contains("append");

            let allowed_dirs = if is_write {
                &profile.allow_fs_write
            } else {
                &profile.allow_fs_read
            };

            for path_str in target_paths {
                let target_path = Path::new(path_str);
                let mut allowed = false;
                for dir_str in allowed_dirs {
                    let dir = Path::new(dir_str);
                    if target_path.starts_with(dir) {
                        allowed = true;
                        break;
                    }
                }
                if !allowed {
                    let op_type = if is_write { "write" } else { "read" };
                    return Some(GovernanceVerdict::Deny {
                        stage: "sandbox_policy",
                        reason: format!(
                            "tool '{}' blocked: {} access to path '{}' is not allowed by sandbox profile",
                            call.name, op_type, path_str
                        ),
                    });
                }
            }
        }

        None
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::new()
    }
}
